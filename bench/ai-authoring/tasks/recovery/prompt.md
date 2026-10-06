# Task: `recovery`

Write a Rust library crate whose crate root (`src/lib.rs`) exposes exactly these
two public items:

```rust
#[derive(Debug)]
pub enum RecoveryError {
    Unit { index: u32 },
    Deadline,
    NoStore,
    NoLedger,
}

pub async fn recover(
    world: ai_task_support::recovery::World,
    store_dir: std::path::PathBuf,
    deadline: std::time::Duration,
) -> Result<u64, RecoveryError>
```

## The world

`ai_task_support::recovery` is provided by the harness. Two of its types matter.

`World` is `Clone + Send + Sync + 'static`. Its units are indexed `0..width`:

```rust
pub const fn width(&self) -> u32   // how many units the world has
pub async fn unit(&self, scope: &lgwks_bot::script::Scope, index: u32)
    -> Result<u64, ai_task_support::recovery::UnitError>
```

`UnitError::Unit { index }` names the index that failed. A unit that succeeds
**records its own value durably**, through `lgwks_bot::script::remember`, under
the step name `ai_task_support::recovery::unit_step(index)`. That is the whole
point: a unit whose record is committed does not run its body again on a later
attempt at the same step. The harness counts each unit body inside the record, so
`world.stats().runs(index)` is `Some` of *the number of times the body actually
ran* — reading `Some(1)` for a unit that completed twice is the signal that the
record was ignored. It is `None` for an index this world does not have, which is
not the same answer as `Some(0)`.

`Ledger` is the effect. `world.ledger()` reaches it:

```rust
pub fn applied(&self, name: &str) -> bool   // has this effect already been applied?
pub fn apply(&self, name: &str)             // apply it, unconditionally; counts duplicates
pub fn applies(&self) -> u64                // how many times it was applied, duplicates included
```

`apply` refuses nothing. Reading `applied` first is the only way to avoid a
duplicate, and it is the only deduplication the harness offers.

Also available, all on `ai_task_support::recovery`:

- `unit_step(index: u32) -> String` — the step name a unit records under.
- `at_rest(&World) -> bool` — whether no unit body is live.
- `SETTLE: std::time::Duration` — how long a cancelled body may take to count out.

## `recover` must satisfy this contract

1. **It completes.** After enough calls, every unit of the world has run and
   `recover` resolves with `Ok(total)`, where `total` is the sum of the values
   every unit returned.

2. **A second call finishes the work without redoing completed units.** Calling
   `recover` again with the same `store_dir` must resolve with the same `Ok(total)`
   and must leave `world.stats().runs(index) == Some(1)` for every unit that
   completed on the first call. A resumed attempt replays the records; it does
   not re-run the bodies behind them.

3. **No duplicate effect.** Across any number of calls, `world.ledger().applies()`
   must be exactly `1`. The effect is applied under the name `"recover"`.

4. **The overall deadline is honoured.** If `deadline` passes before the work is
   finished, resolve with `Err(RecoveryError::Deadline)`, apply no further units,
   and leave no unit body running.

5. **Dropping the returned future leaves nothing live.** If the caller drops the
   future returned by `recover` mid-run, then after `SETTLE` has elapsed,
   `at_rest(&world)` must be `true`: no unit body may still be running.

## What makes this hard

You need a **run identity** that is the same on every attempt, and you need it
without a fresh random value per call — the second call must find the first call's
records. You need to **install a durable run store** on a `Host` so a unit's
`remember` has somewhere to land. You need a **helper** that builds the task body
once and is reused by both the first attempt and the resume. And you need to apply
the ledger effect **only once**, across both attempts.

The harness kills or drops the first attempt part-way through; your code never
sees that happen and must simply be correct when `recover` is called again.

You may use only the items on the API sheet that accompanies this task, plus
`std` and `ai_task_support`. Do not use any crate that is not on the sheet.

Reply with exactly one fenced ```rust block. The block must be the complete
contents of `src/lib.rs` and nothing else — no prose outside the block.