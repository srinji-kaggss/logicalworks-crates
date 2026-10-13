//! Black-box acceptance for ticking a `Bot` on the caller's async runtime.
//!
//! The defect these pin: a tick that drives its verb futures with a
//! thread-parking executor parks the thread it was called on, and inside a
//! current-thread runtime that thread is the one holding the driver. The timer
//! never fires, the socket never reports ready, the sibling task a verb is
//! waiting on never runs, and the tick never returns — a deadlock, not a slow
//! tick. The repair has three parts and this file covers all three: the tick is
//! awaitable ([`Bot::tick_async`]), the synchronous adapter refuses on a thread
//! a runtime already drives ([`BotError::TickInsideRuntime`]) rather than
//! parking it, and the synchronous adapter still works where it is the only
//! option — a plain thread, on futures that need no reactor.
//!
//! # Why every body runs in a child process
//!
//! These are liveness defects, so the failure mode is a hang. An in-process
//! timeout is not enough on its own: the timer that would report the timeout
//! needs the very driver the defect parks, so a regression would wedge the
//! suite instead of failing it — and libtest has no per-test deadline. Each
//! test therefore re-executes its own binary ([`guarded`]) and the parent waits
//! on that child with a deadline, kills it, and reports the timeout as a
//! failure. The child is the reporter; the parent is the one that survives to
//! report it.
//!
//! Timed tests also carry an in-process [`timeout`] around the tick. That
//! bounds a hang the *runtime* could report (a verb awaiting something that
//! never arrives while the driver is alive), which is a different failure from
//! the one the process watchdog catches.
#![cfg(all(feature = "rt", feature = "time", feature = "sync", feature = "macros"))]

use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use lgwks_bot::rt::sync::{Mutex, mpsc};
use lgwks_bot::rt::task::JoinSet;
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::spec::{
    AbandonReason, EffectEvidence, EffectKey, PendingWork, RetryPolicy, TransitionHold,
};
use lgwks_bot::{
    Auth, Bot, BotError, Builder, Cap, DispatchCertainty, EffectLifetime, Execute, GrantSet,
    Observe, block_on,
};

#[path = "support/effects.rs"]
mod effects;
#[path = "support/poll.rs"]
mod poll;
#[path = "support/yield_once.rs"]
mod yield_once;

use effects::memory_scope as test_effects;
use poll::admit_poll;
use yield_once::YieldOnce;

/// What a test reports when its precondition did not hold.
///
/// These tests cross three error domains (`BotError`, `std::io` and the
/// watchdog's own string reports), so they return `Box<dyn Error>` and
/// propagate each with `?`. A mismatch is then a named failure carrying the
/// reason, rather than an unwind that reports only that something unwound.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The environment variable that tells a re-executed child it is the guarded
/// run rather than the parent.
const GUARD_ENV: &str = "LGWKS_BOT_ECS_TICK_CHILD";

/// How many times the parent looks at the child before giving up on it.
///
/// With [`GUARD_POLL_INTERVAL`] this is a twelve-second deadline. The guarded
/// bodies finish in milliseconds; the margin is for a loaded machine, and every
/// second of it is spent only when something is already wrong.
const GUARD_POLLS: u32 = 600;

/// How long the parent waits between looks at the child.
const GUARD_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The budget a body's own [`timeout`] allows the tick it guards.
const TICK_BUDGET: Duration = Duration::from_secs(10);

/// The sibling tests' channel bound: one message, which is all they send.
const CHANNEL_CAPACITY: usize = 1;

/// How long a timer-backed source or action waits before resolving.
///
/// Non-zero on purpose: a timer that resolves without the reactor having to
/// turn would pass on a parked thread only by accident.
const TIMER_DELAY: Duration = Duration::from_millis(1);

/// The value the sibling-task test's channel carries.
const SENTINEL: u32 = 7;

/// How many times the sibling task beats before it sends.
const HEARTBEATS: usize = 3;

/// How long the sibling waits between heartbeats.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1);

/// How long the cancellation test lets a tick run before it is dropped.
///
/// Far shorter than the sibling's delay, so the drop happens while the tick is
/// parked on the channel. The tick cannot have completed at this point however
/// fast the machine is, because completion requires the sibling's send.
const CANCEL_AFTER: Duration = Duration::from_millis(1);

// ── The child-process watchdog ─────────────────────────────────────────────

