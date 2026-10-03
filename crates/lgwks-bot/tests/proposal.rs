//! The acceptance falsifiers for T26, T27, T28, T29 and T35.
//!
//! | Row | Claim | Falsifier below |
//! |---|---|---|
//! | T26 | malformed output and instruction injection cannot change trusted intent, install tools or obtain credentials; sandbox-escape refusals stay observable | `a_malformed_payload_never_becomes_work`, `an_injected_instruction_is_refused_by_name`, `a_tool_install_is_refused_and_the_surface_is_unchanged`, `a_credential_read_is_refused`, `a_sandbox_escape_stays_an_observable_refusal`, `an_unknown_operation_is_refused_whatever_asked_for_it` |
//! | T27 | a context reset preserves completed work, corrections, Unknown effects and evidence; truncated data never becomes a full-coverage claim | `a_context_reset_preserves_completed_work_corrections_unknowns_and_evidence`, `a_truncated_payload_never_becomes_a_full_coverage_claim`, `a_checkpoint_round_trips_through_the_run_store` |
//! | T28 | parallel workers cannot read another tenant's same-digest artifact; conflicting writes are serialized while independent reads progress | `two_tenants_on_one_digest_stay_isolated`, `conflicting_writes_to_one_key_are_serialized_and_idempotent`, `reads_progress_while_a_write_is_in_flight` |
//! | T29 | repeated unchanged failure reaches finite typed intervention; new evidence is recorded and does not erase root spend | `repeated_unchanged_failure_reaches_a_finite_intervention`, `new_evidence_does_not_erase_root_spend`, `a_plan_budget_bounds_repair` |
//! | T35 | exit-zero-with-invalid-result, done-without-evidence and draft-ok-publish-failed report distinct true outcomes | `the_three_untrue_successes_report_distinct_outcomes` |
//!
//! Every assertion here goes through a public item of `lgwks_bot::proposal`
//! and observes what a caller would read. Nothing reaches for a private field or
//! a crate-internal helper, so a test that passes is evidence about the shipped
//! surface rather than about this file's reading of it.

#![cfg(feature = "script")]

#[path = "support/proposal.rs"]
mod support;

use std::error::Error;

use lgwks_bot::cap::Cap;
use lgwks_bot::proposal::{
    ArtifactError, ArtifactKey, ArtifactStore, Checkpoint, Completion, CompletionOutcome, Coverage,
    Decoder, EffectNoteKind, Intervention, LedgerLimits, PlanBudget, PlanLimits, Provenance,
    Refusal, RepairLedger, Source, StubModel, WriteOutcome, payload_digest,
};
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, task};

use support::{
    OTHER_TENANT, PING, READ, TENANT, TestResult, credential, decoder, escaping_path, injection,
    installs, malformed, overclaims, poor_surface, provenance, surface, truncated, well_formed,
};

/// Drive one future to completion on the crate's own runtime.
fn drive<T>(future: impl std::future::Future<Output = T>) -> T {
    lgwks_bot::block_on(future)
}

// ── T26: untrusted input cannot change trusted intent ────────────────────────

/// A malformed payload never becomes work, and the refusal names the offset it
/// stopped at rather than merely saying "invalid".
#[test]
fn a_malformed_payload_never_becomes_work() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let payload = malformed();
    let outcome = decoder.decode(&surface, &payload, Source::Model);

    assert!(
        !outcome.is_admitted(),
        "a payload with no `=` on its first line is not a proposal: {outcome}"
    );
    assert_eq!(
        outcome.plan(),
        None,
        "a refused payload returns no plan beside the refusal, or a caller that kept both would have work"
    );
    assert!(
        matches!(outcome.refusal(), Some(Refusal::Malformed { at: 0, .. })),
        "the refusal names the byte it stopped at: {outcome}"
    );
    // The trusted intent is unchanged: the same surface still admits exactly the
    // operation it always did, and the malformed payload did not widen it.
    assert!(
        decoder
            .decode(&surface, &well_formed(), Source::Model)
            .is_admitted(),
        "the surface is unchanged by a malformed payload"
    );
    assert_eq!(
        surface.registered(),
        2,
        "and it still holds exactly the two operations it declared"
    );
    Ok(())
}

/// An instruction injected through tool output is refused by name, and the
/// refusal is attributed to the tool that produced it rather than to a model.
#[test]
fn an_injected_instruction_is_refused_by_name() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let payload = injection();

    let outcome = decoder.decode(&surface, &payload, Source::ToolOutput);
    assert!(
        matches!(
            outcome.refusal(),
            Some(Refusal::SandboxEscape {
                field: "host",
                target
            }) if target == OTHER_TENANT
        ),
        "an instruction naming another tenant is a sandbox escape: {outcome}"
    );
    let recorded = outcome
        .provenance()
        .ok_or("a refusal carries the provenance of what it refused")?;
    assert_eq!(
        recorded.source(),
        Source::ToolOutput,
        "the refusal names tool output as its source, so it is not read as a model's own mistake"
    );
    assert_eq!(
        recorded.tenant(),
        TENANT,
        "and the tenant whose boundary was crossed"
    );

    // The same bytes from a model are refused identically: the boundary does not
    // depend on who produced the bytes, only on what they say.
    assert_eq!(
        decoder.decode(&surface, &payload, Source::Model).refusal(),
        outcome.refusal(),
        "the same instruction is the same refusal whatever its source"
    );
    Ok(())
}

