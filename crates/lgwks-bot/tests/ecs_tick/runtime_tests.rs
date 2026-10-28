//! The guarded bodies: each runs in the child process its `#[test]` spawns.

use super::*;

/// Run `future` on a current-thread runtime built for this call: the runtime
/// the defect deadlocked, where the calling thread is the driver.
///
/// The free `lgwks_bot::block_on` drives the multi-thread process driver (ADR
/// 0006 R2), where a parked caller leaves the workers running and the
/// deadlock cannot happen, so these bodies name the runtime they are about.
/// The crate's builder keeps current-thread crate-private (`script!` is the one
/// supported interface, #526), so this reaches the engine through the
/// `lgwks_deps` facade. Panics, as the free one does, if the OS refuses the
/// runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut builder = lgwks_deps::tokio::runtime::Builder::new_current_thread();
    match builder.enable_all().build() {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => std::panic::resume_unwind(Box::new(format!(
            "ecs_tick: the OS refused a current-thread runtime: {error}"
        ))),
    }
}

// ── The guarded bodies ─────────────────────────────────────────────────────

/// The synchronous adapter refuses inside the shipped current-thread runtime.
///
/// This is the refusal itself, and the bot is timer-backed so the same call had
/// something to deadlock on before the refusal existed. Two facts are asserted
/// together: the call is refused *and* the bot it was refused for is unharmed,
/// because a refusal that left the tick half-started would be a worse answer
/// than the error.
pub(super) fn refusal_current_thread() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = timer_bot("refusal-current-thread", &log)?;

    // A current-thread runtime, built per call by `block_on` above: on it the
    // calling thread is the driver.
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
pub(super) fn refusal_one_worker() -> TestResult {
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
pub(super) fn timer_effects_in_order() -> TestResult {
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
pub(super) fn sibling_channel_completion() -> TestResult {
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
pub(super) fn sibling_verdict<E: std::fmt::Display>(
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
pub(super) fn a_cancelled_tick_leaves_the_bot_usable() -> TestResult {
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
pub(super) fn immediate_verbs_on_the_shipped_runtime() -> TestResult {
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
pub(super) fn runtime_independent_verbs_on_the_sync_adapter() -> TestResult {
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
pub(super) fn socket_source_on_the_shipped_runtime() -> TestResult {
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
