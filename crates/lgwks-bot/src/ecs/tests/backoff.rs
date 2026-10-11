//! `ecs` tests: the retry policy's backoff and deadline, on the bot's clock
//! (issue #401).
//!
//! Source: AWS Architecture Blog, "Exponential Backoff And Jitter" (2015) —
//! a retry that is not spaced out turns one refusal into a synchronized storm.
//! Requirement: every field of `lgwks_std::retry::RetryPolicy` is honoured,
//! and each is measured on the clock the bot declares, never on a free
//! `Instant`. A virtual clock is what lets an hour of backoff run in no time.

use super::*;

/// A bot whose first entry always refuses without delivering and whose second
/// records what reached it, under `policy` on `clock`.
fn refusing_bot(
    policy: RetryPolicy,
    clock: &Clock,
    attempts: &Rc<Cell<usize>>,
    log: &Rc<RefCell<Vec<u16>>>,
) -> Result<EcsBot, Box<dyn std::error::Error>> {
    Ok(EcsBot::builder("backoff")
        .with_clock(clock.clone())
        .with_retry_policy(policy)
        .observe(Script::new(vec![200; 16]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(attempts)),
        )
        .on(|value: &u16| *value >= 200, Record(Rc::clone(log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?)
}

/// A tick that must refuse, returning the refusal.
fn refusal(bot: &mut EcsBot) -> Result<BotError, Box<dyn std::error::Error>> {
    refused(bot.tick(), |fired| {
        format!("expected a refusal, {fired} fired")
    })
}

#[test]
fn a_retry_waits_out_its_backoff_on_the_declared_clock() -> TestResult {
    const BASE: Duration = Duration::from_secs(10);
    let clock = Clock::virtual_at(Duration::ZERO);
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let policy = RetryPolicy::new(3, BASE, Duration::from_secs(3600));
    let mut bot = refusing_bot(policy, &clock, &attempts, &log)?;

    assert!(matches!(refusal(&mut bot)?, BotError::DomainError { .. }));
    assert_eq!(attempts.get(), 1, "the first attempt runs at once");

    // The clock has not moved, so the backoff has not elapsed: the entry is
    // held, not retried, and the tick says work is still owed.
    assert!(matches!(
        refusal(&mut bot)?,
        BotError::PendingTransition { .. }
    ));
    assert_eq!(attempts.get(), 1, "a retry before its backoff is not sent");
    assert!(matches!(
        first_hold(&bot),
        Some(TransitionHold::Failed {
            attempts: 1,
            budget: 3,
            ..
        })
    ));

    // The first backoff is at most `BASE`: jitter only ever shortens it.
    clock.advance(BASE)?;
    assert!(matches!(refusal(&mut bot)?, BotError::DomainError { .. }));
    assert_eq!(attempts.get(), 2, "the retry runs once the backoff elapsed");

    // The second is at most twice `BASE`; once it elapses the budget's last
    // attempt runs and spends it.
    clock.advance(BASE.saturating_mul(2))?;
    assert!(matches!(refusal(&mut bot)?, BotError::DomainError { .. }));
    assert_eq!(attempts.get(), 3);
    assert!(matches!(
        first_hold(&bot),
        Some(TransitionHold::Abandoned {
            reason: AbandonReason::AttemptsExhausted { attempts: 3 },
            ..
        })
    ));
    assert!(
        log.borrow().is_empty(),
        "nothing behind the refusing entry ran while it was held or abandoned"
    );
    Ok(())
}

#[test]
fn a_spent_deadline_abandons_instead_of_retrying() -> TestResult {
    // Five attempts are allowed, and a zero deadline is already spent at the
    // first failure, so the entry is given up on for the deadline and the
    // budget is not what ended it.
    let clock = Clock::virtual_at(Duration::ZERO);
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let policy = RetryPolicy::new(5, Duration::from_millis(1), Duration::ZERO);
    let mut bot = refusing_bot(policy, &clock, &attempts, &log)?;

    assert!(matches!(refusal(&mut bot)?, BotError::DomainError { .. }));
    let abandoned = first_hold(&bot);
    assert!(
        matches!(
            abandoned,
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::DeadlineExceeded { attempts: 1 },
                ..
            })
        ),
        "got {abandoned:?}"
    );
    let rendered = abandoned.map(|hold| hold.to_string());
    assert!(
        rendered
            .as_deref()
            .is_some_and(|text| text.contains("retry deadline spent")),
        "the report names the deadline: {rendered:?}"
    );

    // Time passing does not revive it: an abandonment waits for evidence.
    clock.advance(Duration::from_secs(60))?;
    assert!(matches!(
        refusal(&mut bot)?,
        BotError::PendingTransition { .. }
    ));
    assert_eq!(attempts.get(), 1, "an abandoned entry is never retried");
    Ok(())
}

#[test]
fn a_backoff_that_would_reach_the_deadline_abandons_at_once() -> TestResult {
    // The policy refuses an attempt at its deadline, so a backoff that lands
    // there is an attempt that would only be refused: the entry is abandoned
    // now rather than held for nothing. A one-hour floor on the backoff
    // against a one-second deadline is past it for any jitter but the one
    // that shortens it below a second, which the entropy of this action does
    // not draw.
    let clock = Clock::virtual_at(Duration::ZERO);
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let policy = RetryPolicy::new(5, Duration::from_secs(3600), Duration::from_secs(1))
        .with_max_delay(Duration::from_secs(3600));
    let mut bot = refusing_bot(policy, &clock, &attempts, &log)?;

    assert!(matches!(refusal(&mut bot)?, BotError::DomainError { .. }));
    assert!(matches!(
        first_hold(&bot),
        Some(TransitionHold::Abandoned {
            reason: AbandonReason::DeadlineExceeded { attempts: 1 },
            ..
        })
    ));
    Ok(())
}

#[test]
fn the_default_policy_retries_on_the_next_tick() -> TestResult {
    // No backoff by default: a tick is already caller-paced, so the declared
    // default retries on the very next tick even on a clock that never moves.
    let clock = Clock::virtual_at(Duration::ZERO);
    let attempts = Rc::new(Cell::new(0));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("default")
        .with_clock(clock)
        .observe(Script::new(vec![200; 16]))
        .on(
            |value: &u16| *value >= 200,
            Refuses::undelivered(Rc::clone(&attempts)),
        )
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;
    for expected in 1..=3 {
        assert!(matches!(refusal(&mut bot)?, BotError::DomainError { .. }));
        assert_eq!(attempts.get(), expected);
    }
    assert!(matches!(
        first_hold(&bot),
        Some(TransitionHold::Abandoned {
            reason: AbandonReason::AttemptsExhausted { attempts: 3 },
            ..
        })
    ));
    Ok(())
}

#[test]
fn jitter_entropy_is_a_function_of_the_action_and_the_attempt() {
    let one = derive_action_id("backoff", 0, 0, "test::refuses");
    let other = derive_action_id("backoff", 0, 1, "test::record");
    let draw = super::super::attempts::jitter_entropy;
    assert_eq!(
        draw(one, 1),
        draw(one, 1),
        "a replay backs off the same way"
    );
    assert_ne!(draw(one, 1), draw(one, 2), "each attempt draws afresh");
    assert_ne!(
        draw(one, 1),
        draw(other, 1),
        "two actions do not retry in step"
    );
}
