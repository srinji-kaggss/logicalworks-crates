# 4.9 Performance — fastest correct implementation

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — measured honestly on both paths: the synchronous tick loses by 118x to
2 665x, and the async facade loses by 2% to 17% at p50, with one scenario's
interval spanning parity.**

Full method, raw numbers, fairness gate and exclusions are in
[`../bench/README.md`](../../bench/README.md). The summary, with the loss stated at
the same volume as the wins:

| scenario | `lgwks_bot` ns/tick | hand-rolled loop ns/tick | ratio | 95% CI |
|---|---:|---:|---:|---|
| `poll-only-64x100` | 3 720.9 | 31.4 | 117.99x slower | [117.35, 119.05] |
| `steady-64x100` | 4 909.2 | 31.9 | 154.37x slower | [151.59, 156.10] |
| `churn-64x1` | 138 037.7 | 52.1 | 2 664.56x slower | [2 630.82, 2 698.80] |
| `fanout-1x64` | 116 680.7 | 45.2 | 2 578.12x slower | [2 555.76, 2 601.45] |
| `wide-256x10` | 66 270.8 | 138.4 | 477.51x slower | [473.00, 481.37] |

A hand-rolled loop doing provably identical work is **118x to 2 665x faster**.
If your workload is a hot path, use the loop. This document says so with a
number rather than burying it. #279's targets (≤ 10x quiet, ≤ 30x churn) are
**missed** on every scenario.

These ratios are larger than the 72x–256x this section used to carry because
that run's bots fired effects with no ledger, no warrant and no record, and
skipped 99 of every 100 polls through a digest seam that has since been
removed; neither configuration is one the crate builds today
([`../bench/README.md`](../../bench/README.md) states both). The per-stage profile
(`--profile`, measured on the tick path itself) itemises the gap: the effect
path (`act` — ledger, warrant and journal record) is the largest stage in every
scenario, 41% of a quiet tick and 84–98% of the effect-heavy ones; poll is
445 ns on a quiet tick, down from 7 064 ns before per-source change ticks. A
quiet tick still makes 20 heap allocations (`--alloc-report`), flat in the
number of chains. Admission and cost stay linear in source count with no
superlinear term. `Auth::check` was quadratic in the capability count and is now
`n·log2 n`: 12.9 µs → 3.6 µs at 128 capabilities.

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
