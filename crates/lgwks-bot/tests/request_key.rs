//! T30/T17 through the public request-keyed submission.
//!
//! | Test | Row | What it pins |
//! |---|---|---|
//! | `a_duplicate_identical_request_reattaches_without_rerunning` | T30 | the same key and input returns the recorded report with the body never entered again |
//! | `a_reattach_survives_a_reopened_store` | T30 | the receipt is durable: a fresh host over the same file reattaches |
//! | `same_key_with_a_different_payload_is_a_typed_conflict` | T30 | a different digest under one key is `RequestError::Conflict` naming both digests, and no body runs |
//! | `distinct_request_keys_are_distinct_runs` | T30 | two keys over one host never collapse into one run |
//! | `two_tenants_never_share_a_request_run` | T30 | one key, two tenants over one store file: two runs, each attributed to its owner |
//! | `a_dropped_client_leaves_the_request_in_flight_for_a_later_client` | T17 | a dropped waiter leaves a durable receipt and no terminal; a later client reattaches and gets the uncertainty and the run to settle |
//! | `a_malformed_request_key_is_refused` | T30 | a key outside the identifier set is a typed refusal at construction |
//! | `a_submission_without_a_store_is_refused` | T30 | a durable submission with nowhere to record refuses rather than degrading |

#![cfg(all(feature = "script", feature = "ephemeral"))]

use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lgwks_bot::script::FlowError;
use lgwks_bot::task::{Disposition, Host, RequestError, RequestKey, RunStore, Submission, task};

// The scratch directory, shared with the resume targets.
#[path = "support/resume.rs"]
mod shared;
// The request fixtures themselves.
#[path = "support/request.rs"]
mod request;

use request::{
    counting_task, drop_after_first_step, host_on, host_with_deadline, overrunning_task,
    parking_task, stop_mid_run, stored_host, two_step_task,
};
use shared::Scratch;

type TestResult = Result<(), Box<dyn Error>>;

/// The disposition a submission reported, which every arm but `InFlight` carries.
fn disposition_of<O>(submission: &Submission<O>) -> Result<Disposition, Box<dyn Error>> {
    submission
        .report()
        .map(lgwks_bot::task::Report::disposition)
        .ok_or_else(|| {
            "an in-flight submission carries no report, so it reports no disposition".into()
        })
}

/// The same key with the same input reattaches and does not re-run the body.
#[test]
fn a_duplicate_identical_request_reattaches_without_rerunning() -> TestResult {
    let scratch = Scratch::new("req-dupe")?;
    let host = stored_host("acme", scratch.path())?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let key = RequestKey::new("order-42")?;

    let first = lgwks_bot::block_on(host.submit(&key, &work, 7u32))?;
    assert!(
        matches!(first, Submission::Executed(_)),
        "a first submission executes"
    );
    let first_report = first
        .report()
        .ok_or("a first submission carries a report")?;
    assert_eq!(first_report.disposition(), Disposition::Succeeded);
    assert_eq!(first_report.output().copied(), Some(7));
    let run = first.run_id().ok_or("a submission names a run")?;
    assert_eq!(counter.get(), 1, "the first submission ran the body once");

    let second = lgwks_bot::block_on(host.submit(&key, &work, 7u32))?;
    assert!(
        matches!(second, Submission::Reattached(_)),
        "a repeat under the same key and input reattaches"
    );
    assert_eq!(
        second.report().and_then(|report| report.output().copied()),
        Some(7),
        "the recorded output comes back without the body running"
    );
    assert_eq!(
        second.run_id(),
        Some(run),
        "one key derives one run on every submission"
    );
    assert_eq!(
        counter.get(),
        1,
        "the reattach never entered the body again"
    );
    Ok(())
}

/// The receipt is durable: a fresh host over the same file reattaches.
#[test]
fn a_reattach_survives_a_reopened_store() -> TestResult {
    let scratch = Scratch::new("req-reopen")?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let key = RequestKey::new("once")?;

    let first =
        lgwks_bot::block_on(stored_host("acme", scratch.path())?.submit(&key, &work, 4u32))?;
    let run = first.run_id().ok_or("a submission names a run")?;
    assert_eq!(
        first.report().and_then(|report| report.output().copied()),
        Some(4)
    );
    assert_eq!(counter.get(), 1);

    // A second host opens the same file; the receipt is found without a second
    // lookup table, because the run identity is derived from tenant and key.
    let reopened = stored_host("acme", scratch.path())?;
    let again = lgwks_bot::block_on(reopened.submit(&key, &work, 4u32))?;
    assert!(
        matches!(again, Submission::Reattached(_)),
        "a reopened store reattaches to the recorded outcome"
    );
    assert_eq!(again.run_id(), Some(run));
    assert_eq!(
        again.report().and_then(|report| report.output().copied()),
        Some(4)
    );
    assert_eq!(counter.get(), 1, "the reopened reattach ran no body");
    Ok(())
}

