# Async matched-semantics measurement

`lgwks_bot::rt::supervise::Supervisor` measured against **pinned raw Tokio** doing
the same work, at the same in-flight bound, with the same failure and
cancellation semantics. Part of #152.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --rounds=11 --alloc-report --json=bench/async/results.json
```

`--rounds=<n>` sets the paired-round count (default 15). `--alloc-report` runs a
separate, counted window; it is never mixed into a timed round.

## Its own workspace root, and why

The comparison needs a raw `tokio` edge the four published crates must never
author. `lgwks-deps check` discovers packages through Cargo's own
`workspace_members`, so a separate root is invisible to the gate — the same
arrangement `bench/` uses.

It is a *separate* root from `bench/`, not a second binary in it, because
`bench/` deliberately builds `lgwks_bot` with `rt` withdrawn so its measured path
is exactly the ECS schedule. Cargo features are per-package and not per-binary,
so a second binary there would have forced the async surface into that build and
made its headline "no tokio edge is compiled" claim false.

## The fairness gate, and what it caught

Every round compares the two sides' **terminal counts field by field**: tasks
placed, tasks that completed, cancelled, aborted, spawns refused, and the sum of
each body's own work counter. Any difference aborts the run and prints the field
that diverged. A ratio computed on unequal work is not a slower engine, it is a
different program.

Two real asymmetries were found and fixed *in the harness*, not in the crate:

- **`Supervisor` reports a task cancelled if the token was cancelled when the
  body returned**, even if the body finished its work first. That reading is
  correct and conservative. But `shutdown` cancels first, so draining through
  `shutdown` measures "who cancelled" rather than "who completed". The facade
  side is therefore drained with its token intact (`reap` until the completion
  counter reaches the placed count) — the drain a caller who cares about
  completion performs.
- **`Supervisor` retains terminal outcomes in a buffer capped at the in-flight
  bound** and counts the loss in `reports_dropped`. The baseline has no
  reporting layer and so bounds it at nothing. Comparing retained-list lengths
  would compare reporting granularity, not execution. The retention asymmetry is
  **reported separately**, as the asymmetry it is, and is not part of the gate.

The gate is not decoration: it refused four times during development, each time on
a real difference between the two sides.

## The §3 workload matrix — every row, with its receipt

`--matrix` drives each row lgwks_bot can actually drive today and records what it
produced. A row that fails aborts the run with the row named, because a matrix
with one quietly-skipped row reads as coverage.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --matrix --json=bench/async/matrix.json
```

Recorded output, Apple M5 Pro, macOS 27.0, rustc 1.98.0:

| row | shape | placed | completed | cancelled | aborted | work units |
|---|---|---:|---:|---:|---:|---:|
| sequential-composition | 64 bodies, bound 1, nothing overlaps | 64 | 64 | 0 | 0 | 64 |
| high-fanout-slow-consumer | 1024 tasks, 4 KiB each, bound 32, 8-slot channel | 1024 | 1024 | 0 | 0 | 1024 |
| cancel-at-saturation | 64 bodies fill the ceiling, cancelled while full | 64 | 0 | 64 | 0 | 64 |
| two-tenants | 2048 tasks each, bound 32 each, run together | 4096 | 4096 | 0 | 0 | 4096 |
| sustained-burst-reconnect | 512 sustained, 1024 burst, 128 reconnect, bound 16 | 1664 | 1664 | 0 | 0 | 1664 |
| durable-history | 1000 records through `FileJournal` | 1000 | 1000 | 0 | 0 | 1000 |
| durable-history | 10000 records through `FileJournal` | 10000 | 10000 | 0 | 0 | 10000 |
| durable-history | 100000 records through `FileJournal` | 100000 | 100000 | 0 | 0 | 100000 |

Three of these rows assert more than their receipt, and the receipt alone would
not show what they check:

- **`cancel-at-saturation`** fills the ceiling with bodies that end *only* when
  cancelled, cancels while every permit is held, and then asserts that
  `in_flight()` returned to zero, that the counts partition, that every parked
  body reported cancellation, and that a *spent* supervisor refuses later
  admissions. The receipt shows 64 cancelled and 0 completed; the assertions are
  what make that a cleanup observation rather than a tally.
