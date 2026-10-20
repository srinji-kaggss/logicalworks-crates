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
than a counter's opinion. Each row's `ceiling` is read back from its engine
(#639); records before it carry `Semaphore::MAX_PERMITS` under that name. Re-run
2026-10-20: load average **3.86**, 3.94 s wall, largest RSS 2,659,598,336 bytes:

| tier | side | reached | completed | p50 us | p99 us | peak RSS | RSS per in-flight task |
|---:|---|---:|---:|---:|---:|---:|---:|
| 100 | facade | 100 | 100 | 3,518.5 | 3,584.0 | 3,801,088 B | 38,011 B |
| 100 | baseline | 100 | 100 | 4,141.1 | 4,202.5 | 5,062,656 B | 50,627 B |
| 1,000 | facade | 1,000 | 1,000 | 4,857.9 | 5,398.5 | 6,848,512 B | 6,849 B |
| 1,000 | baseline | 1,000 | 1,000 | 4,694.0 | 5,185.5 | 7,487,488 B | 7,487 B |
| 10,000 | facade | 10,000 | 10,000 | 14,663.7 | 14,893.1 | 25,247,744 B | 2,525 B |
| 10,000 | baseline | 10,000 | 10,000 | 11,288.6 | 12,140.5 | 30,261,248 B | 3,026 B |
| 100,000 | facade | 100,000 | 100,000 | 97,648.6 | 103,153.7 | 208,109,568 B | 2,081 B |
| 100,000 | baseline | 100,000 | 100,000 | 68,812.8 | 69,861.4 | 259,325,952 B | 2,593 B |
| 1,048,576 | facade | **1,048,576** | **1,048,576** | 923,795.5 | 1,068,498.9 | 2,129,149,952 B | **2,031 B** |
| 1,048,576 | baseline | **1,048,576** | **1,048,576** | 672,137.2 | 691,011.6 | 2,659,598,336 B | 2,536 B |

**The 1,048,576 tier was reached, not clamped** — the host admitted 2^20
concurrently supervised tasks on both sides and every one of them completed,
which is the issue's "1,000,000 in flight, concurrently admitted" with the tier
named as the power of two rather than rounded.

**Peak RSS per in-flight task at that tier: 2,031 bytes for the facade, 2,536 for
the baseline.** The facade is the cheaper of the two per concurrent task, by
20%, and the per-task figure falls monotonically with the tier — 38 KB at 100,
2.1 KB at 100,000, 2.0 KB at 1,048,576 — because the fixed cost of the process is
amortised over more of it. 2.13 GB of resident set for a million simultaneously
live supervised tasks is the number a caller sizing a machine needs, and it is
this host's, on a 24 GB machine.

The latency columns are the honest half of this table and they are unflattering:
at 100,000 concurrent tasks the facade's p50 is **97.6 ms against the baseline's
68.8 ms (1.42x)** and at 1,048,576 it is **923.8 ms against 672.1 ms (1.37x)**.
Admitting a million tasks costs the facade about 40% more wall time than the
engine alone, at both the wide tiers and the widest.

## The concurrency ladder — every tier, both sides, bound 64

`--tiers` measures facade and raw baseline at 100, 1,000, 10,000 and 100,000
tasks, **bound 64 on every tier** — that is throughput through 64 slots, *not*
concurrency, which is why the in-flight tiers above exist. The original ladder
read (macOS 27.0, rustc 1.98.0) is retained in `results.json` history and in the
git log; it is superseded for concurrency claims by `--inflight`.

Peak RSS for the whole four-tier process: 3,637,248 bytes. Wall time: 2.57 s.

**Peak RSS is read in-process from `/proc/self/status` (`VmHWM`) on Linux.** macOS
has no in-process equivalent without a `getrusage` edge, so the figure is a `ps`
sample of the process's *current* RSS: the in-flight tiers take it while the
whole tier is parked, and the ladder after each tier ends and keeps the largest,
which is a lower bound on the peak and labelled as one. Both records carry
`rss_source`, so a sample is never reported under the name of a high-water mark.
The macOS peak is checked by the `/usr/bin/time -l` wrapper: on 2026-10-20 the
ladder's sample read 4,177,920 bytes against 4,325,376 maximum resident set size.
