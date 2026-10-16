# What a failed tick means: confusion and reporting order

Pages of this guide: [polls and actions](failures.md) · [worked example](failures-example.md) · [uncertain effects](failures-effects.md) · [reporting order](failures-order.md)

## Confusion inside a chain

Two failures come from the wiring rather than from a domain, and both are typed
so they cannot be mistaken for a condition that simply did not fire.

- A condition whose `Evaluate<T>` implementation returns `Err` stops the chain
  with that error (`crates/lgwks-bot/src/ecs/fold.rs:690`). A structural failure in a
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
(`crates/lgwks-bot/src/ecs/resources.rs:79`); `tick` takes it after the schedule runs, and
it takes precedence over the `PendingTransition` report because it carries the
typed variant a retry classifier matches on.
