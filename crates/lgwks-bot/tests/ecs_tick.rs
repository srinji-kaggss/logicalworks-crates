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
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::task::{Context, Poll};
use std::time::Duration;

use lgwks_bot::rt::sync::{Mutex, mpsc};
use lgwks_bot::rt::task::spawn;
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::spec::{AbandonReason, EffectEvidence, RetryPolicy, TransitionHold};
use lgwks_bot::{Auth, Bot, BotError, Builder, Cap, Execute, GrantSet, Observe, block_on};

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
                    return Err(format!(
                        "the guarded child for `{test_name}` failed on its own: {status}"
                    )
                    .into());
                }
                let recorded = match std::fs::read_to_string(&marker) {
                    Ok(recorded) => recorded,
                    Err(error) => {
                        return Err(format!(
                            "the guarded child for `{test_name}` exited successfully but never ran \
                             the body ({error}); the child's filter matched no test"
                        )
                        .into());
                    }
                };
                if recorded != test_name {
                    return Err(format!(
                        "the guarded child for `{test_name}` wrote a marker for `{recorded}`"
                    )
                    .into());
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

/// Wait one poll interval without blocking an executor.
///
/// The parent branch of [`guarded`] is a plain process with no runtime and no
/// reactor, so the workspace's ban on `std::thread::sleep` — which exists
/// because blocking an executor thread stalls every task on it — has nothing to
/// protect here: there is no executor, and the wait is this watchdog's subject
/// rather than its scaffolding.
#[expect(
    clippy::disallowed_methods,
    reason = "the watchdog's parent branch is a plain process with no async runtime and no \
              reactor to stall; a poll interval is the wait's subject, and `rt::time::sleep` \
              cannot be awaited here"
)]
fn pause_between_polls() {
    std::thread::sleep(GUARD_POLL_INTERVAL);
}

// ── Futures that need no reactor ───────────────────────────────────────────

/// A future that yields once and then resolves, without a reactor.
///
/// This is the shape a runtime-independent future has: it makes progress by
/// waking its own waker, so any executor that re-polls on a wake completes it —
/// including the thread-parking one behind the synchronous adapter. It needs no
/// timer, socket, or driver to do it, which is exactly the property that makes
/// it a fair subject for the adapter that has none.
struct YieldOnce {
    /// Whether this future has returned `Pending` once already.
    yielded: bool,
    /// The value it resolves to.
    value: u32,
}

impl YieldOnce {
    /// A future that yields once and then resolves to `value`.
    fn new(value: u32) -> Self {
        Self {
            yielded: false,
            value,
        }
    }
}