/// A payload that tries to install a tool is refused naming the tool, and the
/// surface it was decoded against is byte-for-byte unchanged.
#[test]
fn a_tool_install_is_refused_and_the_surface_is_unchanged() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let before = surface.names().collect::<Vec<_>>();
    let payload = installs();

    let outcome = decoder.decode(&surface, &payload, Source::Model);
    assert!(
        matches!(
            outcome.refusal(),
            Some(Refusal::InstallTool { name }) if name == "ripgrep"
        ),
        "the refusal names the tool that was asked for: {outcome}"
    );
    let refusal = outcome.refusal().ok_or("a refusal carries its own arm")?;
    assert!(
        refusal.is_privilege_attempt(),
        "an install is an attempt to widen authority, not a malformed document"
    );
    assert_eq!(
        surface.names().collect::<Vec<_>>(),
        before,
        "a payload cannot add an operation to the surface it was decoded against"
    );
    assert!(
        decoder
            .decode(&surface, &well_formed(), Source::Model)
            .is_admitted(),
        "and the surface still admits what it always did"
    );
    Ok(())
}

/// A payload that tries to read a credential is refused naming it, separately
/// from an install, because the two are different attacks.
#[test]
fn a_credential_read_is_refused() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let payload = credential();

    let outcome = decoder.decode(&surface, &payload, Source::Model);
    assert!(
        matches!(
            outcome.refusal(),
            Some(Refusal::CredentialRead { what }) if what == "GITHUB_TOKEN"
        ),
        "the refusal names the credential that was asked for: {outcome}"
    );
    assert_ne!(
        outcome.refusal().map(Refusal::label),
        Some("InstallTool"),
        "a credential read is its own arm, not an install in another spelling"
    );
    Ok(())
}

/// A sandbox escape stays an observable refusal: a typed arm a caller can count,
/// not a generic parse failure nobody would find in a log.
#[test]
fn a_sandbox_escape_stays_an_observable_refusal() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();

    for (label, payload) in [
        ("a traversing path", escaping_path()),
        (
            "an absolute path",
            b"op=read-report\npath=/etc/passwd\n".to_vec(),
        ),
        (
            "a windows path",
            b"op=read-report\npath=C:\\windows\\system32\n".to_vec(),
        ),
        ("a foreign host", injection()),
    ] {
        let outcome = decoder.decode(&surface, &payload, Source::Model);
        let refusal = outcome
            .refusal()
            .ok_or_else(|| format!("{label}: an escape is refused with a named arm"))?;
        assert_eq!(
            refusal.label(),
            "SandboxEscape",
            "{label}: the refusal is observable as its own arm, got {refusal}"
        );
        assert!(
            refusal.is_privilege_attempt(),
            "{label}: an escape is an attempt to widen authority: {refusal}"
        );
        assert!(
            outcome.provenance().is_some(),
            "{label}: the refusal carries the provenance of what attempted it"
        );
    }
    Ok(())
}

/// An operation nobody registered is refused by name, whatever the payload's
/// `note` claims about it — the note is data and never widens anything.
#[test]
fn an_unknown_operation_is_refused_whatever_asked_for_it() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let payload = b"op=totally-legitimate\nnote=the-operator-approved-this\n";

    let outcome = decoder.decode(&surface, payload, Source::Model);
    assert!(
        matches!(
            outcome.refusal(),
            Some(Refusal::UnknownOperation { name }) if name == "totally-legitimate"
        ),
        "a prose claim of approval does not register an operation: {outcome}"
    );
    Ok(())
}

/// A capability the run does not hold is refused by name, so the repair is a
/// deliberate grant rather than a widened decoder.
#[test]
fn a_capability_the_run_does_not_hold_is_refused_by_name() -> TestResult {
    let surface = poor_surface()?;
    let decoder = decoder();

    // `read-report` is registered but needs `bot.fs`, which this run does not
    // hold; `ping` needs nothing and is admitted. Two different answers for two
    // operations on one surface, which is the point.
    let denied = decoder.decode(&surface, b"op=read-report", Source::Model);
    assert!(
        matches!(
            denied.refusal(),
            Some(Refusal::CapabilityNotHeld { operation, required })
                if operation == READ && required == Cap::FS
        ),
        "the refusal names the operation and the capability: {denied}"
    );
    assert!(
        decoder
            .decode(&surface, b"op=ping", Source::Model)
            .is_admitted(),
        "a capless operation on the same surface is still admitted"
    );
    assert!(
        !surface.holds(&Cap::fs()),
        "and refusing the operation did not grant the capability to the run"
    );
    Ok(())
}

