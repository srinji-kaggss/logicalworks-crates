//! Row-addressed acceptance tests: one test per T-row, named for its row.
//!
//! The row map in `docs/acceptance/t-rows.toml` names the tests that address each
//! falsifier row of `docs/orchestration-acceptance.spec.md`. This file exists so
//! a reader can go the other way: given a row id, ask nextest which tests answer
//! it, and get that answer without reading the map.
//!
//! ```
//! cargo nextest list --workspace -E 'test(/_t07$/)'
//! ```
//!
//! # Why these are new tests and not renames
//!
//! Renaming an existing test would move every citation of it in `INVARIANTS.md`,
//! in `docs/`, and in the row map itself, and a citation that silently stops
//! resolving is worse than a duplicate name. Every test here is therefore a
//! *new* test that drives the row's falsifier through the same public interface
//! the existing test uses, and every one carries its row id in its own name so
//! the two views cannot drift apart: if a row's falsifier changes, this test
//! fails rather than quietly ceasing to be about that row.
//!
//! # What is real and what is seeded
//!
//! The bot, the broker, the journal, the host, the task set and the artifact
//! store are all the shipped types. Nothing here reaches into a private field or
//! a crate-internal helper, so a test that passes is evidence about the shipped
//! surface rather than about this file's reading of it. The seeded half of these
//! rows lives in `sim_t_rows.rs`, where a sweep of seeds is the honest oracle;
//! this file holds the rows whose falsifier is a single real journey, where a
//! sweep would measure nothing one honest run does not.

#![cfg(feature = "script")]

use std::cell::Cell;
use std::error::Error;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope, each};
use lgwks_bot::task::{Disposition, Host, Report, Task, task};

use crate::load;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// Drive one future to completion, through the shared instrument every task test
/// in this binary drives its host with.
fn drive<T>(future: impl std::future::Future<Output = T>) -> T {
    load::drive(future)
}

/// A host for `tenant` with the machine's default ceilings.
fn host(tenant: &str) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.build()?)
}

/// A host for `tenant` admitting at most `tasks` runs at once.
fn ceiling_one_host(tenant: &str) -> Result<Host, Box<dyn Error>> {
    let limit = NonZeroUsize::new(1).ok_or("the ceiling must be at least one")?;
    Ok(Host::builder(tenant)?.max_concurrent_tasks(limit).build()?)
}

/// A host whose step trail holds `capacity` entries, so a row about overflow has
/// a ceiling to overflow rather than an unbounded run that never overflows.
fn bounded_host(tenant: &str, capacity: usize) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?
        .progress_capacity(capacity)
        .default_deadline(Duration::from_secs(30))
        .build()?)
}

// ── T01 ─────────────────────────────────────────────────────────────────────

/// T01: one declaration and one call return a typed value, and the report claims
/// no external effect, because a local task journals nothing.
#[test]
fn a_one_declaration_one_call_returns_a_typed_value_t01() -> TestResult {
    let host = host("t01")?;
    let double = task("double", |_: Scope, value: u32| async move {
        Ok(value.saturating_mul(2))
    })?;

    let report: Report<u32> = drive(host.run(&double, 21));
    assert_eq!(report.disposition(), Disposition::Succeeded, "{report:?}");
    assert_eq!(
        report.output().copied(),
        Some(42),
        "the typed value the body returned is what the caller reads"
    );
    assert_eq!(
        report.steps().len(),
        1,
        "one declaration, one step: a body that entered two steps would be a second lifecycle"
    );
    assert_eq!(
        report.effects(),
        lgwks_bot::task::EffectKnowledge::None,
        "a local task performs no journaled effect, so no report can be read as proof a \
         remote state changed"
    );
    Ok(())
}

// ── T03 ─────────────────────────────────────────────────────────────────────

