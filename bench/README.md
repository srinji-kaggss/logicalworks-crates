# The `lgwks_bot` benchmark rig

This directory measures `lgwks_bot` against a hand-rolled baseline that performs
the *same work*, and reports where the bot is faster, where it is slower, and by
how much. It is a measurement instrument, not a consumer of the crate: it is its
own Cargo workspace root so that the estate's dependency contract never sees it,
and it is not published.

## Running it

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-target \
  cargo run --release --manifest-path bench/Cargo.toml -- --json=bench/results.json
```

`--capcheck-only` skips the timing scenarios. The rig writes a JSON copy of its
results and prints a human table; `results.json` in this directory is the
committed record of the run described below.

## The headline, stated before the method

**`lgwks_bot` is not state of the art on throughput, and this rig does not claim
it is.** On this machine the bot is between **89x and 230x slower** than a
hand-rolled loop doing provably identical work. That is the honest result, and
the rest of this document is about what the 100x buys and where it goes.

| scenario | bot ns/tick | baseline ns/tick | ratio (bot/baseline) | 95% CI |
|---|---:|---:|---:|---|
| `poll-only-64x100` | 7 548.5 | 65.2 | 115.60x | [111.78, 118.20] |
| `steady-64x100` | 7 720.1 | 65.1 | 122.19x | [117.88, 124.06] |
| `churn-64x1` | 15 070.6 | 64.9 | 229.83x | [168.19, 233.73] |
| `fanout-1x64` | 4 390.8 | 49.7 | 88.83x | [87.89, 97.52] |
| `wide-256x10` | 31 293.5 | 252.2 | 119.20x | [118.61, 122.36] |

Every interval excludes parity, so each of these is a distinguishable
difference rather than noise. The `churn-64x1` interval is the widest, which is
what a scenario whose cost depends on how much work the scheduler actually
dispatches should look like.

**Machine and toolchain.** Apple M5 Pro, 15 cores, macOS 27.0, rustc 1.98.0,
`opt-level = 3`, `lto = true`, `codegen-units = 1`. One machine, one run: these
are absolute numbers for this host, not a cross-platform claim.

## Method

### The baseline is the null hypothesis, not a strawman

The comparator is a hand-rolled loop that polls the same sources, detects the
same changes, evaluates the same condition, and performs the same effect. It
holds no ECS world, no ledger, no `Auth` proof and no capability set. The
question it answers is: *what does this work cost when nothing is being
guaranteed?* Everything above that line is the price of the guarantees.

### The fairness gate, on two axes

The rig **refuses to print a timing** unless the bot and the baseline agree on
both:

- the number of **effects** fired, and
- the number of **condition evaluations** performed.

Both are counted, tick for tick, and a mismatch is a hard failure that aborts
the run.

The second axis exists because of a defect this rig had. The first revision
gated on effects alone, and `fanout-1x64` reported a **2937x** ratio. The cause
was in the baseline: it returned `entries` as an integer rather than looping
over them, so it produced the correct effect count while doing none of the work.
The gate passed, because effect counts were all it looked at. With evaluations
counted, the same scenario measures 88.83x — a factor of 33 of pure artifact,
which is what an unguarded benchmark of this kind produces.

This is recorded rather than quietly fixed because the failure mode is the
general one: a benchmark whose only check is "did both sides produce the same
answer" cannot detect a comparison that is unfair in the work dimension.

### Paired measurement

Within one round the bot is timed and then the baseline is timed on the same
input window, and the unit of analysis is the *ratio* `bot_i / baseline_i` for
round `i`. Machine drift lands on both legs and cancels. The reported statistic
is the median ratio with a 10 000-resample percentile bootstrap 95% confidence
interval (seeded, so the interval is reproducible).

The median rather than the mean because benchmark timings are right-skewed: the
tail is scheduler preemption and nothing else, and the mean reports a number no
invocation ever achieved.

### The workload is deterministic by construction

Each source's value is `tick / period` — a pure function of the tick index.
There is no RNG anywhere in the workload, so the effect count is not merely
reproducible but computable in closed form. A random workload can only be
checked for self-consistency; this one can be checked against arithmetic.

## What the numbers say

### Cost is almost entirely in the poll, not the decision

`poll-only-64x100` polls 64 sources and detects change with **no entries to
walk**: 7 548.5 ns/tick. `steady-64x100` adds a full
`(condition, action)` entry to every chain: 7 720.1 ns/tick.

Adding the entire decision-and-effect layer to 64 chains costs **172 ns/tick —
2.2%** of the total. Roughly **98% of a tick is the poll and change-detection
phase**, before the bot has decided anything. An optimisation aimed at the
condition or the action is aimed at the wrong 2%.

### Change detection pays, but modestly

`steady-64x100` (one change per 100 ticks) costs 7 720.1 ns/tick.
`churn-64x1` (every source moves every tick) costs 15 070.6 ns/tick. Making the
workload change 100x more often costs **1.95x** more time. The change-detection
saving is real and it is bounded — it is not the order of magnitude the phrase
"change-triggered" might suggest, because the poll happens either way.

### Cost is linear in sources

`wide-256x10` runs 256 sources at 122.2 ns per source-tick; `steady-64x100` runs
64 sources at 120.6 ns per source-tick. The baseline is likewise flat at ~1.0 ns
per source-tick across 64, 256 and the poll-only shape. There is no superlinear
term in source count to find.

### Per-entry walk cost

`fanout-1x64` walks 64 entries on one source every tick: 4 390.8 ns/tick, or
**~68.6 ns per entry walked** (condition plus action plus ledger). The
`churn-64x1` delta implies ~117 ns per walked entry. So a decided effect costs
on the order of 70–120 ns, against ~1 ns for the same decision in the baseline.

### Admission is free

`Bot::builder(..).build(&grants)` admits 64 chains in 0.07–0.5 ms. Capability
admission and schedule validation are not a cost worth optimising.

### Determinism holds, and is exercised

Replaying a 500-tick scenario produces an identical per-tick effect sequence.
This is a *correctness* observation and no timing can make it; the formal
statement and proof are in `../proofs/`, and the claim is that the schedule is a
function of its inputs.

## A defect this rig found: `Auth::check` was super-linear

**These measurements were taken before the fix recorded at the end of this
section, and have not been re-measured since.** They are kept because they are
the evidence the fix was made on.

| required caps | ns/call | ns per cap |
|---:|---:|---:|
| 1 | 3.40 | 3.40 |
| 8 | 135.59 | 16.95 |
| 32 | 1 029.85 | 32.18 |
| 128 | 12 886.19 | 100.67 |

`Auth::check` iterated the required capabilities and asked, for each, whether a
`Vec<Cap>` contains it — a linear scan inside a linear scan. The cost grew
**3 794x for a 128x increase in capabilities**, and a single check reached
**12.9 microseconds** at 128 capabilities.

The growth was not proportional to the capability count, and at large counts it
fit a quadratic term closely: at 128 capabilities the measured 12 886 ns is
within 1% of 128² scaled by the ~0.79 ns a single `Cap` string comparison costs
in that loop. The small-count numbers are dominated by fixed call overhead — at
one capability the whole check is 3.40 ns — which is why the headline growth
figure (3 794x) is *below* the 16 384x a pure quadratic would give. The
quadratic term is what governs once the count is large, which is the regime the
shipped configuration never reaches and a custom domain can.

With the four shipped capabilities this was invisible (3.4 ns). It was a real
scalability limit for a custom domain that requires many, and `Cap::new` accepts
any dotted name by convention, so nothing prevented one.

**The fix has since landed, and it is the one this section proposed.** Capability
lists are a set, so the sorted representation is now the only one:
`Auth::new` sorts and de-duplicates at mint
(`crates/lgwks-bot/src/cap.rs:365`), and both the membership question
(`covers_cap`) and the coverage question (`uncovered`) are binary searches over
that sorted slice (`crates/lgwks-bot/src/cap.rs:382`). The product the table above
measures therefore moves from `required.len() * granted.len()` to
`required.len() * log2(granted.len())`, paid once per mint rather than once per
check. The derivation is stated on the type itself, at
`crates/lgwks-bot/src/cap.rs:345`.

## What is deliberately excluded

**No raw-`bevy_ecs` comparator.** The bot is built on `bevy_ecs`, so a
raw-bevy comparison looks like the obvious one. It is not: a raw-bevy
reimplementation of this schedule would be *this rig's* code, measuring the
author's fluency with bevy's API rather than the bot's cost. The decomposition
above answers the same question — where does the time go — using the bot's own
code paths and the counted-work gate.

**No CEL / zen-engine / rhai comparator.** Those are real engines, but they are
expression evaluators, and this crate's `Evaluate` verb is a thin wrapper over a
Rust closure: the comparison would measure a closure call against an interpreter
and would say nothing about the schedule, which is where 98% of the cost is.
They are excluded as off-axis, not as unflattering.

**No throughput claim in either direction against Home Assistant, n8n, Node-RED
or Temporal.** Those are separate processes with separate I/O models. An
in-process microbenchmark against them would be a category error dressed as a
result.

## What the 100x buys

The bot is slower than a loop. What the loop does not have:

- **No effect without a grant.** Every side effect passes a capability check,
  and the guarantee is stated formally and proved (`proofs/`, T4).
- **Replay exactness.** The effects that ran can be re-derived from what was
  recorded, and this is proved (T6).
- **A total condition language.** Every condition terminates within a bound
  determined by its own structure, which a language with iteration does not
  have (T1, T7).
- **Determinism by construction**, exercised here and stated formally.

Whether that is a good trade depends on the workload. For an automation bot
whose stated design point is running for weeks — the crate's own framing — a
tick rate of ~130 000/s is not a binding constraint, and the guarantees are the
reason to choose this crate over a for-loop. For a hot path, the loop wins and
this rig says so with a number.