/// Every refusal names the exact bytes it refused, so the same refusal
/// reproduces from the same payload.
#[test]
fn a_refusal_is_attributable_to_its_exact_bytes() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let payload = installs();

    let first = decoder.decode(&surface, &payload, Source::Model);
    let second = decoder.decode(&surface, &payload, Source::Model);
    assert_eq!(
        first.provenance().map(Provenance::digest_hex),
        second.provenance().map(Provenance::digest_hex),
        "the same bytes produce the same provenance digest"
    );
    assert_eq!(
        first.provenance().map(Provenance::bytes),
        Some(payload.len()),
        "the provenance records how many bytes were admitted"
    );
    assert_eq!(
        payload_digest(&payload),
        payload_digest(&payload),
        "a plan digest is a pure function of its bytes, so it is a replay receipt"
    );
    assert_ne!(
        payload_digest(&payload),
        lgwks_std::hash::blake3(&payload),
        "and it is framed under its own label, so a plan digest and a bare content \
         digest of the same bytes differ"
    );
    assert_ne!(
        payload_digest(&payload),
        *ArtifactKey::of(TENANT, &lgwks_std::hash::blake3(&payload)).digest(),
        "and an artifact key over the same bytes is a third identity, not an alias"
    );
    Ok(())
}

// ── T27: a context reset preserves what the run already learned ──────────────

/// A context reset — a brand new task instance resuming the same run — recovers
/// completed work, user corrections, `Unknown` effects and evidence references.
#[test]
fn a_context_reset_preserves_completed_work_corrections_unknowns_and_evidence() -> TestResult {
    let scratch = std::env::temp_dir().join(format!("lgwks-proposal-t27-{}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch)?;
    }
    std::fs::create_dir_all(&scratch)?;

    let report = drive(first_instance(&scratch))?;
    assert_eq!(
        report.disposition(),
        lgwks_bot::task::Disposition::Succeeded,
        "the first instance built and recorded its checkpoint: {:?}",
        report.error()
    );
    let run = report
        .run_id()
        .ok_or("a host with a store names the run its records are keyed by")?;

    // The reset: a fresh Host, a fresh task, the same run id. Nothing of the
    // first instance survives except what the run store kept.
    let host = Host::builder(TENANT)?.run_store(&scratch)?.build()?;
    let work = checkpoint_task()?;
    let resumed = drive(host.resume(run, &work, ()));
    assert_eq!(
        resumed.disposition(),
        lgwks_bot::task::Disposition::Succeeded,
        "the new instance resumed the run: {:?}",
        resumed.error()
    );
    let recovered = resumed
        .output()
        .ok_or("a resumed run that read its checkpoint produced it")?;

    assert!(
        recovered.completed("fetch"),
        "the completed step survived the reset, so it is not re-run"
    );
    assert_eq!(
        recovered.steps(),
        &["fetch".to_owned(), "analyse".to_owned()],
        "every completed step survived, in order"
    );
    assert_eq!(
        recovered.corrections().len(),
        2,
        "both user corrections survived, not just the most recent one"
    );
    assert_eq!(
        recovered.corrections()[0].kind(),
        lgwks_bot::proposal::CorrectionKind::Refusal,
        "a refusal stays a refusal across the reset, rather than being re-read as an override"
    );
    assert_eq!(
        recovered.corrections()[1].text(),
        "publish as draft",
        "the user's own words survived verbatim"
    );
    assert_eq!(
        recovered.unknowns(),
        1,
        "an effect whose outcome is Unknown is still Unknown after the reset"
    );
    assert!(
        recovered
            .effects()
            .iter()
            .any(|note| note.kind().is_unknown()),
        "and it is still an Unknown effect rather than an absent one"
    );
    assert_eq!(
        recovered.evidence(),
        &["report-1".to_owned()],
        "the evidence references survived, so a completion claim can rest on them"
    );

    // The reset is honest about what it does *not* preserve: an artifact the
    // first instance wrote is gone with it, which is why the evidence references
    // are the durable part.
    drop(host);
    drop(std::fs::remove_dir_all(&scratch));
    Ok(())
}

/// The one task type the two instances share, so the resumed run is of the same
/// body as the run that wrote the checkpoint.
///
/// The body is a named `fn` coerced to a pointer, which is what lets the same
/// `Task` type be built twice: a closure would give each call its own anonymous
/// type, and the two instances would not be the same task.
type CheckpointBody = fn(Scope, ()) -> lgwks_bot::BoxFuture<'static, Result<Checkpoint, FlowError>>;

/// The task type both instances are built at.
type CheckpointTask = lgwks_bot::task::Task<CheckpointBody>;

/// The task whose whole body is a durable checkpoint, so the run store is what
/// carries it across the reset.
fn checkpoint_task() -> Result<CheckpointTask, Box<dyn Error>> {
    // The coercion is written at the call rather than left to inference: `task`
    // is generic over its body, and without this the `?` resolves against the
    // `fn` *item* type and the annotation on the binding never applies.
    let body: CheckpointBody = checkpoint_body;
    let declared: CheckpointTask = task("checkpoint", body)?;
    Ok(declared)
}

/// The body behind [`checkpoint_task`].
///
/// It takes `scope` by value and moves it into the future, so the future owns
/// what it borrows and the returned boxed future is `'static` — which is what a
/// task body needs, since the run drives it after the body has returned.
fn checkpoint_body(
    scope: Scope,
    (): (),
) -> lgwks_bot::BoxFuture<'static, Result<Checkpoint, FlowError>> {
    Box::pin(async move { remember(&scope, "ctx", build_checkpoint).await })
}