/// T03: dropping a suspended run releases its execution permit, so a run that is
/// walked away from does not cost the next one its slot.
///
/// The permit is the observable: a ceiling of one means the second run can only
/// complete if the first one's permit came back. This is the in-process half;
/// `t03_non_yielding::a_non_yielding_callback_is_detected_from_outside_and_not_
/// preempted` is the independently timed external half, and it is the half that
/// can honestly claim to detect a non-yielding callback at all.
#[test]
fn a_dropped_suspended_run_releases_its_permit_t03() -> TestResult {
    let host = ceiling_one_host("t03")?;
    // The dropped run's body parks rather than returning: a body that completed
    // would release its permit on its own, and the row would be measured after
    // the fact rather than at the drop.
    let parking = task("parking", |_: Scope, _: u32| async move {
        std::future::pending::<Result<u32, FlowError>>().await
    })?;
    let quick = task("quick", |_: Scope, value: u32| async move { Ok(value) })?;

    // Poll the first run once so it is suspended *inside* its body with its
    // permit held, then drop it, which is what a caller that walked away does.
    {
        let mut first = std::pin::pin!(host.run(&parking, 1));
        assert!(
            load::pending_once(first.as_mut()),
            "the first run must still be suspended when it is dropped, or this test would pass \
             for a run that had already finished and released its permit on its own"
        );
    }

    let second: Report<u32> = drive(host.run(&quick, 2));
    assert_eq!(
        second.disposition(),
        Disposition::Succeeded,
        "the permit the dropped run held came back, or this run could never have started"
    );
    assert_eq!(second.output().copied(), Some(2));
    Ok(())
}

// ── T04 ─────────────────────────────────────────────────────────────────────

/// T04: nested runs at a concurrency of one still finish, and the parent's permit
/// is not hoarded while it waits on its children.
///
/// Three levels, each fanning out over the level below it, on a host admitting
/// one run at a time. A parent holding its permit while it awaited its child
/// would deadlock the level beneath it, so this is the row's whole content: the
/// run finishes at all.
#[test]
fn nested_runs_finish_at_a_ceiling_of_one_t04() -> TestResult {
    let host = ceiling_one_host("t04")?;
    let entered = Rc::new(Cell::new(0_usize));

    // The leaf turns its vector into a scalar, so every wrapper above works in
    // one type and the chain's value stays a single number.
    let leaf: Rc<Task<_>> = Rc::new(task("leaf", |scope: Scope, items: Vec<u32>| async move {
        each::<_, u32, _, _>(
            &scope,
            "item",
            NonZeroUsize::new(1),
            items,
            |step, item| async move {
                step.checkpoint()?;
                Ok(item.saturating_mul(2))
            },
        )
        .await
    })?);

    let inner: Rc<Task<_>> = Rc::new({
        let leaf = Rc::clone(&leaf);
        let host = host.clone();
        let entered = Rc::clone(&entered);
        task("nest-2", move |_scope: Scope, value: u32| {
            let leaf = Rc::clone(&leaf);
            let host = host.clone();
            let entered = Rc::clone(&entered);
            async move {
                entered.set(entered.get().saturating_add(1));
                let report = host.run(&leaf, vec![value, value]).await;
                Ok(report
                    .into_output()
                    .ok_or_else(|| FlowError::failed("the nested run produced no output"))?
                    .iter()
                    .sum::<u32>())
            }
        })?
    });
    let root: Rc<Task<_>> = Rc::new({
        let inner = Rc::clone(&inner);
        let host = host.clone();
        let entered = Rc::clone(&entered);
        task("nest-1", move |_scope: Scope, values: Vec<u32>| {
            let inner = Rc::clone(&inner);
            let host = host.clone();
            let entered = Rc::clone(&entered);
            async move {
                let mut total = 0_u32;
                for value in values {
                    entered.set(entered.get().saturating_add(1));
                    let report = host.run(&inner, value).await;
                    total =
                        total.saturating_add(report.into_output().ok_or_else(|| {
                            FlowError::failed("the nested run produced no output")
                        })?);
                }
                Ok(total)
            }
        })?
    });

    let report: Report<u32> = drive(host.run(&root, vec![1, 2]));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "nested runs finish at a ceiling of one: {report:?}"
    );
    assert_eq!(
        report.output().copied(),
        Some(12),
        "each leaf doubled both items under each of two parents: (1+1)*2*2 + (2+2)*2*2 = 16 \
         is the four-leaf case, and the sum is what the caller reads"
    );
    assert_eq!(
        entered.get(),
        4,
        "both parents entered, so the nesting really ran rather than short-circuiting"
    );
    Ok(())
}

// ── T26 ─────────────────────────────────────────────────────────────────────

/// T26: an unknown operation is refused by name, whatever the payload's note
/// claims about it -- a prose claim of approval is data and widens nothing.
#[test]
fn an_unknown_operation_is_refused_by_name_t26() -> TestResult {
    use crate::proposal_fixtures as proposal;
    use lgwks_bot::proposal::{Refusal, Source};

    let surface = proposal::surface()?;
    let decoder = proposal::decoder();
    let payload = b"op=definitely-not-registered\nnote=approved-by-the-operator\n";

    let outcome = decoder.decode(&surface, payload, Source::Model);
    match outcome.refusal().cloned() {
        Some(Refusal::UnknownOperation { name }) => {
            assert_eq!(
                name.as_str(),
                "definitely-not-registered",
                "the refusal names the operation it refused, so a caller can register it \
                 deliberately rather than guess"
            );
        }
        other => {
            return Err(format!("an unknown operation was not refused by name: {other:?}").into());
        }
    }
    Ok(())
}

