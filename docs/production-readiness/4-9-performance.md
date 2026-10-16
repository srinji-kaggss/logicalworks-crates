# 4.9 Performance — fastest correct implementation

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — measured honestly on both paths: the synchronous tick loses by 28x to
2 605x, and the async facade loses by 2% to 17% at p50, with one scenario's
interval spanning parity.**

Full method, raw numbers, fairness gate and exclusions are in
[`../bench/README.md`](../../bench/README.md). The summary, with the loss stated at
the same volume as the wins (one run of `bench/` on this host with #279's
changes; the last column is the run on main `940e253cd` just before them):

| scenario | `lgwks_bot` ns/tick | hand-rolled loop ns/tick | ratio | 95% CI | before #279 |
|---|---:|---:|---:|---|---:|
| `poll-only-64x100` | 896.0 | 32.3 | 27.58x slower | [27.40, 28.05] | 47.22x |
| `steady-64x100` | 2 055.4 | 32.4 | 63.65x slower | [63.09, 64.20] | 83.09x |
| `churn-64x1` | 133 986.7 | 51.5 | 2 605.27x slower | [2 547.15, 2 662.27] | 2 698.16x |
| `fanout-1x64` | 110 259.8 | 42.7 | 2 574.46x slower | [2 527.44, 2 588.59] | 2 553.41x |
| `wide-256x10` | 54 248.3 | 135.6 | 401.05x slower | [397.17, 410.04] | 429.32x |

A hand-rolled loop doing provably identical work is **28x to 2 605x faster**.
If your workload is a hot path, use the loop. This document says so with a
number rather than burying it. #279's targets (≤ 10x quiet, ≤ 30x churn) are
**missed** on every scenario. A second run of the same binary read 29.47x,
66.61x, 2 691.57x, 2 569.68x and 409.54x, so the run-to-run spread is about two
to three points on the quiet rows and wider than any bootstrap interval; the
quiet rows moved by far more than that, and the effect-heavy rows did not move
(this change does not touch the effect path).

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
chain. Means: 4.2 (`poll-only`), 8.0 (`steady`), 878.0 (`churn`), 345.0
(`fanout`), 347.4 (`wide`) allocations per tick, against 0.0 for the baseline.

**The gap, itemised by stage** (`--profile`, same run; ns per tick, which
includes the one tick in a hundred that moves, and the instrument adds about
209 ns). `poll-only` (plain tick 907.8): poll 412 (47.7%), schedule 282 (32.6%),
the other four 168 together. `steady` (plain 2 075.3): poll 401 (20.3%),
schedule 278 (14.1%), fingerprint 79, compare 36, decide 60, act 1 125 (56.8%).
`churn`, `fanout` and `wide`: act is 83.9%, 98.7% and 79.7%. The ≤ 10x quiet
target is not reachable by this route: it asks for a tick of about 323 ns
against a baseline of 32.3 ns, and the poll stage alone is 412 ns, 6.4 ns per
source for the `revision()` read, the change-tick compare and two declared-state
reads. The ≤ 30x churn target is not reachable by design: each effect writes a
ledger transition, a warrant and a journal record, and `act` is where the time
goes.

These ratios are larger than the 72x–256x this section once carried because
that run's bots fired effects with no ledger, no warrant and no record, and
skipped 99 of every 100 polls through a digest seam that has since been
removed; neither configuration is one the crate builds today
([`../bench/README.md`](../../bench/README.md) states both). Admission and cost
stay linear in source count with no superlinear term. `Auth::check` was
quadratic in the capability count and is now `n·log2 n`: 12.9 µs → 3.6 µs at
128 capabilities.

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
