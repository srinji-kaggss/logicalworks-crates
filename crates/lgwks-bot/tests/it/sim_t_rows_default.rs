//! Seeded sweeps for the default-feature rows: the sweep half of
//! [`t_rows_default`](super::t_rows_default).
//!
//! Each test drives the same public interface as its real journey, once per
//! seed, and [`check_deterministic`] requires the trace to be identical across
//! two passes: a nondeterministic run fails even when every assertion happens
//! to pass. The trace covers dispositions, counts and committed values, never
//! elapsed time, and its hash is the receipt a reader compares rather than a
//! log they must diff.
//!
//! The scenarios use `Host`, `MemoryJournal`, `Checkpoint` and the `gh`
//! decision types directly: the shipped types, never a reimplementation of the
//! contract under test.

#![cfg(feature = "script")]

use crate::scratch::Scratch;
use crate::t_rows_default::effect_key;

use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lgwks_bot::cap::Cap;
use lgwks_bot::domain::gh::{
    CommitId, PrSnapshot, PullRequest, Repository, ReviewPayload, ReviewRecord,
};
#[cfg(not(feature = "process"))]
use lgwks_bot::domain::gh::{Gh, GhError};
use lgwks_bot::gate::GrantSet;
use lgwks_bot::journal::{EffectEvent, EffectJournal, MemoryJournal};
use lgwks_bot::proposal::{Checkpoint, CorrectionKind, EffectNoteKind};
use lgwks_bot::script::{FlowError, Scope, remember};
#[cfg(feature = "ephemeral")]
use lgwks_bot::task::{DefinitionIdentity, RepairError};
use lgwks_bot::task::{Disposition, Host, Report, RequestError, RequestKey, Submission, task};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// How many seeds each sweep covers.
const SEEDS_PER_SWEEP: u64 = 8;

