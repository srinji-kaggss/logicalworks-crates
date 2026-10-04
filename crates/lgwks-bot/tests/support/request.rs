//! Shared fixture for the request-key submission targets (`request_key` and
//! `sim_request_key`).
//!
//! One definition of the two bodies and the two host constructors the durable
//! submission tests need, so the two targets cannot drift: a copy per file is
//! how one target's "the body ran once" stops meaning the same thing as the
//! other's.
#![allow(
    dead_code,
    reason = "each including request target uses a different subset of the fixture"
)]

use std::cell::Cell;
use std::error::Error;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::task::Poll;
use std::time::Instant;

use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, RunStore, Task, task};

/// The named future every request body returns.
pub type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// A host for `tenant` over a store opened under `dir`.
///
/// # Errors
///
/// The tenant validation or the store open.
pub fn stored_host(tenant: &str, dir: &Path) -> Result<Host, Box<dyn Error>> {
    Host::builder(tenant)?
        .run_store(dir)?
        .build()
        .map_err(Into::into)
}

/// A host for `tenant` over a store handle the caller already opened, so two
/// tenants can share one file.
///
/// # Errors
///
/// The tenant validation or the host build.
pub fn host_on(tenant: &str, store: RunStore) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.store(store).build()?)
}

/// A host per tenant, each over a clone of one shared store handle.
///
/// One definition for every multi-tenant request target, so two tenants cannot
/// be given two independent store handles (two writers over one file) by
/// accident.
///
/// # Errors
///
/// The tenant validation or the host build for any tenant.
pub fn hosts_over(tenants: &[&str], store: &RunStore) -> Result<Vec<Host>, Box<dyn Error>> {
    let mut hosts = Vec::with_capacity(tenants.len());
    for tenant in tenants {
        hosts.push(host_on(tenant, store.clone())?);
    }
    Ok(hosts)
}

/// How many distinct run spellings `runs` holds.
///
/// One definition, so "every key derived a distinct run" is measured the same
/// way in the swept tier and in the opt-in measurement.
pub fn distinct_runs(runs: &[String]) -> usize {
    let mut sorted = runs.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.len()
}

/// A task whose whole body is one durable `remember` step that counts how many
/// times the body was entered.
///
/// The counter is the instrument: "reattached without re-running" and
/// "re-executed" both return the value, and only the number of body entries
/// distinguishes them.
///
/// # Errors
///
/// [`FlowError::InvalidName`] for the fixed name.
pub fn counting_task(
    counter: Rc<Cell<u32>>,
) -> Result<Task<impl Fn(Scope, u32) -> BodyFuture>, FlowError> {
    task("counted", move |scope: Scope, value: u32| -> BodyFuture {
        counter.set(counter.get().saturating_add(1));
        Box::pin(
            async move { remember(&scope, "v", || async move { Ok::<_, FlowError>(value) }).await },
        )
    })
}

/// A task that records one durable step, signals it, then parks forever.
///
/// The shape a client that drops its waiter mid-effect leaves behind: the first
/// step's record is durable (the signal is set only after `remember` returns),
/// and the second step's effect is in flight with no record.
///
/// # Errors
///
/// [`FlowError::InvalidName`] for the fixed name.
pub fn parking_task(
    recorded: Arc<AtomicBool>,
) -> Result<Task<impl Fn(Scope, u32) -> BodyFuture>, FlowError> {
    task("parking", move |scope: Scope, value: u32| -> BodyFuture {
        let recorded = Arc::clone(&recorded);
        Box::pin(async move {
            let first =
                remember(&scope, "first", || async move { Ok::<_, FlowError>(value) }).await?;
            recorded.store(true, Ordering::SeqCst);
            std::future::pending::<Result<(), FlowError>>().await?;
            Ok(first)
        })
    })
}