/// Run `body` in a child process, and fail the parent if that child does not
/// finish.
///
/// The child is this same test binary, re-executed with [`GUARD_ENV`] set and
/// filtered to `test_name`, so it runs exactly one body and does not recurse:
/// the marker is what tells the child it is the child. A body that returns
/// `Ok` leaves a marker file behind, which the parent checks — a successful
/// exit with no marker means the filter matched nothing and the test never ran,
/// which would otherwise be indistinguishable from a pass.
///
/// The parent's deadline is enforced by killing the child, because a hung child
/// cannot report on itself: it is parked on the driver it was supposed to be
/// driving, so nothing inside it will ever run again.
fn guarded(test_name: &'static str, body: fn() -> TestResult) -> TestResult {
    if std::env::var_os(GUARD_ENV).is_some() {
        let outcome = body();
        if outcome.is_ok() {
            std::fs::write(marker_path(test_name), test_name)?;
        }
        return outcome;
    }

    let marker = marker_path(test_name);
    discard_marker(&marker)?;
    let executable = std::env::current_exe()?;
    let mut child = std::process::Command::new(executable)
        .args([test_name, "--exact", "--nocapture"])
        .env(GUARD_ENV, test_name)
        .spawn()?;

    for _ in 0..GUARD_POLLS {
        match child.try_wait()? {
            Some(status) => {
                if !status.success() {
                    let refusal = Err(format!(
                        "the guarded child for `{test_name}` failed on its own: {status}"
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "guarded: returning an error to the caller");
                    return refusal;
                }
                let recorded = match std::fs::read_to_string(&marker) {
                    Ok(recorded) => recorded,
                    Err(error) => {
                        let refusal = Err(format!(
                            "the guarded child for `{test_name}` exited successfully but never ran \
                         the body ({error}); the child's filter matched no test"
                        )
                        .into());
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "guarded: returning an error to the caller");
                        return refusal;
                    }
                };
                if recorded != test_name {
                    let refusal = Err(format!(
                        "the guarded child for `{test_name}` wrote a marker for `{recorded}`"
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "guarded: returning an error to the caller");
                    return refusal;
                }
                discard_marker(&marker)?;
                return Ok(());
            }
            None => pause_between_polls(),
        }
    }

    match child.kill().and_then(|()| child.wait()) {
        Ok(_killed) => Err(format!(
            "the guarded child for `{test_name}` did not finish within {} polls of \
             {GUARD_POLL_INTERVAL:?} and was killed. A tick that never returns is the defect \
             this bounds.",
            GUARD_POLLS
        )
        .into()),
        Err(error) => Err(format!(
            "the guarded child for `{test_name}` timed out and could not be killed: {error}"
        )
        .into()),
    }
}

/// Where the child records that it reached the end of its body.
fn marker_path(test_name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lgwks-bot-ecs-tick-{test_name}.marker"))
}

/// Remove a marker, tolerating its absence.
///
/// Absence is the expected state before a run, so a missing file is not an
/// error; anything else is, because a marker that cannot be cleared would let a
/// previous run's success stand in for this one's.
fn discard_marker(marker: &std::path::Path) -> TestResult {
    match std::fs::remove_file(marker) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Wait one poll interval between looks at the child.
///
/// `std::process::Child` has no timed wait, so the parent branch of [`guarded`]
/// polls `try_wait`, and the deadline is the poll count times this interval.
/// The parent is a plain process with no runtime, so nothing here can stall an
/// executor. The wait is a bounded receive on a one-slot channel whose sender
/// is held for the call and never sends: it can end only by timing out, so
/// every pause is exactly one interval, and a disconnect cannot cut it short
/// and turn the deadline into a busy loop.
fn pause_between_polls() {
    let (_held, wait) = std::sync::mpsc::sync_channel::<()>(1);
    let timed_out = matches!(
        wait.recv_timeout(GUARD_POLL_INTERVAL),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    );
    debug_assert!(timed_out, "nothing sends on the pause channel");
}

// ── Futures that need no reactor ───────────────────────────────────────────

// ── Sources ────────────────────────────────────────────────────────────────

/// A source that awaits a timer before yielding its value.
///
/// The timer is the subject: on a current-thread runtime it resolves only if
/// the thread polling this future returns to the driver, which is precisely
/// what a thread-parking poll prevents.
struct TimerSource {
    /// The value each poll yields.
    value: u32,
    /// How long the poll waits before yielding it.
    delay: Duration,
}

impl Observe for TimerSource {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        sleep(self.delay).await;
        Ok(self.value)
    }

    fn domain_id(&self) -> &str {
        "test::timer_source"
    }
}

/// A source that awaits a future needing no reactor.
struct YieldSource {
    /// The value each poll yields.
    value: u32,
}

impl Observe for YieldSource {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(YieldOnce::new(self.value).await)
    }

    fn domain_id(&self) -> &str {
        "test::yield_source"
    }
}