impl Future for YieldOnce {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u32> {
        let this = self.get_mut();
        if this.yielded {
            return Poll::Ready(this.value);
        }
        this.yielded = true;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

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
            return Err(BotError::DomainError {
                domain: "test::channel_source".to_owned(),
                cause: "this source was already drained by an earlier poll".to_owned(),
            });
        };
        match receiver.recv().await {
            Some(value) => Ok(value),
            None => Err(BotError::DomainError {
                domain: "test::channel_source".to_owned(),
                cause: "the sibling task dropped its sender before sending".to_owned(),
            }),
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

/// `true` for any value the socket source can yield.
#[cfg(all(feature = "io", feature = "net"))]
fn is_positive(seen: &u32) -> bool {
    *seen > 0
}

// ── The guarded bodies ─────────────────────────────────────────────────────

/// The synchronous adapter refuses inside the shipped current-thread runtime.
///
/// This is the refusal itself, and the bot is timer-backed so the same call had
/// something to deadlock on before the refusal existed. Two facts are asserted
/// together: the call is refused *and* the bot it was refused for is unharmed,
/// because a refusal that left the tick half-started would be a worse answer
/// than the error.
fn refusal_current_thread() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("refusal-current-thread")
        .observe(TimerSource {
            value: 1,
            delay: TIMER_DELAY,
        })
        .on(
            is_one,
            TimerAction {
                label: "ticked",
                log: Rc::clone(&log),
            },
        )
        .build(&GrantSet::empty())?;

    // The shipped current-thread runtime: `lgwks_bot::block_on` builds one per
    // call, and on this runtime the calling thread is the driver.
    let refused = block_on(async { bot.tick() });
    match refused {
        Err(BotError::TickInsideRuntime) => {}
        other => {
            return Err(format!(
                "the synchronous adapter must refuse on a thread the shipped current-thread \
                 runtime is driving; it returned {other:?} instead"
            )
            .into());
        }
    }

    let fired = block_on(async { bot.tick_async().await })?;
    assert_eq!(
        fired, 1,
        "the awaited tick must run the one effect the timer source selects"
    );
    let again = block_on(async { bot.tick_async().await })?;
    assert_eq!(
        again, 0,
        "the source's value did not move, so change detection must select nothing"
    );
    assert_eq!(
        log.borrow().as_slice(),
        ["ticked"],
        "the effect must have run exactly once, after the refusal"
    );
    Ok(())
}

/// The refusal is not conditional on the runtime being single-threaded.
///
/// A one-worker multi-thread runtime is the case where parking the calling
/// thread happens to leave the driver alive, so a defect that only *sometimes*
/// deadlocks would pass here and fail on the current-thread runtime. The
/// adapter refuses on both because which thread it was handed is not knowable
/// from it.
fn refusal_one_worker() -> TestResult {
    let Some(workers) = NonZeroUsize::new(1) else {
        return Err("one is non-zero".into());
    };
    let runtime = Builder::new().worker_threads(Some(workers)).build()?;

    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("refusal-one-worker")
        .observe(TimerSource {
            value: 1,
            delay: TIMER_DELAY,
        })
        .on(
            is_one,
            TimerAction {
                label: "ticked",
                log: Rc::clone(&log),
            },
        )
        .build(&GrantSet::empty())?;

    let refused = runtime.block_on(async { bot.tick() });
    match refused {
        Err(BotError::TickInsideRuntime) => {}
        other => {
            return Err(format!(
                "the synchronous adapter must refuse on a one-worker runtime too; it returned \
                 {other:?} instead"
            )
            .into());
        }
    }

    let ticked = runtime.block_on(async { timeout(TICK_BUDGET, bot.tick_async()).await });
    match ticked {
        Ok(Ok(fired)) => assert_eq!(
            fired, 1,
            "the awaited tick must run the one effect the timer source selects"
        ),
        Ok(Err(error)) => {
            return Err(format!("the awaited tick failed on a one-worker runtime: {error}").into());
        }
        Err(_elapsed) => {
            return Err(format!(
                "the awaited tick did not finish within {TICK_BUDGET:?} on a one-worker runtime"
            )
            .into());
        }
    }
    assert_eq!(
        log.borrow().as_slice(),
        ["ticked"],
        "the effect must have run exactly once, after the refusal"
    );
    Ok(())
}

/// Two timer-backed chains complete, in declaration order, on the shipped
/// current-thread runtime.
///
/// The order is the assertion that matters: the effects are awaited one at a
/// time, so a completion order that depends on which timer fired first would
/// show up here as a flipped log. Both chains are selected on the same tick, so
/// the log is the tick's whole effect program.
fn timer_effects_in_order() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("timer-order")
        .observe(TimerSource {
            value: 1,
            delay: TIMER_DELAY,
        })
        .on(
            is_one,
            TimerAction {
                label: "first",
                log: Rc::clone(&log),
            },
        )
        .observe(TimerSource {
            value: 2,
            delay: TIMER_DELAY,
        })
        .on(
            is_two,
            TimerAction {
                label: "second",
                log: Rc::clone(&log),
            },
        )
        .build(&GrantSet::empty())?;

    let ticked = block_on(async { timeout(TICK_BUDGET, bot.tick_async()).await });
    match ticked {
        Ok(Ok(fired)) => assert_eq!(fired, 2, "both chains' conditions hold on the first tick"),
        Ok(Err(error)) => {
            return Err(format!("the awaited tick failed: {error}").into());
        }
        Err(_elapsed) => {
            return Err(format!("the awaited tick did not finish within {TICK_BUDGET:?}").into());
        }
    }
    assert_eq!(
        log.borrow().as_slice(),
        ["first", "second"],
        "effects run in declaration order, not in the order their timers resolve"
    );
    Ok(())
}

