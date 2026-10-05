# The measurement rigs

Two instruments live here, each its own Cargo workspace root so neither is seen
by the estate's dependency contract:

- **This file.** The `lgwks_bot` rig, which measures the bot against a
  hand-rolled baseline doing the same work.
- **[`std-measure/`](std-measure/)**, the `lgwks_std` before/after harness for
  issues #153, #154, #160 and #164. It reproduces the #154 G2 table and the
  #164 retry flat-latency rows from raw per-call samples, with its committed
  output in [`std-measure/results.txt`](std-measure/results.txt).

---

# The `lgwks_bot` benchmark rig

This directory measures `lgwks_bot` against a hand-rolled baseline that performs
the *same work*, and reports where the bot is faster, where it is slower, and by
how much. It is a measurement instrument, not a consumer of the crate: it is its
own Cargo workspace root so that the estate's dependency contract never sees it,
and it is not published.

## Two rigs, two claims

| directory | measures | build |
|---|---|---|
| `bench/` (this one) | the **synchronous** ECS schedule against a hand-rolled loop | `lgwks_bot` with `rt` withdrawn — no tokio edge compiled at all |
| `bench/async/` | `lgwks_bot::rt` against **pinned raw Tokio**, matched semantics | `lgwks_bot` with `rt`, plus a raw `tokio` edge the estate never authors |

They are separate roots rather than two binaries here because Cargo features are
per-package and not per-binary: a second binary in this workspace would have
forced the async surface into the build below and made its "no tokio edge"
headline false. Both roots are invisible to `lgwks-deps check`, which discovers
packages through Cargo's own `workspace_members`.

`bench/async/` carries its own fairness gate — it compares terminal counts field
by field and aborts on any mismatch — and its own results, in
[`async/README.md`](async/README.md).

## Running it

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-target \
  cargo run --release --manifest-path bench/Cargo.toml -- --json=bench/results.json
