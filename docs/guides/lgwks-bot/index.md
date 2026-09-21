# lgwks_bot

A Rust automation library built around four verbs: Observe, Evaluate, Execute,
Query. `Bot` runs change-triggered condition/action chains on one ECS schedule.
An optional runtime surface provides asynchronous task and I/O facilities.
Capability checks apply to participating library calls; the crate is not a
sandbox.

```sh
cargo add lgwks_bot
```

```rust
use lgwks_bot::{Bot, Cap, GrantSet};
```

The package is `lgwks_bot` (underscore) in the directory `crates/lgwks-bot`
(hyphen), imported as `use lgwks_bot::...`. There is no `lgwks-bot` package to
add.

## What a bot is

A bot binds sources to effects. You give it an `Observe` source, a condition
over that source's output, and an `Execute` action; the bot polls the source on
each tick and runs the action on the ticks where the polled value moved. A
condition that stays true does not re-fire, which is the difference between this
and a timer that re-evaluates a predicate every interval.

`Bot` is not a separate type with a separate implementation. `crates/lgwks-bot/src/spec.rs:329`
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

`Bot::tick` is the synchronous adapter (`crates/lgwks-bot/src/ecs.rs:1999`): it
drives the non-`Send` verb futures on a thread-parking executor, so there is
nothing to await. `Bot::tick_async` is the same tick awaited on the caller's
executor (`crates/lgwks-bot/src/ecs.rs:1752`), and it is the one to call from
inside a runtime — `tick` refuses there with `BotError::TickInsideRuntime`
rather than park the thread that owns the reactor. Do not write
`bot.tick().await`; that is `tick` returning `usize`, then a `usize` that is not
a future. See [failures](failures.md) for why the refusal is not a failed tick.

## Supported versions

`crates/lgwks-bot/Cargo.toml` declares version `0.4.2` and `rust-version =
"1.98"`, edition 2024.

The version in the manifest is not the whole story. Five modules on `main` are
absent from the `lgwks_bot-v0.4.2` tag:

| Module | In the 0.4.2 tag | Verified by |
|---|---|---|
| `session` | no | `git show lgwks_bot-v0.4.2:crates/lgwks-bot/src/lib.rs` |
| `language` | no | same |
| `semantic` | no | same |
| `interface` | no | same |
| `frontier` | no | same |

Everything else in this guide describes symbols the 0.4.2 tag exports. The two
pages that cover the unreleased modules say so at the top:
[sessions](sessions.md) and [resolution](resolution.md).

## Install and import

Add the crate and the default features come with it:

```sh
cargo add lgwks_bot
```

That is `rt`, `time`, `sync`, and `macros` (`crates/lgwks-bot/Cargo.toml`). The
default build pulls tokio through the `lgwks_deps` storefront's `tokio` feature.

`--no-default-features` withdraws the async surface and the tokio edge. It does
**not** remove `bevy_ecs`. The ECS substrate is unconditional, so a no-default
build still compiles the schedule and the four verbs, driving them on
`lgwks_std::task`. If your reason for disabling defaults was to avoid the ECS
substrate, this crate cannot do that; see [features](features.md) for the
feature-by-feature facts.

## Non-goals

These are absent, and the absence is stated in the source rather than left for
you to discover.

- **No model client.** The crate runs your logic and gates its authority. It
  does not call a model, hold a prompt, or know what an LLM is.
- **No sandbox.** Capability gating "stops confused-deputy calls and accidental
  ungated use; it is not a sandbox, since in-process code can always dial out
  directly" (`crates/lgwks-bot/src/cap.rs:15`). See [authority](authority.md).
- **No revocable authority.** `GrantSet` has no `revoke`, and a built bot holds
  a clone of the set it was admitted with. See [authority](authority.md).
- **No spec materializer yet.** `BotSpec` validates a JSON document. There is no
  `Bot::from_spec`; you build through the builder chain, and
  `crates/lgwks-bot/src/spec.rs:574` validates shape only. Unlike the entries
  around it, this one is scheduled work rather than a permanent limit: it is
  recorded as open in `experience/invariants/sdk.yaml`, and closing it means
  building a `domain_id -> constructor` registry.
- **No `#[tokio::main]` equivalent.** Entry to the async surface is
  `Runtime::block_on`.
- **No exactly-once delivery.** An action that may have taken effect after its
  request was sent fails as `BotError::EffectIndeterminate`, and reconciling
  that is your decision. See [failures](failures.md).

## Where to go next

| Page | Answers |
|---|---|
| [Getting started](getting-started.md) | One source, one condition, one effect, compiled and run |
| [Features](features.md) | Every feature, what it adds, and what it does not |
| [Authority](authority.md) | Admission, the `Auth` snapshot, and the no-revocation boundary |
| [Failures](failures.md) | Poll failure, partial action failure, unknown effect |
| [Background work](background-work.md) | Bounded fan-out, supervision, and the cancellation limits |
| [Sessions](sessions.md) | Unreleased. Validated guidance flows |
| [Resolution](resolution.md) | Unreleased. Lexical and semantic resolution, and what `Degraded` means |
