//! What a failed tick means, as a program rather than as prose.
//!
//! This is the example `docs/guides/lgwks-bot/failures.md` shows, kept where the
//! workspace gate compiles and runs it: the guide's claims about a partial run,
//! about an unchanged source, and about what is still owed are assertions here,
//! so a change in `tick`'s behaviour breaks the build instead of silently
//! invalidating the page an operator reads at three in the morning.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::spec::{AbandonReason, TransitionHold};
use lgwks_bot::{Auth, Bot, BotError, Cap, Execute, GrantSet, Observe};

/// A source whose value the test drives by hand.
struct Reading(Arc<AtomicU32>);

impl Observe for Reading {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(self.0.load(Ordering::SeqCst))
    }

    fn domain_id(&self) -> &str {
        "test::reading"
    }
}

/// Counts its own successful runs.
struct Count(Arc<AtomicU32>);

impl Execute for Count {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::count"
    }
}

/// Fails with `DomainError`: the effect did not happen, so a retry is a retry.
struct Fail;

impl Execute for Fail {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: "test::fail".into(),
            cause: "the second action refused".into(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::fail"
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let value = Arc::new(AtomicU32::new(0));
    let counted = Arc::new(AtomicU32::new(0));
    let mut bot = Bot::builder("two-actions")
        .observe(Reading(Arc::clone(&value)))
        .on(|_: &u32| true, Count(Arc::clone(&counted)))
        .on(|_: &u32| true, Fail)
        .build(&GrantSet::empty())?;

    // Deliberately not `expect_err`: the workspace forbids `expect` on a
    // recoverable path, and this is one — a tick that returns `Ok` here is the
    // assertion failing, which is what the arm reports.
    let error = match bot.tick() {
        Ok(fired) => return Err(format!("the second action was reported as {fired} fired").into()),
        Err(error) => error,
    };
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "got {error:?}"
    );
    assert_eq!(
        counted.load(Ordering::SeqCst),
        1,
        "the action before the failure already ran"
    );

    // The source holds still, and the refused entry is attempted again anyway:
    // the work is recorded, not re-derived from the change filter. The action
    // ahead of it has already succeeded and is not replayed to get there.
    assert!(
        matches!(bot.tick(), Err(BotError::DomainError { .. })),
        "a refused action is retried while it has budget"
    );
    assert_eq!(
        counted.load(Ordering::SeqCst),
        1,
        "and the acknowledged effect is not replayed"
    );

    // Third attempt: the budget is spent, and the tick reports the action's own
    // error while `pending` names the entry it gave up on.
    assert!(
        matches!(bot.tick(), Err(BotError::DomainError { .. })),
        "the last attempt of the budget still reports the refusal"
    );
    let pending = bot.pending();
    assert_eq!(
        pending.len(),
        1,
        "one entry is left owing an answer: {pending:?}"
    );
    assert!(
        matches!(
            pending[0].hold(),
            TransitionHold::Abandoned {
                reason: AbandonReason::AttemptsExhausted { .. },
                ..
            }
        ),
        "it was given up on, not quietly dropped: {:?}",
        pending[0].hold()
    );
    assert_eq!(
        bot.tick()?,
        0,
        "the rest of the chain is resolved, so nothing fires"
    );

    // A new source value is new work — for the entries that were not given up
    // on. The abandoned entry is terminal until evidence revives it, so it is
    // not retried merely because the source moved.
    value.store(1, Ordering::SeqCst);
    assert_eq!(bot.tick()?, 1, "the new revision's work runs");
    assert_eq!(
        counted.load(Ordering::SeqCst),
        2,
        "the first entry ran once for each revision, and the abandoned second \
         entry was not retried"
    );
    Ok(())
}