/// Drive `submit` until its first durable step is recorded, then drop it.
///
/// The loop polls the same future a real caller awaits and re-wakes itself, so
/// the runtime keeps turning until the first `remember` has been acknowledged
/// (its signal is set only after it returns). Dropping the future there leaves
/// exactly the state a client that walked away mid-effect leaves: a receipt and
/// a first record on the disk, and no terminal outcome.
///
/// One definition for both request targets, so "drop the waiter" means the same
/// thing in the unit test and in the simulation.
///
/// A submission that returns before its first record is a body that never reached one, and
/// the flag says so to every caller that reads it afterwards rather than the loop spinning
/// on a future that can make no further progress.
pub fn drop_after_first_step<F: Future>(mut submit: Pin<Box<F>>, recorded: &AtomicBool) {
    lgwks_bot::block_on(std::future::poll_fn(|context| {
        if recorded.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        // A finished submission is never polled again: the body sets the
        // flag inside the very poll that may also finish `submit`, and
        // re-polling a completed future is a panic. Either way the waiter is
        // dropped below, so the caller still sees the state a client that
        // walked away leaves.
        if submit.as_mut().poll(context).is_ready() {
            return Poll::Ready(());
        }
        context.waker().wake_by_ref();
        Poll::Pending
    }));
    drop(submit);
}

/// How long a stop thread waits for a body's first record before it gives up
/// and stops the host anyway.
///
/// A bound rather than an open wait, because a body that never records would
/// otherwise park the whole suite until CI killed it with nothing to read. The
/// ceiling is orders of magnitude above any observed run and costs the thread
/// one clock read per turn.
pub const STOP_THREAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Submit `work` under `key` while another OS thread stops `host` once the
/// body's first durable step is committed.
///
/// The stop has to come from another OS thread — the body cannot stop its own
/// host, or it would be cancelled before any step ran — and it waits on the
/// body's *record* signal rather than on any step counter, because a step whose
/// record has not landed is not a step a resume can replay.
///
/// # Errors
///
/// Whatever the submission reported, or an explanation naming which side failed.
pub fn stop_mid_run<F>(
    host: &lgwks_bot::task::Host,
    key: &lgwks_bot::task::RequestKey,
    work: &lgwks_bot::task::Task<F>,
    input: u32,
    recorded: &Arc<AtomicBool>,
) -> Result<lgwks_bot::task::Submission<u32>, Box<dyn Error>>
where
    F: Fn(Scope, u32) -> BodyFuture,
{
    let stopper_host = host.clone();
    let stopper_recorded = Arc::clone(recorded);
    let (reported, stopped) = std::thread::scope(|scope| {
        let stopper = scope.spawn(move || {
            let limit = Instant::now().checked_add(STOP_THREAD_LIMIT);
            while !stopper_recorded.load(Ordering::SeqCst) {
                if limit.is_none_or(|until| Instant::now() >= until) {
                    // Bounded rather than open: a body that never records would
                    // otherwise park the suite until CI killed it, with nothing
                    // to read. Stopping here makes the run a seeded failure
                    // instead, which names the seed.
                    stopper_host.cancel();
                    return false;
                }
                std::thread::park_timeout(std::time::Duration::from_millis(1));
            }
            stopper_host.cancel();
            true
        });
        let reported = lgwks_bot::block_on(host.submit(key, work, input));
        (reported, stopper.join())
    });
    match stopped {
        Ok(true) => {}
        Ok(false) => {
            let refusal = Err(
                "the body never recorded its first step, so the stop had nothing to follow".into(),
            );
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "stop_mid_run: returning an error to the caller");
            return refusal;
        }
        Err(_) => {
            let refusal = Err("the thread that stopped the host panicked".into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "stop_mid_run: returning an error to the caller");
            return refusal;
        }
    }
    reported.map_err(Into::into)
}

/// How many times a two-step body's *first* durable step's body ran.
///
/// Separate from an entry counter because the question both the stopped-host
/// and the seeded family ask is narrower than "did the body run": a resume
/// replays the *recorded* first step without running its body, so an entry
/// counter would report two entries where one effect happened. The body runs
/// exactly once only if the step record was absent when it was reached.
///
/// The counter is [`AtomicU32`] rather than a `Cell` because the stop has to be
/// fired from another OS thread, and an [`Arc`] of it shares one definition
/// between the fixture and the handle that stops the host.
#[derive(Debug, Default, Clone)]
pub struct FirstStepRuns(Arc<AtomicU32>);

