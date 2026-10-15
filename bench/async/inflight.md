# In-flight tiers and the concurrency ladder

That many tasks concurrently admitted, against throughput through 64 slots. Part of the [async measurement](README.md).

## In-flight tiers — that many tasks concurrently admitted

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --inflight --json=bench/async/inflight.json
```

This is the distinction from the concurrency ladder and the point of this row:
each tier's **bound is the tier**, and every body parks on a gate until the whole
tier is admitted, so the peak in-flight count is what the bodies observed rather
than a counter's opinion. Load average **9.70**, 4.81 s wall, peak RSS
2,720,841,728 bytes:

| tier | side | reached | completed | p50 us | p99 us | peak RSS | RSS per in-flight task |
|---:|---|---:|---:|---:|---:|---:|---:|
| 100 | facade | 100 | 100 | 4,841.5 | 4,939.8 | 3,407,872 B | 34,079 B |
| 100 | baseline | 100 | 100 | 5,267.5 | 5,333.0 | 4,734,976 B | 47,350 B |
| 1,000 | facade | 1,000 | 1,000 | 5,808.1 | 6,201.3 | 7,569,408 B | 7,569 B |
| 1,000 | baseline | 1,000 | 1,000 | 5,562.4 | 5,939.2 | 8,257,536 B | 8,258 B |
| 10,000 | facade | 10,000 | 10,000 | 16,302.1 | 16,810.0 | 27,115,520 B | 2,712 B |
| 10,000 | baseline | 10,000 | 10,000 | 12,173.3 | 13,156.4 | 32,669,696 B | 3,267 B |
| 100,000 | facade | 100,000 | 100,000 | 100,794.4 | 113,639.4 | 210,501,632 B | 2,105 B |
| 100,000 | baseline | 100,000 | 100,000 | 71,958.5 | 74,580.0 | 267,206,656 B | 2,672 B |
| 1,048,576 | facade | **1,048,576** | **1,048,576** | 1,007,681.5 | 1,218,445.3 | 2,137,751,552 B | **2,039 B** |
| 1,048,576 | baseline | **1,048,576** | **1,048,576** | 726,663.2 | 745,537.5 | 2,720,727,040 B | 2,595 B |

**The 1,048,576 tier was reached, not clamped** — the host admitted 2^20
concurrently supervised tasks on both sides and every one of them completed,
which is the issue's "1,000,000 in flight, concurrently admitted" with the tier
named as the power of two rather than rounded.

**Peak RSS per in-flight task at that tier: 2,039 bytes for the facade, 2,595 for
the baseline.** The facade is the cheaper of the two per concurrent task, by
21%, and the per-task figure falls monotonically with the tier — 34 KB at 100,
2.1 KB at 100,000, 2.0 KB at 1,048,576 — because the fixed cost of the process is
amortised over more of it. 2.14 GB of resident set for a million simultaneously
live supervised tasks is the number a caller sizing a machine needs, and it is
this host's, on a 24 GB machine.

The latency columns are the honest half of this table and they are unflattering:
at 100,000 concurrent tasks the facade's p50 is **100.8 ms against the baseline's
72.0 ms (1.40x)** and at 1,048,576 it is **1,007.7 ms against 726.7 ms (1.39x)**.
Admitting a million tasks costs the facade about 40% more wall time than the
engine alone, at both the wide tiers and the widest.

## The concurrency ladder — every tier, both sides, bound 64

`--tiers` measures facade and raw baseline at 100, 1,000, 10,000 and 100,000
tasks, **bound 64 on every tier** — that is throughput through 64 slots, *not*
concurrency, which is why the in-flight tiers above exist. The original ladder
read (macOS 27.0, rustc 1.98.0) is retained in `results.json` history and in the
git log; it is superseded for concurrency claims by `--inflight`.

Peak RSS for the whole four-tier process: 3,637,248 bytes. Wall time: 2.57 s.

**Peak RSS is read in-process from `/proc/self/status` on Linux.** macOS has no
in-process equivalent without a `getrusage` edge, so the column is a `ps` sample
of the process's *current* RSS taken at the run's peak, labelled as such in the
record's `rss_source`, and the process peak is measured by the `/usr/bin/time -l`
wrapper. A peak figure reported under the name of the other is a false claim,
and an unavailable one reported as a small one is worse than none.
