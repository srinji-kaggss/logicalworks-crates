# What a bot is

Part of [lgwks_bot](../index.md).


A bot binds sources to effects. You give it an `Observe` source, a condition
over that source's output, and an `Execute` action; the bot polls the source on
each tick and runs the action on the ticks where the polled value moved. A
condition that stays true does not re-fire, which is the difference between this
and a timer that re-evaluates a predicate every interval.

`Bot` is not a separate type with a separate implementation. `crates/lgwks-bot/src/spec.rs:456`
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

`Bot::tick` is the synchronous adapter (`crates/lgwks-bot/src/ecs/bot.rs:567`): it
drives the non-`Send` verb futures on a thread-parking executor, so there is
nothing to await. `Bot::tick_async` is the same tick awaited on the caller's
executor (`crates/lgwks-bot/src/ecs/bot.rs:393`), and it is the one to call from
inside a runtime — `tick` refuses there with `BotError::TickInsideRuntime`
rather than park the thread that owns the reactor. Do not write
`bot.tick().await`; that is `tick` returning `usize`, then a `usize` that is not
a future. See [failures](../failures.md) for why the refusal is not a failed tick.
