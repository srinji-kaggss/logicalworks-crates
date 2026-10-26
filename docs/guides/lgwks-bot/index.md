# lgwks_bot

Pages of this guide: [overview](index.md) · [non-goals and next steps](index-scope.md)

A Rust automation library built around four verbs: Observe, Evaluate, Execute,
Query. One ECS world is the core: `Bot` runs change-triggered condition/action
chains on its schedule. One driver on tokio sits below the world for tasks and
I/O, used only where rebuilding a piece makes no sense, and `script!` is the
framework above it ([ADR 0006](../../adr/0006-one-ecs-world-one-tokio-axum-driver.md)).
Capability checks apply to participating library calls; the crate is not a
sandbox.

```sh
cargo add lgwks_bot
```

```rust
lgwks_bot::script! {
    flow count(items: Vec<u8>) -> usize:
        give back items.len()
}
```

The package is `lgwks_bot` (underscore) in the directory `crates/lgwks-bot`
(hyphen), written as `lgwks_bot::script!`. There is no `lgwks-bot` package to
add. [Getting started](getting-started.md) runs a whole program.

## What a bot is

A bot binds sources to effects. You give it an `Observe` source, a condition
over that source's output, and an `Execute` action; the bot polls the source on
each tick and runs the action on the ticks where the polled value moved. A
condition that stays true does not re-fire, which is the difference between this
and a timer that re-evaluates a predicate every interval.

`Bot` is not a separate type with a separate implementation. `crates/lgwks-bot/src/spec.rs:337`
re-exports the ECS bot under the shorter name:

```rust
pub use crate::ecs::{
    AbandonReason, EcsBot as Bot, EcsBuilder as BotBuilder, EcsObserveBuilder as ObserveBuilder,
    EffectEvidence, PendingWork, RetryPolicy, TransitionHold, WorkId,
};
```

There is one execution path. `crates/lgwks-bot/Cargo.toml` states that
`bevy_ecs` is not optional and not behind a feature, because a default-off flag
"would mean nothing exercises it, the workspace gate never compiles it, and it
rots into a second opinion nobody chose."

`Bot::tick` is the synchronous adapter (`crates/lgwks-bot/src/ecs/bot.rs:483`): it
drives the non-`Send` verb futures on a thread-parking executor, so there is
nothing to await. `Bot::tick_async` is the same tick awaited on the caller's
executor (`crates/lgwks-bot/src/ecs/bot.rs:304`), and it is the one to call from
inside a runtime — `tick` refuses there with `BotError::TickInsideRuntime`
rather than park the thread that owns the reactor. Do not write
`bot.tick().await`; that is `tick` returning `usize`, then a `usize` that is not
a future. See [failures](failures.md) for why the refusal is not a failed tick.

## Supported versions

`crates/lgwks-bot/Cargo.toml` declares version `4.0.0` and `rust-version =
"1.98"`, edition 2024. Which release first carries a symbol, and what each
major removed, is in [`CHANGELOG.md`](../../../CHANGELOG.md) and
[`docs/changelog/`](../../changelog); 4.0.0 removed `EcsBot::world` and
`Clock::sleep` under ADR 0006.

## Install and import

Add the crate and the default features come with it:

```sh
cargo add lgwks_bot
```

That is `script`, which brings `rt`, `time`, `sync`, and `macros`
(`crates/lgwks-bot/Cargo.toml`). The default build pulls tokio through the
`lgwks_deps` storefront's `tokio` feature.

`--no-default-features` withdraws the async surface and the tokio edge. It does
**not** remove `bevy_ecs`. The ECS substrate is unconditional, so a no-default
build still compiles the schedule and the four verbs, driving them on
`lgwks_std::task`. If your reason for disabling defaults was to avoid the ECS
substrate, this crate cannot do that; see [features](features.md) for the
feature-by-feature facts.
