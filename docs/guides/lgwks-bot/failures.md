# What a failed tick means

`Bot::tick` returns `Result<usize, BotError>`. Three different things can
produce the `Err`, and they mean different things for your data. The distinction
is the difference between a retry and a duplicate.

Cases one and two are the two the `observe` and `fire` systems produce, and the
program under case two is compiled and run as
`crates/lgwks-bot/examples/failed_tick.rs`. Case three is what the error variant
behind an indeterminate effect means.

## Case one: a poll failed

Nothing ran. `observe` returns before committing anything:

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

Actions run in declaration order inside the `fire` system, and there is no
rollback:

```rust,ignore
match lgwks_std::task::block_on(entry.action.run_any(grants, value)) {
    Ok(_) => result.fired = result.fired.saturating_add(1),
    Err(error) => {
        *state = EntryState::DefinitelyFailed { attempts, cause };
        result.failure = Some(error);
        break;
    }
}
```

Every action before the failure already ran, and its effect is live. `Err` here
means "this run did not finish", not "nothing happened".

What the old substrate did next was lose the rest. `observe` committed the new
values and bumped the revisions *before* `fire` ran, and `fire` selected sources
with `Changed<Revision>`, so the next tick polled an unchanged source, found
nothing eligible, and never attempted the actions after the failure again. The
work was lost, not queued.

Eligible work is now recorded in its own structure, keyed by `(chain, entry)`,
and `fire` walks *that* rather than the change set: `Changed<Revision>` only
opens a transition. Three consequences follow, and the example below asserts all
three.

- **The failed entry is retried while it has budget, even if the source never
  moves again.** `RetryPolicy` sets the budget (three attempts by default;
  `RetryPolicy::ONE_ATTEMPT` for a domain whose failures are always terminal).
- **The entries after it wait.** The walk stops at the first entry it cannot
  settle, so an acknowledged effect is never replayed to reach a successor.
- **Giving up is reported, not silent.** When the budget is spent the entry is
  abandoned, the tick still returns the action's own typed error, and
  `Bot::pending()` names the entry, the reason (`AbandonReason`), and the source
  revision. A `pending()` that lists anything is work that is still owed, and a
  clean `Ok` tick — with `Err(BotError::PendingTransition)` in place of silence
  whenever work is held and nothing failed — cannot be read as "the transition
  was handled".

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
    assert_eq!(bot.tick()?, 0, "the rest of the chain is resolved, so nothing fires");

    // A new source value is new work for the entries that were not given up on.
    value.store(1, Ordering::SeqCst);
    assert_eq!(bot.tick()?, 1, "the new revision's work runs");
    assert_eq!(counted.load(Ordering::SeqCst), 2);
    Ok(())
}
```

## Case three: the effect may already have happened

`BotError` separates "the effect did not happen" from "the effect may have
happened" as two variants, not one variant with a flag.

| Variant | Means | A retry is |
|---|---|---|
| `BotError::DomainError` | The action did not take effect | a retry |
| `BotError::EffectIndeterminate` | The effect may be live | a possible duplicate |

`EffectIndeterminate` is documented as the case you reach "when a request timed
out, or a connection dropped, *after* the request was sent". Its `Display`
renders the domain, the cause, and the words "may have taken effect", so an
operator reading a log line sees the indeterminacy rather than having to infer
it.

Write your retry classifier against the variant, never against the cause string:

```rust
fn may_have_taken_effect(error: &lgwks_bot::BotError) -> bool {
    matches!(*error, lgwks_bot::BotError::EffectIndeterminate { .. })
}
```

`BotError` is `#[non_exhaustive]`, so a `match` on it needs a wildcard arm. That
is deliberate: a new variant becomes a compile-time prompt at your `match` rather
than a silent fallthrough.

An indeterminate effect is the one case the substrate refuses to decide on its
own. The entry is held — reported through `Bot::pending()` as
`TransitionHold::OutcomeUnknown`, and never attempted again by the tick loop,
whatever the budget says — until the caller says what happened:

```rust,ignore
bot.resolve_effect(work, EffectEvidence::Applied)?;   // it happened: recorded, not replayed
bot.resolve_effect(work, EffectEvidence::NotApplied)?; // it did not: eligible again
```

`resolve_effect` refuses with `BotError::NoSuchWork` when the entry is not held,
because accepting evidence for an entry that has an answer would let a caller
believe an effect was acknowledged when nothing was.

The ledger is in memory, so this is a report to the caller that owns the run, not
durability: an effect left in doubt is owed an answer by whoever holds the bot,
including across a restart. Delivering exactly-once across an external effect
needs that intent stored outside the process.

## Confusion inside a chain

Two failures come from the wiring rather than from a domain, and both are typed
so they cannot be mistaken for a condition that simply did not fire.

- A condition whose `Evaluate<T>` implementation returns `Err` stops the chain
  with that error, and the entry is left exactly as it was: nothing was
  attempted, so nothing about the effect is claimed. A structural failure in a
  condition is `BotError::EvaluateError`, not `false`.
- The erased chain wrappers downcast the observed value back to the type the
  condition was registered with. A mismatch in the condition is
  `EvaluateError`; a mismatch in the action's input is `BotError::DomainError`
  naming the action's domain (`crates/lgwks-bot/src/spec.rs`). The `Auth` is
  issued before the downcast in the action path, so a type mismatch fails
  without a side effect.

## The order errors are reported in

A poll failure stops the tick before any effect: all sources are polled before
any `Revision` is written, so a failing poll cannot leave one source updated and
another not, and `tick` returns the first error in declaration order.

An action failure does not stop the later chains. `fire` walks every chain in
declaration order, parks the first failure it saw, and reports that one, while
the chains behind the failing one still run and still record their work. The
error is parked in the `TickError` resource rather than returned, because an
exclusive system returns `()` and cannot propagate; `tick` takes it after the
schedule runs, and it takes precedence over the `PendingTransition` report
because it carries the typed variant a retry classifier matches on.
