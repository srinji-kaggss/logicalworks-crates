# Closed-loop results and the workload matrix

The scenario table, every §3 matrix row with its receipt, and the committed records. Part of the [async measurement](README.md).

## Results — the closed-loop scenario table

Apple M5 Pro, macOS 27.0, rustc 1.99.0, `opt-level=3`, `lto=true`,
`codegen-units=1`, at `origin/main` 22f1d526 (2026-10-15). **15 paired rounds,
load average 6.86, no CI run live.** One host, one run.

```sh
cargo build --release --locked --manifest-path bench/async/Cargo.toml
bench/async/target/release/lgwks-bench-async --rounds=15 --alloc-report --json=bench/async/results.json
```

| scenario | tasks | bound | facade p50 | facade p99 | base p50 | base p99 | p50 ratio | p99 ratio | 95% CI (paired, p50) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| quiet-async-bot | 256 | 8 | 0.412 ms | 0.565 ms | 0.352 ms | 0.400 ms | 1.17x | 1.41x | [1.09, 1.24] distinguishes |
| high-fanout | 2048 | 32 | 2.260 ms | 2.452 ms | 2.212 ms | 2.395 ms | 1.02x | 1.02x | [0.97, 1.06] spans parity |
| at-capacity | 512 | 4 | 1.182 ms | 1.529 ms | 1.116 ms | 1.402 ms | 1.06x | 1.09x | [1.04, 1.13] distinguishes |
| single-permit | 512 | 1 | 3.848 ms | 4.019 ms | 3.600 ms | 4.339 ms | 1.07x | 0.93x | [1.06, 1.08] distinguishes |

The p99 target of ≤ 2x raw Tokio holds on every row. A second run of the same
binary a minute later (it wrote no record) read p50 1.26x / 1.00x / 1.15x / 1.06x
and p99 1.05x / 1.01x / 1.12x / 1.05x, so the run-to-run spread on this host is
about 0.1x at p50.

Bare synchronous floor (no scheduler): 7 µs for 4,096 counter bumps. It is a
reference line and is **never** multiplied into the ratio above.

Peak RSS for the whole process: **4,308,992 bytes** (`/usr/bin/time -l`, maximum
resident set size, on that second run; peak memory footprint 2,621,800 bytes).

Allocation model, 1,024 tasks at bound 8, counted in a window separate from every
timed round: facade **3,544 allocations / 933,352 bytes (3.46 per task)**, baseline
2,066 / 320,992 (2.02 per task).

**The closed-loop table used to read 1.36x–4.49x, and the reason it no longer
does is the finding.** The drain polled `reap()` behind a `tokio::time::sleep`,
so every drain paid Tokio's 1 ms timer granularity, which #320's commit message
names as the whole of the 3–5x gap. A fixed cost per drain weighs most on the
smallest workload: `quiet-async-bot` (a ~0.35 ms baseline) read 4.49x and
`high-fanout` (a ~2.2 ms baseline) 1.53x. `Supervisor::wait_idle` joins on the
`JoinSet` wakeup instead (#320, `b1b1a3434`), and the same four scenarios now sit
at 1.02x–1.17x at p50. What remains on `quiet-async-bot` is the per-task
bookkeeping, itemised with its guarantee in [allocations.md](allocations.md).
`single-permit`'s p99 read below its baseline's on the committed run (0.93x) and
above it on the second (1.05x). The paired interval is on p50 only, so each p99
figure is a point reading, not an interval.

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
