# Getting started with lgwks_bot

One source, one condition, one effect. The program below compiles as a
standalone `main` against `lgwks_bot = "0.4.2"` and its default features. Every
assertion in it was run.

## The manifest

```toml
[package]
name = "queue-watch"
version = "0.1.0"
edition = "2024"

[dependencies]
lgwks_bot = "0.4.2"
```

## The program

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::{Auth, Bot, BotError, Cap, Execute, GrantSet, Observe};

/// The observed source. A real one reads a socket, a file, or an API; this one
/// reads a counter the caller owns, so the example is deterministic.
struct QueueDepth {
    depth: Arc<AtomicU32>,
}

impl Observe for QueueDepth {
    /// `PartialEq` is this executor's one extra bound: the condition here *is*
    /// change detection, so the observed value has to be comparable.
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(self.depth.load(Ordering::SeqCst))
    }

    fn domain_id(&self) -> &str {
        "queue::depth"
    }
}

/// The effect. Delivering a notification is what needs `bot.notify`, so that is
/// the capability the action declares.
struct Page {
    caps: Vec<Cap>,
    pages: Arc<AtomicU32>,
}

impl Execute for Page {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        // Checked before the effect, not after it.
        call.0.check(self.required_caps())?;
        self.pages.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "notify::page"
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let depth = Arc::new(AtomicU32::new(3));
    let pages = Arc::new(AtomicU32::new(0));

    // The grant set is required at build, not only at run: a bot declaring a
    // capability it was not granted fails here.
    let grants = GrantSet::empty().grant(Cap::notify());

    let mut bot = Bot::builder("queue-watch")
        .observe(QueueDepth {
            depth: Arc::clone(&depth),
        })
        .on(|seen: &u32| *seen > 10, Page {
            caps: vec![Cap::notify()],
            pages: Arc::clone(&pages),
        })
        .build(&grants)?;

    // Tick one: depth is 3. A source with no remembered value counts as
    // changed, so the condition is evaluated; it is false, so nothing fires.
    let first = bot.tick()?;
    assert_eq!(first, 0, "depth 3 is under the threshold");
    assert_eq!(bot.revisions(), vec![1], "the first poll is a change");

    // Tick two: depth is unchanged. `Revision` does not move, so the condition
    // is not even re-evaluated.
    let second = bot.tick()?;
    assert_eq!(second, 0);
    assert_eq!(bot.revisions(), vec![1], "a held value is not a change");

    // Tick three: depth moves past the threshold, and the effect runs once.
    depth.store(11, Ordering::SeqCst);
    let third = bot.tick()?;
    assert_eq!(third, 1, "one action fired");
    assert_eq!(pages.load(Ordering::SeqCst), 1);

    // Tick four: depth is still 11. The condition is still true and the effect
    // does not run again, because nothing moved.
    let fourth = bot.tick()?;
    assert_eq!(fourth, 0, "a held value does not re-fire");
    assert_eq!(pages.load(Ordering::SeqCst), 1);

    println!("fired: {third}, revisions: {:?}", bot.revisions());
    Ok(())
}
```

It prints `fired: 1, revisions: [2]`.

## What each piece is

`Observe` produces the value. `poll` takes an `(Auth, ())` tuple because a read
can cross a trust boundary too; `call.0.check(self.required_caps())?` is what
makes the proof load-bearing rather than decorative.

`Observe::Output` must be `PartialEq`. `EcsBuilder::observe` requires it
(`crates/lgwks-bot/src/ecs.rs:3517`), because the condition on this executor *is*
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
(`crates/lgwks-bot/src/ecs.rs:3111`), so the effects fire in the order you wrote
the `.on` calls.

`build(&grants)` returns `Result<Bot, BotError>`. Two things make it fail:
`GrantSet::admit` rejects a source or action whose `required_caps` the set does
not cover (`crates/lgwks-bot/src/ecs.rs:3741`), and `Schedule::initialize` with
`ambiguity_detection: LogLevel::Error` rejects a schedule whose systems cannot be
totally ordered (`crates/lgwks-bot/src/ecs.rs:2637`).

`Bot::tick` returns `Result<usize, BotError>`, where the `usize` is the number of
actions that fired. The bot is `mut` because a tick advances its world. `tick`
is the synchronous adapter and must be called from outside an async runtime;
inside one, `await bot.tick_async()` returns the same `Result` from the same
work, and `tick` refuses with `BotError::TickInsideRuntime` rather than park the
thread the runtime is driving.

## Inspecting a bot

Four accessors exist for tests and instrumentation, all read-only
(`crates/lgwks-bot/src/ecs.rs:2719`):

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
tick `observe_fold` treats it as changed (`crates/lgwks-bot/src/ecs.rs:3059`), so
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
