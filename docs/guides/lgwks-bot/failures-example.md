# What a failed tick means: a worked example

Pages of this guide: [polls and actions](failures.md) · [worked example](failures-example.md) · [uncertain effects](failures-effects.md) · [reporting order](failures-order.md)

## Case two: a worked example

`crates/lgwks-bot/examples/failed_tick.rs` is the program, and the workspace gate
compiles and runs it: two actions behind one source, the second of which fails.
Its assertions are this page's claims. The first tick reports `DomainError`
with the first action already run once; the second tick attempts the refused
entry again and does not replay the action ahead of it; the third spends the
budget, and `pending` names the abandoned entry as `TransitionHold::Abandoned`
with `AbandonReason::AttemptsExhausted`; the next tick is `PendingTransition`;
and a new source value runs the first action again while the abandoned entry
stays a barrier.