/// FNV-1a over trace lines: stable across runs, processes and platforms, so
/// the determinism assertion compares receipts rather than logs.
///
/// Plain wrapping arithmetic over bytes — no hasher with per-process random
/// state — because a hash that varies by itself would fail the very property
/// it is meant to check.
fn trace_hash(lines: &[String]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for line in lines {
        for byte in line.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Run `sweep` twice, under two pass tags, and require identical traces.
///
/// The two passes share nothing: every sweep opens its own scratch directories
/// per pass, so equal traces prove the seeds decide the outcome rather than
/// leftover state. Returns the trace for the test's own assertions.
fn check_deterministic(
    sweep: impl Fn(&str) -> Result<Vec<String>, Box<dyn Error>>,
) -> Result<Vec<String>, Box<dyn Error>> {
    let first = sweep("a")?;
    let second = sweep("b")?;
    assert_eq!(
        first, second,
        "the trace never depends on the pass, only on the seeds"
    );
    assert_eq!(
        trace_hash(&first),
        trace_hash(&second),
        "and its hash agrees across both passes"
    );
    Ok(first)
}

/// A host over a fresh store beside its scratch directory, so the store
/// outlives every read against it.
fn stored_pair(tag: &str, tenant: &str) -> Result<(Scratch, Host), Box<dyn Error>> {
    let scratch = Scratch::new(tag)?;
    let host = Host::builder(tenant)?.run_store(scratch.path())?.build()?;
    Ok((scratch, host))
}

/// A host over a fresh store and ledger, granting nothing.
///
/// Gated with its callers: only the repair sweeps need a ledger, and ledgers
/// are keyed by run identities the entropy source mints.
#[cfg(feature = "ephemeral")]
fn repairable_pair(tag: &str, tenant: &str) -> Result<(Scratch, Host), Box<dyn Error>> {
    let scratch = Scratch::new(tag)?;
    let host = Host::builder(tenant)?
        .grants(GrantSet::empty())
        .run_store(scratch.path())?
        .repair_ledger(scratch.path())?
        .build()?;
    Ok((scratch, host))
}

// ── T05 ─────────────────────────────────────────────────────────────────────

/// T05 sweep: seed-sized journals always verify with exact counts.
#[test]
fn journal_counts_are_exact_for_every_seed_t05() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let mut journal = MemoryJournal::new();
            let events = 4 + seed;
            for attempt in 1..=events {
                let attempt_no = seed
                    .checked_mul(32)
                    .and_then(|base| base.checked_add(attempt))
                    .ok_or("seed arithmetic stays in range")?;
                let key = effect_key(&attempt_no.to_string(), "1")?;
                journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
            }
            journal.verify()?;
            assert_eq!(
                u64::try_from(journal.committed().len())?,
                events,
                "seed {seed}: the journal holds exactly what the seed appended"
            );
            trace.push(format!("{seed}:{events}"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T13 ─────────────────────────────────────────────────────────────────────

/// T13 sweep: a repair charges exactly its spend for every seeded spend.
///
/// Needs `ephemeral`, with the real journey: ledger controls are keyed by run
/// identities the entropy source mints.
#[cfg(feature = "ephemeral")]
#[test]
fn repairs_charge_exactly_their_spend_for_every_seed_t13() -> TestResult {
    check_deterministic(|pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let (_scratch, host) = repairable_pair(&format!("s13-{pass}-{seed}"), "acme")?;
            let declared = task("emit-metered", move |scope: Scope, value: u32| async move {
                let step = scope.enter("emit")?;
                step.require(&[Cap::new(Cap::NET)])?;
                Ok(value)
            })?;
            let spend = 1 + seed;
            let report: Report<u32> = lgwks_bot::block_on(host.run(&declared, 7u32));
            assert_eq!(
                report.disposition(),
                Disposition::Blocked,
                "seed {seed}: the emit reaches for authority the host does not grant"
            );
            let run_id = report.run_id().ok_or("a stored run names its run id")?;
            let ticket = report
                .repair()
                .ok_or("a blocked run carries a repair ticket")?
                .clone();
            let before = host
                .run_ledger()
                .ok_or("a repairable host keeps a ledger")?
                .control(run_id)
                .ok_or("a blocked run has a control state")?;
            let grant = GrantSet::empty().grant(Cap::new(Cap::NET));
            let repaired: Report<u32> =
                lgwks_bot::block_on(host.repair(&ticket, &grant, &declared, 7u32, spend))?;
            assert_eq!(
                repaired.disposition(),
                Disposition::Succeeded,
                "seed {seed}: the authorized repair settles the run"
            );
            let after = host
                .run_ledger()
                .ok_or("a repairable host keeps a ledger")?
                .control(run_id)
                .ok_or("a repaired run has a control state")?;
            assert_eq!(
                after.spend(),
                before.spend().saturating_add(spend),
                "seed {seed}: the repair charged exactly its spend"
            );
            trace.push(format!("{seed}:{spend}:{}", after.spend()));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T15 ─────────────────────────────────────────────────────────────────────

/// T15 sweep: compatible resumes replay for every seeded value, and resumes
/// under a changed input are typed input drifts that record nothing new.
///
/// Needs `ephemeral`, with the real journey: compatible resumes replay runs
/// the entropy source minted. The input axis, beside the real journey's
/// task-name axis: two axes swept, neither re-proving the other.
#[cfg(feature = "ephemeral")]
#[test]
fn resumes_replay_or_refuse_drift_for_every_seed_t15() -> TestResult {
    const CODEC: &str = "lgwks.bot.s15.v1";
    check_deterministic(|pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let scratch = Scratch::new(&format!("s15-{pass}-{seed}"))?;
            let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
            let entered = Rc::new(Cell::new(0_u32));
            let value = u32::try_from(seed)?.saturating_add(1000);
            let declared = host
                .definition(
                    "replay-me",
                    1,
                    Some(crate::t_rows_default::input_digest_of(value)),
                    1,
                )
                .with_codec(CODEC);
            let counted = crate::t_rows_default::counted_u32_task!("replay-me", &entered)?;
            let first: Report<u32> =
                lgwks_bot::block_on(host.run_under(&declared, &counted, value));
            assert_eq!(first.disposition(), Disposition::Succeeded);
            let run_id = first.run_id().ok_or("a stored run names its run")?;
            assert_eq!(entered.get(), 1);
            drop(host);
            let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
            let replayed: Report<u32> =
                lgwks_bot::block_on(host.resume_under(run_id, &declared, &counted, value));
            assert_eq!(replayed.disposition(), Disposition::Succeeded);
            assert_eq!(replayed.output(), Some(&value));
            assert_eq!(
                entered.get(),
                1,
                "seed {seed}: the replay did not re-record the step"
            );
            let changed = value.saturating_add(1);
            let drifted_input = DefinitionIdentity::new(
                "replay-me",
                1,
                crate::t_rows_default::input_digest_of(changed),
                1,
            )
            .with_codec(CODEC);
            let drifted: Report<u32> =
                lgwks_bot::block_on(host.resume_under(run_id, &drifted_input, &counted, value));
            assert_eq!(
                crate::t_rows_default::incompatible_axis(drifted.error())?,
                "input",
                "seed {seed}: a changed input drifts on the input axis"
            );
            assert_eq!(
                entered.get(),
                1,
                "seed {seed}: the refused resume recorded nothing new"
            );
            trace.push(format!("{seed}:{value}:replayed:input-drift-refused"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T17 ─────────────────────────────────────────────────────────────────────

/// T17 sweep: dropped waiters leave uncertainty and later clients settle the
/// same run, for every seeded key.
#[test]
fn dropped_waiters_settle_under_the_same_run_for_every_seed_t17() -> TestResult {
    check_deterministic(|pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let (_scratch, host) = stored_pair(&format!("s17-{pass}-{seed}"), "acme")?;
            let key = RequestKey::new(&format!("s17-key-{seed}"))?;
            let entered = Arc::new(AtomicBool::new(false));
            let parking = crate::t_rows_default::parking_task!("parking", &entered)?;
            crate::t_rows_default::drive_until_entered(
                Box::pin(host.submit(&key, &parking, 9u32)),
                &entered,
            );
            let uncertain = lgwks_bot::block_on(host.submit(&key, &parking, 9u32))?;
            assert!(
                uncertain.report().is_none(),
                "seed {seed}: an in-flight submission carries no report"
            );
            let in_flight = match uncertain {
                Submission::InFlight(in_flight) => in_flight.run(),
                other => {
                    return Err(format!(
                        "seed {seed}: the dropped waiter leaves uncertainty: {other:?}"
                    )
                    .into());
                }
            };
            let settled_count = Rc::new(Cell::new(0_u32));
            let settler = crate::t_rows_default::counted_u32_task!("settler", &settled_count)?;
            let settled_report: Report<u32> =
                lgwks_bot::block_on(host.resume(in_flight, &settler, 9u32));
            assert_eq!(settled_report.disposition(), Disposition::Succeeded);
            assert_eq!(settled_report.output(), Some(&9));
            assert_eq!(settled_report.run_id(), Some(in_flight));
            assert_eq!(
                settled_count.get(),
                1,
                "seed {seed}: the settling body ran once"
            );
            trace.push(format!("{seed}:uncertain:settled"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T22 ─────────────────────────────────────────────────────────────────────

/// T22 sweep: subject names validate identically for every seeded repository.
///
/// A pure sweep over the constructor, because the row's absence proof is
/// compile-time: what varies by seed is the spelling, and the verdict for a
/// spelling never depends on the pass.
#[test]
fn subject_names_validate_identically_for_every_seed_t22() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let name = format!("acme/svc-{seed}");
            let verdict = Repository::new(&name).map(|repository| repository.as_str().to_owned());
            assert!(verdict.is_ok(), "seed {seed}: a well-formed name validates");
            trace.push(format!("{seed}:{verdict:?}"));
        }
        Ok(trace)
    })?;
    assert!(
        Repository::new("").is_err(),
        "the empty name is refused outside the sweep too"
    );
    Ok(())
}

// ── T23 ─────────────────────────────────────────────────────────────────────

/// T23 sweep: every seeded non-empty subset of the four capabilities comes back
/// as exactly that subset, in reach order.
///
/// No store and no ledger, with the real journey: run identities need the
/// entropy source, so the ticket half stays with the `ephemeral` tests.
#[test]
fn shortfalls_name_exactly_the_seeded_subset_for_every_seed_t23() -> TestResult {
    const CAPS: [&str; 4] = [Cap::NET, Cap::FS, Cap::SYS, Cap::NOTIFY];
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let wanted: Vec<Cap> = CAPS
                .iter()
                .enumerate()
                .filter(|&(index, _)| (seed & (1u64 << index) != 0) || (seed == 0 && index == 0))
                .map(|(_, name)| Cap::new(*name))
                .collect();
            let expected: Vec<String> = CAPS
                .iter()
                .enumerate()
                .filter(|&(index, _)| (seed & (1u64 << index) != 0) || (seed == 0 && index == 0))
                .map(|(_, name)| name.to_string())
                .collect();
            let host = Host::builder("acme")?.grants(GrantSet::empty()).build()?;
            let blunt = task("blunt", move |_scope: Scope, value: u32| async move {
                Ok(value)
            })?
            .requiring(&wanted);
            let report: Report<u32> = lgwks_bot::block_on(host.run(&blunt, 3u32));
            assert_eq!(report.disposition(), Disposition::Blocked);
            let shortfall = report.needs().ok_or("a blocked run names its shortfall")?;
            let named: Vec<String> = shortfall
                .shortages()
                .map(|shortage| shortage.required().as_str().to_owned())
                .collect();
            assert_eq!(
                named, expected,
                "seed {seed}: the shortfall is exactly the seeded subset"
            );
            trace.push(format!("{seed}:{}", named.join("+")));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T24 ─────────────────────────────────────────────────────────────────────

/// T24 sweep: tickets apply once and redeliveries are refused for every seeded
/// need pair.
///
/// Needs `ephemeral`, with the real journey: tickets are keyed by run
/// identities the entropy source mints. The reach names a seeded pair at the
/// root scope, beside the real journey's single step reach: both mint tickets
/// through the same ledger arms, and the pair exercises the partial-grant
/// denial the single reach cannot.
#[cfg(feature = "ephemeral")]
#[test]
fn tickets_apply_once_for_every_seed_t24() -> TestResult {
    const NEEDS: [&str; 4] = [Cap::NET, Cap::FS, Cap::SYS, Cap::NOTIFY];
    check_deterministic(|pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let first_need = NEEDS[usize::try_from(seed)? % NEEDS.len()];
            let second_need = NEEDS[usize::try_from(seed)?.saturating_add(1) % NEEDS.len()];
            let (_scratch, host) = repairable_pair(&format!("s24-{pass}-{seed}"), "acme")?;
            let bonus = u32::try_from(seed)?;
            let declared = task(
                "pair-reach-sweep",
                move |scope: Scope, value: u32| async move {
                    scope.require(&[Cap::new(first_need), Cap::new(second_need)])?;
                    Ok(value.saturating_add(bonus))
                },
            )?;
            let report: Report<u32> = lgwks_bot::block_on(host.run(&declared, 7u32));
            assert_eq!(
                report.disposition(),
                Disposition::Blocked,
                "seed {seed}: the pair reach is Blocked with one ticket"
            );
            let ticket = report
                .repair()
                .ok_or("a blocked run carries a repair ticket")?
                .clone();
            let named: Vec<&str> = ticket.needs().iter().map(Cap::as_str).collect();
            assert_eq!(
                named,
                [first_need, second_need],
                "seed {seed}: the ticket names the whole pair"
            );
            let exact = GrantSet::empty()
                .grant(Cap::new(first_need))
                .grant(Cap::new(second_need));
            let settled: Report<u32> =
                lgwks_bot::block_on(host.repair(&ticket, &exact, &declared, 7u32, 1))?;
            assert_eq!(settled.disposition(), Disposition::Succeeded);
            assert_eq!(settled.output(), Some(&7u32.saturating_add(bonus)));
            let dupe = lgwks_bot::block_on(host.repair(&ticket, &exact, &declared, 7u32, 1));
            assert!(
                matches!(
                    dupe,
                    Err(RepairError::AlreadyApplied) | Err(RepairError::StaleEpoch { .. })
                ),
                "seed {seed}: a redelivered ticket is refused"
            );
            // A grant covering only half the pair is denied, not half-applied.
            let half = GrantSet::empty().grant(Cap::new(first_need));
            let denied = lgwks_bot::block_on(host.repair(&ticket, &half, &declared, 7u32, 1));
            assert!(
                matches!(denied, Err(RepairError::NotAuthorized { .. })),
                "seed {seed}: a partial grant is denied"
            );
            trace.push(format!(
                "{seed}:{first_need}+{second_need}:applied:refused:denied"
            ));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T27 ─────────────────────────────────────────────────────────────────────

/// T27 sweep: seed-built checkpoints round-trip exact.
#[test]
fn checkpoints_round_trip_for_every_seed_t27() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let mut checkpoint = Checkpoint::new();
            checkpoint.complete(&format!("step-{seed}-a"))?;
            checkpoint.complete(&format!("step-{seed}-b"))?;
            if seed % 2 == 0 {
                checkpoint.correct(CorrectionKind::Override, &format!("correction-{seed}"))?;
            } else {
                checkpoint.correct(CorrectionKind::Refusal, &format!("refusal-{seed}"))?;
            }
            checkpoint.observe_effect(&format!("effect-{seed}"), EffectNoteKind::Unknown)?;
            checkpoint.record_evidence(&format!("evidence-{seed}"))?;
            let bytes = checkpoint.to_record()?;
            let back = Checkpoint::from_record(&bytes)?;
            assert!(back.completed(&format!("step-{seed}-a")));
            assert!(back.completed(&format!("step-{seed}-b")));
            assert_eq!(back.unknowns(), 1, "seed {seed}: the unknown stays unknown");
            assert_eq!(back.corrections().len(), 1);
            assert_eq!(back.effects().len(), 1);
            assert_eq!(back.evidence(), checkpoint.evidence());
            trace.push(format!("{seed}:1:{}bytes", bytes.len()));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T30 ─────────────────────────────────────────────────────────────────────

/// T30 sweep: keys reattach, conflicts refuse and tenants stay apart for every
/// seeded key.
#[test]
fn keys_reattach_conflict_and_isolate_for_every_seed_t30() -> TestResult {
    check_deterministic(|pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let scratch = Scratch::new(&format!("s30-{pass}-{seed}"))?;
            let store = lgwks_bot::task::RunStore::open(scratch.path().join("store"))?;
            let alpha = Host::builder("alpha")?.store(store.clone()).build()?;
            let beta = Host::builder("beta")?.store(store.clone()).build()?;
            let entered = Rc::new(Cell::new(0_u32));
            let work = crate::t_rows_default::counted_u32_task!("counted", &entered)?;
            let key = RequestKey::new(&format!("s30-key-{seed}"))?;
            let value = u32::try_from(seed)?.saturating_add(500);
            let first = lgwks_bot::block_on(alpha.submit(&key, &work, value))?;
            assert!(matches!(first, Submission::Executed(_)));
            let run_id = first.run_id().ok_or("a submission names a run")?;
            let dupe = lgwks_bot::block_on(alpha.submit(&key, &work, value))?;
            assert!(matches!(dupe, Submission::Reattached(_)));
            assert_eq!(dupe.run_id(), Some(run_id));
            let conflict = lgwks_bot::block_on(alpha.submit(&key, &work, value.saturating_add(1)));
            assert!(
                matches!(conflict, Err(RequestError::Conflict(_))),
                "seed {seed}: a different payload under one key conflicts"
            );
            let theirs = lgwks_bot::block_on(beta.submit(&key, &work, value))?;
            assert!(matches!(theirs, Submission::Executed(_)));
            assert!(
                theirs.run_id() != Some(run_id),
                "seed {seed}: two tenants never share a request run"
            );
            assert_eq!(
                entered.get(),
                2,
                "seed {seed}: two bodies ran, one per tenant"
            );
            trace.push(format!("{seed}:reattached:conflict:isolated"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T31 ─────────────────────────────────────────────────────────────────────

/// T31 sweep: seeded subjects validate and the runnerless adapter always
/// refuses `NoRunner`.
///
/// The `NoRunner` arm compiles only without `process`, like the real
/// journey: with a runner bound the same call runs a real client. Subject
/// validation runs in both.
#[test]
fn runnerless_calls_refuse_norunner_for_every_seed_t31() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let repository = Repository::new(format!("acme/svc-{seed}"))?;
            let pull = PullRequest::new(repository, 4800 + seed);
            #[cfg(not(feature = "process"))]
            {
                let adapter = Gh::new(pull.repository().clone());
                let snapshot = lgwks_bot::block_on(adapter.snapshot(&pull));
                assert!(
                    matches!(snapshot, Err(GhError::NoRunner)),
                    "seed {seed}: nothing executes without a runner"
                );
            }
            #[cfg(feature = "process")]
            {
                let _ = &pull;
            }
            trace.push(format!("{seed}:validated"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T32 ─────────────────────────────────────────────────────────────────────

/// T32 sweep: seeded head pairs detect moves exactly when the shas differ.
#[test]
fn moves_detect_exactly_when_shas_differ_for_every_seed_t32() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let head_a: String = format!("a{seed:039}");
            let head_b: String = if seed % 2 == 0 {
                head_a.clone()
            } else {
                format!("b{seed:039}")
            };
            let base = format!("c{seed:039}");
            let read: PrSnapshot = lgwks_std::json::from_str(&format!(
                r#"{{"number":{},"head":{{"sha":"{}"}},"base":{{"sha":"{}"}}}}"#,
                4800 + seed,
                head_a,
                base
            ))?;
            let current: PrSnapshot = lgwks_std::json::from_str(&format!(
                r#"{{"number":{},"head":{{"sha":"{}"}},"base":{{"sha":"{}"}}}}"#,
                4800 + seed,
                head_b,
                base
            ))?;
            let moved = read.head_sha() != current.head_sha();
            assert_eq!(
                moved,
                seed % 2 != 0,
                "seed {seed}: a move is detected exactly when the shas differ"
            );
            trace.push(format!("{seed}:moved={moved}"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T33 ─────────────────────────────────────────────────────────────────────

/// T33 sweep: seeded bodies verify exactly when they match the whole payload.
#[test]
fn bodies_verify_exactly_when_they_match_for_every_seed_t33() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let sha = format!("d{seed:039}");
            let subject = CommitId::new(&sha)?;
            let intended = ReviewPayload::new(&subject, "COMMENT", format!("review-{seed}"), "m")?;
            let exact =
                ReviewRecord::new(70 + seed, subject.as_str(), "COMMENTED", intended.body());
            assert!(
                exact.matches(&intended),
                "seed {seed}: the exact read-back verifies"
            );
            let other = ReviewRecord::new(
                70 + seed,
                subject.as_str(),
                "COMMENTED",
                &format!("other-{seed}"),
            );
            assert!(
                !other.matches(&intended),
                "seed {seed}: a different body never verifies"
            );
            trace.push(format!("{seed}:verified:unmatched"));
        }
        Ok(trace)
    })?;
    Ok(())
}

// ── T34 ─────────────────────────────────────────────────────────────────────

/// T34 sweep: seeded records prove their exact identities.
#[test]
fn records_prove_their_exact_identities_for_every_seed_t34() -> TestResult {
    check_deterministic(|_pass| {
        let mut trace = Vec::new();
        for seed in 0..SEEDS_PER_SWEEP {
            let sha = format!("e{seed:039}");
            let record = ReviewRecord::new(
                8000 + seed,
                &sha,
                if seed % 2 == 0 {
                    "APPROVED"
                } else {
                    "CHANGES_REQUESTED"
                },
                &format!("body-{seed}"),
            );
            let commit = record.commit_id().ok_or("a record names its commit")?;
            let body = record.body().ok_or("a record carries its body")?;
            assert_eq!(record.id(), 8000 + seed);
            assert_eq!(commit, sha);
            trace.push(format!("{seed}:{}:{commit}:{body}", record.id()));
        }
        Ok(trace)
    })?;
    Ok(())
}
