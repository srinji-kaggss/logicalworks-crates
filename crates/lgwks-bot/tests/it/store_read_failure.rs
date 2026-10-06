//! A store that cannot be read is an error, not a disagreement.
//!
//! INV-BOT-7 says a read failure is never absence, and this file is where that
//! rule stops being about the *effect journal* and starts being about the durable
//! step store. It matters twice over here, because a `false` from the
//! compatibility check does not merely lose an error — it asserts something
//! false and specific. "These records were written under a different definition"
//! is a claim about a definition, and it sends the caller to a new run, a
//! migration or an edit. A device failing to answer establishes nothing about
//! any of those, so folding the two together tells an operator to go and change
//! code when what is wrong is their disk.
//!
//! It also destroys the only signal that separates the faults. Before this file
//! an unreadable store and a genuinely drifted one produced the same
//! `Failed { reason }` at the step, distinguished only by whether the text
//! happened to mention a device. A caller branching on the variant could not
//! tell them apart at all.
//!
//! | Test | What it pins |
//! |---|---|
//! | `an_unreadable_store_is_refused_as_itself_and_not_as_a_drift` | an injected index-read failure reaches the step as the store's own typed error, never as `Incompatible` and never as a replay |
//! | `the_fault_is_one_shot_and_the_step_after_it_replays` | arming the injector costs exactly one refusal; the store is not left permanently unreadable |
//! | `a_compatible_resume_still_replays_after_no_fault` | the control: with no fault armed the same resume replays its records and runs no body |

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::scratch::Scratch;

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::resume_fixtures as shared;

use lgwks_bot::effect::{InputIdentity, RunId};
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{DefinitionIdentity, Disposition, Host, StoreError, Task, task};

type TestResult = Result<(), Box<dyn Error>>;

/// The future the task body returns, so `Task` has one type every call site passes.
type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type [`one_step`] builds: a function pointer returning a named future.
type OneStep = Task<fn(Scope, u32) -> BodyFuture>;

/// How many times a step's body has been constructed.
///
/// The evidence that a step did *not* run. The counter moves inside `remember`'s
/// closure, which is only reached *after* the record lookup, so a step that
/// constructed no closure is one that was refused before its work was trusted.
static BODY_CONSTRUCTIONS: AtomicU64 = AtomicU64::new(0);

/// The count of bodies constructed since the process started.
fn constructions() -> u64 {
    BODY_CONSTRUCTIONS.load(Ordering::SeqCst)
}

/// The one future the body returns, boxed so the task body has a nameable type.
///
/// The box is what makes the body a *function pointer* rather than an `async fn`,
/// which is what lets one body serve every call site: `Task` stores its body's
/// own future type, so two separately written closures are two types and a
/// `Task` built by one cannot be passed where the other is expected.
fn durable_body(scope: Scope, value: u32) -> BodyFuture {
    Box::pin(async move {
        remember(&scope, "v", || async move {
            BODY_CONSTRUCTIONS.fetch_add(1, Ordering::SeqCst);
            Ok::<_, FlowError>(value)
        })
        .await
    })
}

/// The task this file drives.
fn one_step() -> Result<OneStep, FlowError> {
    task("read-failure", durable_body)
}

/// A host whose store is `<dir>/acme.runstore`.
fn host_in(dir: &Path) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder("acme")?.run_store(dir)?.build()?)
}

/// The durable-value schema this file's runs declare.
///
/// A named schema rather than the default, so the axis these rows could move is
/// named rather than left to a reader to infer from what is absent.
const CODEC: &str = "lgwks.bot.schema.v1.read-failure";

/// The value every run in this file is given.
///
/// Fixed, because the input digest is one of the four axes and a per-test draw
/// would make a difference in it indistinguishable from a difference in anything
/// else the test varies.
const INPUT: u32 = 7;

/// The definition these runs are recorded under.
fn identity(host: &Host) -> DefinitionIdentity {
    host.definition("read-failure", 1, Some(input_digest(INPUT)), 1)
        .with_codec(CODEC)
}

/// The content digest of one integer, as `Host::definition` is given it.
fn input_digest(input: u32) -> lgwks_std::hash::Digest {
    let mut hasher = lgwks_std::hash::Hasher::new();
    input.write_identity(&mut hasher);
    hasher.finalize()
}

/// Run the first attempt, which must succeed and leave one committed record.
///
/// The host and the run id come back together because a caller that drops one
/// without the other has nothing to resume, and that mistake reads as a store
/// fault rather than as a dropped handle.
fn first_attempt(dir: &Path, work: &OneStep) -> Result<(Host, RunId), Box<dyn Error>> {
    let host = host_in(dir)?;
    let declared = identity(&host);
    let report = lgwks_bot::block_on(host.run_under(&declared, work, INPUT));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the first attempt must succeed, got: {:?}",
        report.error()
    );
    let run = report.run_id().ok_or("a stored run must name its run id")?;
    Ok((host, run))
}

/// A host over a *reopened* store, so the claim is about a restart rather than a
/// map the first attempt left warm.
///
/// The throwaway open-then-drop is not ceremony: it is what forces the returned
/// handle to replay the file rather than share the first one's index, and it is
/// the difference between "a restart works" and "a `HashMap` still had its
/// entries".
fn resume_over_reopened(dir: &Path) -> Result<Host, Box<dyn Error>> {
    let warm = host_in(dir)?;
    drop(warm);
    host_in(dir)
}

/// A scratch store with one committed record and the run id of its first attempt.
///
/// Every row below opens a scratch store, runs one durable step, keeps the run id
/// and drops the writing host; a copy per row is the block the commit guard
/// refuses. The writing host is dropped inside the fixture, so every resume a row
/// makes is over a reopened file rather than a map the first attempt left warm.
struct Seeded {
    /// Where this fixture's store lives.
    dir: PathBuf,
    /// The task whose one durable step was recorded.
    work: OneStep,
    /// The run the first attempt minted.
    run: RunId,
}