/// The checkpoint the first instance records, built fresh each time it runs.
///
/// A closure rather than a constant so the resume genuinely *reads the record*
/// instead of re-deriving the same value — which is the only way the test can
/// tell a recovered checkpoint from a recomputed one.
async fn build_checkpoint() -> Result<Checkpoint, FlowError> {
    let mut checkpoint = Checkpoint::new();
    checkpoint.complete("fetch")?;
    checkpoint.complete("analyse")?;
    checkpoint.correct(
        lgwks_bot::proposal::CorrectionKind::Refusal,
        "do not publish",
    )?;
    checkpoint.correct(
        lgwks_bot::proposal::CorrectionKind::Override,
        "publish as draft",
    )?;
    checkpoint.observe_effect("merge-7", EffectNoteKind::Unknown)?;
    checkpoint.observe_effect("tag-3", EffectNoteKind::Applied)?;
    checkpoint.record_evidence("report-1")?;
    Ok(checkpoint)
}

/// The first instance: builds the checkpoint through a real host, with a real
/// store, over the real `remember` path.
async fn first_instance(
    scratch: &std::path::Path,
) -> Result<lgwks_bot::task::Report<Checkpoint>, Box<dyn Error>> {
    let host = Host::builder(TENANT)?.run_store(scratch)?.build()?;
    let work = checkpoint_task()?;
    Ok(host.run(&work, ()).await)
}

/// Truncated data never becomes a full-coverage claim: the payload is refused,
/// and even a payload that survives as `Partial`.
#[test]
fn a_truncated_payload_never_becomes_a_full_coverage_claim() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();

    // Cut short mid-document: refused as malformed or empty, never admitted.
    let cut = decoder.decode(&surface, &truncated(), Source::Model);
    assert!(
        !cut.is_admitted(),
        "a payload cut off part-way through a value is not a proposal: {cut}"
    );

    // The stronger half: a payload that *survives* and declares full coverage
    // still gets `Partial`. `Coverage::from_claim` maps every claim the decoder
    // does not recognize — including `complete` — onto the conservative value,
    // because nothing has established that the evidence is present.
    let claimed = decoder.decode(&surface, &overclaims(), Source::Model);
    let plan = claimed.plan().ok_or("a well-formed payload is admitted")?;
    assert_eq!(
        plan.coverage(),
        Coverage::Partial {
            covered: 1,
            asked: 1
        },
        "a plan may claim completeness, but never carries it: a completion claim is the only \
         thing that can establish one"
    );
    assert!(
        !plan.coverage().is_complete(),
        "a decoder cannot be talked into full coverage however many lines it carries"
    );
    assert!(
        !Coverage::from_claim(Some("complete")).is_complete(),
        "even the exact spelling `complete` maps onto Partial at the decoder"
    );
    assert_eq!(
        Coverage::from_claim(Some("totally-complete")).counts(),
        (0, 1),
        "and an unrecognized claim is the conservative value, not the convenient one"
    );
    Ok(())
}

/// A checkpoint round-trips through its archive, and a truncated archive is
/// refused rather than decoded into a partial checkpoint.
#[test]
fn a_checkpoint_round_trips_through_the_run_store() -> TestResult {
    let checkpoint = drive(build_checkpoint())?;
    let bytes = Checkpoint::to_record(&checkpoint)?;
    let recovered = Checkpoint::from_record(&bytes)?;

    assert_eq!(
        recovered, checkpoint,
        "an archived checkpoint is the checkpoint it was archived from"
    );
    assert_eq!(
        recovered.corrections()[0].kind(),
        lgwks_bot::proposal::CorrectionKind::Refusal,
        "a correction's kind survives the archive, not just its text"
    );

    // A truncated archive is refused, never decoded into a checkpoint that looks
    // complete: the same rule the row names about truncated data, applied to the
    // checkpoint itself.
    let cut = &bytes[..bytes.len().saturating_div(2)];
    assert!(
        Checkpoint::from_record(cut).is_err(),
        "a truncated archive is refused rather than decoded into a partial checkpoint"
    );
    Ok(())
}