/// A tick on the shipped current-thread runtime completes a sibling task's
/// channel send, and the sibling's heartbeats prove it was running all along.
///
/// The heartbeat counter is the part that distinguishes "the tick waited" from
/// "the tick happened to finish": the tick cannot return until the send
/// arrives, and the send cannot arrive until three timer intervals have
/// elapsed on the same thread the tick is awaiting on. A parked thread fails
/// both halves at once.
fn sibling_channel_completion() -> TestResult {
    let (sender, receiver) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    let beats = Arc::new(AtomicUsize::new(0));
    let sibling_beats = Arc::clone(&beats);
    let log = Rc::new(RefCell::new(Vec::new()));
    let action_log = Rc::clone(&log);

    let mut bot = Bot::builder("sibling")
        .observe(ChannelSource {
            receiver: Mutex::new(Some(receiver)),
        })
        .on(
            is_sentinel,
            YieldAction {
                label: "received",
                log: action_log,
            },
        )
        .build(&GrantSet::empty())?;

    let ticked: usize = block_on(async move {
        let sibling = spawn(async move {
            for _ in 0..HEARTBEATS {
                sleep(HEARTBEAT_INTERVAL).await;
                sibling_beats.fetch_add(1, SeqCst);
            }
            if let Err(error) = sender.send(SENTINEL).await {
                return Err(format!("the sibling's send failed: {error}"));
            }
            Ok::<(), String>(())
        });

        let ticked = timeout(TICK_BUDGET, bot.tick_async()).await;
        match sibling.await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => return Err(message),
            Err(error) => return Err(format!("the sibling task did not finish: {error}")),
        }
        match ticked {
            Ok(Ok(fired)) => Ok(fired),
            Ok(Err(error)) => Err(format!("the awaited tick failed: {error}")),
            Err(_elapsed) => Err(format!(
                "the awaited tick did not finish within {TICK_BUDGET:?}"
            )),
        }
    })?;

    assert_eq!(
        beats.load(SeqCst),
        HEARTBEATS,
        "the sibling must have run every heartbeat while the tick was awaiting it"
    );
    assert_eq!(
        ticked, 1,
        "the tick completes only because the sibling's send arrived"
    );
    assert_eq!(
        log.borrow().as_slice(),
        ["received"],
        "the effect must have run exactly once, from the value the sibling sent"
    );
    Ok(())
}

/// A tick cancelled mid-observe leaves the bot usable and the runtime alive.
///
/// Cancellation is the reason the staging resources are overwritten wholesale
/// rather than appended to: the dropped tick wrote nothing, so the next one
/// starts from the same state as the first. The channel is what makes this
/// observable — the message is still there to be received, because a dropped
/// `recv` does not consume it.
fn a_cancelled_tick_leaves_the_bot_usable() -> TestResult {
    let (sender, receiver) = mpsc::channel::<u32>(CHANNEL_CAPACITY);
    let log = Rc::new(RefCell::new(Vec::new()));
    let action_log = Rc::clone(&log);

    let mut bot = Bot::builder("cancelled")
        .observe(ChannelSource {
            receiver: Mutex::new(Some(receiver)),
        })
        .on(
            is_sentinel,
            YieldAction {
                label: "received",
                log: action_log,
            },
        )
        .build(&GrantSet::empty())?;

    let outcome: (bool, usize) = block_on(async move {
        let sibling = spawn(async move {
            sleep(HEARTBEAT_INTERVAL).await;
            if let Err(error) = sender.send(SENTINEL).await {
                return Err(format!("the sibling's send failed: {error}"));
            }
            Ok::<(), String>(())
        });

        let cancelled = timeout(CANCEL_AFTER, bot.tick_async()).await;
        let dropped = cancelled.is_err();
        let resumed = timeout(TICK_BUDGET, bot.tick_async()).await;
        match sibling.await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => return Err(message),
            Err(error) => return Err(format!("the sibling task did not finish: {error}")),
        }
        match resumed {
            Ok(Ok(fired)) => Ok((dropped, fired)),
            Ok(Err(error)) => Err(format!("the tick after the cancellation failed: {error}")),
            Err(_elapsed) => Err(format!(
                "the tick after the cancellation did not finish within {TICK_BUDGET:?}"
            )),
        }
    })?;

    let (dropped, resumed) = outcome;
    assert!(
        dropped,
        "the first tick must have been cancelled at {CANCEL_AFTER:?}, before the sibling's send"
    );
    assert_eq!(
        resumed, 1,
        "the tick after the cancellation must run the effect the message selects"
    );
    assert_eq!(
        log.borrow().as_slice(),
        ["received"],
        "the cancelled tick must not have run the effect, and the resumed tick must have run it once"
    );
    Ok(())
}

/// Immediately-ready verbs need no reactor, and still run on a runtime.
///
/// Scope coverage rather than a regression: this passes on the thread-parking
/// executor too, because nothing here awaits anything. It is here so a future
/// change that makes the *simple* case runtime-dependent fails loudly.
fn immediate_verbs_on_the_shipped_runtime() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("immediate")
        .observe(YieldSource { value: 1 })
        .on(
            is_one,
            YieldAction {
                label: "ran",
                log: Rc::clone(&log),
            },
        )
        .build(&GrantSet::empty())?;

    let fired = block_on(async { bot.tick_async().await })?;
    assert_eq!(
        fired, 1,
        "the only chain's condition holds on the first tick"
    );
    assert_eq!(
        log.borrow().as_slice(),
        ["ran"],
        "the effect must have run exactly once"
    );
    Ok(())
}

