# The `lgwks_bot` rig: what the numbers say

Part of the [measurement rigs](README.md). Read off the headline table in
[bot-headline.md](bot-headline.md) and the profile in [bot-profile.md](bot-profile.md):
on this tree a quiet tick costs 896.0–2 055.4 ns, and the effect-heavy scenarios
cost 54 248.3–133 986.7 ns.

## Cost is in the act, not the poll

`poll-only-64x100` polls 64 sources and detects change with **no entries to
walk**: 896.0 ns/tick. `steady-64x100` adds a full `(condition, action)` entry to
every chain: 2 055.4 ns/tick, **2.29x** the poll-only cost.

The profile says where the difference is. On `steady-64x100` the effect path
(`act` — ledger, warrant and journal record) is 56.8% of the tick and poll plus
change detection (`poll` + `fingerprint` + `compare`) is 26.1%. On
`poll-only-64x100`, which fires nothing, `act` is 3.6% and poll is 47.7%. On the
effect-heavy scenarios `act` is 79.7–98.7% of the tick. The old file's "98% of a
tick is poll and change detection" was true of a tree in which effects wrote no
record; on this tree an optimisation aimed at anything but the effect path is
aimed at the smaller share, once a tick is no longer quiet.

## Change detection still pays, and the gap widened again

`steady-64x100` (one change per 100 ticks) costs 2 055.4 ns/tick. `churn-64x1`
(every source moves every tick) costs 133 986.7 ns/tick. Making the workload
change 100x more often costs **65.2x** more time, against 48.9x measured on main
`940e253cd` just before #279 (28.1x in the record this file carried, then 4.23x
and 1.95x before that).

The widening is the same thing compounding: each change made a quiet tick
cheaper — first the digest that skipped polls, then the change ticks and
`PollScratch`, now the allocation-free quiet tick that skips the decision pass —
without making a churning tick cheaper. A workload that never goes quiet pays
about sixty-five times what a quiet one does, and nearly all of that multiple is
the per-effect record, not the per-source poll.

## Cost is linear in sources only at equal effect rates

`wide-256x10` runs 256 sources at 211.9 ns per source-tick; `steady-64x100` runs
64 sources at 32.1 ns per source-tick; `poll-only-64x100` at 14.0 ns. The
baseline is flat at ~0.5 ns per source-tick across all three shapes, so there is
no superlinear term in the work itself. The bot figures differ because the
effect rate differs: `wide-256x10` fires roughly 25.6 effects per tick,
`steady-64x100` about 0.64, and each effect carries the record the profile
attributes to `act`. Per source, the poll is linear; per effect, the record
dominates.

## Per-entry walk cost

`fanout-1x64` walks 64 entries on one source every tick: 110 259.8 ns/tick, or
**~1 723 ns per entry walked** (condition plus action plus ledger, warrant and
record). The `churn-64x1` marginal cost over `steady-64x100` is ~2 061 ns per
walked entry. A decided effect therefore costs on the order of two microseconds,
against ~0.5–0.8 ns for the same decision in the baseline. The two-microsecond
figure is the ledger write, the warrant and the durable record a dispatched
effect is written through — the T4/T6 guarantees itemised in [what the
multiplier buys](bot-defects.md) — not the condition evaluation, which the
profile measures at 465 ns for all 64 entries of `fanout-1x64`.

## Admission is free

`Bot::builder(..).build(&grants)` admits 64 chains in 0.035–0.121 ms across the
five scenarios. Capability admission and schedule validation are not a cost
worth optimising.

## Determinism holds, and is exercised

Replaying a 500-tick scenario produces an identical per-tick effect sequence
(192 effects in the run above). This is a *correctness* observation and no timing
can make it; the formal statement and proof are in `../proofs/`, and the claim is
that the schedule is a function of its inputs.

## What this change could not reach

The 10x quiet target asks the whole tick to cost ten baseline ticks. The
baseline does one comparison per source and nothing else; the bot reads each
source's revision, compares its change tick, reads its declared cache state
before the polls and again after them, and dispatches two systems. Removing the
allocation and the decision pass took the `poll-only-64x100` tick from 1 452 ns
to 908 ns (profile, plain window) and left the observation phase as 412 ns of
it. Going
further means reading fewer things per source per tick, which is a change to what
a source may declare between two reads, not a cost that can be shaved: it is a
semantic decision about the declared-state contract (INV-BOT-120), not an
optimisation, and it is named here rather than made.
