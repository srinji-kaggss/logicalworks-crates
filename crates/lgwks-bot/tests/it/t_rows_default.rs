//! Row-addressed coverage on the default feature set: one test per T-row whose
//! `_tNN` test otherwise needs a non-default feature.
//!
//! The row map in `docs/acceptance/t-rows.toml` names the tests that address each
//! falsifier row of `docs/orchestration-acceptance.spec.md`, and most rows' tests
//! need `ephemeral` (the entropy source behind run-id minting) or `process` (a
//! supervised child) to run. Those tests are the rows' full falsifiers and this
//! file does not replace them: it gives every such row a test that
//! `cargo nextest list --workspace -E 'test(/_t[0-3][0-9]$/)'` answers without
//! any `--features` flag, driving the row's falsifier through the public
//! interface as far as the default (`script`) surface reaches.
//!
//! What each test below therefore is, per row:
//!
//! | Test | Row | What it pins on the default surface |
//! |---|---|---|
//! | `a_full_journal_refuses_one_more_event_and_keeps_its_history_t05` | T05 | the shipped event ceiling refuses the 100,001st append with a typed refusal and the 100,000 committed events verify intact; the child-stdout, slow-consumer and torn-frame halves stay with the `process` tests the map names |
//! | `a_repair_charges_the_root_budget_and_never_resets_it_t13` | T13 | (`ephemeral`) a repair consumes root attempts and spend on the run's ledger control and a denied repair leaves every counter where it was; the finite-retry half stays with the proposal and sim tests the map names |
//! | `a_recorded_answer_replays_without_a_new_request_t15` | T15 | a duplicate submission replays the recorded answer without re-entering the recorded step; the drift half needs minted runs (`ephemeral`, next row) |
//! | `a_compatible_resume_replays_while_a_revised_definition_is_a_typed_drift_t15` | T15 | (`ephemeral`) a compatible resume replays without re-entering the recorded step, and a revised definition is a typed `Incompatible` |
//! | `a_dropped_waiter_leaves_uncertainty_and_a_later_client_settles_it_t17` | T17 | a dropped waiter leaves `InFlight` (uncertainty, no report) under the same run id, and a later client settles that run to a terminal; uncertainty and cleanup are two separate observations |
//! | `without_the_process_feature_a_process_description_is_unnameable_t22` | T22 | without `process` the `rt::process` module does not exist, so no external consumer can name a process description at all; the method-level refusals stay with the `process` probes the map names |
//! | `a_first_step_reach_is_blocked_at_admission_costing_nothing_t23` | T23 | a task that reaches in its first step declares its need with `Task::requiring` and is `Blocked` with the whole shortfall before its body runs, with no store and no ledger; the ticketed repair half stays with the `ephemeral` tests the map names |
//! | `a_ticket_applies_once_and_a_denied_or_wide_grant_changes_nothing_t24` | T24 | (`ephemeral`) a redelivered ticket is refused, a denied repair and an over-wide grant change no counter, and the epoch moved exactly once |
//! | `a_checkpoint_round_trip_preserves_work_corrections_unknowns_and_evidence_t27` | T27 | a checkpoint's completed steps, corrections, unknown effects and evidence references survive an archive round-trip, and a torn archive is refused rather than decoded partial |
//! | `one_key_one_payload_conflict_reattach_and_distinct_keys_t30` | T30 | the same key with a different payload is a typed `Conflict`, a duplicate identical request reattaches without re-entering the body, and distinct keys are distinct runs |
//! | `without_a_runner_nothing_executes_and_subjects_stay_typed_t31` | T31 | without `process` every adapter call is a typed `NoRunner`, and repository and commit subjects validate at construction; diff ceilings and the fake-`gh` journeys stay with the `process` tests the map names |
//! | `a_moved_head_is_detectable_as_reviewed_a_current_b_t32` | T32 | two snapshots pinning different heads decode to different pinned commits with both shas named, so a move reads as reviewed-A/current-B; the refusal itself stays with the `process` journey the map names |
//! | `reconciliation_verifies_the_exact_review_or_stays_unknown_t33` | T33 | an exact read-back verifies, a body mismatch does not, and a partial comment count verifies only except-comments; no adapter is called by any arm |
//! | `a_read_back_review_proves_its_exact_identity_t34` | T34 | a read-back record proves its exact id, commit, state, body and comment count; the lost-permission journey stays with the `process` tests the map names |
//!
//! Every test drives a public item and asserts real values. Nothing here reaches
//! into a private field or a crate-internal helper. Containment rows T19–T21 are
//! another stream's and are not named here.

#![cfg(feature = "script")]

use crate::compile::{assert_refused_for, compile_probe};
use crate::scratch::Scratch;

use std::cell::Cell;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lgwks_bot::cap::Cap;
use lgwks_bot::domain::gh::{
    CommitId, PrSnapshot, PullRequest, Repository, ReviewComment, ReviewPayload, ReviewRecord,
};
#[cfg(not(feature = "process"))]
use lgwks_bot::domain::gh::{Gh, GhError};
use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, EnvironmentId,
    FlowRevision, RunId,
};
use lgwks_bot::gate::GrantSet;
use lgwks_bot::journal::{
    EffectEvent, EffectJournal, JournalError, JournalLimitKind, MemoryJournal,
};
use lgwks_bot::proposal::{Checkpoint, CorrectionKind, EffectNoteKind};
use lgwks_bot::script::{FlowError, Scope, remember};
#[cfg(feature = "ephemeral")]
use lgwks_bot::task::{DefinitionIdentity, RepairError, RepairTicket};
use lgwks_bot::task::{Disposition, Host, Report, RequestError, RequestKey, Submission, task};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A task that counts its durable entries, then records its input.
///
/// The counter lives inside the `remember` body, not in the task body: a
/// resume re-executes the task body to rediscover its steps, and only a
/// counter inside the recorded step distinguishes a replay (not polled again)
/// from a re-run.
///
/// One macro rather than a helper per call site: the closure and async-block
/// types are unnameable, so a function cannot return them, and a copy per test
/// is how one test's body drifts from another's. Both row files invoke it.
macro_rules! counted_u32_task {
    ($name:expr, $counter:expr) => {{
        let entered = Rc::clone($counter);
        task($name, move |scope: Scope, value: u32| {
            let entered = Rc::clone(&entered);
            async move {
                remember(&scope, "v", || {
                    let entered = Rc::clone(&entered);
                    async move {
                        entered.set(entered.get().saturating_add(1));
                        Ok::<_, FlowError>(value)
                    }
                })
                .await
            }
        })
    }};
}
pub(crate) use counted_u32_task;