/// The synchronous adapter drives reactor-free verbs on a plain thread.
///
/// Scope coverage for the other half of the contract: the adapter is not
/// merely a refusal. This body runs on a libtest thread with no runtime in
/// sight, and every future it drives resolves by waking its own waker, so the
/// thread-parking executor completes them without a driver. `spawn_blocking`
/// is deliberately not used here even though it is the canonical blocking
/// adapter: it requires a runtime context, which is exactly what this path does
/// not have.
fn runtime_independent_verbs_on_the_sync_adapter() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("sync-adapter")
        .observe(YieldSource { value: 1 })
        .on(
            is_one,
            YieldAction {
                label: "ran",
                log: Rc::clone(&log),
            },
        )
        .build(&GrantSet::empty())?;

    let fired = bot.tick()?;
    assert_eq!(
        fired, 1,
        "the only chain's condition holds on the first tick"
    );
    assert_eq!(
        log.borrow().as_slice(),
        ["ran"],
        "the effect must have run exactly once"
    );
    let unchanged = bot.tick()?;
    assert_eq!(
        unchanged, 0,
        "the source's value did not move, so change detection must select nothing"
    );
    Ok(())
}

/// A loopback socket source completes on the shipped current-thread runtime.
///
/// Sockets are the other driver the defect parks: an accepted connection is
/// only reported by the reactor, so a source that connects, writes and accepts
/// is a reactor round-trip per step. The client end is this source itself, so
/// the test needs no sibling task to make the connection arrive.
#[cfg(all(feature = "io", feature = "net"))]
fn socket_source_on_the_shipped_runtime() -> TestResult {
    use lgwks_bot::rt::io::{AsyncReadExt, AsyncWriteExt};
    use lgwks_bot::rt::net::{TcpListener, TcpStream};

    /// A source that talks to itself over loopback.
    struct SocketSource {
        /// The listening end, bound before the tick starts.
        listener: TcpListener,
    }

    impl Observe for SocketSource {
        type Output = u32;

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(Observe::required_caps(self))?;
            let domain = "test::socket_source".to_owned();
            let peer = self
                .listener
                .local_addr()
                .map_err(|error| BotError::DomainError {
                    domain: domain.clone(),
                    cause: error.to_string(),
                })?;
            let mut client =
                TcpStream::connect(peer)
                    .await
                    .map_err(|error| BotError::DomainError {
                        domain: domain.clone(),
                        cause: error.to_string(),
                    })?;
            if let Err(error) = client.write_all(b"7").await {
                return Err(BotError::DomainError {
                    domain,
                    cause: error.to_string(),
                });
            }
            let (mut server, _origin) =
                self.listener
                    .accept()
                    .await
                    .map_err(|error| BotError::DomainError {
                        domain: "test::socket_source".to_owned(),
                        cause: error.to_string(),
                    })?;
            let mut byte = [0u8; 1];
            if let Err(error) = server.read_exact(&mut byte).await {
                return Err(BotError::DomainError {
                    domain: "test::socket_source".to_owned(),
                    cause: error.to_string(),
                });
            }
            Ok(match byte[0] {
                b'7' => 7,
                _ => 0,
            })
        }

        fn domain_id(&self) -> &str {
            "test::socket_source"
        }
    }

    let log = Rc::new(RefCell::new(Vec::new()));
    let action_log = Rc::clone(&log);
    let outcome: usize = block_on(async move {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| format!("binding loopback failed: {error}"))?;
        let mut bot = Bot::builder("socket")
            .observe(SocketSource { listener })
            .on(
                is_positive,
                YieldAction {
                    label: "connected",
                    log: action_log,
                },
            )
            .build(&GrantSet::empty())
            .map_err(|error| format!("building the socket bot failed: {error}"))?;
        match timeout(TICK_BUDGET, bot.tick_async()).await {
            Ok(Ok(fired)) => Ok(fired),
            Ok(Err(error)) => Err(format!("the awaited socket tick failed: {error}")),
            Err(_elapsed) => Err(format!(
                "the awaited socket tick did not finish within {TICK_BUDGET:?}"
            )),
        }
    })?;
    assert_eq!(
        outcome, 1,
        "the socket source's value arrived over loopback and selected the one effect"
    );
    assert_eq!(
        log.borrow().as_slice(),
        ["connected"],
        "the effect must have run exactly once"
    );
    Ok(())
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

#[test]
fn a_synchronous_tick_inside_the_shipped_current_thread_runtime_is_refused() -> TestResult {
    guarded(REFUSAL_CURRENT_THREAD, refusal_current_thread)
}

