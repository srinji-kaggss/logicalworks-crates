//! ECS bot unit tests, part 03: the retry backoff at the ledger (issue #401).

use super::*;

/// Drive `future` to its end on a no-op waker. Every journal here answers at
/// once, so a future still pending after one turn is a defect, not a wait.
fn settle<T>(future: impl Future<Output = T>) -> Result<T, Box<dyn std::error::Error>> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => Ok(value),
        Poll::Pending => Err("begin_attempt waited on a journal that answers at once".into()),
    }
}

/// A one-entry bot on `clock` whose action never delivers, counting its runs.
fn refusing_bot(
    policy: RetryPolicy,
    clock: &Clock,
    attempts: &Rc<Cell<usize>>,
) -> Result<EcsBot, Box<dyn std::error::Error>> {
    Ok(EcsBot::builder("backoff")
        .with_retry_policy(policy)
        .with_clock(clock.clone())
        .observe(Holds::new(200))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(attempts)),
        )
        .with_effects(test_effects()?)
        .build(&net_grants())?)
}

/// The tick's retry reading, as the driver would hand it to the ledger.
fn reading(bot: &mut EcsBot) -> RetryReading {
    let mut policy = bot.world.resource_mut::<Policy>();
    policy.begin_tick();
    policy.reading()
}

/// The ledger refuses to begin an entry inside its backoff, even for a caller
/// that reaches past the walk, and begins it at the instant the backoff ends.
///
/// The walk already stops at such an entry, so no tick reaches this guard; the
/// guard is what keeps that true for every other caller of the ledger, the same
/// reason the unsettled-effect refusal sits here rather than in the walk.
/// Source: gRPC "Connection Backoff Protocol": a retry never begins before its
/// backoff has elapsed.
#[test]
fn the_ledger_refuses_an_attempt_inside_its_backoff() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let clock = Clock::virtual_at(Duration::ZERO);
    let policy = RetryPolicy::new(4, Duration::from_secs(10), Duration::MAX);
    let mut bot = refusing_bot(policy, &clock, &attempts)?;
    refused(bot.tick(), |fired| format!("a refusing action fired {fired}"))?;
    let Some(TransitionHold::Failed { retry_at, .. }) = first_hold(&bot) else {
        return Err(format!("the refusal leaves the entry Failed: {:?}", first_hold(&bot)).into());
    };
    let work = WorkId { chain: 0, entry: 0 };
    if let Some(early) = retry_at.checked_sub(Duration::from_nanos(1)) {
        clock.advance(early)?;
        let retry = reading(&mut bot);
        let inside = settle(bot.world.non_send_mut::<Ledger>().begin_attempt(
            work,
            EffectLifetime::Local,
            &retry,
        ))?;
        assert!(
            matches!(inside, Err(BotError::NoSuchWork { .. })),
            "a caller one nanosecond early is refused: {inside:?}"
        );
        assert!(
            matches!(first_hold(&bot), Some(TransitionHold::Failed { .. })),
            "the refusal leaves the entry exactly where it was"
        );
    }
    clock.advance(retry_at.saturating_sub(clock.now()))?;
    let retry = reading(&mut bot);
    let at_end = settle(bot.world.non_send_mut::<Ledger>().begin_attempt(
        work,
        EffectLifetime::Local,
        &retry,
    ))?;
    assert!(at_end.is_ok(), "the backoff's own end admits the attempt: {at_end:?}");
    assert!(
        matches!(first_hold(&bot), Some(TransitionHold::Unrecorded)),
        "the begun attempt is held as in flight: {:?}",
        first_hold(&bot)
    );
    assert_eq!(attempts.get(), 1, "the ledger began the attempt; nothing ran it");
    Ok(())
}

/// The deadline runs from the first attempt and a retry never resets it: a
/// retry that comes due after the deadline is given up rather than begun, and
/// the reason names the deadline and the time that ran.
///
/// No backoff, so every retry is due at once and only the deadline can end the
/// sequence. The ticks are a second apart, so the fourth reaches its retry at
/// exactly the deadline, which the policy refuses (`elapsed >= deadline`).
#[test]
fn a_late_retry_past_the_deadline_is_given_up_not_begun() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let clock = Clock::virtual_at(Duration::from_secs(5));
    let policy = RetryPolicy::new(100, Duration::ZERO, Duration::from_secs(3));
    let mut bot = refusing_bot(policy, &clock, &attempts)?;
    for tick in 1..=4 {
        let error = refused(bot.tick(), |fired| format!("tick {tick} fired {fired}"))?;
        let expected = if tick < 4 {
            matches!(error, BotError::DomainError { .. })
        } else {
            matches!(error, BotError::PendingTransition { .. })
        };
        assert!(expected, "tick {tick} reported {error:?}");
        clock.advance(Duration::from_secs(1))?;
    }
    assert!(
        matches!(
            first_hold(&bot),
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::DeadlineExhausted {
                    attempts: 3,
                    elapsed,
                },
                ..
            }) if elapsed == Duration::from_secs(3)
        ),
        "the fourth tick finds the deadline spent and begins nothing: {:?}",
        first_hold(&bot)
    );
    assert_eq!(attempts.get(), 3, "three attempts ran, all inside the deadline");
    Ok(())
}