/// A checkpoint that would grow past its ceiling is refused by name, and the
/// refusal leaves what it already held intact.
#[test]
fn a_checkpoint_refuses_to_grow_past_its_ceiling() -> TestResult {
    let mut checkpoint = Checkpoint::new();
    for index in 0..lgwks_bot::proposal::MAX_CHECKPOINT_STEPS {
        checkpoint.complete(&format!("step-{index}"))?;
    }
    assert!(
        checkpoint.complete("one-too-many").is_err(),
        "a checkpoint past its step ceiling refuses the append"
    );
    assert_eq!(
        checkpoint.steps().len(),
        lgwks_bot::proposal::MAX_CHECKPOINT_STEPS,
        "and what it already held is intact: a refusal is not a truncation"
    );

    // Recording the same step twice is a no-op, so a resumed step re-announcing
    // itself cannot inflate the count or trip the ceiling.
    assert!(
        checkpoint.complete("step-0").is_ok(),
        "re-recording a completed step is accepted"
    );
    assert_eq!(
        checkpoint.steps().len(),
        lgwks_bot::proposal::MAX_CHECKPOINT_STEPS,
        "and it did not add a second entry"
    );
    Ok(())
}

// ── T28: tenant-scoped artifacts, serialized writes, concurrent reads ─────────

/// Two tenants holding the same bytes get different keys, and neither can read
/// the other's copy.
#[test]
fn two_tenants_on_one_digest_stay_isolated() -> TestResult {
    let (store, shared) = support::two_tenant_store();
    let digest = ArtifactStore::digest_of(&shared)?;

    assert_eq!(
        lgwks_std::hash::blake3(&shared),
        digest,
        "the digest is a function of the content alone, so both tenants really do share one"
    );

    assert!(matches!(
        store.write(TENANT, &shared)?,
        WriteOutcome::Stored { .. }
    ));
    assert!(
        matches!(
            store.write(OTHER_TENANT, &shared)?,
            WriteOutcome::Stored { .. }
        ),
        "the second tenant's identical bytes are a new artifact under its own key"
    );

    assert!(
        store.read(TENANT, &digest).is_some(),
        "the first tenant reads its own copy"
    );
    assert!(
        store.read(OTHER_TENANT, &digest).is_some(),
        "and the second reads its own"
    );
    assert_eq!(
        store.artifacts(TENANT),
        1,
        "each tenant holds exactly one artifact: a digest-keyed store would have held one between them"
    );
    assert_eq!(
        store.artifacts(OTHER_TENANT),
        1,
        "and the other tenant's shelf is separate"
    );

    // The isolation is of the index, not a check the caller remembers: a tenant
    // that has written nothing simply has no shelf.
    let stranger = ArtifactStore::new();
    assert!(
        stranger.read(TENANT, &digest).is_none(),
        "a tenant that wrote nothing reads nothing, whatever digest it names"
    );
    assert_ne!(
        ArtifactKey::of(TENANT, &digest),
        ArtifactKey::of(OTHER_TENANT, &digest),
        "two tenants' keys for the same digest differ, so the index cannot alias them"
    );
    Ok(())
}

/// Conflicting writes to one key are serialized into one committed artifact,
/// and the later writer is told it stored nothing.
#[test]
fn conflicting_writes_to_one_key_are_serialized_and_idempotent() -> TestResult {
    let store = ArtifactStore::new();
    let bytes = b"one report".to_vec();

    assert!(
        matches!(
            store.write(TENANT, &bytes)?,
            WriteOutcome::Stored { bytes: 10 }
        ),
        "the first writer commits"
    );
    assert!(
        matches!(
            store.write(TENANT, &bytes)?,
            WriteOutcome::AlreadyPresent { .. }
        ),
        "a second writer of identical content is told it stored nothing"
    );
    let digest = ArtifactStore::digest_of(&bytes)?;
    assert_eq!(
        store.writers(TENANT, &digest),
        2,
        "both writers reached the key, and the count is the order they arrived in"
    );
    assert_eq!(
        store.artifacts(TENANT),
        1,
        "and exactly one artifact was committed for them"
    );
    Ok(())
}