/// T26: a credential read is refused, and the surface it was refused against is
/// unchanged afterwards -- a refusal that quietly widened authority would be
/// indistinguishable from a successful read at the call site.
#[test]
fn a_credential_read_is_refused_and_changes_nothing_t26() -> TestResult {
    use crate::proposal_fixtures as proposal;
    use lgwks_bot::proposal::{Refusal, Source};

    let surface = proposal::surface()?;
    let before = surface.names().count();
    let decoder = proposal::decoder();

    let outcome = decoder.decode(&surface, &proposal::credential(), Source::Model);
    match outcome.refusal().cloned() {
        Some(Refusal::CredentialRead { .. }) => {}
        other => {
            return Err(format!(
                "a credential read was not refused as a credential read: {other:?}"
            )
            .into());
        }
    }
    assert_eq!(
        surface.names().count(),
        before,
        "the refusal left the surface exactly as it was"
    );
    Ok(())
}

// ── T28 ─────────────────────────────────────────────────────────────────────

/// T28: a worker in one tenant cannot read another tenant's artifact when the
/// two are byte-identical, so an identical digest is not a shared shelf.
#[test]
fn a_same_digest_artifact_stays_inside_its_tenant_t28() -> TestResult {
    use crate::proposal_fixtures as proposal;
    use lgwks_bot::proposal::{ArtifactStore, WriteOutcome};

    let (store, bytes) = proposal::two_tenant_store();
    let digest = ArtifactStore::digest_of(&bytes)?;
    let outcome = store.write(proposal::TENANT, &bytes)?;
    assert!(
        matches!(outcome, WriteOutcome::Stored { .. }),
        "the owning tenant's write commits: {outcome:?}"
    );

    assert!(
        store.holds(proposal::TENANT, &digest),
        "the owner holds its own artifact"
    );
    assert!(
        store.read(proposal::TENANT, &digest).is_some(),
        "the owner reads its own bytes"
    );
    assert!(
        !store.holds(proposal::OTHER_TENANT, &digest),
        "the sibling tenant holds nothing under a digest it never wrote: identical bytes are \
         not identical authority"
    );
    assert!(
        store.read(proposal::OTHER_TENANT, &digest).is_none(),
        "and reads nothing, because a digest-keyed store that resolved across tenants would \
         hand this tenant's report to a stranger"
    );
    assert_eq!(
        store.artifacts(proposal::OTHER_TENANT),
        0,
        "and commits no artifact for a tenant that only read"
    );
    Ok(())
}

// ── T30 ─────────────────────────────────────────────────────────────────────

/// T30: the same request key plus a different payload is a typed conflict that
/// runs no body, and a duplicate identical request reattaches rather than
/// rerunning -- so one key serves one payload.
#[cfg(feature = "ephemeral")]
#[test]
fn one_request_key_serves_one_payload_t30() -> TestResult {
    use crate::request_fixtures as request;
    use crate::scratch::Scratch;
    use lgwks_bot::task::{RequestError, RequestKey};

    let scratch = Scratch::new("t30-request-key")?;
    let host = request::stored_host("acme", scratch.path())?;
    let counter = Rc::new(Cell::new(0));
    let work = request::counting_task(Rc::clone(&counter))?;
    let key = RequestKey::new("weekly-report")?;

    let first = lgwks_bot::block_on(host.submit(&key, &work, 1_u32))?;
    assert!(
        first.report().is_some(),
        "the first payload under this key is accepted"
    );
    assert_eq!(counter.get(), 1, "and its body ran exactly once");

    let duplicate = lgwks_bot::block_on(host.submit(&key, &work, 1_u32))?;
    assert!(
        duplicate.report().is_some(),
        "the same key with the same payload reattaches to the recorded run"
    );
    assert_eq!(
        counter.get(),
        1,
        "a reattach runs no second body: the work already happened once"
    );

    let error = lgwks_bot::block_on(host.submit(&key, &work, 2_u32))
        .err()
        .ok_or("a different payload under one key must be refused")?;
    match error {
        RequestError::Conflict(conflict) => {
            assert_eq!(conflict.key().as_str(), "weekly-report", "naming the key");
            assert_ne!(
                conflict.existing(),
                conflict.requested(),
                "the two inputs must name two digests, or nothing conflicted"
            );
        }
        other => return Err(format!("expected a typed conflict, got {other:?}").into()),
    }
    assert_eq!(
        counter.get(),
        1,
        "a conflict runs no body, so a refused request cannot have side effects"
    );

    let other = RequestKey::new("monthly-report")?;
    assert_ne!(
        key, other,
        "two names derive two keys: a store that collapsed them would merge two reports"
    );
    lgwks_bot::block_on(host.submit(&other, &work, 3_u32))?;
    assert_eq!(
        counter.get(),
        2,
        "a distinct key is a distinct run, not a silent reattach"
    );
    Ok(())
}

