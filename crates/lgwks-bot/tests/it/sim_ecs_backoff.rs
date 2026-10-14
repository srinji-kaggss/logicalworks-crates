//! Seeded sweeps of the ECS retry schedule: a definite failure waits out its
//! backoff on the bot's declared clock, and the policy's deadline bounds the
//! attempts (issue #401).
//!
//! Sources:
//! - Marc Brooker, "Exponential Backoff And Jitter", AWS Architecture Blog
//!   (2015): the delay doubles per retry up to a cap, and full jitter only ever
//!   shortens it. The bound asserted on every `retry_at` is that formula.
//! - gRPC, "Connection Backoff Protocol" (doc/connection-backoff.md): a retry is
//!   not begun before its backoff has elapsed, and the overall deadline is
//!   checked before the next attempt rather than after it.
//! - `lgwks_std::retry::RetryPolicy` (INV-STD-RETRY): the policy is the pure
//!   arithmetic; the caller sleeps on its own clock and bounds the delay by the
//!   remaining deadline. These families are that caller, the shipped bot.
//!
//! What is real: the bot, its ledger, the `MemoryJournal`, and the policy. What
//! the seed decides is the policy and how many times the action refuses. The
//! clock is a virtual one the scenario advances, so a ten-second backoff costs
//! one `advance`, and the trace records instants on that clock, never wall
//! time, which is what makes the same seed hash to the same trace.

// The bot's clock is reached through `lgwks_bot::rt`, which exists only with
// the `rt` feature; `cargo clippy --no-default-features` refused it unguarded.
#![cfg(feature = "rt")]

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family::band_family;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;
use std::time::Duration;

use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::rt::clock::Clock;
use lgwks_bot::spec::{AbandonReason, Bot, RetryPolicy, TransitionHold};
use lgwks_bot::{Auth, BotError, Cap, DispatchCertainty, EffectLifetime, Execute, GrantSet};

use sim::Band;
use sim::rig::{FixedSource, identity, is_value, scope};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// An action that refuses before delivering anything for its first `refusals`
/// entries and lands after that, recording the bot-clock instant of each entry.
#[derive(Clone)]
struct Flaky {
    /// Entries that refuse before the first that lands. `usize::MAX` never lands.
    refusals: usize,
    /// The clock the bot runs on, read when the body is entered.
    clock: Clock,
    /// The instant of every entry, in order.
    entered: Rc<RefCell<Vec<Duration>>>,
}