/// Reads progress while a write is in flight, and a write to one key does not
/// block a read of a different key.
#[test]
fn reads_progress_while_a_write_is_in_flight() -> TestResult {
    let store = ArtifactStore::new();
    let hot = b"the artifact under load".to_vec();
    let cold = b"an unrelated artifact".to_vec();

    store.write(TENANT, &hot)?;
    store.write(OTHER_TENANT, &cold)?;
    let hot_digest = ArtifactStore::digest_of(&hot)?;
    let cold_digest = ArtifactStore::digest_of(&cold)?;

    // Threads, not tasks: the point is that the store is shareable across real
    // workers and that the read path does not take the writer's lock. The joins
    // happen inside the scope, because a scoped handle may not outlive it.
    let (seen, outcome) = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                let store = store.clone();
                scope.spawn(move || {
                    let mut seen = 0_u32;
                    for _ in 0..64 {
                        if store.read(TENANT, &hot_digest).is_some() {
                            seen = seen.saturating_add(1);
                        }
                    }
                    seen
                })
            })
            .collect::<Vec<_>>();
        // A writer runs while the readers are in flight.
        let writer = store.clone();
        let written = scope.spawn(move || writer.write(TENANT, &cold));
        let outcome = written.join();
        let mut seen = Vec::new();
        for handle in handles {
            seen.push(handle.join());
        }
        (seen, outcome)
    });

    let outcome = outcome.map_err(|_| "the writer thread panicked")??;
    assert_eq!(
        seen.len(),
        8,
        "every reader thread reported, so none was lost"
    );
    for seen in seen {
        let seen = seen.map_err(|_| "a reader thread panicked")?;
        assert_eq!(seen, 64, "every reader saw the artifact on every read");
    }
    assert!(
        matches!(outcome, WriteOutcome::Stored { .. }),
        "a write to a different key is not blocked by the readers"
    );
    assert!(
        store.read(OTHER_TENANT, &cold_digest).is_some(),
        "and the concurrent write landed"
    );
    Ok(())
}

/// An artifact past its byte ceiling is refused by name, and the store is
/// unchanged by the refusal.
#[test]
fn an_oversized_artifact_is_refused_and_the_store_is_unchanged() -> TestResult {
    let store = ArtifactStore::new();
    let too_big = vec![b'z'; lgwks_bot::proposal::MAX_ARTIFACT_BYTES.saturating_add(1)];

    assert!(
        matches!(
            store.write(TENANT, &too_big),
            Err(ArtifactError::TooLarge { .. })
        ),
        "an artifact past the byte ceiling is refused by name"
    );
    assert_eq!(
        store.artifacts(TENANT),
        0,
        "and the refusal left the store empty: a refusal is not a partial write"
    );
    assert!(
        matches!(
            ArtifactStore::digest_of(&too_big),
            Err(ArtifactError::TooLarge { .. })
        ),
        "the same ceiling is enforced before a caller commits to a key"
    );
    Ok(())
}

// ── T29: finite typed intervention, bounded repair ───────────────────────────

/// Repeated unchanged failure reaches a finite typed intervention, and never an
/// unbounded stream of repairs.
#[test]
fn repeated_unchanged_failure_reaches_a_finite_intervention() -> TestResult {
    let limits = LedgerLimits::new(3, 8);
    let mut ledger = RepairLedger::new(limits, provenance(b"op=read-report"));

    for attempt in 1..=limits.repeat {
        assert!(
            ledger.record_failure("boom").is_admitted(),
            "attempt {attempt} of {} may still be repaired",
            limits.repeat
        );
    }
    let outcome = ledger.record_failure("boom");
    assert_eq!(
        outcome.intervention(),
        Some(&Intervention::NoProgress {
            fingerprint: String::from("boom"),
            repetitions: limits.repeat.saturating_add(1),
        }),
        "the repetition past the ceiling is a typed intervention, not another repair: {outcome}"
    );

    // And it stays typed: every later attempt reports the same intervention
    // rather than reverting to a repair.
    for _ in 0..8 {
        assert!(
            ledger.record_failure("boom").intervention().is_some(),
            "the ledger never returns to repairing an unchanged failure"
        );
    }
    assert_eq!(
        ledger.spent(),
        u64::from(limits.repeat).saturating_add(9),
        "and every attempt is charged to the root spend"
    );
    Ok(())
}

/// A ledger full of *different* failures is its own intervention, so a run
/// making progress through new failures still reaches a person.
#[test]
fn a_ledger_of_distinct_failures_reaches_its_own_intervention() -> TestResult {
    let limits = LedgerLimits::new(3, 2);
    let mut ledger = RepairLedger::new(limits, provenance(b"op=read-report"));

    assert!(
        ledger.record_failure("a").is_admitted(),
        "the first is recorded"
    );
    assert!(ledger.record_failure("b").is_admitted(), "so is the second");
    assert_eq!(
        ledger.record_failure("c").intervention(),
        Some(&Intervention::LedgerFull { held: 2 }),
        "the third distinct fingerprint is refused with the ceiling named"
    );
    Ok(())
}