/// A different payload under one key is a typed conflict naming both digests.
#[test]
fn same_key_with_a_different_payload_is_a_typed_conflict() -> TestResult {
    let scratch = Scratch::new("req-conflict")?;
    let host = stored_host("acme", scratch.path())?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let key = RequestKey::new("order-7")?;

    let first = lgwks_bot::block_on(host.submit(&key, &work, 1u32))?;
    assert!(first.report().is_some(), "the first payload is accepted");

    let error = lgwks_bot::block_on(host.submit(&key, &work, 2u32))
        .err()
        .ok_or("a different payload under one key must be refused")?;
    match error {
        RequestError::Conflict(conflict) => {
            assert_eq!(conflict.key().as_str(), "order-7");
            assert_ne!(
                conflict.existing(),
                conflict.requested(),
                "the two inputs must name two digests"
            );
            let rendered = conflict.to_string();
            assert!(
                rendered.contains(&conflict.existing().to_hex())
                    && rendered.contains(&conflict.requested().to_hex()),
                "the conflict names both digests: {rendered}"
            );
        }
        other => return Err(format!("expected a typed conflict, got {other:?}").into()),
    }
    assert_eq!(counter.get(), 1, "a conflict runs no body");
    Ok(())
}

/// Two keys over one host never collapse into one run.
#[test]
fn distinct_request_keys_are_distinct_runs() -> TestResult {
    let scratch = Scratch::new("req-distinct")?;
    let host = stored_host("acme", scratch.path())?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;

    let one = lgwks_bot::block_on(host.submit(&RequestKey::new("a-1")?, &work, 5u32))?;
    let two = lgwks_bot::block_on(host.submit(&RequestKey::new("b-1")?, &work, 5u32))?;
    assert!(one.report().is_some() && two.report().is_some());
    assert_ne!(
        one.run_id(),
        two.run_id(),
        "two keys over one host derive two runs"
    );
    assert_eq!(counter.get(), 2, "each distinct key ran its own body once");
    Ok(())
}

/// One key, two tenants sharing one store file: two runs, each attributed to its
/// owner.
#[test]
fn two_tenants_never_share_a_request_run() -> TestResult {
    let scratch = Scratch::new("req-tenants")?;
    let shared = scratch.join("shared.runstore");
    // One store handle, cloned into both tenants: two handles over one file
    // would be two writers, which the store's own length fence refuses.
    let store = RunStore::open(&shared)?;
    let alpha = host_on("alpha", store.clone())?;
    let beta = host_on("beta", store)?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let key = RequestKey::new("same-key")?;

    let mine = lgwks_bot::block_on(alpha.submit(&key, &work, 3u32))?;
    let theirs = lgwks_bot::block_on(beta.submit(&key, &work, 3u32))?;
    let mine_run = mine.run_id().ok_or("alpha's submission names a run")?;
    let theirs_run = theirs.run_id().ok_or("beta's submission names a run")?;
    assert_ne!(
        mine_run, theirs_run,
        "one key under two tenants derives two runs"
    );
    assert_eq!(counter.get(), 2, "each tenant ran its own body");
    assert_eq!(
        alpha
            .run_store()
            .ok_or("a stored host keeps a store")?
            .tenant_of(mine_run)
            .as_deref(),
        Some("alpha")
    );
    assert_eq!(
        beta.run_store()
            .ok_or("a stored host keeps a store")?
            .tenant_of(theirs_run)
            .as_deref(),
        Some("beta")
    );
    Ok(())
}

