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
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

#[cfg(all(feature = "io", feature = "net"))]
use lgwks_bot::DispatchCertainty;
use lgwks_bot::rt::sync::{Mutex, mpsc};
use lgwks_bot::rt::task::JoinSet;
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::{
    Auth, Bot, BotError, Builder, Cap, EffectLifetime, Execute, GrantSet, Observe, block_on,
};

#[path = "support/effects.rs"]
mod effects;
#[path = "support/guard.rs"]
mod guard;
#[path = "support/undelivered.rs"]
mod undelivered;
#[path = "support/yield_once.rs"]
mod yield_once;

use effects::memory_scope as test_effects;
use guard::guarded;
use undelivered::undelivered;
use yield_once::YieldOnce;

/// What a test reports when its precondition did not hold.
///
/// These tests cross three error domains (`BotError`, `std::io` and the
/// watchdog's own string reports), so they return `Box<dyn Error>` and
/// propagate each with `?`. A mismatch is then a named failure carrying the
/// reason, rather than an unwind that reports only that something unwound.
type TestResult = Result<(), Box<dyn std::error::Error>>;

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
    let mut bot = timer_bot("refusal-current-thread", &log)?;

    // The shipped current-thread runtime: `lgwks_bot::block_on` builds one per
    // call, and on this runtime the calling thread is the driver.
    let refused = block_on(async { bot.tick() });
    match refused {
        Err(BotError::TickInsideRuntime) => {}
        other => {
            let refusal = Err(format!(
                "the synchronous adapter must refuse on a thread the shipped current-thread \
             runtime is driving; it returned {other:?} instead"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refusal_current_thread: returning an error to the caller");
            return refusal;
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
        let refusal = Err("one is non-zero".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refusal_one_worker: returning an error to the caller");
        return refusal;
    };
    let runtime = Builder::new().worker_threads(Some(workers)).build()?;

    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = timer_bot("refusal-one-worker", &log)?;

    let refused = runtime.block_on(async { bot.tick() });
    match refused {
        Err(BotError::TickInsideRuntime) => {}
        other => {
            let refusal = Err(format!(
                "the synchronous adapter must refuse on a one-worker runtime too; it returned \
             {other:?} instead"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refusal_one_worker: returning an error to the caller");
            return refusal;
        }
    }

    let ticked = runtime.block_on(async { timeout(TICK_BUDGET, bot.tick_async()).await });
    match ticked {
        Ok(Ok(fired)) => assert_eq!(
            fired, 1,
            "the awaited tick must run the one effect the timer source selects"
        ),
        Ok(Err(error)) => {
            let refusal =
                Err(format!("the awaited tick failed on a one-worker runtime: {error}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refusal_one_worker: returning an error to the caller");
            return refusal;
        }
        Err(_elapsed) => {
            let refusal = Err(format!(
                "the awaited tick did not finish within {TICK_BUDGET:?} on a one-worker runtime"
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refusal_one_worker: returning an error to the caller");
            return refusal;
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
    // The two chains named rather than numbered inline, because the order claim
    // is about *these two* effects: a name says which one a log line is, and a
    // second copy of the builder differing only in a literal is a row that can
    // drift without anything noticing.
    const FIRST: u32 = 1;
    const SECOND: u32 = 2;

    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("timer-order")
        .observe(TimerSource {
            value: FIRST,
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
            value: SECOND,
            delay: TIMER_DELAY,
        })
        .on(
            is_two,
            TimerAction {
                label: "second",
                log: Rc::clone(&log),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    let ticked = block_on(async { timeout(TICK_BUDGET, bot.tick_async()).await });
    match ticked {
        Ok(Ok(fired)) => assert_eq!(fired, 2, "both chains' conditions hold on the first tick"),
        Ok(Err(error)) => {
            let refusal = Err(format!("the awaited tick failed: {error}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "timer_effects_in_order: returning an error to the caller");
            return refusal;
        }
        Err(_elapsed) => {
            let refusal =
                Err(format!("the awaited tick did not finish within {TICK_BUDGET:?}").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "timer_effects_in_order: returning an error to the caller");
            return refusal;
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

    let mut bot = channel_bot("sibling", receiver, action_log)?;

    let ticked: usize = block_on(async move {
        // A tracked set, not a handle this scope could drop: the sibling is
        // joined below, and the set aborts it if this scope unwinds first.
        let mut siblings = JoinSet::new();
        siblings.spawn(async move {
            for _ in 0..HEARTBEATS {
                sleep(HEARTBEAT_INTERVAL).await;
                sibling_beats.fetch_add(1, SeqCst);
            }
            sender
                .send(SENTINEL)
                .await
                .map_err(|error| format!("the sibling's send failed: {error}"))
        });

        let ticked = timeout(TICK_BUDGET, bot.tick_async()).await;
        sibling_verdict(siblings.join_next().await)?;
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

/// What joining the sibling task proves: its own verdict, or why it has none.
///
/// One reading of the join for both sibling tests, so a sibling that failed,
/// panicked or was never joined reads the same in each.
fn sibling_verdict<E: std::fmt::Display>(
    joined: Option<Result<Result<(), String>, E>>,
) -> Result<(), String> {
    match joined {
        Some(Ok(verdict)) => verdict,
        Some(Err(error)) => Err(format!("the sibling task did not finish: {error}")),
        None => Err(String::from("the sibling task was never joined")),
    }
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

    let mut bot = channel_bot("cancelled", receiver, action_log)?;

    let outcome: (bool, usize) = block_on(async move {
        let mut siblings = JoinSet::new();
        siblings.spawn(async move {
            sleep(HEARTBEAT_INTERVAL).await;
            sender
                .send(SENTINEL)
                .await
                .map_err(|error| format!("the sibling's send failed: {error}"))
        });

        let cancelled = timeout(CANCEL_AFTER, bot.tick_async()).await;
        let dropped = cancelled.is_err();
        let resumed = timeout(TICK_BUDGET, bot.tick_async()).await;
        sibling_verdict(siblings.join_next().await)?;
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
    let mut bot = immediate_bot("immediate", &log)?;

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
    let mut bot = immediate_bot("sync-adapter", &log)?;

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

    /// The domain name this source's refusals carry.
    const DOMAIN: &str = "test::socket_source";

    /// A failure on a socket operation that certainly delivered nothing.
    ///
    /// One mapping rather than three copies: `poll` reads the listener's address,
    /// connects a client and accepts a connection, and each of those refused the
    /// same way -- `NotDelivered`, because no byte had gone out when any of them
    /// failed. Naming it is what lets the one arm that is genuinely different --
    /// the write, which fails `Unsettled` because the connection existed -- read
    /// as the exception it is.
    fn not_delivered(error: std::io::Error) -> BotError {
        BotError::DomainError {
            domain: DOMAIN.to_owned(),
            certainty: DispatchCertainty::NotDelivered,
            cause: error.to_string(),
        }
    }

    /// Connects to `listener`, writes `7`, and reads the byte back.
    ///
    /// The whole round trip is one helper because the certainty of a failure is
    /// a property of the round trip, not of one call. Three of the four steps
    /// fail `NotDelivered` -- nothing had gone out -- and the write fails
    /// `Unsettled`, because by then the connection existed and the byte may
    /// already have reached the peer. Inline, that distinction sat in the middle
    /// of the chain where it was easiest to read as one more of the same.
    async fn round_trip(listener: &TcpListener) -> Result<u8, BotError> {
        let peer = listener.local_addr().map_err(not_delivered)?;
        let mut client = TcpStream::connect(peer).await.map_err(not_delivered)?;
        if let Err(error) = client.write_all(b"7").await {
            let refusal = Err(BotError::DomainError {
                domain: DOMAIN.to_owned(),
                certainty: DispatchCertainty::Unsettled,
                cause: error.to_string(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "round_trip: returning an error to the caller");
            return refusal;
        }
        let (mut server, _origin) = listener.accept().await.map_err(not_delivered)?;
        let mut byte = [0u8; 1];
        if let Err(error) = server.read_exact(&mut byte).await {
            let refusal = Err(not_delivered(error));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "round_trip: returning an error to the caller");
            return refusal;
        }
        Ok(byte[0])
    }

    /// A source that talks to itself over loopback.
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
            let byte = round_trip(&self.listener).await?;
            Ok(match byte {
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
        // Formatted rather than propagated, because this closure reports
        // `String` and the scope's error is boxed: the two do not convert into
        // one another, and flattening them would name the wrong failure.
        let effects =
            test_effects().map_err(|error| format!("building the effect scope failed: {error}"))?;
        let mut bot = Bot::builder("socket")
            .observe(SocketSource { listener })
            .on(
                is_positive,
                YieldAction {
                    label: "connected",
                    log: action_log,
                },
            )
            .with_effects(effects)
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