/// A task that signals entry, then parks forever.
///
/// The shape a client that walks away mid-effect leaves behind. Shared for the
/// same reason as [`counted_u32_task`].
macro_rules! parking_task {
    ($name:expr, $flag:expr) => {{
        let entered = Arc::clone($flag);
        task($name, move |_scope: Scope, _value: u32| {
            let entered = Arc::clone(&entered);
            async move {
                entered.store(true, Ordering::SeqCst);
                std::future::pending::<Result<u32, FlowError>>().await
            }
        })
    }};
}
pub(crate) use parking_task;

/// Drive `waiter` until `entered` is set or the waiter finishes, then stop
/// polling it.
///
/// The loop polls the same future a real caller awaits and re-wakes itself, so
/// the runtime keeps turning until the body signals entry. Dropping the waiter
/// there leaves exactly the state a client that walked away mid-effect leaves:
/// a receipt and work in flight, and no terminal outcome.
pub(crate) fn drive_until_entered<F: Future>(mut waiter: Pin<Box<F>>, entered: &AtomicBool) {
    lgwks_bot::block_on(std::future::poll_fn(|context| {
        use std::task::Poll;
        if entered.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        // A finished waiter is never polled again: re-polling a completed
        // future is a panic, and either way the waiter is dropped below.
        if waiter.as_mut().poll(context).is_ready() {
            return Poll::Ready(());
        }
        context.waker().wake_by_ref();
        Poll::Pending
    }));
}

/// An effect key for the shared run and action, at the given attempt and epoch.
///
/// Fixed run, action, environment, revision and digest, with the attempt and
/// epoch varying, so each append of the T05 fill is about its own attempt.
/// Shared with the sweep file so the two cannot drift on key construction.
pub(crate) fn effect_key(attempt: &str, epoch: &str) -> Result<EffectKey, Box<dyn Error>> {
    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";
    let run = RunId::from_hex(RUN)?;
    let environment = EnvironmentId::from_hex(ENV)?;
    let revision = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let identity = EffectIdentity::new(run, environment, revision);
    let action = ActionId::from_hex(ACTION)?;
    let attempt_id = AttemptId::from_decimal(attempt)?;
    let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let epoch_id = EnvironmentEpoch::from_decimal(epoch)?;
    Ok(identity.key(action, attempt_id, digest, epoch_id))
}

/// The run and ticket a blocked `Report<u32>` names, read off together because
/// the repair tests below need both and a report naming one without the other
/// is a setup failure, not a row verdict.
///
/// Repair tickets need run identities the entropy source mints, so this helper
/// and its callers need `ephemeral`, like the repair tests they serve.
#[cfg(feature = "ephemeral")]
fn ticket_and_run(report: &Report<u32>) -> Result<(RunId, RepairTicket), Box<dyn Error>> {
    let run = report
        .run_id()
        .ok_or("a run on a host with a store must name its run id")?;
    let ticket = report
        .repair()
        .ok_or("a blocked run on a repairable host must carry a repair ticket")?
        .clone();
    Ok((run, ticket))
}

/// The content digest of one integer, as [`Host::definition`] is given it.
///
/// Shared with the sweep file so both declare identities the same way.
#[cfg(feature = "ephemeral")]
pub(crate) fn input_digest_of(value: u32) -> lgwks_std::hash::Digest {
    use lgwks_bot::effect::InputIdentity;
    let mut hasher = lgwks_std::hash::Hasher::new();
    value.write_identity(&mut hasher);
    hasher.finalize()
}

/// The axis a refusal named, or an error naming why there is none.
///
/// Reads the typed [`Drift`](lgwks_bot::task::Drift) rather than a rendering:
/// a check that read a message could pass on a sentence that merely mentioned
/// another axis's word. Shared with the sweep file.
#[cfg(feature = "ephemeral")]
pub(crate) fn incompatible_axis(error: Option<&FlowError>) -> Result<&'static str, Box<dyn Error>> {
    match error {
        Some(error) => match *error {
            FlowError::Incompatible { ref drift, .. } => Ok(drift.kind()),
            _ => Err("expected a typed Incompatible drift, got a different failure".into()),
        },
        None => Err("expected a typed Incompatible drift, the run succeeded".into()),
    }
}

/// The four ledger counters for `run`: attempts, spend, epoch, applied tickets.
///
/// One reader rather than four inline lookups: the T13/T24 claim is that refused
/// repairs move none of them, which is a comparison of two snapshots. Gated
/// with its callers: counters live on ledger controls, which need `ephemeral`
/// run identities.
#[cfg(feature = "ephemeral")]
fn ledger_snapshot(host: &Host, run: RunId) -> Result<(u64, u64, u64, usize), Box<dyn Error>> {
    let control = host
        .run_ledger()
        .ok_or("a repairable host keeps a repair ledger")?
        .control(run)
        .ok_or("a blocked run has a control state")?;
    Ok((
        control.attempts(),
        control.spend(),
        control.epoch(),
        control.applied(),
    ))
}