/// A dropped waiter leaves a durable receipt and no terminal; a later client
/// reattaches and gets the uncertainty and the run to settle.
#[test]
fn a_dropped_client_leaves_the_request_in_flight_for_a_later_client() -> TestResult {
    let scratch = Scratch::new("req-inflight")?;
    let host = stored_host("acme", scratch.path())?;
    let recorded = Arc::new(AtomicBool::new(false));
    let work = parking_task(Arc::clone(&recorded))?;
    let key = RequestKey::new("long-running")?;

    // Drive the submission until its first durable step is recorded, then drop
    // the waiter. The shared driver polls the same future a real caller awaits;
    // the signal is set only after `remember` returned, so seeing it means the
    // record is on the disk.
    drop_after_first_step(Box::pin(host.submit(&key, &work, 9u32)), &recorded);

    // A later client, same key and input, reattaches to the receipt. The run
    // never reached a terminal outcome, so this is the in-flight arm — not a
    // fabricated success and not a re-run of the parked body.
    let later = lgwks_bot::block_on(host.submit(&key, &work, 9u32))?;
    match later {
        Submission::InFlight(in_flight) => {
            assert_eq!(in_flight.task(), "parking");
            assert!(
                in_flight.records() >= 2,
                "the receipt and the first step's record both survive the drop, saw {}",
                in_flight.records()
            );
            let run = in_flight.run();
            assert_eq!(
                host.run_store()
                    .ok_or("a stored host keeps a store")?
                    .record_count(run),
                in_flight.records(),
                "the reported count is the store's own"
            );
        }
        other => return Err(format!("expected an in-flight report, got {other:?}").into()),
    }
    Ok(())
}

/// A key outside the identifier set is a typed refusal at construction.
#[test]
fn a_malformed_request_key_is_refused() -> TestResult {
    for refused in ["", "has space", "has/slash", "has\nnewline"] {
        let error = RequestKey::new(refused)
            .err()
            .ok_or_else(|| format!("the key {refused:?} must be refused"))?;
        assert!(
            matches!(error, FlowError::InvalidRequestKey { .. }),
            "a bad key is a typed refusal, not a string: {error}"
        );
    }
    assert_eq!(
        RequestKey::new("ok-key_1.v2:3")?.as_str(),
        "ok-key_1.v2:3",
        "a key of allowed characters is accepted verbatim"
    );
    Ok(())
}

/// A durable submission with no store refuses rather than degrading to a run.
#[test]
fn a_submission_without_a_store_is_refused() -> TestResult {
    let host = Host::builder("acme")?.build()?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let error = lgwks_bot::block_on(host.submit(&RequestKey::new("k")?, &work, 1u32))
        .err()
        .ok_or("a host with no store must refuse a durable submission")?;
    assert!(
        matches!(error, RequestError::NoStore),
        "the refusal names the missing store: {error}"
    );
    assert_eq!(counter.get(), 0, "no body ran for a refused submission");
    Ok(())
}

/// A body whose record already exists is not consulted again: the recorded
/// value wins even when a differently-shaped body would answer otherwise.
#[test]
fn a_recorded_request_answers_over_a_changed_body() -> TestResult {
    let scratch = Scratch::new("req-changed")?;
    let host = stored_host("acme", scratch.path())?;
    let key = RequestKey::new("stable")?;

    let first = task(
        "value",
        |scope: lgwks_bot::script::Scope, n: u32| async move {
            lgwks_bot::script::remember(&scope, "s", || async move { Ok::<_, FlowError>(n) }).await
        },
    )?;
    let recorded = lgwks_bot::block_on(host.submit(&key, &first, 1u32))?;
    assert_eq!(
        recorded
            .report()
            .and_then(|report| report.output().copied()),
        Some(1)
    );

    // Same key and input, a body that would answer differently. The receipt is
    // the truth about that request, so the recorded value wins.
    let second = task(
        "value",
        |scope: lgwks_bot::script::Scope, n: u32| async move {
            lgwks_bot::script::remember(&scope, "s", || async move {
                Ok::<_, FlowError>(n.saturating_mul(100))
            })
            .await
        },
    )?;
    let again = lgwks_bot::block_on(host.submit(&key, &second, 1u32))?;
    assert_eq!(
        again.report().and_then(|report| report.output().copied()),
        Some(1),
        "the recorded value answers; the changed body never runs"
    );
    assert!(matches!(again, Submission::Reattached(_)));
    Ok(())
}