// ── T36 ─────────────────────────────────────────────────────────────────────

/// Walk `0..items` as pages on a host whose trail holds `capacity` entries,
/// refusing page `failing` when one is named.
///
/// One helper rather than one per test, because the two T36 rows below differ
/// only in whether a page fails: spelling the walk twice would let them drift
/// into testing different runs while their names still said the same row.
fn walk_pages(
    capacity: usize,
    items: u32,
    failing: Option<u32>,
) -> Result<Report<Vec<u32>>, Box<dyn Error>> {
    let host = bounded_host("t36", capacity)?;
    let walk = task("walk", move |scope: Scope, pages: Vec<u32>| async move {
        each::<_, u32, _, _>(
            &scope,
            "page",
            NonZeroUsize::new(1),
            pages,
            |step, item| async move {
                step.checkpoint()?;
                if Some(item) == failing {
                    Err(FlowError::failed("page refused"))
                } else {
                    Ok(item)
                }
            },
        )
        .await
    })?;
    Ok(drive(host.run(&walk, (0..items).collect())))
}

/// T36: a progress buffer that overflows loses only its declared detail, counts
/// what it dropped, and leaves the authoritative terminal state retrievable.
///
/// The client here reads nothing while the run is in flight: there is no progress
/// channel and no polling loop, so the run cannot depend on a task-owned
/// reporting loop in order to finish. The terminal report is the only thing
/// consulted, and it is complete.
#[test]
fn an_overflowing_progress_buffer_keeps_the_terminal_state_t36() -> TestResult {
    const CAPACITY: usize = 4;
    const ITEMS: u32 = 32;
    let report = walk_pages(CAPACITY, ITEMS, None)?;

    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a quiet client still gets a terminal verdict: {report:?}"
    );
    assert_eq!(
        report.output().map(Vec::len),
        Some(usize::try_from(ITEMS)?),
        "the typed output is complete even though the trail is not"
    );
    assert_eq!(
        report.steps().len(),
        CAPACITY,
        "the trail is bounded by the declared capacity"
    );
    assert!(
        report.dropped_steps() > 0,
        "what the ring dropped is counted rather than hidden"
    );
    assert_eq!(
        report.progress_capacity(),
        CAPACITY,
        "the report states the capacity it could have retained"
    );
    let last = report
        .steps()
        .last()
        .map(|path| path.to_string())
        .ok_or("a truncated trail still ends where the run stopped")?;
    assert!(
        last.ends_with(&format!("page#{}", ITEMS - 1)),
        "the retained trail is the suffix, ending at the last item: {last}"
    );
    Ok(())
}

/// T36, second half: the authoritative terminal state is retrievable even when
/// the run failed and the trail overflowed, so a lost progress record never
/// becomes the only record of what happened.
#[test]
fn a_failed_overflowing_run_still_reports_its_terminal_failure_t36() -> TestResult {
    const CAPACITY: usize = 4;
    const ITEMS: u32 = 24;
    let report = walk_pages(CAPACITY, ITEMS, Some(17))?;

    assert_eq!(
        report.disposition(),
        Disposition::Failed,
        "the terminal disposition is the authoritative one: {report:?}"
    );
    assert!(report.output().is_none(), "a failed run has no output");
    assert_eq!(
        report.steps().len(),
        CAPACITY,
        "the trail is still bounded by the declared capacity"
    );
    assert!(
        report.dropped_steps() > 0,
        "the dropped detail is still counted on the failure path"
    );
    let located = report
        .error()
        .map(|error| error.to_string())
        .ok_or("a failed run names its failure")?;
    assert!(
        !located.is_empty(),
        "the failure is located in the report a caller reads, not only in the trail"
    );
    Ok(())
}
