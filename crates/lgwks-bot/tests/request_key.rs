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

use lgwks_bot::script::FlowError;
use lgwks_bot::task::{Disposition, Host, RequestError, RequestKey, RunStore, Submission, task};

// The scratch directory, shared with the resume targets.
#[path = "support/resume.rs"]
mod shared;
// The request fixtures themselves.
#[path = "support/request.rs"]
mod request;

use request::{counting_task, drop_after_first_step, host_on, parking_task, stored_host};
use shared::Scratch;

type TestResult = Result<(), Box<dyn Error>>;

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