/// A host stopped mid-run does not record the stop as the request's outcome, so
/// the request is still completable and a later submission of the same key
/// re-drives the body from its recorded step.
#[test]
fn a_host_stop_mid_run_leaves_the_request_resumable() -> TestResult {
    let scratch = Scratch::new("req-stopped")?;
    let key = RequestKey::new("interrupted")?;
    let counter = request::FirstStepRuns::new();
    // The first run: a body that waits forever after its first durable step, so
    // the host's stop is the only thing that can end it.
    let host = stored_host("acme", scratch.path())?;
    let recorded = Arc::new(AtomicBool::new(false));
    let never = Arc::new(AtomicBool::new(false));
    let parked = two_step_task(Arc::clone(&recorded), Arc::clone(&never), counter.clone())?;

    let stopped = stop_mid_run(&host, &key, &parked, 11u32, &recorded)?;
    assert_eq!(
        disposition_of(&stopped)?,
        Disposition::Cancelled,
        "the caller sees the host's stop: a stop arrived after admission, so the run was admitted \
         and then cancelled"
    );
    let run = stopped.run_id().ok_or("the stopped run names a run")?;
    let stopped_records = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .record_count(run);
    assert!(
        stopped_records >= 2,
        "the stop left the receipt and the first step's record, saw {stopped_records}"
    );
    assert_eq!(
        counter.count(),
        1,
        "the stopped run performed the first effect exactly once"
    );

    // A host that did not stop, over the *reopened* file, sees a request with a
    // receipt and no terminal record — which is the only shape a resume can
    // start from.
    let reopened = stored_host("acme", scratch.path())?;
    let later =
        lgwks_bot::block_on(reopened.submit(&key, &work_two_step(counter.clone())?, 11u32))?;
    let after_submit = counter.count();
    match later {
        Submission::InFlight(in_flight) => {
            assert_eq!(
                in_flight.run(),
                run,
                "the in-flight request names the same run"
            );
            assert_eq!(
                in_flight.task(),
                "two-step",
                "the in-flight report names the task"
            );
            assert!(
                in_flight.records() >= 2,
                "the receipt and the first step's record survive the stop, saw {}",
                in_flight.records()
            );
        }
        other => {
            return Err(format!(
                "a host stop reattached as {:?}; a request whose run was stopped has no verdict of \
                 its own to reattach to",
                other.report().map(lgwks_bot::task::Report::disposition)
            )
            .into());
        }
    }
    assert_eq!(
        after_submit, 1,
        "a submission of an in-flight request reports the uncertainty without re-running the body"
    );

    // A resume drives it to completion. The first step's record is on the disk,
    // so its effect must not happen a second time.
    let settled =
        lgwks_bot::block_on(reopened.resume(run, &work_two_step(counter.clone())?, 11u32));
    assert_eq!(
        settled.disposition(),
        Disposition::Succeeded,
        "the resumed run completes"
    );
    assert_eq!(
        settled.output().copied(),
        Some(11),
        "the resumed run answers the input"
    );
    assert_eq!(
        counter.count(),
        1,
        "the first effect happened once across the stop, the in-flight report and the resume"
    );
    Ok(())
}

/// A body of the shape that lets a run go on to its second step, sharing
/// `counter` so the test can read how many times the first effect ran.
///
/// # Errors
///
/// [`FlowError::InvalidName`] for the fixed name.
fn work_two_step(
    counter: request::FirstStepRuns,
) -> Result<
    lgwks_bot::task::Task<impl Fn(lgwks_bot::script::Scope, u32) -> request::BodyFuture>,
    FlowError,
> {
    two_step_task(
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        counter,
    )
}