```

`--capcheck-only` skips the timing scenarios. The rig writes a JSON copy of its
results and prints a human table; `results.json` in this directory is the
committed record of the run described below.

### The journals, and why the timing run no longer finishes

Every bot this rig admits dispatches an external effect, and an external effect
requires a journal whose `durability()` meets `ProcessCrash`
(`crates/lgwks-bot/src/ecs.rs`). `MemoryJournal` reports `Ephemeral` and is
refused at the first dispatch, so the rig journals to
`$TMPDIR/lgwks-bench-journal-<pid>/bot-<n>.journal`, one file per bot, and
leaves them there: a journal is the record of what left the process, and a
measurement rig that deleted it would discard the evidence it exists to
produce. Remove the directory yourself between runs.

The consequence is in the runtime, not in the arguments. One append on this
machine's filesystem is a `write` plus an `fsync`, and `--alloc-report` costs
what the run says:

| scenario | effects the report fires | row cost, ms/tick |
|---|---:|---:|
| `poll-only-64x100` | 0 | 0.0 |
| `steady-64x100` | 128 | 3.5 |
| `churn-64x1` | 31 232 | 798.3 |
| `fanout-1x64` | 31 744 | 501.9 |
| `wide-256x10` | 32 256 | 160.4 |

That is about 8 minutes for the counting run, and the timing scenarios repeat
that workload over 24 rounds plus warm-up, so they no longer complete at all.
The cost is proportional to how many effects a tick fires, not to what the
schedule decides, which is the cost of the configuration the crate now requires;
see the section above on what the committed table does and does not cover.

## The headline, stated before the method

**`lgwks_bot` is not state of the art on throughput, and this rig does not claim
it is.** On this machine the bot is between **72x and 256x slower** than a
hand-rolled loop doing provably identical work. That is the honest result, and
the rest of this document is about what that multiplier buys and where it goes.

| scenario | bot ns/tick | baseline ns/tick | ratio (bot/baseline) | 95% CI |
|---|---:|---:|---:|---|
| `poll-only-64x100` | 2 540.0 | 30.9 | 82.57x | [81.22, 83.38] |
| `steady-64x100` | 2 582.1 | 30.8 | 84.03x | [83.78, 84.42] |
| `churn-64x1` | 10 931.6 | 42.6 | 256.42x | [254.26, 257.39] |
| `fanout-1x64` | 2 329.5 | 32.2 | 72.38x | [71.99, 72.51] |
| `wide-256x10` | 12 726.5 | 132.4 | 96.01x | [95.45, 96.33] |

Every interval excludes parity, so each of these is a distinguishable
difference rather than noise. The `churn-64x1` interval is the widest, which is
what a scenario whose cost depends on how much work the scheduler actually
dispatches should look like.

**Machine and toolchain.** Apple M5 Pro, 15 cores, macOS 27.0, rustc 1.99.0,
`opt-level = 3`, `lto = true`, `codegen-units = 1`. One machine, one run: these
are absolute numbers for this host, not a cross-platform claim.

### What these figures are and are not, as of the file-backed journal

**Every number in this document predates a change to the rig's own
configuration, and that change is on the bot's side of the comparison.** An
admitted bot is now refused at assembly unless it is given an effect scope, and
the scope has to carry a journal whose durability promise meets what an external
handoff requires. `MemoryJournal` reports `Ephemeral` and is refused, so the
rig's measured bots dispatch through a `FileJournal` on this machine's disk, and
the write and `fsync` an effect needs are now inside every timed window.

That moves the bot leg by roughly three orders of magnitude on the scenarios
that fire effects, and it moves `churn-64x1` and `wide-256x10` by more than
the runtime of a full timing run on this filesystem. The figures below are
therefore left exactly as measured rather than restated against a configuration
they were not taken on, and read as what they are:

- They are the cost of **the schedule and the four verbs**, measured against an
  in-memory journal — which the crate would now refuse to dispatch an external
  effect through.
- They are **not** the cost of `lgwks_bot` as it has to be configured today,
  which includes a durable record per effect.
- They are **not** the cost of durability either. Nothing here measures what
  `journal::FileJournal` costs per append; the only figure this repository has
  for that is the crate's own note that a four-rung attempt costs four flushes
  at about 3.3 ms each (`crates/lgwks-bot/src/journal/file.rs`).

Nothing in the measured table has been recomputed, because a full timing run
against the file-backed journal does not complete in a usable time on this
machine. Regenerating them is a deliberate future run, not something to
estimate; the wall-clock figure the allocation report prints beside each row
(`--alloc-report`, `row cost`, ms/tick) is the honest cost of a measured window
as the rig stands, and it is not a substitute for a re-measured table.

## Method

### The baseline is the null hypothesis, not a strawman

The comparator is a hand-rolled loop that polls the same sources, detects the
same changes, evaluates the same condition, and performs the same effect. It
holds no ECS world, no ledger, no `Auth` proof, no capability set and no
journal. The question it answers is: *what does this work cost when nothing is
being guaranteed?* Everything above that line is the price of the guarantees —
and the durable journal is now the largest single term in that price.

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
counted, the same scenario measures 72.38x — a factor of 41 of pure artifact,
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
walk**: 2 540.0 ns/tick. `steady-64x100` adds a full
`(condition, action)` entry to every chain: 2 582.1 ns/tick.

Adding the entire decision-and-effect layer to 64 chains costs **42.1 ns/tick —
1.6%** of the total. Roughly **98% of a tick is the poll and change-detection
phase**, before the bot has decided anything. An optimisation aimed at the
condition or the action is aimed at the wrong 1.6%.

### Change detection pays, and the gap widened

`steady-64x100` (one change per 100 ticks) costs 2 582.1 ns/tick.
`churn-64x1` (every source moves every tick) costs 10 931.6 ns/tick. Making the
workload change 100x more often costs **4.23x** more time.

This ratio moved. In the previous record it was 1.95x, because a quiet tick and
a churning tick cost about the same: the poll happened either way and the
decision layer was a small part of the total. Both changes measured in the
section below made a quiet tick cheaper without making a churning one cheaper,
so a workload that never goes quiet now pays about four times what a quiet one
does. The change-detection saving is real and it is bounded, and it is larger
than the phrase "change-triggered" suggested when this document was first
written.

### Cost is linear in sources

`wide-256x10` runs 256 sources at 49.7 ns per source-tick; `steady-64x100` runs
64 sources at 40.3 ns per source-tick. The baseline is likewise flat at ~0.5 ns
per source-tick across 64, 256 and the poll-only shape. There is no superlinear
term in source count to find. The gap between the two bot figures is the fixed
per-tick cost spread over four times as many sources, not a second-order term.

### Per-entry walk cost

`fanout-1x64` walks 64 entries on one source every tick: 2 329.5 ns/tick, or
**~36.4 ns per entry walked** (condition plus action plus ledger). The
`churn-64x1` delta implies ~131.1 ns per walked entry. A decided effect
therefore costs somewhere between 36 and 131 ns depending on how much of the
tick the source moved, against ~0.5 ns for the same decision in the baseline.

### Admission is free

`Bot::builder(..).build(&grants)` admits 64 chains in 0.066–0.43 ms. Capability
admission and schedule validation are not a cost worth optimising.

### Determinism holds, and is exercised

Replaying a 500-tick scenario produces an identical per-tick effect sequence.
This is a *correctness* observation and no timing can make it; the formal
statement and proof are in `../proofs/`, and the claim is that the schedule is a
function of its inputs.

### What the two changes bought, measured against each other

The figures above are one run of the current tree. To attribute them to the two
changes a reader cannot see, the rig was run three times in one session: on the
commit this branch starts from, then on the tip, then on the starting commit
again. The third run is the control, and it is what makes the comparison
usable — this machine's baseline has moved by a factor of two between sessions
before, so two trees are only comparable when they are timed against each other
inside one. Here the baseline legs agreed across all three runs (31 to 43
ns/tick on the 64-source scenarios, 127 to 132 on `wide-256x10`), and the
control's two runs agreed with each other to within 4.3%, 0.3% at best.

The `before` column is the control pair's mean; `after` is the run above. The
paired ratio is the median of `bot_i / baseline_i` within a round, which is the
statistic drift cancels out of.

| scenario | before ns/tick | after ns/tick | bot | paired ratio |
|---|---:|---:|---:|---:|
| `poll-only-64x100` | 6 906.6 | 2 540.0 | **2.72x faster** | 216.98x → 82.57x |
| `steady-64x100` | 5 150.3 | 2 582.1 | **1.99x faster** | 161.54x → 84.03x |
| `churn-64x1` | 10 147.6 | 10 931.6 | unchanged | 252.32x → 256.42x |
| `fanout-1x64` | 2 545.4 | 2 329.5 | **1.09x faster** | 81.80x → 72.38x |
| `wide-256x10` | 20 617.9 | 12 726.5 | **1.62x faster** | 159.06x → 96.01x |

`churn-64x1` is marked unchanged rather than slower, and the paired ratio is why.
Its `ns/tick` rose 7.7%, but the baseline leg rose 6% in the same run, so most of
that is the machine rather than the change. On the paired ratio the move is
252.32x to 256.42x — 1.6% — and the control's own two intervals, [251.39, 257.31]
and [248.56, 253.45], bracket the tip's. The runs are not distinguishable at
that scenario.

The two changes were an end to rebuilding the tick's own scratch state on every
tick, and a source-level digest that lets a chain skip the poll when its source
has not moved. The second is why the scenarios where sources hold still gained
the most, and why `churn-64x1`, where every source moves every tick, did not
gain at all. That is the shape the change is meant to have. A source that
reports no digest is polled exactly as before, so a domain that does not
implement one loses nothing and gains nothing.

All five scenarios passed the fairness gate in all three runs, so the two trees
produced identical effect and condition-evaluation counts.

## A defect this rig found, and the fix it was measured against

**The table below is the pre-fix measurement, kept because it is the evidence
the fix was made on. The measurement of the fixed code follows it.**

| required caps | ns/call | ns per cap |
|---:|---:|---:|
| 1 | 3.40 | 3.40 |
| 8 | 135.59 | 16.95 |
| 32 | 1 029.85 | 32.18 |
| 128 | 12 886.19 | 100.67 |

`Auth::check` iterated the required capabilities and asked, for each, whether a
`Vec<Cap>` contains it, a linear scan inside a linear scan. The cost grew
**3 794x for a 128x increase in capabilities**, and a single check reached
**12.9 microseconds** at 128 capabilities.

The growth was not proportional to the capability count, and at large counts it
fitted a quadratic term closely: at 128 capabilities the measured 12 886 ns is
within 1% of 128² scaled by the ~0.79 ns a single `Cap` string comparison costs
in that loop. The small-count numbers are dominated by fixed call overhead — at
one capability the whole check is 3.40 ns — which is why the headline growth
figure (3 794x) is *below* the 16 384x a pure quadratic would give. The
quadratic term is what governs once the count is large, which is the regime the
shipped configuration never reaches and a custom domain can.

With the four shipped capabilities this was invisible (3.4 ns). It was a real
scalability limit for a custom domain that requires many, and `Cap::new` accepts
any dotted name by convention, so nothing prevented one.

**The fix has since landed.** Capability lists are a set, so the sorted
representation is now the only one: `Auth::new` sorts and de-duplicates at mint
(`crates/lgwks-bot/src/cap.rs:365`), and both the membership question
(`covers_cap`) and the coverage question (`uncovered`) are binary searches over
that sorted slice (`crates/lgwks-bot/src/cap.rs:382`). The product this table
measures moves from `required.len() * granted.len()` to
`required.len() * log2(granted.len())`, paid once per mint rather than once per
check. The derivation is stated on the type itself
(`crates/lgwks-bot/src/cap.rs:345`).

Measured after the fix, in the same rig:

| required caps | ns/call | ns per cap |
|---:|---:|---:|
| 1 | 4.85 | 4.85 |
| 8 | 91.47 | 11.43 |
| 32 | 546.95 | 17.09 |
| 128 | 3 638.08 | 28.42 |

The growth is **750.5x for a 128x increase in capabilities**, against 16 384x
for a pure quadratic and 896x for the `n * log2 n` the fix implements. At 128
capabilities a check fell from 12.9 microseconds to 3.6, a 3.5x improvement, and
the few-capability end is within noise of where it was (4.85 ns against 3.40).
The remaining cost at 128 is the `n * log2 n` term plus the per-call overhead,
so the curve is still rising; a `HashSet` would flatten it to constant and costs
a hash on every mint, which is the trade this crate did not take.

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

## What the multiplier buys

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
tick rate of ~390 000/s is not a binding constraint, and the guarantees are the
reason to choose this crate over a for-loop. For a hot path, the loop wins and
this rig says so with a number.

---

# The `lgwks_ast` parse-budget rig

Per-grammar parse throughput, peak resident set size, and the share of a checked
parse spent in the validation walk, measured against the three grammars' inputs a
hostile file actually carries. Produced by
`crates/lgwks-ast/examples/parse_budget.rs`; the committed evidence is
[`ast-budget.tsv`](ast-budget.tsv), 224 rows.

```sh
cargo run --release -p lgwks_ast --features full --example parse_budget -- --help-ish
# one process per (grammar, shape) and per tier, because peak RSS is a process
# high-water mark and one process over everything could only report the widest:
AST_BUDGET_SHAPE_BYTES=262144 AST_BUDGET_TIMEOUT=120 \
  scripts/measure-ast-budget.sh /tmp/lgwks-ast-budget