/// Run the first attempt for `scratch` and return everything a resume needs.
///
/// # Errors
///
/// Whatever the store, the task or the first attempt reports.
fn seeded(scratch: &Scratch) -> Result<Seeded, Box<dyn Error>> {
    let dir = scratch.path().join("store");
    let work = one_step()?;
    let (host, run) = first_attempt(&dir, &work)?;
    drop(host);
    Ok(Seeded { dir, work, run })
}

/// A store that cannot answer is refused as itself, never as a drift.
///
/// The row. The fault is armed on the real `RunStore` and the resume goes through
/// `Host::resume_under` over a reopened file, so the check under test is the one
/// the run path actually makes and not a hand-called method. The refusal must be
/// the store's own error — distinguishable from `Incompatible` by the variant
/// alone, with no reading of the message — and the step's body must never have
/// been constructed, because a store that could not say whether it held a record
/// must not run the work as though it held none.
#[test]
fn an_unreadable_store_is_refused_as_itself_and_not_as_a_drift() -> TestResult {
    let scratch = Scratch::new("read-failure")?;
    let seeded = seeded(&scratch)?;
    let reopened = resume_over_reopened(&seeded.dir)?;
    let store = reopened.run_store().ok_or("a stored host keeps a store")?;
    // Armed after the reopen, so it is the resume's read that fails and not the
    // host's own admission reads — which is what makes the refusal the step's.
    store.fail_next_index_read();

    let before = constructions();
    let refused = lgwks_bot::block_on(reopened.resume_under(
        seeded.run,
        &identity(&reopened),
        &seeded.work,
        INPUT,
    ));

    let error = refused
        .error()
        .ok_or("a store that cannot be read must not succeed")?;
    assert_eq!(
        refused.disposition(),
        Disposition::Failed,
        "a read failure is a failure, not a compatibility and not a replay"
    );
    assert!(
        !matches!(error, FlowError::Incompatible { .. }),
        "a read failure must not be reported as a definition drift, got: {error}"
    );
    // The typed half of the row: the refusal is the store's own error, reached by
    // its variant and its wrapped kind, not by reading the rendered text. A string
    // that happened to mention a device would satisfy a `contains` while the
    // payload was some other arm entirely.
    let refusal = shared::store_refusal(error).ok_or(
        "a read failure must reach the caller as FlowError::Store carrying the store's own error",
    )?;
    assert!(
        matches!(refusal, StoreError::Storage { .. }),
        "the wrapped refusal must be the device's own storage error, got: {refusal:?}"
    );
    assert_eq!(
        constructions().saturating_sub(before),
        0,
        "the step body ran despite the store being unable to say whether it held a record"
    );
    Ok(())
}

/// The injector is one-shot, so one device fault does not brick a store.
///
/// A fault that stayed armed would make every later read of the same handle fail
/// forever, which is a worse defect than the one it exists to observe: it turns
/// one transient error into a store that cannot be read again without a restart.
/// The recovery is asserted here rather than left to the control row, because "it
/// recovers" and "it never failed" are different facts and only the second would
/// satisfy a suite that had stopped injecting.
#[test]
fn the_fault_is_one_shot_and_the_step_after_it_replays() -> TestResult {
    let scratch = Scratch::new("one-shot")?;
    let seeded = seeded(&scratch)?;
    let reopened = resume_over_reopened(&seeded.dir)?;
    let store = reopened.run_store().ok_or("a stored host keeps a store")?;
    store.fail_next_index_read();

    let first = lgwks_bot::block_on(reopened.resume_under(
        seeded.run,
        &identity(&reopened),
        &seeded.work,
        INPUT,
    ));
    assert!(
        first.error().is_some(),
        "the armed fault must cost one refusal"
    );

    // The store is ordinary again: the records were never touched, so this is the
    // compatible replay the third row proves, and it must run no body.
    let before = constructions();
    let second = lgwks_bot::block_on(reopened.resume_under(
        seeded.run,
        &identity(&reopened),
        &seeded.work,
        INPUT,
    ));
    assert_eq!(
        second.disposition(),
        Disposition::Succeeded,
        "the fault was one-shot, so the next resume is an ordinary replay, got: {:?}",
        second.error()
    );
    assert_eq!(
        second.output(),
        Some(&INPUT),
        "the replayed value is the recorded one"
    );
    assert_eq!(
        constructions().saturating_sub(before),
        0,
        "the recovered resume re-ran the durable step"
    );
    Ok(())
}

/// The control: with no fault armed, a compatible resume replays and runs nothing.
///
/// Without this row the two above would pass on a store that refused everything,
/// which is the failure INV-BOT-7 guards against in the other direction.
#[test]
fn a_compatible_resume_still_replays_after_no_fault() -> TestResult {
    let scratch = Scratch::new("control")?;
    let seeded = seeded(&scratch)?;
    let reopened = resume_over_reopened(&seeded.dir)?;
    let before = constructions();
    let replayed = lgwks_bot::block_on(reopened.resume_under(
        seeded.run,
        &identity(&reopened),
        &seeded.work,
        INPUT,
    ));
    assert_eq!(
        replayed.disposition(),
        Disposition::Succeeded,
        "got: {:?}",
        replayed.error()
    );
    assert_eq!(replayed.output(), Some(&INPUT));
    assert_eq!(
        constructions().saturating_sub(before),
        0,
        "a compatible resume with no fault armed must replay, not re-run"
    );
    Ok(())
}