/// A refusal before admission is the host declining to finish the request, so it
/// records no terminal outcome and never reattaches as a verdict.
#[test]
fn a_refusal_before_admission_is_not_recorded_as_the_outcome() -> TestResult {
    let scratch = Scratch::new("req-refused")?;
    let store = RunStore::open(scratch.join("shared.runstore"))?;
    let stopping = host_on("acme", store.clone())?;
    let healthy = host_on("acme", store)?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let key = RequestKey::new("refused-before-admission")?;

    // A host that is already stopped refuses before admission, so no body ever
    // runs. The receipt is still written: it binds the key to this input, which
    // is what makes the refusal this one call's and not the request's.
    stopping.cancel();
    let refused = lgwks_bot::block_on(stopping.submit(&key, &work, 6u32))?;
    assert_eq!(
        disposition_of(&refused)?,
        Disposition::Refused,
        "a stopped host refuses before admission rather than cancelling a run that never started"
    );
    assert_eq!(counter.get(), 0, "no body ran for a refused admission");

    // The healthy host submits the same key and the same input. The key is bound
    // to the input by the receipt the refused call wrote, so the healthy host
    // takes the same door a reattach takes — and finds no recorded verdict,
    // because a refusal is the host's and not the request's.
    let seen = lgwks_bot::block_on(healthy.submit(&key, &work, 6u32))?;
    let run = seen.run_id().ok_or("the healthy submission names a run")?;
    let in_flight = match seen {
        Submission::InFlight(in_flight) => in_flight,
        other => {
            return Err(format!(
                "a refusal before admission reattached as {:?}; a key nothing ever completed \
                 must stay completable",
                other.report().map(lgwks_bot::task::Report::disposition)
            )
            .into());
        }
    };
    assert_eq!(
        in_flight.run(),
        run,
        "the in-flight request names the derived run"
    );
    assert_eq!(
        in_flight.records(),
        1,
        "the receipt is the only record the refused call wrote: no step ran, so no step was \
         recorded, and no terminal verdict"
    );
    assert_eq!(
        counter.get(),
        0,
        "the healthy host did not run the body either"
    );

    // A resume drives it on the host that can admit it. This is the control on
    // the other side: what *is* the request's own verdict is recorded, and a
    // repeat reattaches to it.
    let settled = lgwks_bot::block_on(healthy.resume(run, &work, 6u32));
    assert_eq!(
        settled.disposition(),
        Disposition::Succeeded,
        "the healthy host runs the request to its own verdict"
    );
    assert_eq!(
        counter.get(),
        1,
        "the body ran once, on the host that admitted it"
    );
    let repeat = lgwks_bot::block_on(healthy.submit(&key, &work, 6u32))?;
    assert!(
        matches!(repeat, Submission::Reattached(_)),
        "a success is the request's verdict, so a repeat reattaches to it"
    );
    assert_eq!(repeat.run_id(), Some(run), "a reattach names the same run");
    assert_eq!(counter.get(), 1, "the reattach ran no body");
    Ok(())
}

/// A failure reason whose archived `@terminal` record is refused by the store's
/// per-record byte ceiling.
///
/// The ceiling itself, not some large number: the archived record carries this
/// reason plus the disposition code, the step path and the framing head, so a
/// reason at the ceiling is already past what one record may carry, and the
/// smallest refusal is the one that proves the *declared bound* is what refused
/// the write rather than some other accident of size.
const OVER_CEILING_REASON: usize = lgwks_bot::task::MAX_RECORD_BYTES;

/// A store that refuses the `@terminal` write is reported as `Failed`, not
/// returned as a success nobody can find later.
///
/// Recording an outcome and reporting success are one fact (INV-BOT-102), so
/// the resume that *reached* a verdict but could not record it reports the
/// refusal — and, critically, records nothing: the request stays unsettled, so a
/// later client is told `InFlight` rather than being handed a verdict that was
/// never written. This is the error path through `Host::resume`, and it had no
/// coverage at all.
///
/// The refusal is real rather than mocked: the run's failure reason is archived
/// into the `@terminal` record, and one byte past the store's declared
/// per-record ceiling makes the framing refuse with the typed ceiling error.
#[test]
fn a_store_that_refuses_the_terminal_write_reports_the_refusal() -> TestResult {
    let scratch = Scratch::new("req-ceiling")?;
    let store = RunStore::open(scratch.join("shared.runstore"))?;
    let parking = host_on("acme", store.clone())?;
    let settling = host_on("acme", store.clone())?;
    let recorded = Arc::new(AtomicBool::new(false));
    let parked = parking_task(Arc::clone(&recorded))?;
    let key = RequestKey::new("verdict-the-store-will-refuse")?;

    // The first submission leaves the request in flight: the body parks after
    // its first durable record, so there is a receipt, a record, and no verdict.
    drop_after_first_step(Box::pin(parking.submit(&key, &parked, 1u32)), &recorded);
    let seen = lgwks_bot::block_on(settling.submit(&key, &parked, 1u32))?;
    let run = match seen {
        Submission::InFlight(in_flight) => in_flight.run(),
        other => {
            return Err(format!(
                "a dropped waiter must leave the request in flight; got {other:?}"
            )
            .into());
        }
    };

    // A body whose failure reason is at the ceiling a terminal record may carry,
    // so the settle-time write is refused by the store's own bound.
    let oversize = "r".repeat(OVER_CEILING_REASON);
    let oversize_task = task(
        "oversize",
        move |_scope: lgwks_bot::script::Scope, _: u32| {
            let oversize = oversize.clone();
            async move { Err::<u32, _>(FlowError::failed(oversize)) }
        },
    )?;

    let settled = lgwks_bot::block_on(settling.resume(run, &oversize_task, 1u32));
    assert_eq!(
        settled.disposition(),
        Disposition::Failed,
        "a run that reached a verdict the store refused to record is Failed, not Succeeded"
    );
    let error = settled.error().ok_or("the refusal names its failure")?;
    let rendered = error.to_string();
    assert!(
        rendered.contains("was not recorded"),
        "the report names that the outcome was not recorded: {rendered}"
    );
    assert!(
        rendered.contains("record's bytes"),
        "the report names the ceiling that refused it, rather than only that something failed: \
         {rendered}"
    );

    // Nothing was recorded as the verdict, so the request is still unsettled: a
    // reopen reports `InFlight` rather than handing a later client a verdict
    // that was never written.
    let reopened = host_on("acme", store)?;
    let after = lgwks_bot::block_on(reopened.submit(&key, &oversize_task, 1u32))?;
    assert!(
        matches!(after, Submission::InFlight(_)),
        "a request whose verdict the store refused is still unsettled, not reattached to; got {:?}",
        after.report().map(lgwks_bot::task::Report::disposition)
    );
    Ok(())
}