#[test]
fn a_synchronous_tick_inside_a_one_worker_runtime_is_refused() -> TestResult {
    guarded(REFUSAL_ONE_WORKER, refusal_one_worker)
}

#[test]
fn timer_backed_effects_run_in_order_on_the_shipped_current_thread_runtime() -> TestResult {
    guarded(TIMER_ORDER, timer_effects_in_order)
}

#[test]
fn a_tick_on_the_shipped_current_thread_runtime_completes_a_sibling_send() -> TestResult {
    guarded(SIBLING, sibling_channel_completion)
}

#[test]
fn a_cancelled_tick_leaves_the_bot_usable_and_the_runtime_alive() -> TestResult {
    guarded(CANCELLED, a_cancelled_tick_leaves_the_bot_usable)
}

#[test]
fn immediate_verbs_run_on_the_shipped_current_thread_runtime() -> TestResult {
    guarded(IMMEDIATE, immediate_verbs_on_the_shipped_runtime)
}

#[test]
fn runtime_independent_verbs_run_on_the_synchronous_adapter() -> TestResult {
    guarded(SYNC_ADAPTER, runtime_independent_verbs_on_the_sync_adapter)
}

#[test]
#[cfg(all(feature = "io", feature = "net"))]
fn a_loopback_socket_source_completes_on_the_shipped_current_thread_runtime() -> TestResult {
    guarded(SOCKET, socket_source_on_the_shipped_runtime)
}

// ── The ledger's repairs: generation, barrier, and binding ─────────────────
//
// The tests below are not `guarded`, and the reason is the watchdog's subject
// rather than a shortcut: `guarded` exists for a body that may *never return* —
// a tick parked on a driver its own poll is blocking — so it re-executes the
// binary as a child and reads a marker. Every body here drives immediate verbs
// on the synchronous adapter and cannot park, so a watchdog would add a process
// spawn and, worse, replace an assertion's own diff with "the guarded child
// failed on its own". The failure this file's tests are read for is the
// assertion, so the assertion is what has to survive.

/// A source whose value the test moves by hand.
///
/// The three repairs are about what the substrate does *between* polls — which
/// generation a settlement belongs to, whether an abandoned entry is a barrier,
/// and which value an open transition is bound to — so the source has to be the
/// one part with no timing in it. A value the test sets makes "the source moved"
/// a fact the test states rather than one it waits for, which is what lets each
/// counterexample be written down exactly as the review states it.
struct Dial {
    /// The value the next poll yields.
    value: Rc<Cell<u32>>,
}

impl Observe for Dial {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(self.value.get())
    }

    fn domain_id(&self) -> &str {
        "test::dial"
    }
}

/// An action whose outcome the test chooses, recording the value it ran with.
///
/// `uncertain` is the state the ledger exists for: an attempt that returns
/// [`BotError::EffectIndeterminate`] may already be live, so the entry is held
/// rather than retried. Flipping it back is how a test produces a *settled*
/// generation and then a fresh one at the same slot, which is the shape every
/// generation counterexample needs.
struct Doubtful {
    /// Whether the next attempt is indeterminate instead of succeeding.
    uncertain: Rc<Cell<bool>>,
    /// The value each attempt actually ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for Doubtful {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.seen.borrow_mut().push(*call.1);
        if self.uncertain.get() {
            return Err(BotError::EffectIndeterminate {
                domain: "test::doubtful".to_owned(),
                cause: "the acknowledgment never arrived".to_owned(),
            });
        }
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::doubtful"
    }
}

/// The revision and hold of the first entry the bot is still holding, as one
/// value, so an assertion can say *which generation* is held rather than only
/// that something is.
fn held(bot: &Bot) -> Option<(usize, usize, u64, TransitionHold)> {
    bot.pending().first().map(|work| {
        (
            work.id().chain(),
            work.id().entry(),
            work.revision(),
            work.hold().clone(),
        )
    })
}