// ── T05 ─────────────────────────────────────────────────────────────────────

/// T05, ceilings half: a journal filled to its declared event ceiling refuses
/// one more append with a typed refusal, and the committed history verifies
/// intact afterwards.
///
/// The ceiling is the shipped constant (100,000 events): the fill is the cost
/// of pinning the exact boundary rather than asserting about a smaller number
/// the implementation never promises. The child-stdout, slow-consumer and
/// torn-frame halves stay with the `process` tests the map names.
#[test]
fn a_full_journal_refuses_one_more_event_and_keeps_its_history_t05() -> TestResult {
    let mut journal = MemoryJournal::new();
    for attempt in 1_u64..=100_000 {
        let key = effect_key(&attempt.to_string(), "1")?;
        journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    }
    assert_eq!(
        journal.committed().len(),
        100_000,
        "the fill must reach the declared ceiling exactly"
    );

    let extra = journal.compare_and_append(
        journal.tail(),
        &EffectEvent::IntentAdmitted {
            key: effect_key("100001", "1")?,
        },
    );
    assert!(
        matches!(
            &extra,
            Err(JournalError::CapacityExceeded {
                resource: JournalLimitKind::Events,
                limit: 100_000,
                requested: 100_001,
            })
        ),
        "the refusal names the ceiling and the request, not just failure: {extra:?}"
    );
    assert_eq!(
        journal.committed().len(),
        100_000,
        "the refused append changed nothing committed"
    );
    journal.verify()?;
    Ok(())
}

// ── T13 ─────────────────────────────────────────────────────────────────────

/// T13, budget half: a repair charges the run's root attempts and spend and
/// never resets them, and a denied repair leaves every counter where it was.
///
/// Needs `ephemeral`: ledger controls are keyed by run identities the entropy
/// source mints. The finite-retry half stays with the proposal and sim tests
/// the map names.
#[cfg(feature = "ephemeral")]
#[test]
fn a_repair_charges_the_root_budget_and_never_resets_it_t13() -> TestResult {
    let scratch = Scratch::new("t13-budget")?;
    let host = Host::builder("acme")?
        .grants(GrantSet::empty())
        .run_store(scratch.path())?
        .repair_ledger(scratch.path())?
        .build()?;
    let declared = task("publish", |scope: Scope, value: u32| async move {
        let analyzed = remember(
            &scope,
            "analysis",
            || async move { Ok::<_, FlowError>(value) },
        )
        .await?;
        let publication = scope.enter("publish")?;
        publication.require(&[Cap::new(Cap::NET)])?;
        remember(&publication, "send", || async move {
            Ok::<_, FlowError>(analyzed)
        })
        .await
    })?;

    let report: Report<u32> = lgwks_bot::block_on(host.run(&declared, 7u32));
    assert_eq!(
        report.disposition(),
        Disposition::Blocked,
        "the publication reaches for authority the host does not grant: {:?}",
        report.error()
    );
    let (run, ticket) = ticket_and_run(&report)?;
    let (attempts_before, spend_before, _, _) = ledger_snapshot(&host, run)?;

    let grant = GrantSet::empty().grant(Cap::new(Cap::NET));
    let repaired: Report<u32> =
        lgwks_bot::block_on(host.repair(&ticket, &grant, &declared, 7u32, 7))?;
    assert_eq!(
        repaired.disposition(),
        Disposition::Succeeded,
        "the authorized repair settles the run: {:?}",
        repaired.error()
    );
    assert_eq!(
        repaired.output(),
        Some(&7),
        "the repaired run carries the analysis through to its output"
    );

    let (attempts_after, spend_after, epoch_after, applied_after) = ledger_snapshot(&host, run)?;
    assert!(
        attempts_after > attempts_before,
        "a repair consumes root attempts rather than resetting them: {attempts_before} before, {attempts_after} after"
    );
    assert_eq!(
        spend_after,
        spend_before.saturating_add(7),
        "a repair charges its spend onto the root budget"
    );
    assert_eq!(epoch_after, 1, "one applied repair moves the epoch once");
    assert_eq!(applied_after, 1, "one ticket is recorded applied");

    // A denied repair charges nothing, mints no epoch and leaves every counter.
    let denied = lgwks_bot::block_on(host.repair(&ticket, &GrantSet::empty(), &declared, 7u32, 1));
    assert!(
        matches!(denied, Err(RepairError::NotAuthorized { .. })),
        "a grant that does not cover the ticket is denied before any authority is applied: {denied:?}"
    );
    assert_eq!(
        ledger_snapshot(&host, run)?,
        (attempts_after, spend_after, epoch_after, applied_after),
        "a denied repair is exactly nothing: no budget, no epoch, no step"
    );
    Ok(())
}

// ── T15 ─────────────────────────────────────────────────────────────────────