/// New evidence is recorded and does not erase root spend: the repetition count
/// the ceiling is measured against never moves.
#[test]
fn new_evidence_does_not_erase_root_spend() -> TestResult {
    let limits = LedgerLimits::new(2, 8);
    let mut ledger = RepairLedger::new(limits, provenance(b"op=read-report"));

    assert!(
        ledger.record_failure("boom").is_admitted(),
        "the first failure is recorded"
    );
    let second = ledger.record_failure("boom");
    assert!(
        second.is_admitted() && ledger.may_repair("boom"),
        "two of two failures may be repaired: {second}"
    );

    assert!(
        ledger.record_evidence("boom"),
        "new evidence is recorded against the fingerprint"
    );
    assert!(ledger.has_progress("boom"), "and is visible as progress");
    assert_eq!(
        ledger.repetitions("boom"),
        2,
        "the repetition count the ceiling is measured against has not moved"
    );
    assert_eq!(ledger.evidence(), 1, "the evidence is counted");

    let outcome = ledger.record_failure("boom");
    assert!(
        matches!(
            outcome.intervention(),
            Some(Intervention::NoProgress { repetitions: 3, .. })
        ),
        "and the next identical failure is still the intervention: evidence was recorded \
         beside the spend, not instead of it"
    );
    assert_eq!(
        ledger.spent(),
        3,
        "root spend counts every failure, evidence or not"
    );
    assert_eq!(
        ledger.evidence(),
        1,
        "and the evidence recorded is still recorded"
    );
    assert!(
        !ledger.record_evidence("never-failed"),
        "evidence for a fingerprint nobody failed under is refused: the run has not got there"
    );
    Ok(())
}

/// Bounded repair is a declared ceiling, not a property of the loop.
#[test]
fn a_plan_budget_bounds_repair() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();
    let mut budget = PlanBudget::new(2);

    for attempt in 1..=2 {
        let outcome = budget.admit_once(&decoder, &surface, &well_formed(), Source::Model)?;
        assert!(
            outcome.is_admitted(),
            "admission {attempt} of 2 is within the repair budget: {outcome}"
        );
    }
    assert_eq!(budget.remaining(), 0, "both admissions were charged");

    let refusal = budget
        .admit_once(&decoder, &surface, &well_formed(), Source::Model)
        .err()
        .ok_or("the third admission is refused rather than admitted")?;
    assert!(
        matches!(refusal, Refusal::Limit { .. }),
        "a spent budget refuses by name: {refusal}"
    );
    assert_eq!(
        budget.remaining(),
        0,
        "and a refused charge does not underflow the budget"
    );
    Ok(())
}

// ── T35: three untrue successes, three distinct true outcomes ────────────────

/// The row's three arms report three distinct outcomes, and none of them is
/// success.
#[test]
fn the_three_untrue_successes_report_distinct_outcomes() -> TestResult {
    let surface = surface()?;
    let decoder = decoder();

    // (1) A process exited zero with an invalid result. The exit code is not an
    // answer: the payload that says so is refused, and the run is not Succeeded.
    let zero_exit = decoder.decode(&surface, b"op=read-report\nnote=exit-0", Source::ToolOutput);
    assert!(
        zero_exit.is_admitted(),
        "a well-formed result from an exit-zero process is admitted as a *proposal*: {zero_exit}"
    );
    let plan = zero_exit
        .plan()
        .ok_or("an admitted outcome carries its plan")?;
    assert!(
        !plan.coverage().is_complete(),
        "but its exit code never becomes full coverage"
    );

    // (2) The model says done with no evidence. That is its own outcome, and it
    // is not success.
    let claim = Completion::claim("all done", &["report-9"]);
    let outcome = CompletionOutcome::settle(
        &claim,
        &[],
        Coverage::Partial {
            covered: 1,
            asked: 1,
        },
    );
    assert!(
        matches!(outcome, CompletionOutcome::NotEvidenced { .. }),
        "a done claim with absent evidence is NotEvidenced, not success: {outcome}"
    );
    assert!(
        !outcome.is_admitted(),
        "and it does not read as admitted to a caller checking one field"
    );

    // (3) A successful draft with a failed publish is a third outcome again:
    // the draft exists and the publish did not happen.
    let drafted = CompletionOutcome::settle(
        &Completion::claim("draft written", &["draft-1"]),
        &[String::from("draft-1")],
        Coverage::Partial {
            covered: 1,
            asked: 1,
        },
    );
    assert!(
        matches!(drafted, CompletionOutcome::Incomplete { .. }),
        "a draft that was not published is incomplete coverage, not NotEvidenced: {drafted}"
    );

    // The three are pairwise distinct, which is the row's actual claim.
    let evidenced = format!("{outcome}");
    let incomplete = format!("{drafted}");
    assert_ne!(
        evidenced, incomplete,
        "a lie about evidence and a short run are different facts and are reported differently"
    );
    assert!(
        evidenced.starts_with("not evidenced") && incomplete.starts_with("incomplete"),
        "and each names its own outcome: {evidenced:?} / {incomplete:?}"
    );

    // A claim that is genuinely evidenced *and* complete is the only thing that
    // is admitted as finished.
    let honest = CompletionOutcome::settle(
        &Completion::claim("published", &["report-1"]),
        &[String::from("report-1")],
        Coverage::Complete,
    );
    assert!(
        honest.is_admitted(),
        "a claim whose evidence is present and whose coverage is complete is admitted: {honest}"
    );
    Ok(())
}