```

**Machine and toolchain.** Apple M5 Pro, 15 cores, macOS 27.0, rustc 1.99.0,
release profile. One machine, one run: these are absolute numbers for this host,
not a cross-platform claim. The process floor — the same binary, all 28 grammar
tables linked in, parsing nothing — is **2.1 MiB**, and every "over floor" figure
below is that subtracted, because the binary carries every grammar whether or not
this run used it.

## Four shapes per grammar, at the crate's own byte ceiling

| shape | what it is | why it is a different failure |
|---|---|---|
| `representative` | the grammar's own valid source tiled to 2 MiB | the number a tool sees on real code |
| `nested` | a delimiter pair (or an indentation run) nested as deep as the budget allows | the input `MAX_AST_DEPTH` exists for |
| `longline` | the representative source with its newlines removed | one line of megabytes; the worst parser case in the set |
| `unbalanced` | the nested openers with no closers | forces recovery at the deepest level |

`nested` and `unbalanced` are measured at **256 KiB**, not the 2 MiB ceiling, and
that is a measurement finding rather than a convenience: at 256 KiB of nested
braces the **Dart** grammar takes **97.5 seconds** in the parser, and at 2 MiB it
did not finish in 120 s. The two ceilings are named separately in
`ast-budget.tsv` (`bytes` on every row) so a smaller figure is never read as the
ceiling's. Representative and `longline` are at the full 2 MiB.

## Throughput and peak RSS at 2 MiB, per grammar

`parse` is bare `tree_sitter::Parser::parse`; `checked` is the whole
`try_parse`; `walk share` is the validation walk's share of parse-plus-walk.

| grammar | parse MB/s p50 | p99 | checked MB/s p50 | walk share | peak RSS MiB | over floor |
|---|---:|---:|---:|---:|---:|---:|
| `solidity` | 17 | 17 | 14 | 25% | 100 | 98 |
| `yaml` | 16 | 16 | 15 | 8% | 137 | 136 |
| `rust` | 12 | 11 | 10 | 24% | 198 | 197 |
| `c` | 11 | 10 | 9 | 21% | 220 | 218 |
| `cpp` | 11 | 11 | 9 | 19% | 220 | 218 |
| `css` | 11 | 9 | 8 | 18% | 170 | 169 |
| `go` | 10 | 9 | 8 | 18% | 146 | 144 |
| `python` | 10 | 9 | 8 | 18% | 203 | 202 |
| `typescript` | 9 | 8 | 7 | 22% | 240 | 238 |
| `bash` | 8 | 8 | 7 | 15% | 218 | 216 |
| `csharp` | 8 | 7 | 7 | 18% | 330 | 328 |
| `haskell` | 8 | 7 | 6 | 17% | 301 | 299 |
| `dart` | 7 | 7 | 6 | 19% | 325 | 323 |
| `elixir` | 7 | 6 | 5 | 16% | 277 | 275 |
| `java` | 7 | 6 | 6 | 18% | 196 | 195 |
| `tsx` | 7 | 6 | 6 | 20% | 286 | 285 |
| `nix` | 6 | 5 | 5 | 12% | 114 | 112 |
| `scala` | 6 | 6 | 6 | 16% | 317 | 316 |
| `hcl` | 5 | 4 | 4 | 18% | 319 | 317 |
| `swift` | 5 | 4 | 5 | 20% | 361 | 359 |
| `html` | 4 | 4 | 3 | 10% | 153 | 152 |
| `json` | 4 | 3 | 3 | 37% | 294 | 292 |
| `kotlin` | 4 | 4 | 3 | 20% | 264 | 262 |
| `markdown` | 4 | 4 | 3 | 20% | 460 | 458 |
| `ruby` | 4 | 4 | 3 | 15% | 723 | 721 |
| `javascript` | 3 | 1 | 2 | 14% | 253 | 252 |
| `php` | 3 | 2 | 3 | 10% | 273 | 271 |
| `lua` | 2 | 2 | 2 | 19% | 310 | 309 |

**The documented ceiling: 723 MiB resident, for one parse of a 2 MiB file with the
Ruby grammar.** That is the worst grammar at the crate's own byte ceiling, and it
is what the crate's own ceiling costs in resident memory. `ruby` is 6.4x the
cheapest (`solidity`, 100 MiB) for the same 2 MiB, so a caller cannot size a
parser from the input alone — it has to name the grammar.

**The validation walk is 8%–37% of a checked parse on clean source**, and the
share is a *floor*, not a typical figure: it is measured on the walk the public
`inspect_ast` performs under a node cap, while the checked parse's own walk also
carries the depth cap, so it is cheaper. The walk is the larger share where the
tree is many nodes for few bytes — `json` at 37% is 1.5 M nodes in 2 MiB, while
`yaml` at 8% is a much smaller tree for the same bytes.

## The adversarial shapes, and the finding behind them

Sorted by bare-parse p50. These are the rows where the crate's bounds earn their
keep, and the rows where they do not reach.

| grammar | shape | bytes | parse p50 | walk p50 | walk share | peak RSS |
|---|---|---:|---:|---:|---:|---:|
| `dart` | nested | 262 144 | **97.500 s** | 15.5 ms | 0.0% | 11 MiB |
| `scala` | longline | 501 490 | **29.812 s** | 10.3 ms | 0.0% | 75 MiB |
| `dart` | unbalanced | 131 072 | 24.897 s | 7.8 ms | 0.0% | — (killed at 120 s) |
| `ruby` | longline | 1 647 756 | 5.591 s | 59.0 ms | 1.0% | 692 MiB |
| `html` | nested | 262 141 | 4.484 s | 10.6 ms | 0.2% | 152 MiB |
| `haskell` | longline | 1 997 280 | 1.508 s | 60.1 ms | 3.8% | 310 MiB |
| `go` | longline | 1 880 190 | 1.075 s | 54.5 ms | 4.8% | 226 MiB |

**The parser, not the walk, is the unbounded work on hostile input.** On the three
worst rows the validation walk is 10–15 ms and the parse is 25–97 seconds: the
walk is three to four orders of magnitude cheaper. `dart` at 256 KiB of nested
braces is the extreme measured case — **97.5 seconds for a quarter of the byte
ceiling**, and at the ceiling it did not finish in 120 s. tree-sitter's GLR parser
is super-linear in nesting depth on some grammars. These are the **bare** parse
times, with no deadline. A checked parse (`try_parse`) now stops the parser at
`DEFAULT_PARSE_DEADLINE`, 10 s, and answers `ParseError::TimedOut`; a caller
names a tighter one through `try_parse_within`. The parser checks every hundred
operations, so the thread is released within the deadline plus microseconds
rather than after 97 s (INV-AST-5).

Two rows are recorded as killed rather than finished: `dart` `nested` and
`unbalanced` at 2 MiB, and `scala` `longline` at 2 MiB, all by the rig's own
120-second bound. `scala` `longline` is in the table at **512 KiB**, where it
completes in 29.8 s; the row in `ast-budget.tsv` carries
`note bytes=524288_not_2097152_unfinished_at_120s`. A rig that stopped at the
first of those would have reported 108 rows instead of 112, which is why the
timeout records the partial row rather than dropping it.

## Markdown is refused before the grammar sees it

`nested` and `unbalanced` on the markdown grammar are not slow: they are refused.

| shape | bytes | outcome | peak RSS |
|---|---:|---|---:|
| `representative` | 2 097 147 | accepted | 460 MiB |
| `longline` | 1 828 282 | accepted | 308 MiB |
| `nested` | 262 144 | `container-nesting-too-deep` | 4 MiB |
| `unbalanced` | 262 144 | `container-nesting-too-deep` | 4 MiB |

4 MiB against a 2.1 MiB floor, because nothing was parsed. Before
`MAX_MARKDOWN_CONTAINERS_PER_LINE` existed, those two rows *aborted the process*:
tree-sitter-markdown's external scanner serializes its open block containers into
a fixed 1 024-byte buffer and asserts when they do not fit, and every container
shape that overflows does so at 255 open containers. The full measurement — 17
shapes bisected from a child process, and the guard proved from 1 065 of them —
is in `INVARIANTS.md` under INV-AST-4.

## Bounded fan-out: 100, 1 000, 10 000, 100 000

`try_parse` of a 64 KiB source per parse, over a bounded fan-out. The level's
index range is split into one contiguous slice per worker at admission, so there
is no queue to overflow and no lock to hold, and `thread::scope` joins every
worker. Eight workers on fifteen cores; **the host's cap is fifteen, so eight was
chosen and named rather than assumed**.

| grammar | level | p50 | p99 | max | wall | throughput | peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| `rust` | 100 | 5 625 ns | 181 125 ns | 194 875 ns | 0.0004 s | 15 519 MB/s | 195 MiB |
| `rust` | 1 000 | 2 500 ns | 21 500 ns | 211 541 ns | 0.0009 s | 73 340 MB/s | 195 MiB |
| `rust` | 10 000 | 1 042 ns | 5 000 ns | 84 875 ns | 0.0030 s | 217 045 MB/s | 196 MiB |
| `rust` | 100 000 | 292 ns | 3 125 ns | 718 833 ns | 0.0122 s | 544 714 MB/s | 198 MiB |
| `dart` | 100 | 708 ns | 50 667 ns | 81 666 ns | 0.0004 s | 14 665 MB/s | 327 MiB |
| `dart` | 1 000 | 1 916 ns | 9 833 ns | 581 375 ns | 0.0009 s | 68 744 MB/s | 329 MiB |
| `dart` | 10 000 | 750 ns | 3 625 ns | 263 834 ns | 0.0030 s | 188 787 MB/s | 330 MiB |
| `dart` | 100 000 | 458 ns | 2 000 ns | 4 941 542 ns | 0.0332 s | 200 275 MB/s | 329 MiB |
| `javascript` | 100 | 2 458 ns | 186 833 ns | 189 750 ns | 0.0004 s | 17 495 MB/s | 258 MiB |
| `javascript` | 1 000 | 2 084 ns | 12 125 ns | 194 500 ns | 0.0008 s | 85 925 MB/s | 258 MiB |
| `javascript` | 10 000 | 1 167 ns | 4 333 ns | 182 541 ns | 0.0031 s | 209 961 MB/s | 257 MiB |
| `javascript` | 100 000 | 375 ns | 2 917 ns | 3 333 583 ns | 0.0119 s | 552 395 MB/s | 259 MiB |

All 112 tier runs exited 0 and refused nothing: 64 KiB of each grammar's own
source is inside every ceiling. **Peak RSS is flat in the level** — 195 to 198 MiB
across four orders of magnitude of concurrency for `rust`, because each worker
holds one tree at a time and the trees are 64 KiB sources. That is the number
worth having: the per-parse bound is what makes a fleet of them bounded.

The p99 falls as the level rises because a worker finishing its slice is retired
and the level's per-parse latency is measured over fewer contending threads, not
because the parse got cheaper: throughput rises by 35x from 100 to 100 000 for
`rust`, which is the eight workers being filled rather than any per-parse
improvement. `dart`'s 4.94 ms maximum at 100 000 is a scheduler artifact of eight
workers on a fifteen-core host, not a parse: `dart`'s p99 at the same level is
2.0 us.
