# What a failed tick means

`Bot::tick` returns `Result<usize, BotError>` (`crates/lgwks-bot/src/ecs.rs:447`).
Three different things can produce the `Err`, and they mean different things for
your data. The distinction is the difference between a retry and a duplicate.

The three cases below are exercised in
`crates/lgwks-bot/src/ecs.rs` tests and in
`crates/lgwks-bot/src/spec.rs` (`a_failing_poll_fires_nothing_and_returns_the_first_error`).

## Case one: a poll failed

Nothing ran. `observe` returns before committing anything
(`crates/lgwks-bot/src/ecs.rs:241`):

```rust,ignore
let values = match polled.into_iter().collect::<Result<Vec<_>, _>>() {
    Ok(values) => values,
    Err(error) => {
        world.resource_mut::<TickError>().0 = Some(error);
        return;
    }
};
```

`collect` into a `Result<Vec<_>, _>` keeps the first error in declaration order
and drops the rest. The early return happens before the observed values are
written and before any `Revision` is bumped, so a source that held still on the
previous tick still holds still, and a source that had moved still shows its
previous revision. No condition is evaluated, so no action runs.

`tick` returns that error and the tick had no effect.

## Case two: an action failed

Actions run in declaration order inside the `fire` system
(`crates/lgwks-bot/src/ecs.rs:284`). The loop breaks on the first failure and
parks it, and there is no rollback:

```rust,ignore
match lgwks_std::task::block_on(entry.action.run_any(&grants.0, value.as_ref())) {
    Ok(_) => fired = fired.saturating_add(1),
    Err(error) => {
        failure = Some(error);
        break 'chains;
    }
}
```

Every action before the failure already ran, and its effect is live. `Err` here
means "this run did not finish", not "nothing happened".

One consequence is easy to miss. `observe` committed the new values and bumped
the revisions *before* `fire` ran (`crates/lgwks-bot/src/ecs.rs:265`), so the
next tick polls the source, finds it unchanged, and does not re-fire the chain.
The actions after the failure are not attempted again until the source moves.
The work is lost, not queued.

The program below asserts that, along with the poll case.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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

    let error = bot.tick().expect_err("the second action fails");
    assert!(matches!(error, BotError::DomainError { .. }), "got {error:?}");
    assert_eq!(
        counted.load(Ordering::SeqCst),
        1,
        "the action before the failure already ran"
    );

    // The failed chain is not retried while the source holds still.
    let second = bot.tick()?;
    assert_eq!(second, 0, "the chain does not re-fire on an unchanged value");
    assert_eq!(counted.load(Ordering::SeqCst), 1);

    // It runs again once the source moves.
    value.store(1, Ordering::SeqCst);
    assert!(bot.tick().is_err());
    assert_eq!(counted.load(Ordering::SeqCst), 2);
    Ok(())
}
```

## Case three: the effect may already have happened

`BotError` separates "the effect did not happen" from "the effect may have
happened" as two variants, not one variant with a flag
(`crates/lgwks-bot/src/error.rs:12`).

| Variant | Means | A retry is |
|---|---|---|
| `BotError::DomainError` | the action did not take effect | a retry |
| `BotError::EffectIndeterminate` | the effect may be live | a possible duplicate |

`EffectIndeterminate` is documented as the case you reach "when a request timed
out, or a connection dropped, *after* the request was sent"
(`crates/lgwks-bot/src/error.rs:80`). Its `Display` renders the domain, the
cause, and the words "may have taken effect", so an operator reading a log line
sees the indeterminacy rather than having to infer it.

Write your retry classifier against the variant, never against the cause string:

```rust
fn may_have_taken_effect(error: &lgwks_bot::BotError) -> bool {
    matches!(*error, lgwks_bot::BotError::EffectIndeterminate { .. })
}
```

`BotError` is `#[non_exhaustive]`, so a `match` on it needs a wildcard arm. That
is deliberate: a new variant becomes a compile-time prompt at your `match` rather
than a silent fallthrough.

Delivering exactly-once across an external effect needs durable intent and an
outcome-unknown record. Neither exists in the inspected source. Until they do,
treat a failed tick as a partial run to be reconciled.

## Confusion inside a chain

Two failures come from the wiring rather than from a domain, and both are typed
so they cannot be mistaken for a condition that simply did not fire.

- A condition whose `Evaluate<T>` implementation returns `Err` stops the chain
  with that error (`crates/lgwks-bot/src/ecs.rs:318`). A structural failure in a
  condition is `BotError::EvaluateError`, not `false`.
- The erased chain wrappers downcast the observed value back to the type the
  condition was registered with. A mismatch in the condition is
  `EvaluateError`; a mismatch in the action's input is
  `BotError::DomainError` naming the action's domain
  (`crates/lgwks-bot/src/spec.rs:134`, `crates/lgwks-bot/src/spec.rs:165`). The
  `Auth` is issued before the downcast in the action path, so a type mismatch
  fails without a side effect.

## The order errors are reported in

Systems stop at the first error and the driver reports that one. `TickError` is
a resource because an exclusive system returns `()` and cannot propagate
(`crates/lgwks-bot/src/ecs.rs:142`), and `tick` takes it after the schedule runs.
For polls, "first" means first in declaration order: all sources are polled
before any `Revision` is written, so a failing poll cannot leave one source
updated and another not.