- **`high-fanout-slow-consumer`** runs 1,024 producers through an 8-slot channel
  into a deliberately slow consumer. The row fails unless the consumer received
  exactly 1,024 payloads *and* exactly 4 MiB, so a lost or duplicated result is
  caught rather than averaged away.
- **`two-tenants`** runs two supervisors concurrently and fails if either tenant
  ends with a non-zero `in_flight`, a refused spawn, or a work-unit count that
  is not its own — a shared permit pool or a shared counter would show up there.

**Rows that cannot be driven are named rather than guessed.** The issue's matrix
also asks for fixed-model AI authoring, which needs a model credential this
machine does not have; that row is absent from the table above rather than
filled with a number nobody measured. `bench/async/results.json` carries the
ladder and the scenario table; `bench/async/matrix.json` carries these receipts.

## The concurrency ladder — every tier, both sides

`--tiers` measures facade and raw baseline at 100, 1,000, 10,000 and 100,000
tasks, bound 64 on every tier, 5 paired rounds each, gated on identical work
*per round*.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  /usr/bin/time -l cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --tiers --json=bench/async/results.json
```

Apple M5 Pro, macOS 27.0, rustc 1.98.0, `opt-level=3`, `lto=true`. Seconds.

| tasks | facade p50 | facade p95 | facade p99 | baseline p50 | baseline p95 | baseline p99 |
|---:|---:|---:|---:|---:|---:|---:|
| 100 | 0.001796 | 0.001881 | 0.001893 | 0.000123 | 0.000321 | 0.000356 |
| 1,000 | 0.003482 | 0.003910 | 0.003943 | 0.001223 | 0.001300 | 0.001311 |
| 10,000 | 0.023004 | 0.028101 | 0.028219 | 0.013128 | 0.016464 | 0.016890 |
| 100,000 | 0.231890 | 0.299734 | 0.303659 | 0.134186 | 0.158088 | 0.160293 |

**Peak RSS for the whole four-tier process: 3,637,248 bytes** (`/usr/bin/time -l`
maximum resident set size). Wall time for the ladder: **2.57 s**.

**Ceiling reached: 100,000 tasks, the top tier the contract names.** The host
drove every tier to completion with `work_units` matching on both sides at every
tier, so nothing here is extrapolated and no figure is carried over from another
platform. The issue's ">1M" is above what this host sustains in a gate lane; the
level actually reached is 100,000 and it is named rather than scaled up.

Peak RSS is read in-process from `/proc/self/status` on Linux. macOS has no
in-process equivalent without a `getrusage` edge, so the column is reported as
`null` there and measured by the `/usr/bin/time -l` wrapper above — a peak-RSS
cell a host cannot fill is worse than none, because an empty cell reads as
"small".

## The mutant baseline — proving the gate can refuse

A gate that has only ever been shown agreeing runs cannot be told apart from one
that always says yes. `--mutant-check` runs the negative control: a side that
places every task exactly as the honest facade does and then **stops draining**,
so the tally it hands the gate claims 512 placed tasks while fewer bodies were
observed to finish. The gate must refuse it, and must refuse it *for a
work-count reason*.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- --mutant-check
```

Recorded output, Apple M5 Pro, macOS 27.0, rustc 1.98.0, 512 tasks at bound 8:

```
refused, as required: completed: facade 505 vs baseline 512
the refusal names a work-count field, so the gate discriminates on work

mutant tally:  placed 512 completed 505 cancelled 0 aborted 0 work_units 509
honest tally:  placed 512 completed 512 cancelled 0 aborted 0 work_units 512
```

The mutant is the honest facade body with one parameter flipped — `Drain::GiveUp`
instead of `Drain::Complete` — so the defect is readable in a diff of one line
rather than by comparing two functions. Both fields diverge here: `completed`
505 against 512, and `work_units` 509 against 512. Which one the gate *names*
depends on the order it checks them and on how far the workers got before the
early reap, so the check accepts any work-count field and prints which one fired
rather than pinning one; a slower host would name `work_units` first.

The mutant is deliberately *unfair* and not merely slow: a slower-but-identical
mutant would pass the gate and print a meaningless ratio, which is the failure
this control exists to rule out.

The mutant check runs alone and exits non-zero if the gate ever accepts it, so a
regression in the gate itself cannot pass quietly.

## Results

