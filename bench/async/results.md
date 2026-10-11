# Closed-loop results and the workload matrix

The scenario table, every §3 matrix row with its receipt, and the committed records. Part of the [async measurement](README.md).

## Results — the closed-loop scenario table

Apple M5 Pro, macOS 27.0, rustc 1.99.0, `opt-level=3`, `lto=true`,
`codegen-units=1`. **15 paired rounds, load average 9.33.** One host, one run.

| scenario | tasks | bound | facade p50 | facade p99 | base p50 | base p99 | ratio | 95% CI (paired) |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| quiet-async-bot | 256 | 8 | 1.692 ms | 1.859 ms | 0.377 ms | 0.771 ms | 4.49x | [2.41, 5.03] distinguishes |
| high-fanout | 2048 | 32 | 4.041 ms | 4.689 ms | 2.646 ms | 2.910 ms | 1.53x | [1.44, 1.58] distinguishes |
| at-capacity | 512 | 4 | 2.891 ms | 3.886 ms | 1.486 ms | 1.918 ms | 1.95x | [1.86, 2.31] distinguishes |
| single-permit | 512 | 1 | 6.560 ms | 8.415 ms | 4.820 ms | 6.806 ms | 1.36x | [1.25, 1.40] distinguishes |

Bare synchronous floor (no scheduler): 7 µs for 4,096 counter bumps. It is a
reference line and is **never** multiplied into the ratio above.

Peak RSS for the whole process: **3,850,240 bytes**.

Allocation model, 1,024 tasks at bound 8, counted in a window separate from every
timed round: facade **3,798 allocations / 977,528 bytes (3.71 per task)**, baseline
2,064 / 320,800 (2.02 per task).

**The spread is itself a result and is not smoothed away.** `quiet-async-bot`
(4.49x) is the smallest workload, where the facade's fixed per-spawn work
dominates; `high-fanout` (1.53x) amortizes it over 2,048 tasks. `single-permit`
is the row to watch: with one permit there is no concurrency for the facade to
account for, so what remains is per-task bookkeeping competing with scheduler
jitter of comparable size, and it sits at 1.36x with a 95% interval that
distinguishes parity on every run here.

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
scenario table; `bench/async/matrix.json` these receipts.

## The raw record

`results.json`, `saturation-backpressure.json`, `saturation-refuse.json`,
`saturation-workers2.json`, `inflight.json`, `overload.json` and
`alloc-attribution.json` in this directory are the committed output of the runs
these documents report, with every point's p50/p95/p99, the admission and refusal counts, the
queue-depth peak and mean, the offered and achieved rates, the peak RSS and its
source, and the correctness counts the gate verified. A run that had *failed* a
gate would have written no file at all — the abort happens before the write.