/// A source that resolves only when a sibling task sends on its channel.
///
/// The receiver lives in an async mutex rather than a `RefCell` for one reason:
/// `recv` needs `&mut self`, and the naive fix is to hold a synchronous borrow
/// guard across the await. That is the same defect in miniature — the guard
/// would be held for as long as the driver is blocked, and a second poll of the
/// same source would then fail rather than wait. The async mutex is made for
/// this and holds no thread.
struct ChannelSource {
    /// The receiving end, kept across polls so a cancelled poll loses nothing.
    receiver: Mutex<Option<mpsc::Receiver<u32>>>,
}

impl Observe for ChannelSource {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        let mut held = self.receiver.lock().await;
        let Some(receiver) = held.as_mut() else {
            let refusal = Err(undelivered(
                "test::channel_source",
                "this source was already drained by an earlier poll",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "poll: returning an error to the caller");
            return refusal;
        };
        match receiver.recv().await {
            Some(value) => Ok(value),
            None => Err(undelivered(
                "test::channel_source",
                "the sibling task dropped its sender before sending",
            )),
        }
    }

    fn domain_id(&self) -> &str {
        "test::channel_source"
    }
}

// ── Actions ────────────────────────────────────────────────────────────────

/// An action that awaits a timer before recording its label.
///
/// Like [`TimerSource`], but on the effect side: the timer here proves the
/// *act* phase is awaited on the caller's runtime too, not just the observe
/// phase. It is a separate phase in a separate part of the tick, so it needs
/// its own evidence.
struct TimerAction {
    /// The label appended when this action runs.
    label: &'static str,
    /// The effects recorded so far, in the order they ran.
    log: Rc<RefCell<Vec<&'static str>>>,
}

impl Execute for TimerAction {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        sleep(TIMER_DELAY).await;
        self.log.borrow_mut().push(self.label);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::timer_action"
    }
}

/// An action that awaits a reactor-free future before recording its label.
struct YieldAction {
    /// The label appended when this action runs.
    label: &'static str,
    /// The effects recorded so far, in the order they ran.
    log: Rc<RefCell<Vec<&'static str>>>,
}

impl Execute for YieldAction {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        // The value it resolves to is not this action's subject: the yield is
        // what makes the effect a two-poll future with no reactor behind it.
        YieldOnce::new(0).await;
        self.log.borrow_mut().push(self.label);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::yield_action"
    }
}

// ── Conditions ─────────────────────────────────────────────────────────────

/// `true` for the value one chain's source yields.
fn is_one(seen: &u32) -> bool {
    *seen == 1
}

/// `true` for the value the other chain's source yields.
fn is_two(seen: &u32) -> bool {
    *seen == 2
}

/// `true` for the value the sibling-task channel carries.
fn is_sentinel(seen: &u32) -> bool {
    *seen == SENTINEL
}

/// The log every action in this file appends its label to.
type Log = Rc<RefCell<Vec<&'static str>>>;