/// A settlement is accepted only for the generation it names.
///
/// The counterexample this closes, stated as the review states it: the entry at
/// `(0, 0)` is indeterminate at revision 1, and the caller reads `pending()`.
/// Its report is delayed. The entry is settled, the transition resolves, the
/// source moves, and the same slot is held again — at revision 2. The delayed
/// report for revision 1 then arrives.
///
/// Before the repair it was accepted, because `resolve_effect` named only
/// `(chain, entry)` and the ledger only ever holds one transition per chain, so
/// a slot is reused by every generation over it and identity alone cannot tell
/// them apart. `Applied` acknowledged an effect at a generation nobody asked
/// about; `NotApplied`, worse, made an attempt the caller never saw eligible for
/// a retry. The revision is the part of the identity that survives the reuse,
/// and it is what this binds.
///
/// Three more things are asserted here, because they are the same invariant read
/// from the other side: a settlement repeated for its own generation is a no-op
/// that succeeds, a settlement *contradicting* its own generation's is refused,
/// and neither refusal is `NoSuchWork` — that variant has to keep meaning "there
/// is no held effect here", or a caller reading it for a stale report is being
/// told the wrong thing about work that is still held.
#[test]
fn a_settlement_names_the_generation_it_settles_and_no_other() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let uncertain = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("generations")
        .observe(Dial {
            value: Rc::clone(&value),
        })
        .on(
            |_observed: &u32| true,
            Doubtful {
                uncertain: Rc::clone(&uncertain),
                seen: Rc::clone(&seen),
            },
        )
        .build(&GrantSet::empty())?;

    // Revision 1, held: the attempt may be live, so nothing settles it on its
    // own and the tick says so rather than guessing.
    match bot.tick() {
        Ok(fired) => {
            return Err(format!("an indeterminate effect was reported as {fired} fired").into());
        }
        Err(error) => assert!(
            matches!(error, BotError::EffectIndeterminate { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    let first = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported")?;
    assert_eq!(first.revision(), 1, "the transition opened at revision 1");
    assert!(
        matches!(
            first.hold(),
            TransitionHold::OutcomeUnknown { attempts: 1, .. }
        ),
        "held as an unknown outcome, not as a decision: {:?}",
        first.hold()
    );

    // The caller's own settlement for revision 1, naming revision 1. Accepted,
    // and the generation it settles is now decided, so the transition has
    // nothing open and is dropped by the next tick.
    bot.resolve_effect(first.id(), first.revision(), EffectEvidence::Applied)?;
    assert_eq!(bot.tick()?, 0, "an acknowledged effect is not replayed");
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is resolved: {:?}",
        bot.pending()
    );

    // The source moves and the same slot is held again — at revision 2. The
    // `WorkId` is identical to the one above; only the revision differs, which
    // is the whole point.
    uncertain.set(true);
    value.set(2);
    match bot.tick() {
        Ok(fired) => {
            return Err(format!("an indeterminate effect was reported as {fired} fired").into());
        }
        Err(error) => assert!(
            matches!(error, BotError::EffectIndeterminate { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    let second = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported again")?;
    assert_eq!(second.id(), first.id(), "the slot is the same one");
    assert_eq!(
        second.revision(),
        2,
        "the generation it belongs to is not, which is the fact identity loses"
    );
    assert_eq!(
        *seen.borrow(),
        vec![1, 2],
        "each attempt ran against the value its own transition was opened under"
    );

    // The delayed report for revision 1 arrives now. It is the same report the
    // caller read above, held across a tick and a movement — a move rather than
    // a clone, because that is what the caller has: the one report it was given.
    //
    // The refusal is asserted as the exact variant and not merely as "not
    // `NoSuchWork`". `EvidenceSuperseded` naming both generations is the
    // invariant: the caller has to be able to tell that its report was about a
    // generation that is gone, which generation it named, and which one stands
    // there now — a generic refusal would leave it guessing whether to retry the
    // report or re-read the work.
    let delayed = first;
    match bot.resolve_effect(delayed.id(), delayed.revision(), EffectEvidence::Applied) {
        Ok(()) => {
            return Err(
                "a settlement computed against revision 1 was accepted against revision 2".into(),
            );
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::EvidenceSuperseded { work, named: 1, current: 2 }
                    if work == delayed.id()
            ),
            "expected the named generation and the live one to be reported, got {error:?}"
        ),
    }
    assert_eq!(
        held(&bot).map(|(chain, entry, revision, _)| (chain, entry, revision)),
        Some((0, 0, 2)),
        "the refusal left the held generation exactly as it was: {:?}",
        bot.pending()
    );

    // `NotApplied` is the sharper half: accepting it would make an attempt the
    // caller was never asked about eligible to run again. It is refused by the
    // same variant — the reason is the generation, not the evidence.
    match bot.resolve_effect(delayed.id(), delayed.revision(), EffectEvidence::NotApplied) {
        Ok(()) => {
            return Err(
                "NotApplied for revision 1 was accepted against revision 2, authorising a \
                 retry of an attempt the caller was never shown"
                    .into(),
            );
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::EvidenceSuperseded { work, named: 1, current: 2 }
                    if work == delayed.id()
            ),
            "a stale generation is a generation complaint, not a contradiction and not \
             'no such work': {error:?}"
        ),
    }
    assert_eq!(
        held(&bot).map(|(chain, entry, revision, _)| (chain, entry, revision)),
        Some((0, 0, 2)),
        "the entry is still held and still refused, not restarted: {:?}",
        bot.pending()
    );

    // The current generation, settled with the evidence that is actually about
    // it: accepted.
    bot.resolve_effect(second.id(), second.revision(), EffectEvidence::Applied)?;

    // The same identity and the same evidence again is a duplicate report, not
    // a contradiction and not an error: a caller whose first delivery was
    // ambiguous has to be able to repeat it.
    bot.resolve_effect(second.id(), second.revision(), EffectEvidence::Applied)?;

    // Evidence contradicting what this generation was settled with is refused,
    // and the settlement it contradicts stands. The refusal names both pieces of
    // evidence, because "your report was refused" without saying which way round
    // leaves the caller unable to tell whether it misread its own observation.
    match bot.resolve_effect(second.id(), second.revision(), EffectEvidence::NotApplied) {
        Ok(()) => {
            return Err(
                "evidence contradicting the generation's own settlement was accepted".into(),
            );
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::EvidenceContradicted {
                    settled: EffectEvidence::Applied,
                    submitted: EffectEvidence::NotApplied,
                    ..
                }
            ),
            "expected the recorded and the refused evidence to be reported, got {error:?}"
        ),
    }
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is still acknowledged: {:?}",
        bot.pending()
    );
    assert_eq!(
        bot.tick()?,
        0,
        "an acknowledged effect is never replayed, whatever was said about it afterwards"
    );
    assert_eq!(*seen.borrow(), vec![1, 2], "and no attempt was run for it");
    Ok(())
}

/// An action that definitely did not happen, counting its attempts.
///
/// "Definitely" is the whole distinction: [`BotError::DomainError`] is the one
/// failure the substrate reads as a fact about the effect rather than a doubt
/// about it, so it is what spends an attempt budget and what eventually makes an
/// entry abandoned.
struct Refuses {
    /// Whether the next attempt refuses. The test flips this to let the entry
    /// through once it has been revived, which is how the barrier is shown to
    /// lift rather than merely to hold.
    refusing: Rc<Cell<bool>>,
    /// How many times the action was called.
    attempts: Rc<Cell<usize>>,
}

impl Execute for Refuses {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.attempts.set(self.attempts.get().saturating_add(1));
        if self.refusing.get() {
            return Err(BotError::DomainError {
                domain: "test::refuses".to_owned(),
                cause: "refused".to_owned(),
            });
        }
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::refuses"
    }
}

/// An action that succeeds and records the value it ran against.
struct Noted(Rc<RefCell<Vec<u32>>>);

impl Execute for Noted {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.borrow_mut().push(*call.1);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::noted"
    }
}