impl Flaky {
    /// An action over `clock` that refuses `refusals` times, then lands.
    fn new(refusals: usize, clock: &Clock) -> Self {
        Self {
            refusals,
            clock: clock.clone(),
            entered: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// How many times the body ran.
    fn entries(&self) -> usize {
        self.entered.borrow().len()
    }
}

impl Execute for Flaky {
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
        let earlier = self.entries();
        self.entered.borrow_mut().push(self.clock.now());
        if earlier < self.refusals {
            // `NotDelivered`: the effect definitely did not happen, so the entry
            // is retried under the policy rather than held for evidence.
            Err(BotError::DomainError {
                domain: "test::flaky".to_owned(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "the upstream refused before anything was delivered".to_owned(),
            })
        } else {
            Ok(())
        }
    }

    fn domain_id(&self) -> &str {
        "test::flaky"
    }
}

/// A one-chain bot running `action` under `policy` on `clock`.
fn bot_over(policy: RetryPolicy, clock: &Clock, action: Flaky) -> Result<Bot, Box<dyn Error>> {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    Ok(Bot::builder("sim-ecs-backoff")
        .with_retry_policy(policy)
        .with_clock(clock.clone())
        .observe(FixedSource)
        .on(is_value, action)
        .with_effects(scope(identity()?, store)?)
        .build(&GrantSet::empty())?)
}

/// The hold the bot reports for its one entry, or `None` once it is resolved.
fn hold_of(bot: &Bot) -> Option<TransitionHold> {
    bot.pending().first().map(|work| work.hold().clone())
}

/// The backoff end and attempt count of the entry's `Failed` hold, or what the
/// entry is instead.
fn failed(bot: &Bot) -> Result<(Duration, u32), String> {
    match hold_of(bot) {
        Some(TransitionHold::Failed {
            retry_at, attempts, ..
        }) => Ok((retry_at, attempts)),
        other => Err(format!(
            "a refused attempt leaves the entry Failed, not {other:?}"
        )),
    }
}

/// The largest backoff Brooker's formula allows after the `attempts`-th
/// failure: `base * 2^(attempts - 1)`, capped at `max_delay`.
fn backoff_ceiling(base: Duration, max_delay: Duration, attempts: u32) -> Duration {
    let doublings = attempts.saturating_sub(1).min(63);
    match u32::try_from(1_u64 << doublings) {
        Ok(factor) => base.saturating_mul(factor).min(max_delay),
        Err(_beyond_u32) => max_delay,
    }
}

/// A definite failure is never retried before its backoff ends, is retried on
/// the first tick at or after it, and the backoff never exceeds the capped
/// exponential the policy declares.
fn a_retry_waits_out_its_backoff(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let base = Duration::from_millis(u64::from(sim.rng().between(1, 2_000)));
        let max_delay = base.saturating_mul(sim.rng().between(1, 8));
        let max_attempts = sim.rng().between(2, 6);
        let refusals = sim.rng().between(1, max_attempts.saturating_sub(1));
        let clock = Clock::virtual_at(Duration::ZERO);
        let action = Flaky::new(usize::try_from(refusals)?, &clock);
        let policy = RetryPolicy::new(max_attempts, base, Duration::MAX).with_max_delay(max_delay);
        let mut bot = bot_over(policy, &clock, action.clone())?;

        for failure in 1..=refusals {
            let failed_at = clock.now();
            let refused = bot.tick();
            assert!(
                matches!(refused, Err(BotError::DomainError { .. })),
                "attempt {failure} refuses: {refused:?}"
            );
            let (retry_at, attempts) = failed(&bot)?;
            assert_eq!(attempts, failure, "the hold counts the attempt just made");
            let backoff = retry_at.saturating_sub(failed_at);
            assert!(
                backoff <= backoff_ceiling(base, max_delay, failure),
                "attempt {failure}: a backoff of {backoff:?} exceeds the capped exponential"
            );
            if let Some(early) = retry_at.checked_sub(Duration::from_nanos(1))
                && early >= failed_at
            {
                clock.advance(early.saturating_sub(clock.now()))?;
                let held = bot.tick();
                assert!(
                    matches!(held, Err(BotError::PendingTransition { .. })),
                    "a tick one nanosecond before the backoff ends holds the entry: {held:?}"
                );
                assert_eq!(
                    action.entries(),
                    usize::try_from(failure)?,
                    "nothing is attempted inside the backoff"
                );
            }
            clock.advance(retry_at.saturating_sub(clock.now()))?;
            sim.trace
                .record_number("retry_at_ns", u64::try_from(retry_at.as_nanos())?);
        }
        assert_eq!(
            bot.tick()?,
            1,
            "the first tick at the backoff's end lands the effect"
        );
        assert!(hold_of(&bot).is_none(), "nothing is held once it lands");
        assert_eq!(
            action.entries(),
            usize::try_from(refusals.saturating_add(1))?,
            "each refusal was retried once and the last attempt landed"
        );
        for (instant, attempt) in action.entered.borrow().iter().zip(1_u64..) {
            sim.trace.record_number(
                "entered_ns",
                u64::try_from(instant.as_nanos())?.saturating_add(attempt),
            );
        }
        Ok(())
    })
}

/// No attempt ever begins at or past the policy's deadline, measured from the
/// first attempt, and the abandonment that ends the sequence says which bound
/// ended it and how long it ran.
fn no_attempt_begins_past_the_deadline(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let base = Duration::from_millis(u64::from(sim.rng().between(1, 1_000)));
        let deadline = Duration::from_millis(u64::from(sim.rng().between(1, 5_000)));
        let max_attempts = sim.rng().between(2, 40);
        let clock = Clock::virtual_at(Duration::ZERO);
        let action = Flaky::new(usize::MAX, &clock);
        let mut bot = bot_over(
            RetryPolicy::new(max_attempts, base, deadline),
            &clock,
            action.clone(),
        )?;

        let mut ended = None;
        for _ in 0..max_attempts {
            let refused = bot.tick();
            assert!(
                matches!(refused, Err(BotError::DomainError { .. })),
                "every attempt refuses: {refused:?}"
            );
            if let Ok((retry_at, _)) = failed(&bot) {
                clock.advance(retry_at.saturating_sub(clock.now()))?;
            } else {
                ended = hold_of(&bot);
                break;
            }
        }

        let entered = action.entered.borrow().clone();
        let first = *entered.first().ok_or("the first attempt ran")?;
        let last = *entered.last().ok_or("an attempt ran")?;
        assert!(
            entered
                .iter()
                .all(|instant| instant.saturating_sub(first) < deadline),
            "every attempt began inside the deadline: {entered:?} against {deadline:?}"
        );
        let ran = u32::try_from(entered.len())?;
        assert!(
            match ended {
                Some(TransitionHold::Abandoned {
                    reason: AbandonReason::DeadlineExhausted { attempts, elapsed },
                    ..
                }) => attempts == ran && elapsed == last.saturating_sub(first),
                Some(TransitionHold::Abandoned {
                    reason: AbandonReason::AttemptsExhausted { attempts },
                    ..
                }) => attempts == ran && attempts == max_attempts,
                _ => false,
            },
            "the sequence ends abandoned, naming its bound, its {ran} attempts and the time \
             from {first:?} to {last:?}: {ended:?}"
        );
        let after = bot.tick();
        assert!(
            matches!(after, Err(BotError::PendingTransition { .. })),
            "an abandoned entry is reported, never retried: {after:?}"
        );
        assert_eq!(
            action.entered.borrow().len(),
            entered.len(),
            "nothing runs after the end"
        );
        sim.trace
            .record_number("attempts", u64::try_from(entered.len())?);
        sim.trace
            .record_number("last_ns", u64::try_from(last.as_nanos())?);
        Ok(())
    })
}

