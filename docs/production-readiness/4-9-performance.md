# 4.9 Performance — fastest correct implementation

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — measured honestly on both paths: the synchronous tick loses by 26x to
2 738x, and the async facade loses by 2% to 17% at p50, with one scenario's
interval spanning parity.**

Full method, raw numbers, fairness gate and exclusions are in
[`../bench/README.md`](../../bench/README.md). The summary, with the loss stated at
the same volume as the wins (one run of `bench/` on this host on 2026-10-20, load
average 2.02 to 2.27 across the three doors, Rust 1.99.0; the last column is the
run on main `940e253cd` just before #279's changes):

| scenario | `lgwks_bot` ns/tick | hand-rolled loop ns/tick | ratio | 95% CI | before #279 |
|---|---:|---:|---:|---|---:|
| `poll-only-64x100` | 834.3 | 32.4 | 25.61x slower | [25.37, 26.11] | 47.22x |
| `steady-64x100` | 2 000.9 | 31.4 | 64.20x slower | [61.36, 65.11] | 83.09x |
| `churn-64x1` | 136 810.7 | 49.9 | 2 738.39x slower | [2 696.48, 2 766.73] | 2 698.16x |
| `fanout-1x64` | 112 783.5 | 42.3 | 2 652.57x slower | [2 643.48, 2 668.72] | 2 553.41x |
| `wide-256x10` | 54 662.7 | 130.1 | 414.36x slower | [401.03, 423.36] | 429.32x |
| `wide-1024x10` | 238 705.6 | 530.6 | 451.45x slower | [440.97, 458.55] | — |
| `wide-4096x10` | 1 167 018.7 | 2 053.1 | 568.06x slower | [550.42, 575.49] | — |
| `wide-16384x10` | 8 471 299.9 | 8 490.6 | 996.37x slower | [988.15, 1 004.88] | — |

The interval is the rig's own: a bootstrap over the per-round paired ratios, so
it bounds the ratio and not the ns/tick column. The four `wide-*x10` rows hold the
change-tick fraction at one in ten and vary only the source count, and the log-log
least squares over them reads **slope 1.206** — 2.31x cost per doubling of the
source count — printed by the same run as `wide scaling`.

A hand-rolled loop doing provably identical work is **26x to 2 706x faster**.
If your workload is a hot path, use the loop. This document says so with a
number rather than burying it. #279's targets (≤ 10x quiet, ≤ 30x churn) are
**missed** on every scenario. The previous run of the same binary read 29.47x,
66.61x, 2 691.57x, 2 569.68x and 409.54x, so the run-to-run spread is about two
to three points on the quiet rows and wider than any bootstrap interval; the
effect-heavy rows did not move at all (no change here touches the effect path).

**What changed, and what it bought.** Three things, all on the quiet tick. A
thread's `block_on` keeps one waker instead of building one per call; the
condition failures of a tick are recorded only for the chains that met one; and a
tick on which no source moved, no transition is held and nothing is parked skips
the observation fold and the decision pass rather than walking every chain to
confirm that nothing happened. **A quiet tick now makes no heap allocation**
(`--alloc-report`, per-tick trace of `steady-64x100`: `98:0 99:0 100:558 101:0
102:0`), down from one — `block_on`'s waker — and from 20 before per-source
change ticks. The 558 on the tick where 64 sources move is the boxed
observation, the ledger transition and the journal records, about 8.7 per moved
chain. Means: 4.3 (`poll-only`), 8.8 (`steady`), 878.1 (`churn`), 345.1
(`fanout`), and 346.1 / 1 404.9 / 5 624.9 / 22 483.2 for the wide rows — linear
in the source count (4.06x, 4.00x, 4.00x per four times the sources) — against
0.0 for the baseline.
A fourth thing, timed against the code it replaced in one binary: a tick that
polled nothing, holds no transition and is not parked settles in place and reads
each source's declared cache state once — `poll-only` 985.4 ns/tick on the old
path against 849.9 on the new (13.8%), `steady` 2 132.3 against 2 004.8 (6.0%)
([`../bench/bot-headline.md`](../../bench/bot-headline.md)).

**The gap, itemised by stage** (`--profile`, same binary; ns per tick, and the
instrument adds about 212 ns). `poll-only` (plain tick 860.3): poll 411 (51.0%),
schedule 222 (27.5%), the other four 141 together. `steady` (plain 2 012.9):
poll 398 (20.7%), schedule 218 (11.4%), fingerprint 76, compare 38, decide 64,
act 1 123 (58.5%). `churn`, `fanout` and `wide-256`: act is 83.9%, 98.7% and
79.9%. The ≤ 10x quiet target is not reachable by this route: it asks for a tick
of about 323 ns against a baseline of 32.4 ns, and the poll stage alone is 411
ns, 6.4 ns per source for the `revision()` read, the change-tick compare and one
declared-state read. The ≤ 30x churn target is not reachable by design: each
effect writes a ledger transition, a warrant and a journal record, and `act` is
where the time goes.

These ratios are larger than the 72x–256x this section once carried because
that run's bots fired effects with no ledger, no warrant and no record, and
skipped 99 of every 100 polls through a digest seam that has since been
removed; neither configuration is one the crate builds today
([`../bench/README.md`](../../bench/README.md) states both). **Admission stays
linear; the tick does not.** Building 16,384 chains costs 3.7 ms against 1.1 ms
at 64 chains, but per fired effect `act` reads 2.9, 3.6, 5.1 and 9.7 µs at 256,
1 024, 4 096 and 16 384 sources — 178x for 64x the sources, while poll,
fingerprint, compare, schedule and decide grow 63x to 70x over the same range.
The profile charges that to the effect dispatch; this run does not separate the
ledger write from the journal record inside it. `Auth::check` was quadratic in
the capability count and is now `n·log2 n`: 12.9 µs → 3.6 µs at 128 capabilities.

**Two exclusions that favour this crate and are named so they cannot be
unnoticed later:**

1. The rig withdraws `rt`. It measures the **synchronous** `Bot::tick` adapter.
    The async runtime is measured separately, by `bench/async/`
    ([`../bench/async/README.md`](../../bench/async/README.md)), against pinned raw
    Tokio doing identical work behind a fairness gate that aborts on any
    work-count mismatch (15 paired rounds, Apple M5 Pro, load average 6.86, main
    `22f1d526`, #553; the required `bench-async-fairness` lane runs the gates,
    not these timings):

    | scenario | tasks | bound | facade / raw p50 | 95% CI | facade / raw p99 |
    |---|---:|---:|---:|---|---:|
    | `quiet-async-bot` | 256 | 8 | 1.17x | [1.09, 1.24] | 1.41x |
    | `high-fanout` | 2,048 | 32 | 1.02x | [0.97, 1.06] | 1.02x |
    | `at-capacity` | 512 | 4 | 1.06x | [1.04, 1.13] | 1.09x |
    | `single-permit` | 512 | 1 | 1.07x | [1.06, 1.08] | 0.93x |

    and 3.46 allocations per task against raw Tokio's 2.02 (the attribution run
    itemises 3.61, cut from 15.33 by removing an eagerly-built `watch` channel
    per cancellation token and a 100 ms timer armed on every uncontended spawn;
    the ≤ 2x-at-p99 latency target is met since #320 — see §4.2). The p99
    column is a point reading; the interval is on p50. This is a closed-loop
    measurement at one bound per scenario; the open-loop saturation curve, the
    six-bound knees, the million concurrently admitted tasks, the overload
    recovery and the 2 CPU / 2 GiB cgroup run are §4.2 (#269).
2. The comparator is a hand-rolled loop in the same process. There is no
   measurement against UiPath, n8n, Node-RED or Temporal. Such a measurement
   would be a category error dressed as a result, and this document does not
   substitute a claim for one.

**What the multiplier buys** (each is proved at the design level, see §4.3):
no effect without a grant (T4), replay exactness (T6), a total condition language
that decides at a finite bound (T1, with T7 stating why a loop constructor needs
a budget), and determinism by construction.