Apple M5 Pro, macOS 27.0, rustc 1.98.0, `opt-level=3`, `lto=true`,
`codegen-units=1`. 11 paired rounds. One host, one run: **absolute numbers, not a
cross-platform claim.**

| scenario | tasks | bound | facade p50 | facade p99 | baseline p50 | baseline p99 | ratio | 95% CI (paired) |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| quiet-async-bot | 256 | 8 | 2.130 ms | 2.763 ms | 0.406 ms | 1.254 ms | 5.25x | [3.33, 5.62] distinguishes |
| high-fanout | 2048 | 32 | 5.461 ms | 7.594 ms | 2.472 ms | 3.439 ms | 2.21x | [1.98, 2.26] distinguishes |
| at-capacity | 512 | 4 | 3.225 ms | 3.824 ms | 1.304 ms | 2.958 ms | 2.47x | [1.97, 2.94] distinguishes |
| single-permit | 512 | 1 | 6.517 ms | 10.574 ms | 4.743 ms | 7.047 ms | 1.37x | [1.28, 1.63] distinguishes |

Bare synchronous floor (no scheduler): 10 µs for 4096 counter bumps. It is a
reference line and is **never** multiplied into the ratio above.

Peak RSS for the whole process: **3.44 MB** (`/usr/bin/time -l`).

Allocation model, 1024 tasks at bound 8, counted in a window separate from every
timed round:

| side | allocations | bytes | per task |
|---|---:|---:|---:|
| facade | 15,702 | 2,019,184 | 15.33 |
| baseline | 2,064 | 320,800 | 2.02 |

## What this says, stated before the reader draws a conclusion

**The facade costs between 1.4× and 5.3× a hand-written raw equivalent, and the
ratio is a cost decomposition, not a verdict.** `Supervisor` maintains a permit
pool, an identity map, a bounded report buffer and a cancellation token per task;
the baseline here does the minimum that preserves the semantics. The facade's
cost *is* the accounting, and the accounting is the thing being compared.

**The spread is itself a result and is not smoothed away.** The quiet-bot row
(5.25×) is the smallest workload, where the facade's fixed per-spawn work
dominates; the fan-out row (2.21×) amortizes it over 2048 tasks. Reporting one
number would hide the fact that the facade's overhead is a *fixed* cost per task,
which is the actionable half of the measurement.

**`single-permit` is the row to watch, and it moved.** On an earlier 11-round run
this row's interval was **[0.97, 1.23] — spanning parity**; on this run it is
[1.28, 1.63]. Both are published because the row is genuinely at the edge of what
this host can separate: with one permit there is no concurrency for the facade to
account for, so the difference that remains is per-task bookkeeping competing with
scheduler jitter of comparable size. A measurement at that ratio with an interval
that straddles 1.0 across runs is the honest description, and a single run's
verdict on it would not be.

## What this does not claim

- No third engine is in this comparison. It is a cost decomposition of a facade
  against its own baseline, **not** a leaderboard and not a claim about any other
  scheduler.
- The absolute numbers are this host, this toolchain, this run.
- Allocation counts are dominated by the facade's per-task accounting structures
  and are reported as counts, not as a verdict. `realloc` is counted at its *new*
  size, so a growing workload is overstated; that approximation is stated in
  `bench/src/alloc_count.rs` rather than left inside a number.
- Nothing here measures durability, restart cost, or history scaling. Those are
  different layers (the issue names them separately) and a number from this rig
  would not speak to them.
- **No concurrency above 2048 tasks was run.** The issue's ">1M concurrent"
  target is not reached here and no extrapolation from 2048 is offered. See the
  NOT DONE list in the delivery report.

## What this does not claim

- No third engine is in this comparison. It is a cost decomposition of a facade
  against its own baseline, **not** a leaderboard and not a claim about any other
  scheduler.
- The absolute numbers are this host, this toolchain, this run.
- Allocation counts are dominated by the facade's per-task accounting structures
  and are reported as counts, not as a verdict.
- Nothing here measures durability, restart cost, or history scaling. Those are
  different layers (the issue names them separately) and a number from this rig
  would not speak to them.

## The raw record

`results.json` in this directory is the committed output of the run above, with
every trial's p50/p99, the paired ratio, the bootstrap interval, and the
correctness counts the gate verified. A run that had *failed* the gate would have
written no file at all — the abort happens before the write.