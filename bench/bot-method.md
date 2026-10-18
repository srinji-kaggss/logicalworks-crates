# The `lgwks_bot` rig: method

Part of the [measurement rigs](README.md); the results are in
[bot-headline.md](bot-headline.md).

## The journals: what each bot dispatches through, and why

Every bot this rig admits dispatches an effect, and an effect goes through the
ledger, the broker, and a journal. Two of those three are the guarantees the
headline ratio is the price of; the third is a measurement choice, and it is
stated here rather than left for a reader to infer.

The rig's action is a counter increment in its own address space, so it declares
`EffectLifetime::Local` and the crate admits an in-memory journal for it. An
external handoff would instead need a journal whose durability meets
`ProcessCrash`, and this machine's filesystem charges a `write` plus an `fsync`
per append — about 3 ms — which would put a disk round-trip inside every timed
window and make the table a measurement of `FileJournal` rather than of the
schedule. That is a different claim, and it is stated when it is made (see the
journal cost figures in `docs/production-readiness.md` §4.6), not folded in here.

`MemoryJournal` retains at most `MAX_JOURNAL_EVENTS` (100 000) events and
**refuses** the next append rather than dropping evidence, so a workload that
fires millions of effects cannot be measured against one journal at all. Two
consequences, both of them visible in the output:

- **Each round builds its own bot** (and its own baseline) and warms both before
  the timer starts. Admission is therefore outside the timed window on both
  legs, and the retained journal is bounded by one round's workload instead of
  by the whole run. `Scenario::ticks_per_round` is sized to stay inside that
  ceiling; a round that overran it would be a rig that stopped measuring.
- **Every row reports the events its busiest round retained and the ceiling it
  had to stay under**, so no reader is told a number nobody ran.

## Which sources use which change-detection path

#279 asks for change ticks where the source reports its own revision, with the
digest path kept for sources that cannot, and for the record of which uses
which. The seam is `Observe::revision() -> Option<u64>`
(`crates/lgwks-bot/src/spec.rs:499`):

- A source that reports `Some(revision)` takes the **change-tick path**: the
  substrate compares the reported revision against the one it committed with
  the value it holds and **skips the poll entirely** when they agree. The rig's
  own source reports the value `tick / period` as its revision
  (`bench/src/main.rs:119`), so the quiet-tick poll cost in the profile is
  measured on this path.
- A source that reports `None` takes the **value-compare path**: the substrate
  polls, and the source compares the fresh value against the held one inside
  its own poll (`poll_any`), where the output type is still concrete and the
  comparison costs no boxing. That is the path **every shipped source** takes
  today, so a domain that reports no revision is polled exactly as before: it
  loses nothing and gains nothing from the change ticks.

The deprecated `Observe::fingerprint` seam is unused and is neither path: it
was read beside the poll rather than in place of one, which is why it was
removed instead of kept as the digest leg.

## The baseline is the null hypothesis, not a strawman

The comparator is a hand-rolled loop that polls the same sources, detects the
same changes, evaluates the same condition, and performs the same effect. It
holds no ECS world, no ledger, no `Auth` proof, no capability set and no
journal. The question it answers is: *what does this work cost when nothing is
being guaranteed?* Everything above that line is the price of the guarantees —
and the durable journal is now the largest single term in that price.

## The fairness gate, on two axes

The rig **refuses to print a timing** unless the bot and the baseline agree on
both the number of **effects** fired and the number of **condition
evaluations** performed. Both are counted, tick for tick, and a mismatch is a
hard failure that aborts the run.

The second axis exists because of a defect this rig had. The first revision
gated on effects alone, and `fanout-1x64` reported a **2937x** ratio. The cause
was in the baseline: it returned `entries` as an integer rather than looping
over them, so it produced the correct effect count while doing none of the work.
The gate passed, because effect counts were all it looked at. With evaluations
counted, the same scenario measured 72.38x on the pre-durable-record tree — a
factor of 41 of pure artifact, which is what an unguarded benchmark of this
kind produces. It is recorded rather than quietly fixed because the failure mode
is the general one: a benchmark whose only check is "did both sides produce the
same answer" cannot detect a comparison that is unfair in the work dimension.

## Paired measurement

Within one round the bot is timed and then the baseline is timed on the same
input window, and the unit of analysis is the *ratio* `bot_i / baseline_i` for
round `i`. Machine drift lands on both legs and cancels. The reported statistic
is the median ratio with a 10 000-resample percentile bootstrap 95% confidence
interval (seeded, so the interval is reproducible). The median rather than the
mean because benchmark timings are right-skewed: the tail is scheduler
preemption and nothing else, and the mean reports a number no invocation ever
achieved. One interval covers one run; two runs of one binary differ by more
than their intervals, and the headline says by how much.

## The workload is deterministic by construction

Each source's value is `tick / period` — a pure function of the tick index.
There is no RNG anywhere in the workload, so the effect count is not merely
reproducible but computable in closed form. A random workload can only be
checked for self-consistency; this one can be checked against arithmetic.
