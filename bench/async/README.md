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
refused, as required: completed: facade 509 vs baseline 512
the refusal names a work-count field, so the gate discriminates on work

mutant tally:  placed 512 completed 509 cancelled 0 aborted 0 work_units 512
honest tally:  placed 512 completed 512 cancelled 0 aborted 0 work_units 512
```

`completed` is the field that fired here, not `work_units`, because the body
bumps its counter before it yields and most bodies were still sitting in that
yield when the early reap landed. A slower host would diverge on `work_units`
instead, which is why the check accepts any work-count field and prints which
one fired rather than pinning one. The mutant is deliberately *unfair* and not
merely slow: a slower-but-identical mutant would pass the gate and print a
meaningless ratio, which is the failure this control exists to rule out.

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