/// A zero deadline admits the first attempt and no retry: the first definite
/// failure is the end, and it says the deadline ended it.
#[test]
fn a_zero_deadline_gives_up_after_the_first_failure() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let action = Flaky::new(usize::MAX, &clock);
    let policy = RetryPolicy::new(5, Duration::from_millis(10), Duration::ZERO);
    let mut bot = bot_over(policy, &clock, action.clone())?;
    assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    assert!(
        matches!(
            hold_of(&bot),
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::DeadlineExhausted {
                    attempts: 1,
                    elapsed: Duration::ZERO,
                },
                ..
            })
        ),
        "the first failure ends it: {:?}",
        hold_of(&bot)
    );
    clock.advance(Duration::from_secs(60))?;
    assert!(matches!(
        bot.tick(),
        Err(BotError::PendingTransition { .. })
    ));
    assert_eq!(action.entries(), 1, "no retry is ever begun");
    Ok(())
}

/// A bot that declares no policy retries as every bot did before the policy
/// carried time: three attempts on three ticks, with no wait between them.
#[test]
fn the_default_policy_retries_three_times_without_waiting() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let action = Flaky::new(usize::MAX, &clock);
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let mut bot = Bot::builder("sim-ecs-backoff-default")
        .with_clock(clock)
        .observe(FixedSource)
        .on(is_value, action.clone())
        .with_effects(scope(identity()?, store)?)
        .build(&GrantSet::empty())?;
    for _ in 0..3 {
        assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    }
    assert!(matches!(
        hold_of(&bot),
        Some(TransitionHold::Abandoned {
            reason: AbandonReason::AttemptsExhausted { attempts: 3 },
            ..
        })
    ));
    assert_eq!(
        action.entered.borrow().as_slice(),
        [Duration::ZERO; 3],
        "all three attempts ran at the same instant: the default waits for nothing"
    );
    Ok(())
}

/// Two entries that fail on the same tick do not retry in lockstep: full jitter
/// spreads their backoffs, so their `retry_at` differ and jitter shortened at
/// least one of them below the full exponential. Brooker's point about jitter
/// is exactly this: without it, clients that failed together retry together.
#[test]
fn entries_that_fail_together_retry_apart() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let backoff = Duration::from_secs(3_600);
    let policy = RetryPolicy::new(3, backoff, Duration::MAX).with_max_delay(backoff);
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    let mut bot = Bot::builder("sim-ecs-backoff-jitter")
        .with_retry_policy(policy)
        .with_clock(clock.clone())
        .observe(FixedSource)
        .on(is_value, Flaky::new(usize::MAX, &clock))
        .observe(FixedSource)
        .on(is_value, Flaky::new(usize::MAX, &clock))
        .with_effects(scope(identity()?, store)?)
        .build(&GrantSet::empty())?;
    assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    let retries: Vec<Duration> = bot
        .pending()
        .iter()
        .filter_map(|work| match *work.hold() {
            TransitionHold::Failed { retry_at, .. } => Some(retry_at),
            _ => None,
        })
        .collect();
    assert!(
        matches!(retries.as_slice(), [first, second] if first != second && first.min(second) < &backoff),
        "both entries back off, apart, and full jitter shortened at least one below \
         {backoff:?}: {retries:?}"
    );
    Ok(())
}

// ── The families, one declared test per band ───────────────────────────────

band_family! {
    a_retry_waits_out_its_backoff_401 => a_retry_waits_out_its_backoff, 43;
    no_attempt_begins_past_the_deadline_401 => no_attempt_begins_past_the_deadline, 44;
}