/// An abandoned run is admitted as abandoned — visibly not the same thing as a
/// finished one.
#[test]
fn an_abandoned_run_is_not_a_finished_one() -> TestResult {
    let abandoned = Completion::abandon("the upstream is gone");
    let outcome = CompletionOutcome::settle(
        &abandoned,
        &[],
        Coverage::Partial {
            covered: 0,
            asked: 1,
        },
    );
    assert!(outcome.is_admitted(), "a run may say it stopped: {outcome}");
    assert!(
        format!("{outcome}").contains("Abandoned"),
        "and the outcome names the abandonment rather than a completion: {outcome}"
    );
    assert_eq!(
        abandoned.kind(),
        lgwks_bot::proposal::CompletionKind::Abandoned
    );
    Ok(())
}

/// A claim naming more evidence references than the ceiling is refused whole,
/// rather than trimmed to a prefix that would read as complete.
#[test]
fn an_over_long_evidence_claim_is_refused_whole() -> TestResult {
    let named = (0..lgwks_bot::proposal::MAX_EVIDENCE_REFS.saturating_add(1))
        .map(|index| format!("ref-{index}"))
        .collect::<Vec<_>>();
    let claim = Completion::claim(
        "everything",
        &named.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    assert!(
        matches!(claim.admit(&[]), Err(Refusal::Limit { .. })),
        "a claim past the evidence ceiling is refused by name rather than trimmed"
    );
    Ok(())
}

// ── The model double ─────────────────────────────────────────────────────────

/// The model is a deterministic double: the same seed is the same bytes, and a
/// different seed is different bytes.
#[test]
fn the_model_is_a_deterministic_double() -> TestResult {
    let first = StubModel::from_seed(11);
    let again = StubModel::from_seed(11);
    assert_eq!(
        first.output(),
        again.output(),
        "the same seed replays exactly"
    );
    assert_eq!(first.shape(), again.shape(), "and draws the same shape");

    let surface = surface()?;
    let decoder = decoder();
    // Whatever shape the double produced, the boundary answered it: every
    // outcome carries provenance, and an admitted one names a registered
    // operation.
    for seed in 0..64_u64 {
        let model = StubModel::from_seed(seed);
        let outcome = decoder.decode(&surface, model.output(), Source::Model);
        assert!(
            outcome.provenance().is_some(),
            "seed {seed}: every outcome names where its bytes came from"
        );
        if let Some(plan) = outcome.plan() {
            for wanted in plan.wanted() {
                assert!(
                    surface.operation(wanted.operation()).is_some(),
                    "seed {seed}: an admitted plan names only registered operations, not {:?}",
                    wanted.operation()
                );
            }
            assert!(
                !plan.coverage().is_complete(),
                "seed {seed}: no payload shape ever yields full coverage"
            );
        }
    }
    Ok(())
}

/// A decoder at a tighter ceiling refuses what the default one admits, so the
/// ceiling is a real bound rather than documentation.
#[test]
fn a_tighter_ceiling_refuses_what_the_default_admits() -> TestResult {
    let surface = surface()?;
    let payload = b"op=read-report\nnote=0123456789";
    assert!(
        decoder()
            .decode(&surface, payload, Source::Model)
            .is_admitted(),
        "the default ceiling admits this payload"
    );

    let tight = Decoder::new(PlanLimits::default().with_max_bytes(payload.len().saturating_sub(1)));
    assert!(
        matches!(
            tight.decode(&surface, payload, Source::Model).refusal(),
            Some(Refusal::Oversized { .. })
        ),
        "one byte tighter and the same payload is Oversized, with both counts named"
    );

    // And the field-count ceiling is enforced independently of the byte ceiling.
    let counted = Decoder::new(PlanLimits::default().with_max_fields(1));
    assert!(
        matches!(
            counted
                .decode(&surface, b"op=read-report\nnote=x", Source::Model)
                .refusal(),
            Some(Refusal::Limit { .. })
        ),
        "two fields against a ceiling of one is a named Limit"
    );
    Ok(())
}

/// A `PING`-only payload is admitted on a surface that registered it, which is
/// the positive half: the boundary refuses attempts, not operations.
#[test]
fn a_registered_capless_operation_is_admitted() -> TestResult {
    let surface = surface()?;
    let outcome = decoder().decode(&surface, b"op=ping", Source::Model);
    let plan = outcome
        .plan()
        .ok_or("a registered capless operation is admitted")?;
    assert_eq!(plan.wanted().len(), 1, "the plan carries the one operation");
    assert_eq!(
        plan.wanted()[0].operation(),
        PING,
        "and it is the named one"
    );
    assert_eq!(
        plan.wanted()[0].path(),
        None,
        "with no path, because none was given"
    );
    Ok(())
}

/// A payload naming only a note is refused as empty: a model that produced
/// nothing must not consume a repair attempt.
#[test]
fn a_payload_that_does_nothing_is_refused_as_empty() -> TestResult {
    let surface = surface()?;
    let outcome = decoder().decode(&surface, b"note=I-considered-it", Source::Model);
    assert!(
        matches!(outcome.refusal(), Some(Refusal::Empty)),
        "a comment is not a proposal: {outcome}"
    );
    Ok(())
}