impl FirstStepRuns {
    /// A counter at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times the first step's body ran.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A task with two durable `remember` steps, the first of which signals once
/// its record is committed.
///
/// The pair is what makes "the run was stopped half-way" an observable state
/// rather than a claim: the first step's record is on the disk when the signal
/// fires, and the second step only runs if `after_first` is set. A test that
/// never sets it is a test whose run ends by the host's stop instead, which is
/// the whole point of the shape. The signal is set only *after* `remember`
/// returned, so observing it means the record is durable.
///
/// `runs` counts how many times the *first* step's body ran, which is what
/// distinguishes a resume that replayed its record from one that re-ran the
/// effect. The counter is reached through an [`Arc`] rather than by name so the
/// closure stays `Fn`: a task body is called once per run, and a body that
/// consumed its own counter could not be called twice.
///
/// # Errors
///
/// [`FlowError::InvalidName`] for the fixed name.
pub fn two_step_task(
    recorded: Arc<AtomicBool>,
    after_first: Arc<AtomicBool>,
    runs: FirstStepRuns,
) -> Result<Task<impl Fn(Scope, u32) -> BodyFuture>, FlowError> {
    let counter = runs.0;
    task("two-step", move |scope: Scope, value: u32| -> BodyFuture {
        let recorded = Arc::clone(&recorded);
        let after_first = Arc::clone(&after_first);
        let first_step = counted_first_step(scope.clone(), &counter, value);
        Box::pin(async move {
            let first = first_step.await?;
            recorded.store(true, Ordering::SeqCst);
            // The second step runs only when the caller has released this body;
            // until then the run ends by the host's stop, which is the state the
            // interrupted-request tests are about.
            while !after_first.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            remember(
                &scope,
                "second",
                || async move { Ok::<_, FlowError>(first) },
            )
            .await
        })
    })
}

/// The durable step both interrupted-request fixtures open with: `value`
/// recorded under `first`, with `counter` counting each time the step's body
/// actually runs.
///
/// One definition so "the recorded step ran once" means the same thing for the
/// stopped, dropped and overrun families: a resume that replays the record does
/// not run the body, so the count stays where the first run left it.
fn counted_first_step(
    scope: Scope,
    counter: &Arc<AtomicU32>,
    value: u32,
) -> impl Future<Output = Result<u32, FlowError>> + use<> {
    let counter = Arc::clone(counter);
    async move {
        remember(&scope, "first", || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<_, FlowError>(value)
            }
        })
        .await
    }
}

/// A task whose body records one durable step and then waits for something that
/// never arrives.
///
/// The shape a *deadline* ends rather than a host stop: the first step's record
/// is committed and its effect counted, so the run has demonstrably made
/// progress, and the second part can only end by the host's declared deadline
/// firing. That is the whole point of the fixture — a deadline that fires before
/// the first step records would prove nothing about a request whose recorded
/// work has to survive it, and a host stop would be the wrong disposition
/// (INV-BOT-102 records neither, so the two must not be confused).
///
/// `runs` is the same [`FirstStepRuns`] the stop fixture uses, so "the recorded
/// step ran once" means one thing across the stopped, dropped and overrun
/// families.
///
/// # Errors
///
/// [`FlowError::InvalidName`] for the fixed name.
pub fn overrunning_task(
    runs: FirstStepRuns,
) -> Result<Task<impl Fn(Scope, u32) -> BodyFuture>, FlowError> {
    let counter = runs.0;
    task(
        "overrunning",
        move |scope: Scope, value: u32| -> BodyFuture {
            let first_step = counted_first_step(scope, &counter, value);
            Box::pin(async move {
                let first = first_step.await?;
                std::future::pending::<()>().await;
                Ok(first)
            })
        },
    )
}

/// A host for `tenant` over the store at `path`, with the deadline `budget`.
///
/// Separate from [`stored_host`] because a deadline is part of what a request's
/// *declaration* fixes, and a fixture that let one target declare it and another
/// inherit the crate default would make "the declared deadline expired" mean two
/// different things in the two places it is claimed.
///
/// # Errors
///
/// The tenant validation, the store open or the host build.
pub fn host_with_deadline(
    tenant: &str,
    dir: &Path,
    budget: std::time::Duration,
) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?
        .default_deadline(budget)
        .run_store(dir)?
        .build()?)
}