/// A failure the *body* produced is the request's verdict, so it is recorded:
/// the control on the two tests above.
#[test]
fn a_failed_run_is_the_requests_recorded_outcome() -> TestResult {
    let scratch = Scratch::new("req-failed")?;
    let host = stored_host("acme", scratch.path())?;
    let counter = Rc::new(Cell::new(0));
    let refusal = task(
        "failing",
        |_scope: lgwks_bot::script::Scope, value: u32| async move {
            if value == 0 {
                Err(FlowError::failed("the body refused this input"))
            } else {
                Ok(value)
            }
        },
    )?;
    let key = RequestKey::new("refused-by-the-body")?;

    let first = lgwks_bot::block_on(host.submit(&key, &refusal, 0u32))?;
    assert_eq!(
        disposition_of(&first)?,
        Disposition::Failed,
        "the body's own error is a failure, not a stop"
    );
    let run = first.run_id().ok_or("the failed run names a run")?;

    let again = lgwks_bot::block_on(host.submit(&key, &refusal, 0u32))?;
    assert!(
        matches!(again, Submission::Reattached(_)),
        "a recorded failure is the request's outcome, so a repeat reattaches to it"
    );
    assert_eq!(
        again.report().map(lgwks_bot::task::Report::disposition),
        Some(Disposition::Failed),
        "the reattached report carries the recorded failure"
    );
    assert_eq!(again.run_id(), Some(run), "a reattach names the same run");
    assert_eq!(
        counter.get(),
        0,
        "the counter task was never submitted under this key"
    );
    Ok(())
}

/// The declared deadline expiring mid-run is the request's own verdict, so it is
/// recorded: the control that separates it from a host stop.
///
/// The two dispositions differ in the way that matters to a key. A stop is the
/// host declining to finish the run, so it records nothing and the key stays
/// completable. A deadline is the run reaching the budget its own declaration
/// fixed, which is a fact about the request and not about this host — so it is
/// recorded, and a later client of the same key reattaches to it instead of
/// re-running a body that has already overrun once.
#[test]
fn an_expired_deadline_is_the_requests_recorded_outcome() -> TestResult {
    let scratch = Scratch::new("req-deadline")?;
    let runs = request::FirstStepRuns::new();
    let budget = Duration::from_millis(50);
    let host = host_with_deadline("acme", scratch.path(), budget)?;
    let work = overrunning_task(runs.clone())?;
    let key = RequestKey::new("overruns-its-budget")?;

    let first = lgwks_bot::block_on(host.submit(&key, &work, 3u32))?;
    assert_eq!(
        disposition_of(&first)?,
        Disposition::DeadlineExceeded,
        "a body that outruns its declared budget is DeadlineExceeded"
    );
    assert_eq!(
        runs.count(),
        1,
        "the first durable step ran and recorded before the deadline fired"
    );
    let run = first.run_id().ok_or("the overrunning run names a run")?;

    let again = lgwks_bot::block_on(host.submit(&key, &work, 3u32))?;
    assert!(
        matches!(again, Submission::Reattached(_)),
        "the recorded deadline is the request's outcome, so a repeat reattaches to it; got {:?}",
        again.report().map(lgwks_bot::task::Report::disposition)
    );
    assert_eq!(
        again.report().map(lgwks_bot::task::Report::disposition),
        Some(Disposition::DeadlineExceeded),
        "the reattached report carries the recorded deadline"
    );
    assert_eq!(again.run_id(), Some(run), "a reattach names the same run");
    assert_eq!(
        runs.count(),
        1,
        "the reattach never entered the body again: a request that already overran \
         its budget must not overrun it a second time"
    );
    Ok(())
}
