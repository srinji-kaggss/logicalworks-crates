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

use std::cell::RefCell;
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