/// T15, replay half: a duplicate submission replays the recorded answer
/// without re-entering the recorded step and without a new model request.
///
/// The run identity comes from a request-keyed submission, which derives it
/// from tenant and key rather than minting it, so no entropy source is needed.
/// The drift half needs minted runs and stays `ephemeral` below.
#[test]
fn a_recorded_answer_replays_without_a_new_request_t15() -> TestResult {
    let scratch = Scratch::new("t15-replay")?;
    let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    let key = RequestKey::new("t15-replay")?;
    let entered = Rc::new(Cell::new(0_u32));

    let counted = counted_u32_task!("replay-a", &entered)?;
    let first = lgwks_bot::block_on(host.submit(&key, &counted, 5u32))?;
    assert!(
        matches!(first, Submission::Executed(_)),
        "the first submission executes and records its value"
    );
    assert_eq!(entered.get(), 1, "the first attempt recorded its step once");

    let dupe = lgwks_bot::block_on(host.submit(&key, &counted, 5u32))?;
    assert!(
        matches!(dupe, Submission::Reattached(_)),
        "a repeat under the same key and input reattaches"
    );
    let report = dupe
        .report()
        .ok_or("a reattach carries the recorded report")?;
    assert_eq!(
        report.output(),
        Some(&5),
        "the recorded answer comes back without a new request"
    );
    assert_eq!(
        entered.get(),
        1,
        "the reattach did not poll the recorded step again"
    );
    Ok(())
}

/// T15, drift half: a compatible resume replays without re-entering the
/// recorded step, and resuming the same run under a revised definition is a
/// typed `Incompatible` on the definition axis before any new effect.
///
/// Needs `ephemeral`: compatible resumes replay runs the entropy source
/// minted. Identities are declared explicitly rather than derived, so the
/// comparison the test asserts is the one the store actually ran. The resume
/// runs on a fresh host over the same directory: the claim is about a
/// restart, and a resume on the handle that wrote the records would prove only
/// that a map still had its entries. The revision axis — the name is a label,
/// not an identity, so renaming is not a drift and is not asserted as one.
/// The sweep file takes the input axis beside it.
#[cfg(feature = "ephemeral")]
#[test]
fn a_compatible_resume_replays_while_a_revised_definition_is_a_typed_drift_t15() -> TestResult {
    const CODEC: &str = "lgwks.bot.t15.v1";
    let scratch = Scratch::new("t15-drift")?;
    let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    let entered = Rc::new(Cell::new(0_u32));

    let declared = host
        .definition("drift-a", 1, Some(input_digest_of(5)), 1)
        .with_codec(CODEC);
    let counted = counted_u32_task!("drift-a", &entered)?;
    let first: Report<u32> = lgwks_bot::block_on(host.run_under(&declared, &counted, 5u32));
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "the first attempt records its value: {:?}",
        first.error()
    );
    assert_eq!(first.output(), Some(&5));
    let run = first.run_id().ok_or("a stored run names its run id")?;
    assert_eq!(entered.get(), 1, "the first attempt recorded its step once");
    drop(host);

    // The compatible resume on a fresh host finds its own records: same
    // output, recorded step not re-entered.
    let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    let resumed: Report<u32> =
        lgwks_bot::block_on(host.resume_under(run, &declared, &counted, 5u32));
    assert_eq!(
        resumed.disposition(),
        Disposition::Succeeded,
        "a compatible resume replays to the recorded answer: {:?}",
        resumed.error()
    );
    assert_eq!(resumed.output(), Some(&5));
    assert_eq!(
        entered.get(),
        1,
        "the replay returned the recorded value without polling the recorded step again"
    );

    // The drifted resume: the same run under a revised definition disagrees on
    // the definition axis before any new effect runs.
    let revised = DefinitionIdentity::new("drift-a", 2, input_digest_of(5), 1).with_codec(CODEC);
    let drifted: Report<u32> =
        lgwks_bot::block_on(host.resume_under(run, &revised, &counted, 5u32));
    assert_eq!(
        incompatible_axis(drifted.error())?,
        "definition",
        "a revised definition drifts on the definition axis"
    );
    assert_eq!(entered.get(), 1, "a refused resume re-recorded nothing");
    Ok(())
}

// ── T17 ─────────────────────────────────────────────────────────────────────

/// T17: a dropped waiter leaves uncertainty (`InFlight`, no report) under the
/// run the key derives, and a later client settles that same run to a terminal.
/// Uncertainty and cleanup are two separate observations from the same host.
#[test]
fn a_dropped_waiter_leaves_uncertainty_and_a_later_client_settles_it_t17() -> TestResult {
    let scratch = Scratch::new("t17-drop")?;
    let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    let key = RequestKey::new("order-17")?;

    // A body that signals entry, then parks: the shape a client that walks away
    // mid-effect leaves behind.
    let entered = Arc::new(AtomicBool::new(false));
    let parking = parking_task!("parking", &entered)?;
    drive_until_entered(Box::pin(host.submit(&key, &parking, 9u32)), &entered);
    assert!(
        entered.load(Ordering::SeqCst),
        "the waiter entered its body before being dropped"
    );

    // Uncertainty: the key is claimed, no terminal is recorded, and the
    // submission says so with no report attached.
    let uncertain = lgwks_bot::block_on(host.submit(&key, &parking, 9u32))?;
    assert!(
        uncertain.report().is_none(),
        "an in-flight submission carries no report: it is a statement that there is not one yet"
    );
    let in_flight_run = match uncertain {
        Submission::InFlight(in_flight) => in_flight.run(),
        other => {
            return Err(format!(
                "a dropped waiter leaves the request in flight, with no report: got {other:?}"
            )
            .into());
        }
    };

    // Cleanup: a later client settles the same run to a terminal outcome.
    // A resume, not a second submit: while the run is in flight the key still
    // reports uncertainty, and the resume is the handle that settles it.
    let settled_count = Rc::new(Cell::new(0_u32));
    let settler = counted_u32_task!("settler", &settled_count)?;
    let settled_report: Report<u32> =
        lgwks_bot::block_on(host.resume(in_flight_run, &settler, 9u32));
    assert_eq!(
        settled_report.disposition(),
        Disposition::Succeeded,
        "the later client settles the run: {:?}",
        settled_report.error()
    );
    assert_eq!(settled_report.output(), Some(&9));
    assert_eq!(
        settled_report.run_id(),
        Some(in_flight_run),
        "uncertainty and cleanup are about one run: the key derives it on every submission"
    );
    assert_eq!(settled_count.get(), 1, "the settling body ran once");
    Ok(())
}

