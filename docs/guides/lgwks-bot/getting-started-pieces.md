# Getting started with lgwks_bot: the pieces

Pages of this guide: [manifest](getting-started.md) · [the program](getting-started-program.md) · [the pieces](getting-started-pieces.md)

## What each piece is

`Observe` produces the value. `poll` takes an `(Auth, ())` tuple because a read
can cross a trust boundary too; `call.0.check(self.required_caps())?` is what
makes the proof load-bearing rather than decorative.

`Observe::Output` must be `PartialEq`. `EcsObserveBuilder::observe` is where the
requirement lands, on the call that closes a chain
(`crates/lgwks-bot/src/ecs/builder.rs:440`), because the condition on this executor *is*
bevy's change detection, and a value that cannot be compared cannot be detected
as changed. `Observe::poll` is `async fn` returning a future that is
deliberately not `Send` (`crates/lgwks-bot/src/verb.rs`), so a domain may hold
thread-local state.

`Bot::builder(name).observe(source).on(condition, action)` is the builder chain.
A closure that is `Fn(&T) -> bool` implements `Evaluate<T>` automatically. If you
need to report *which* condition fired, implement `Evaluate<T>` directly and give
it a `condition_id`.

`Execute::execute_action` takes `(Auth, &Self::Input)`. The tick awaits each one
before starting the next, in declaration order
(`crates/lgwks-bot/src/ecs/bot.rs:442`), so the effects fire in the order you wrote
the `.on` calls.

`build(&grants)` returns `Result<Bot, BotError>`. Two things make it fail:
`GrantSet::admit` rejects a source or action whose `required_caps` the set does
not cover (`crates/lgwks-bot/src/gate.rs:116`), and `Schedule::initialize` with
`ambiguity_detection: LogLevel::Error` rejects a schedule whose systems cannot be
totally ordered (`crates/lgwks-bot/src/ecs/fold.rs:712`).

`Bot::tick` returns `Result<usize, BotError>`, where the `usize` is the number of
actions that fired. The bot is `mut` because a tick advances its world. `tick`
is the synchronous adapter and must be called from outside an async runtime;
inside one, `await bot.tick_async()` returns the same `Result` from the same
work, and `tick` refuses with `BotError::TickInsideRuntime` rather than park the
thread the runtime is driving.

## Inspecting a bot

Four accessors exist for tests and instrumentation, all read-only
(`crates/lgwks-bot/src/ecs/bot.rs:275`):

| Method | Returns |
|---|---|
| `name()` | the name the builder was given |
| `fired()` | effects fired on the most recent tick |
| `revisions()` | the `Revision` of every source, in chain order |
| `source_domains()` | the `domain_id()` of every source, in chain order |

`revisions()` is the one worth knowing about. It is a monotone counter per
source that advances only when the polled value moved, so it is how you check
that "the source held still" really was a hold rather than a condition deciding
not to fire.

A source whose poll has never succeeded has no remembered value. On the next
tick `observe_fold` treats it as changed (`crates/lgwks-bot/src/ecs/ledger.rs:847`), so
the condition is evaluated on the first poll even if the value never moves again.

## A bot with no chains

An empty chain list is valid. A bot that only serves direct `Query` and
`Execute` calls needs no source, and `BotBuilder::build` covers that case:

```rust
use lgwks_bot::{Bot, GrantSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bot = Bot::builder("direct-only").build(&GrantSet::empty())?;
    assert_eq!(bot.source_domains().len(), 0);
    Ok(())
}
```

This is also the shape the repository root README compiles as its quickstart
(`crates/lgwks-bot/examples/quickstart.rs`).

## Where to go next

- [Features](features.md) for which cargo features you actually need.
- [Authority](authority.md) before you design anything around revoking a grant:
  `GrantSet` has no revoke operation, and a built bot holds a snapshot of it.
- [Failures](failures.md) before you write a retry.
- [Background work](background-work.md) if the bot needs work running beside the
  tick rather than inside it.
