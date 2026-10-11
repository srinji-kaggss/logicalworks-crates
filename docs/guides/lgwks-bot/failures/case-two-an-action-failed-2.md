# Case two: an action failed (continued)

Part of [What a failed tick means](../failures.md).

```rust,ignore
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::spec::{AbandonReason, TransitionHold};
use lgwks_bot::{Auth, Bot, BotError, Cap, Execute, GrantSet, Observe};

struct Reading(Arc<AtomicU32>);

impl Observe for Reading {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(self.0.load(Ordering::SeqCst))
    }
    fn domain_id(&self) -> &str { "test::reading" }
}

/// Counts its own successful runs.
struct Count(Arc<AtomicU32>);

impl Execute for Count {
    type Input = u32;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn domain_id(&self) -> &str { "test::count" }
}

/// Fails with `DomainError`: the effect did not happen.
struct Fail;

impl Execute for Fail {
    type Input = u32;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: "test::fail".into(),
            cause: "the second action refused".into(),
        })
    }
    fn domain_id(&self) -> &str { "test::fail" }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let value = Arc::new(AtomicU32::new(0));
    let counted = Arc::new(AtomicU32::new(0));
    let mut bot = Bot::builder("two-actions")
        .observe(Reading(Arc::clone(&value)))
        .on(|_: &u32| true, Count(Arc::clone(&counted)))
        .on(|_: &u32| true, Fail)
        .build(&GrantSet::empty())?;

    let error = match bot.tick() {
        Ok(fired) => return Err(format!("the second action was reported as {fired} fired").into()),
        Err(error) => error,
    };
    assert!(matches!(error, BotError::DomainError { .. }), "got {error:?}");
    assert_eq!(counted.load(Ordering::SeqCst), 1, "the action before the failure already ran");

    // The source holds still, and the refused entry is attempted again anyway.
    // The action ahead of it has already succeeded and is not replayed.
    assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    assert_eq!(counted.load(Ordering::SeqCst), 1, "the acknowledged effect is not replayed");

    // Third attempt: the budget is spent. The tick reports the refusal, and
    // `pending` names the entry it gave up on rather than dropping it.
    assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    let pending = bot.pending();
    assert_eq!(pending.len(), 1, "one entry is left owing an answer: {pending:?}");
    assert!(matches!(
        pending[0].hold(),
        TransitionHold::Abandoned { reason: AbandonReason::AttemptsExhausted { .. }, .. }
    ));
    // The chain is not clean while the abandonment stands: it asks the tick for
    // nothing, but it is work nobody resolved, and the entry behind a
    // prerequisite that was given up on is not work that may proceed.
    assert!(matches!(bot.tick(), Err(BotError::PendingTransition { .. })));

    // A new source value is new work for the entries that were not given up on.
    // The abandoned entry is a barrier: it is not retried because the source
    // moved, nothing behind it is attempted either, and the tick still reports
    // the chain as unresolved even though the first entry ran again.
    value.store(1, Ordering::SeqCst);
    assert!(matches!(bot.tick(), Err(BotError::PendingTransition { .. })));
    assert_eq!(counted.load(Ordering::SeqCst), 2);
    Ok(())
}
```