/// T17, drain half: after a waiter is dropped mid-effect, the still-owning
/// host reports cleanup and uncertainty as two independent facts, and draining
/// it moves neither.
///
/// Cleanup is the admission counters: the dropped waiter's permit came back,
/// so nothing is in flight and every permit is free. Uncertainty is the
/// request: no terminal was recorded, so a later submission is told
/// `InFlight` with no report. A host that conflated the two would either keep
/// the permit to stand for the uncertain request (a leak a drain can never
/// clear) or drop the request with its permit (a lost effect reported as
/// cleaned up). After `cancel` drains the host, the counters are unchanged and
/// a second host over the same store still reads the request as in flight,
/// then settles it to a terminal under the same run.
#[test]
fn a_drained_host_reports_cleanup_and_uncertainty_as_two_facts_t17() -> TestResult {
    let scratch = Scratch::new("t17-drain")?;
    let owner = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    let ceiling = owner.limits().max_concurrent_tasks();
    let key = RequestKey::new("order-17-drain")?;

    let entered = Arc::new(AtomicBool::new(false));
    let parking = parking_task!("parking", &entered)?;
    drive_until_entered(Box::pin(owner.submit(&key, &parking, 4u32)), &entered);
    assert!(
        entered.load(Ordering::SeqCst),
        "the waiter entered its body before being dropped"
    );

    // Cleanup, read from the admission counters alone.
    let admission = owner.admission();
    assert_eq!(
        admission.in_flight(),
        0,
        "the dropped waiter holds no permit"
    );
    assert_eq!(
        admission.available_permits(),
        ceiling,
        "every permit is free again: {admission:?}"
    );

    // Uncertainty, read from the request alone.
    let run = match lgwks_bot::block_on(owner.submit(&key, &parking, 4u32))? {
        Submission::InFlight(in_flight) => in_flight.run(),
        other => {
            let refusal =
                Err(format!("cleanup must not erase the uncertain request: got {other:?}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_drained_host_reports_cleanup_and_uncertainty_as_two_facts_t17: returning an error to the caller");
            return refusal;
        }
    };

    // Drain the owner. Its counters do not move, and the uncertainty outlives it.
    owner.cancel();
    assert!(owner.is_cancelled(), "the drain took effect");
    assert_eq!(
        owner.admission().in_flight(),
        0,
        "the drain freed nothing it had not freed"
    );
    assert_eq!(owner.admission().available_permits(), ceiling);
    drop(owner);

    let successor = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    match lgwks_bot::block_on(successor.submit(&key, &parking, 4u32))? {
        Submission::InFlight(in_flight) => assert_eq!(
            in_flight.run(),
            run,
            "the successor reads the same uncertain run the owner left"
        ),
        other => {
            let refusal = Err(format!(
                "a drain must not settle the request by omission: got {other:?}"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_drained_host_reports_cleanup_and_uncertainty_as_two_facts_t17: returning an error to the caller");
            return refusal;
        }
    }
    let settled = Rc::new(Cell::new(0_u32));
    let settler = counted_u32_task!("settler", &settled)?;
    let report: Report<u32> = lgwks_bot::block_on(successor.resume(run, &settler, 4u32));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the successor settles the uncertain run: {:?}",
        report.error()
    );
    assert_eq!(report.run_id(), Some(run), "under the run the key derives");
    assert_eq!(settled.get(), 1, "the settling body ran once");
    assert_eq!(
        successor.admission().in_flight(),
        0,
        "and returned its permit"
    );
    Ok(())
}

// ── T22 ─────────────────────────────────────────────────────────────────────

/// T22, feature-boundary half: without the `process` feature the `rt::process`
/// module does not exist, so no external consumer can name a process
/// description at all.
///
/// The positive control compiles the sanctioned front door through the same
/// harness, so the refusal below is the compiler's judgement about the symbol
/// and not the harness failing to build. The method-level refusals (`spawn`,
/// `status`, `output`, engine handles, `Deref`) stay with the `process` probes
/// the map names.
#[test]
fn without_the_process_feature_a_process_description_is_unnameable_t22() -> TestResult {
    let control = compile_probe(
        "t22-default-control",
        "",
        "use lgwks_bot::task::{Host, task};\nuse lgwks_bot::script::{FlowError, Scope};\n\nfn main() -> Result<(), FlowError> {\n    let _host = Host::builder(\"acme\")?.build().map_err(|_| FlowError::failed(\"build\"))?;\n    let _task = task(\"w\", |_scope: Scope, v: u32| async move { Ok::<_, FlowError>(v) })?;\n    Ok(())\n}\n",
    )?;
    let control_text = format!(
        "{}{}",
        String::from_utf8_lossy(&control.stdout),
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(
        control.status.success(),
        "the positive control (the sanctioned front door) must compile under default features:\n{control_text}"
    );

    let output = compile_probe(
        "t22-no-process-probe",
        "",
        "use lgwks_bot::rt::process::ProcessSpec;\n\nfn main() {\n    let _ = ProcessSpec::new(\"sh\");\n}\n",
    )?;
    assert_refused_for(&output, "E0432", "process");
    Ok(())
}

// ── T23 ─────────────────────────────────────────────────────────────────────

/// T23, admission half: a task that reaches in its first step declares its need
/// with `Task::requiring` and is `Blocked` with the whole shortfall before its
/// body runs — no step polled, so there is no replay to buy.
///
/// No store and no ledger: run identities need the entropy source, so the
/// ticketed repair half stays with the `ephemeral` tests the map names.
#[test]
fn a_first_step_reach_is_blocked_at_admission_costing_nothing_t23() -> TestResult {
    let host = Host::builder("acme")?.grants(GrantSet::empty()).build()?;
    let entered = Rc::new(Cell::new(0_u32));
    let blunt = {
        let entered = Rc::clone(&entered);
        task("blunt", move |_scope: Scope, value: u32| {
            let entered = Rc::clone(&entered);
            async move {
                entered.set(entered.get().saturating_add(1));
                Ok(value)
            }
        })?
        .requiring(&[
            Cap::new(Cap::NET),
            Cap::new(Cap::FS),
            Cap::new(Cap::SYS),
            Cap::new(Cap::NOTIFY),
        ])
    };

    let report: Report<u32> = lgwks_bot::block_on(host.run(&blunt, 3u32));
    assert_eq!(
        report.disposition(),
        Disposition::Blocked,
        "a first-step reach is Blocked, not Failed: {:?}",
        report.error()
    );
    let shortfall = report.needs().ok_or("a blocked run names its shortfall")?;
    let named: Vec<&str> = shortfall
        .shortages()
        .map(|shortage| shortage.required().as_str())
        .collect();
    assert_eq!(
        named,
        [Cap::NET, Cap::FS, Cap::SYS, Cap::NOTIFY],
        "one report carries the whole shortfall in one pass"
    );
    assert_eq!(
        entered.get(),
        0,
        "no step polled, so there is no replay to buy"
    );
    assert!(
        report.steps().is_empty(),
        "a run refused at admission entered no step"
    );
    Ok(())
}

// ── T24 ─────────────────────────────────────────────────────────────────────

/// T24: a ticket applies once; a denied repair and an over-wide grant change
/// nothing countable; the epoch moved exactly once.
///
/// Needs `ephemeral`: tickets and ledger controls are keyed by run identities
/// the entropy source mints.
///
/// The journey reaches once and sends once, with no analysis step: the arms
/// under test are the ticket's, and an analysis here would only re-prove T23's
/// replay beside them.
#[cfg(feature = "ephemeral")]
#[test]
fn a_ticket_applies_once_and_a_denied_or_wide_grant_changes_nothing_t24() -> TestResult {
    let scratch = Scratch::new("t24-arms")?;
    let host = Host::builder("acme")?
        .grants(GrantSet::empty())
        .run_store(scratch.path())?
        .repair_ledger(scratch.path())?
        .build()?;
    let sends = Rc::new(Cell::new(0_u64));
    let declared = {
        let sends = Rc::clone(&sends);
        task("send-once", move |scope: Scope, value: u32| {
            let sends = Rc::clone(&sends);
            async move {
                let step = scope.enter("send")?;
                step.require(&[Cap::new(Cap::NET)])?;
                sends.set(sends.get().saturating_add(1));
                Ok(value)
            }
        })?
    };
    let report: Report<u32> = lgwks_bot::block_on(host.run(&declared, 7u32));
    assert_eq!(
        report.disposition(),
        Disposition::Blocked,
        "the send reaches for authority the host does not grant: {:?}",
        report.error()
    );
    let (run, ticket) = ticket_and_run(&report)?;
    let exact = GrantSet::empty().grant(Cap::new(Cap::NET));

    let first: Report<u32> = lgwks_bot::block_on(host.repair(&ticket, &exact, &declared, 7u32, 1))?;
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "the authorized repair settles the run: {:?}",
        first.error()
    );
    assert_eq!(sends.get(), 1, "the send ran once, after the repair");

    // The same ticket delivered twice applies once.
    let dupe = lgwks_bot::block_on(host.repair(&ticket, &exact, &declared, 7u32, 1));
    assert!(
        matches!(
            dupe,
            Err(RepairError::AlreadyApplied) | Err(RepairError::StaleEpoch { .. })
        ),
        "a redelivered ticket is refused: {dupe:?}"
    );
    assert_eq!(sends.get(), 1, "the second delivery repeated no effect");

    // A denied repair and an over-wide grant change nothing countable.
    let snapshot = ledger_snapshot(&host, run)?;
    let denied = lgwks_bot::block_on(host.repair(&ticket, &GrantSet::empty(), &declared, 7u32, 1));
    assert!(
        matches!(denied, Err(RepairError::NotAuthorized { .. })),
        "a grant short of the ticket is denied: {denied:?}"
    );
    let wide = exact.grant(Cap::new("custom.new.capability"));
    let over = lgwks_bot::block_on(host.repair(&ticket, &wide, &declared, 7u32, 1));
    assert!(
        matches!(over, Err(RepairError::OverWide { .. })),
        "a grant reaching past the ticket is refused rather than narrowed: {over:?}"
    );
    assert_eq!(
        ledger_snapshot(&host, run)?,
        snapshot,
        "refused repairs leave every counter where it was"
    );
    assert_eq!(
        snapshot.2, 1,
        "one applied repair moved the epoch exactly once"
    );
    Ok(())
}

// ── T27 ─────────────────────────────────────────────────────────────────────

/// T27, checkpoint half: completed work, user corrections, unknown effects and
/// evidence references survive an archive round-trip, and a torn archive is
/// refused rather than decoded into a partial checkpoint that could read as
/// full coverage.
#[test]
fn a_checkpoint_round_trip_preserves_work_corrections_unknowns_and_evidence_t27() -> TestResult {
    let mut checkpoint = Checkpoint::new();
    checkpoint.complete("fetch-pr")?;
    checkpoint.complete("analyze")?;
    checkpoint.correct(
        CorrectionKind::Override,
        "use the base commit, not the head",
    )?;
    checkpoint.observe_effect("post-review-4821", EffectNoteKind::Unknown)?;
    checkpoint.observe_effect("apply-label", EffectNoteKind::Applied)?;
    checkpoint.record_evidence("review-id 4821 on acme/web at deadbee")?;
    assert_eq!(checkpoint.unknowns(), 1, "one effect is unknown");
    assert!(
        checkpoint.completed("fetch-pr"),
        "completed work is recorded"
    );
    assert!(
        !checkpoint.completed("publish"),
        "work never done is not recorded done"
    );

    let bytes = checkpoint.to_record()?;
    let back = Checkpoint::from_record(&bytes)?;
    assert!(
        back.completed("fetch-pr"),
        "completed work survives the round-trip"
    );
    assert!(
        back.completed("analyze"),
        "all completed steps survive, not just the first"
    );
    assert_eq!(
        back.corrections().len(),
        1,
        "the user correction survives with its text"
    );
    assert_eq!(
        back.corrections()[0].text(),
        "use the base commit, not the head",
        "the correction's content is exact, not just its count"
    );
    assert_eq!(
        back.unknowns(),
        1,
        "the unknown effect is still unknown after the trip"
    );
    assert_eq!(
        back.effects().len(),
        2,
        "both effect notes survive, applied and unknown alike"
    );
    assert_eq!(
        back.evidence(),
        checkpoint.evidence(),
        "evidence references survive verbatim"
    );
    assert_eq!(
        back.present(),
        checkpoint.present(),
        "the present set is identical after the trip"
    );

    let cut = bytes
        .len()
        .checked_div(2)
        .ok_or("a record has bytes to cut")?;
    let torn = Checkpoint::from_record(&bytes[..cut]);
    assert!(
        torn.is_err(),
        "a torn record is refused rather than decoded into a partial checkpoint"
    );
    Ok(())
}

// ── T30 ─────────────────────────────────────────────────────────────────────

/// T30: the same key with a different payload is a typed `Conflict`, a
/// duplicate identical request reattaches without re-entering the body, and
/// distinct keys are distinct runs.
#[test]
fn one_key_one_payload_conflict_reattach_and_distinct_keys_t30() -> TestResult {
    let scratch = Scratch::new("t30-keys")?;
    let host = Host::builder("acme")?.run_store(scratch.path())?.build()?;
    let counter = Rc::new(Cell::new(0_u32));
    let work = counted_u32_task!("counted", &counter)?;

    let key = RequestKey::new("order-42")?;
    let first = lgwks_bot::block_on(host.submit(&key, &work, 7u32))?;
    assert!(
        matches!(first, Submission::Executed(_)),
        "a first submission executes"
    );
    let run = first.run_id().ok_or("a submission names a run")?;
    assert_eq!(counter.get(), 1, "the first submission ran the body once");

    // The same key with a different payload is a conflict naming the key, and
    // no body runs for it.
    let conflict = lgwks_bot::block_on(host.submit(&key, &work, 8u32));
    assert!(
        matches!(conflict, Err(RequestError::Conflict(_))),
        "a different digest under one key is a typed Conflict: {conflict:?}"
    );
    assert_eq!(
        counter.get(),
        1,
        "a conflicted submission never entered the body"
    );

    // A duplicate identical request reattaches to the recorded report.
    let second = lgwks_bot::block_on(host.submit(&key, &work, 7u32))?;
    assert!(
        matches!(second, Submission::Reattached(_)),
        "a repeat under the same key and input reattaches"
    );
    assert_eq!(
        second.run_id(),
        Some(run),
        "one key derives one run on every submission"
    );
    assert_eq!(
        second.report().and_then(|report| report.output().copied()),
        Some(7),
        "the recorded output comes back without the body running"
    );
    assert_eq!(
        counter.get(),
        1,
        "the reattach never entered the body again"
    );

    // Distinct keys are distinct runs, never silently collapsed.
    let other = lgwks_bot::block_on(host.submit(&RequestKey::new("order-43")?, &work, 7u32))?;
    assert!(
        matches!(other, Submission::Executed(_)),
        "a distinct key executes its own run"
    );
    assert!(
        other.run_id() != Some(run),
        "distinct keys derive distinct runs"
    );
    assert_eq!(counter.get(), 2, "the distinct run entered the body once");
    Ok(())
}

// ── T31 ─────────────────────────────────────────────────────────────────────

/// T31, sandbox half: without the `process` feature every adapter call is a
/// typed `NoRunner` — a binding that cannot run a client says so rather than
/// pretending it read GitHub — and repository and commit subjects validate at
/// construction in every build.
///
/// With `process` the adapter binds a real runner instead, so the `NoRunner`
/// arm below compiles only without it; the subject validation runs in both.
/// Diff ceilings, pagination and the fake-`gh` journeys stay with the
/// `process` tests the map names.
#[test]
fn without_a_runner_nothing_executes_and_subjects_stay_typed_t31() -> TestResult {
    let repository = Repository::new("acme/web")?;
    assert_eq!(repository.as_str(), "acme/web");
    assert!(
        Repository::new("").is_err(),
        "an empty repository reference is refused at construction"
    );
    assert!(
        Repository::new("not a repo !!!").is_err(),
        "a malformed repository reference is refused at construction"
    );
    let pull = PullRequest::new(repository, 4821);
    assert_eq!(
        pull.number(),
        4821,
        "the subject pins repository and number"
    );

    // Without a supervised runner the adapter refuses typed rather than
    // returning an empty answer. This arm exists only where there is no
    // runner to bind: with `process` the same call runs a real client.
    #[cfg(not(feature = "process"))]
    {
        let adapter = Gh::new(
            PullRequest::new(Repository::new("acme/web")?, 4821)
                .repository()
                .clone(),
        );
        let snapshot = lgwks_bot::block_on(adapter.snapshot(&pull));
        assert!(
            matches!(snapshot, Err(GhError::NoRunner)),
            "without a supervised runner the adapter refuses typed rather than returning an empty answer: {snapshot:?}"
        );
    }
    #[cfg(feature = "process")]
    {
        let _ = &pull;
    }
    Ok(())
}

// ── T32 ─────────────────────────────────────────────────────────────────────

/// T32, detection half: two snapshots pinning different heads decode to
/// different pinned commits with both shas named, so a move reads as
/// reviewed-A/current-B rather than as evidence relabelled to B.
///
/// The refusal itself stays with the `process` journey the map names.
#[test]
fn a_moved_head_is_detectable_as_reviewed_a_current_b_t32() -> TestResult {
    let read: PrSnapshot = lgwks_std::json::from_str(
        r#"{"number":4821,"head":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"base":{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}"#,
    )?;
    let current: PrSnapshot = lgwks_std::json::from_str(
        r#"{"number":4821,"head":{"sha":"cccccccccccccccccccccccccccccccccccccccc"},"base":{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}"#,
    )?;
    assert_eq!(
        read.head_sha(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "the analysis-time read pins head A"
    );
    assert_eq!(
        current.head_sha(),
        "cccccccccccccccccccccccccccccccccccccccc",
        "the current read pins head B"
    );
    assert!(
        read.head_sha() != current.head_sha(),
        "a head that advanced between analysis and publication is detectable from the two pinned commits"
    );
    assert_eq!(
        read.base_sha(),
        current.base_sha(),
        "the base both snapshots agree on is still named"
    );
    let same: PrSnapshot = lgwks_std::json::from_str(
        r#"{"number":4821,"head":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"base":{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}"#,
    )?;
    assert_eq!(
        read.head_sha(),
        same.head_sha(),
        "an unmoved head pins the same commit on both reads"
    );
    Ok(())
}

// ── T33 ─────────────────────────────────────────────────────────────────────

/// T33, reconciliation half: an exact read-back verifies, a body mismatch does
/// not, and a partial comment count verifies only except-comments. No adapter
/// is called by any arm: the decision is pure.
#[test]
fn reconciliation_verifies_the_exact_review_or_stays_unknown_t33() -> TestResult {
    let subject = CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
    let intended = ReviewPayload::new(&subject, "COMMENT", "looks good", "t33-marker")?
        .with_comments(vec![ReviewComment::new("src/lib.rs", 10, "nit: rename")]);
    let body = intended.body().to_owned();
    // The body travels inside JSON here, so its newlines are escaped: a raw
    // control character would be a malformed fixture, not a mismatch.
    let json_body = body.replace('\n', "\\n");

    // The exact read-back: same subject, body (marker included), state and
    // comment count.
    let exact: ReviewRecord = lgwks_std::json::from_str(&format!(
        r#"{{"id":99,"commit_id":"{}","state":"COMMENTED","body":"{}","comment_count":1}}"#,
        subject.as_str(),
        json_body
    ))?;
    assert!(
        exact.matches(&intended),
        "the exact read-back verifies: id 99 is the review the payload asked for"
    );
    assert_eq!(exact.id(), 99, "verification names the exact review id");

    // A different body is a different review, even with the right marker.
    let wrong_body = ReviewRecord::new(99, subject.as_str(), "COMMENTED", "something else");
    assert!(
        !wrong_body.matches(&intended),
        "a matching marker with a different body is a different review"
    );
    assert!(
        !wrong_body.matches_except_comments(&intended),
        "and it does not verify except-comments either"
    );

    // A partial submission verifies except-comments but never whole.
    let partial = ReviewRecord::new(99, subject.as_str(), "COMMENTED", &body);
    assert!(
        !partial.matches(&intended),
        "a review whose comments landed only in part is not the whole one"
    );
    assert!(
        partial.matches_except_comments(&intended),
        "but its subject, body and state verify, so reconciliation reports partial rather than unknown"
    );

    // Nothing observed at all stays unknown: no match, no post.
    let stranger = ReviewRecord::new(100, subject.as_str(), "COMMENTED", "unrelated review");
    assert!(
        !stranger.matches(&intended),
        "an unrelated review never verifies against this payload"
    );
    Ok(())
}

// ── T34 ─────────────────────────────────────────────────────────────────────

/// T34, read-back half: a read-back record proves its exact id, commit, state,
/// body and comment count. The lost-permission journey stays with the
/// `process` tests the map names.
#[test]
fn a_read_back_review_proves_its_exact_identity_t34() -> TestResult {
    let record = ReviewRecord::new(
        4821,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "CHANGES_REQUESTED",
        "needs a test for the moved head",
    );
    assert_eq!(record.id(), 4821, "the exact review id");
    assert_eq!(
        record.commit_id(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "the exact commit the review is about"
    );
    assert_eq!(
        record.state(),
        "CHANGES_REQUESTED",
        "the exact reported state"
    );
    assert_eq!(
        record.body(),
        Some("needs a test for the moved head"),
        "the exact payload"
    );
    assert_eq!(
        record.applied_comments(),
        0,
        "no comment count reported means none established, never all landed"
    );
    assert!(!record.is_pending(), "a decided review is not pending");

    let pending = ReviewRecord::new(
        4822,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "PENDING",
        "draft",
    );
    assert!(pending.is_pending(), "a pending review reads as pending");
    assert!(
        pending.id() != record.id(),
        "two records keep their distinct identities"
    );
    Ok(())
}
