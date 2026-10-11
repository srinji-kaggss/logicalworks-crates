# What a failed tick means

Both entry points return `Result<usize, BotError>`: `Bot::tick` is the
synchronous adapter (`crates/lgwks-bot/src/ecs/bot.rs:568`) and `Bot::tick_async` is
the one to `await` from inside a runtime (`crates/lgwks-bot/src/ecs/bot.rs:393`).
Four different things can produce an `Err`. Three are failures that mean
different things for your data — the distinction is the difference between a
retry and a duplicate — and the fourth is the adapter refusing to run a tick at
all.

Cases one and two are the two the `observe_fold` and `fire_plan` systems produce,
and the program under case two is compiled and run as
`crates/lgwks-bot/examples/failed_tick.rs`. Case three is what the error variant
behind an indeterminate effect means.

## Case one: a poll failed

Nothing ran. The `observe_fold` system returns before committing anything
(`crates/lgwks-bot/src/ecs/ledger.rs:716`):

```rust,ignore
// Pass one: the first error, in declaration order.
let mut first_error = None;
for result in polled.iter_mut() {
    if result.is_err() {
        first_error = std::mem::replace(result, Ok(None)).err();
        break;
    }
}
if let Some(error) = first_error {
    world.resource_mut::<TickError>().0 = Some(error);
    put_polled(world, polled);
    return;
}
```

The first error in declaration order is the one `tick` reports, and it is moved
out of the slot it was found in rather than copied, because `BotError` is
deliberately not `Clone` — a copy would let a caller settle an effect against a
duplicate of the failure instead of the failure. The early return happens before
the observed values are written and before any `Revision` is bumped, so a source
that held still on the previous tick still holds still, and a source that had
moved still shows its previous revision. No condition is evaluated, so no action
runs.

`tick` returns that error and the tick had no effect.

## Not a failure: the adapter refused

`BotError::TickInsideRuntime` is an `Err` from `tick`, and it is not a failed
tick. Nothing was observed, no condition was evaluated and no effect ran,
because the synchronous adapter refused to park the thread before it started.
It is returned whenever `tick` is called while an async runtime is running on
that thread, on a current-thread runtime and on a multi-worker one alike: a
parked thread starves the reactor the bot's own verbs need, so the deadlock it
would otherwise cause is not something the adapter can wait out. Await
`bot.tick_async()` instead, or drive the tick from outside the runtime. The
README's "Running a tick from async code" section has both forms.

## Confusion inside a chain

Two failures come from the wiring rather than from a domain, and both are typed
so they cannot be mistaken for a condition that simply did not fire.

- A condition whose `Evaluate<T>` implementation returns `Err` stops the chain
  with that error (`crates/lgwks-bot/src/ecs/systems.rs:619`). A structural failure in a
  condition is `BotError::EvaluateError`, not `false`. The stop is ordered rather
  than absolute: the steps `fire_plan` recorded before the failing condition are
  still run, so an effect the walk had already cleared does take effect, while
  the entry behind the failing condition is left exactly as it was — nothing was
  attempted, so nothing about its effect is claimed.
  `a_condition_failure_stops_the_walk_after_the_effects_it_cleared` pins the
  ordered half and
  `a_condition_that_cannot_be_evaluated_holds_the_chain_without_claiming_anything`
  pins the other.
- The erased chain wrappers downcast the observed value back to the type the
  condition was registered with. A mismatch in the condition is
  `EvaluateError`; a mismatch in the action's input is
  `BotError::TypeMismatch`, naming the site that caught it and both types
  (`crates/lgwks-bot/src/spec.rs`). It names no domain, because a mismatch
  means no domain was reached — and it is `Terminal`, so it costs one attempt
  rather than a whole retry budget. The `Auth` is issued before the downcast in
  the action path, so a type mismatch fails without a side effect.
- The witness is checked before either downcast. Each chain records the type its
  source produces (`TypeId`, taken where the type was still a parameter) and
  every polled value carries the type it was erased from; `observe_fold` compares
  them at the rendezvous and reports `BotError::TypeMismatch` from site
  `observe_fold rendezvous` if they disagree, committing nothing. This is the
  half a typed builder cannot prove: the builder fixes the pairing at
  construction, and the witness checks it still holds across erasure.

## The order errors are reported in

A poll failure stops the tick before any effect: all sources are polled before
any `Revision` is written, so a failing poll cannot leave one source updated and
another not, and `tick` returns the first error in declaration order.

An action failure does not stop the later chains. `fire_plan` walks every chain
in declaration order and `run_steps` runs them in that same order, parking the
first failure it saw and reporting that one, while the chains behind the failing
one still run and still record their work. `TickError` is a resource rather than
a return value because an exclusive system returns `()` and cannot propagate
(`crates/lgwks-bot/src/ecs/components.rs:71`); `tick` takes it after the schedule runs, and
it takes precedence over the `PendingTransition` report because it carries the
typed variant a retry classifier matches on.

## In this guide

- [Case two: an action failed](failures/case-two-an-action-failed.md)
- [Case three: the effect may already have happened](failures/case-three-the-effect-may-already-have-happened.md)