/// A one-chain bot over a timer-backed source that always selects its effect.
///
/// The fixture three rows share: the current-thread runtime and the one-worker
/// runtime both refuse a synchronous tick on this bot, and the awaited tick that
/// follows must run the one effect. Written once because a per-row copy is free
/// to drift on which source it drives, and a row that drove a different one would
/// be testing something else while claiming this.
fn timer_bot(name: &str, log: &Log) -> Result<Bot, Box<dyn std::error::Error>> {
    Ok(Bot::builder(name)
        .observe(TimerSource {
            value: 1,
            delay: TIMER_DELAY,
        })
        .on(
            is_one,
            TimerAction {
                label: "ticked",
                log: Rc::clone(log),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?)
}

/// A one-chain bot over a channel-backed source, receiving on `receiver`.
///
/// The fixture the sibling-completion and cancelled-tick rows share: both need a
/// receive that a tick can be cancelled *during*, which is the whole of what
/// distinguishes them.
fn channel_bot(
    name: &str,
    receiver: mpsc::Receiver<u32>,
    log: Log,
) -> Result<Bot, Box<dyn std::error::Error>> {
    Ok(Bot::builder(name)
        .observe(ChannelSource {
            receiver: Mutex::new(Some(receiver)),
        })
        .on(
            is_sentinel,
            YieldAction {
                label: "received",
                log,
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?)
}

/// A one-chain bot over a source that needs no reactor at all.
///
/// The fixture the runtime row and the thread-parking row share, so "the shipped
/// runtime runs it" and "the adapter runs it without a driver" are asserted about
/// the same bot.
fn immediate_bot(name: &str, log: &Log) -> Result<Bot, Box<dyn std::error::Error>> {
    Ok(Bot::builder(name)
        .observe(YieldSource { value: 1 })
        .on(
            is_one,
            YieldAction {
                label: "ran",
                log: Rc::clone(log),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?)
}

/// `true` for any value the socket source can yield.
#[cfg(all(feature = "io", feature = "net"))]
fn is_positive(seen: &u32) -> bool {
    *seen > 0
}

/// A domain failure that provably delivered nothing, so a chain may retry it.
///
/// `NotDelivered` and not `Refused`: these fixtures are *transient* refusals a
/// chain is expected to retry while it has budget, and `Refused` classifies as
/// `RetryClass::Never`, which abandons the entry on the first failure.
fn undelivered(domain: &str, cause: &str) -> BotError {
    BotError::DomainError {
        domain: domain.to_owned(),
        certainty: DispatchCertainty::NotDelivered,
        cause: cause.to_owned(),
    }
}

// ── The tests ──────────────────────────────────────────────────────────────

/// The test name, reused as the child's filter and as its completion marker.
const REFUSAL_CURRENT_THREAD: &str =
    "a_synchronous_tick_inside_the_shipped_current_thread_runtime_is_refused";

/// The test name, reused as the child's filter and as its completion marker.
const REFUSAL_ONE_WORKER: &str = "a_synchronous_tick_inside_a_one_worker_runtime_is_refused";

/// The test name, reused as the child's filter and as its completion marker.
const TIMER_ORDER: &str = "timer_backed_effects_run_in_order_on_the_shipped_current_thread_runtime";

/// The test name, reused as the child's filter and as its completion marker.
const SIBLING: &str = "a_tick_on_the_shipped_current_thread_runtime_completes_a_sibling_send";

/// The test name, reused as the child's filter and as its completion marker.
const CANCELLED: &str = "a_cancelled_tick_leaves_the_bot_usable_and_the_runtime_alive";

/// The test name, reused as the child's filter and as its completion marker.
const IMMEDIATE: &str = "immediate_verbs_run_on_the_shipped_current_thread_runtime";

/// The test name, reused as the child's filter and as its completion marker.
const SYNC_ADAPTER: &str = "runtime_independent_verbs_run_on_the_synchronous_adapter";

/// The test name, reused as the child's filter and as its completion marker.
#[cfg(all(feature = "io", feature = "net"))]
const SOCKET: &str = "a_loopback_socket_source_completes_on_the_shipped_current_thread_runtime";

// The wrappers stay at the crate root and the bodies live in `bodies`: the
// child is re-executed with `--exact <name>`, so a wrapper's path is its
// filter, and moving one into a module would make the child match nothing.

#[test]
fn a_synchronous_tick_inside_the_shipped_current_thread_runtime_is_refused() -> TestResult {
    guarded(REFUSAL_CURRENT_THREAD, bodies::refusal_current_thread)
}

#[test]
fn a_synchronous_tick_inside_a_one_worker_runtime_is_refused() -> TestResult {
    guarded(REFUSAL_ONE_WORKER, bodies::refusal_one_worker)
}

#[test]
fn timer_backed_effects_run_in_order_on_the_shipped_current_thread_runtime() -> TestResult {
    guarded(TIMER_ORDER, bodies::timer_effects_in_order)
}

#[test]
fn a_tick_on_the_shipped_current_thread_runtime_completes_a_sibling_send() -> TestResult {
    guarded(SIBLING, bodies::sibling_channel_completion)
}

#[test]
fn a_cancelled_tick_leaves_the_bot_usable_and_the_runtime_alive() -> TestResult {
    guarded(CANCELLED, bodies::a_cancelled_tick_leaves_the_bot_usable)
}

#[test]
fn immediate_verbs_run_on_the_shipped_current_thread_runtime() -> TestResult {
    guarded(IMMEDIATE, bodies::immediate_verbs_on_the_shipped_runtime)
}

#[test]
fn runtime_independent_verbs_run_on_the_synchronous_adapter() -> TestResult {
    guarded(
        SYNC_ADAPTER,
        bodies::runtime_independent_verbs_on_the_sync_adapter,
    )
}

#[test]
#[cfg(all(feature = "io", feature = "net"))]
fn a_loopback_socket_source_completes_on_the_shipped_current_thread_runtime() -> TestResult {
    guarded(SOCKET, bodies::socket_source_on_the_shipped_runtime)
}

#[path = "ecs_tick/bodies.rs"]
mod bodies;
#[path = "ecs_tick/ledger.rs"]
mod ledger;