/// An abandoned entry is never a quiet tick.
///
/// The short half of the counterexample, stated as the review states it: with
/// only the failed entry and nothing behind it, the second tick returned `Ok(0)`
/// while `pending()` still named the abandonment. A caller reading a clean `Ok`
/// as "this chain is handled" read something that was not true — the work had
/// not been handled, it had been given up on, and those call for different
/// responses from whoever owns the run.
///
/// Under [`RetryPolicy::ONE_ATTEMPT`] the first definite failure spends the
/// budget, so the entry is abandoned and the first tick reports the action's own
/// typed error rather than a summary of it. The source then holds still. What
/// the second tick owes the caller is the abandonment, named.
#[test]
fn an_abandoned_entry_is_never_a_quiet_tick() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let mut lonely = Bot::builder("abandoned-alone")
        .with_retry_policy(RetryPolicy::ONE_ATTEMPT)
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| true,
            Refuses {
                refusing: Rc::new(Cell::new(true)),
                attempts: Rc::clone(&attempts),
            },
        )
        .build(&GrantSet::empty())?;

    match lonely.tick() {
        Ok(fired) => return Err(format!("a definite failure was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert_eq!(attempts.get(), 1, "the declared budget was one attempt");
    assert_eq!(
        held(&lonely).map(|(chain, entry, _, hold)| (chain, entry, hold)),
        Some((
            0,
            0,
            TransitionHold::Abandoned {
                reason: AbandonReason::AttemptsExhausted { attempts: 1 },
                cause: "test::refuses: refused".to_owned(),
            }
        )),
        "the abandonment is named after the tick that caused it"
    );

    // The source holds still, so there is nothing to attempt and — before the
    // repair — nothing to report either. The tick must not say the chain is
    // handled while `pending()` says otherwise.
    match lonely.tick() {
        Ok(fired) => {
            return Err(format!(
                "an abandoned entry was reported as {fired} fired while pending() still names \
                 it: {:?}",
                lonely.pending()
            )
            .into());
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::PendingTransition { ref work, outstanding: 1 }
                    if (work.id().chain(), work.id().entry()) == (0, 0)
            ),
            "expected the abandoned entry to be the reported work, got {error:?}"
        ),
    }
    assert_eq!(attempts.get(), 1, "an abandoned entry is not retried");
    assert!(
        !lonely.pending().is_empty(),
        "and it stays reported rather than being dropped"
    );
    Ok(())
}

/// An abandoned entry is a barrier to its successors.
///
/// The long half of the counterexample, stated as the review states it: one
/// chain — reserve a draft, then send it — under [`RetryPolicy::ONE_ATTEMPT`].
/// The first action definitely fails, so the entry is abandoned and the first
/// tick reports the action's own typed error. The source then holds still.
///
/// Before the repair, the second tick walked past the abandoned entry as though
/// it had succeeded and ran the send: the chain executed a command whose
/// prerequisite had been given up on — the draft that was never reserved, then
/// the send of it. Declaration order is not success dependency, and treating an
/// abandonment as "behind us" is what confused the two.
///
/// The invariant is that an abandonment is a *barrier*, not a decision that the
/// work is behind us. Successors under the default sequential disposition are
/// not attempted while it stands, and they stay reported. Abandonment is not a
/// way to finish a chain, and not a way to finish it quietly: the caller either
/// supplies evidence that the effect did not happen — which revives the entry
/// and its successors with it — or the chain stays reported as unresolved.
#[test]
fn an_abandoned_entry_blocks_its_successors() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let refusing = Rc::new(Cell::new(true));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut chained = Bot::builder("abandoned-then-blocked")
        .with_retry_policy(RetryPolicy::ONE_ATTEMPT)
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| true,
            Refuses {
                refusing: Rc::clone(&refusing),
                attempts: Rc::clone(&attempts),
            },
        )
        .on(|_observed: &u32| true, Noted(Rc::clone(&log)))
        .build(&GrantSet::empty())?;

    match chained.tick() {
        Ok(fired) => return Err(format!("a definite failure was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert!(
        log.borrow().is_empty(),
        "the successor is not run in the tick that abandoned its prerequisite"
    );

    // The tick that used to run the successor. It must not: the send would be
    // for a draft that was never reserved.
    match chained.tick() {
        Ok(fired) => {
            return Err(format!(
                "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                log.borrow()
            )
            .into());
        }
        Err(error) => assert!(
            matches!(error, BotError::PendingTransition { .. }),
            "expected the unresolved chain to be reported, got {error:?}"
        ),
    }
    assert!(
        log.borrow().is_empty(),
        "the successor of an abandoned entry is not attempted while it stands abandoned: {:?}",
        log.borrow()
    );

    // Both are still reported — the abandonment and the entry it blocks — so
    // the caller can see which entry is the reason and which is waiting on it.
    let reported: Vec<(usize, usize)> = chained
        .pending()
        .iter()
        .map(|work| (work.id().chain(), work.id().entry()))
        .collect();
    assert_eq!(
        reported,
        vec![(0, 0), (0, 1)],
        "the abandonment and the entry it blocks are both listed: {:?}",
        chained.pending()
    );
    assert!(
        matches!(
            held(&chained).map(|(_, _, _, hold)| hold),
            Some(TransitionHold::Abandoned { .. })
        ),
        "and the first of them is the abandonment, which is the fact that explains the other"
    );

    // Evidence that the effect did not happen is what reopens the chain: the
    // entry is eligible again and its successor with it. This is the only way
    // past a barrier, which is what makes the barrier a decision the caller
    // makes rather than one the substrate makes for them. The action is allowed
    // through this time, so the chain is shown to complete rather than to stay
    // wedged.
    refusing.set(false);
    let blocked = chained
        .pending()
        .into_iter()
        .next()
        .ok_or("the abandoned entry is reported")?;
    chained.resolve_effect(blocked.id(), blocked.revision(), EffectEvidence::NotApplied)?;
    assert_eq!(
        chained.tick()?,
        2,
        "the revived entry and the successor it was blocking both run"
    );
    assert_eq!(
        attempts.get(),
        2,
        "the revived entry attempts again, because evidence is a new fact"
    );
    assert_eq!(
        *log.borrow(),
        vec![1],
        "and the successor runs, because its prerequisite is no longer abandoned"
    );
    assert!(
        chained.pending().is_empty(),
        "the chain is finished: {:?}",
        chained.pending()
    );
    Ok(())
}
