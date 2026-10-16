# The `lgwks_bot` rig: the headline, stated before the method

Part of the [measurement rigs](README.md). Method is in [bot-method.md](bot-method.md),
the per-stage profile in [bot-profile.md](bot-profile.md).

**`lgwks_bot` is not state of the art on throughput, and this rig does not claim
it is.** On this machine the bot is between **28x and 2 605x slower** than a
hand-rolled loop doing provably identical work. That is the honest result, and
the rest of these pages are about what that multiplier buys and where it goes.

| scenario | bot ns/tick | baseline ns/tick | ratio (bot/baseline) | 95% CI | before #279 |
|---|---:|---:|---:|---|---:|
| `poll-only-64x100` | 896.0 | 32.3 | 27.58x | [27.40, 28.05] | 47.22x |
| `steady-64x100` | 2 055.4 | 32.4 | 63.65x | [63.09, 64.20] | 83.09x |
| `churn-64x1` | 133 986.7 | 51.5 | 2 605.27x | [2 547.15, 2 662.27] | 2 698.16x |
| `fanout-1x64` | 110 259.8 | 42.7 | 2 574.46x | [2 527.44, 2 588.59] | 2 553.41x |
| `wide-256x10` | 54 248.3 | 135.6 | 401.05x | [397.17, 410.04] | 429.32x |

Every interval excludes parity, so each of these is a distinguishable difference
rather than noise. The `churn-64x1` and `fanout-1x64` intervals are the widest,
which is what a scenario whose cost depends on how many effects the scheduler
dispatches should look like.

**What the last column is.** The run of the same rig on main `940e253cd` a few
minutes before #279's changes were built, on the same machine in one session. A
second run of the post-change binary read 29.47x, 66.61x, 2 691.57x, 2 569.68x
and 409.54x, so the run-to-run spread is about two to three points on the quiet
rows and wider than any one bootstrap interval, which covers a single run. The
quiet rows moved by far more than that. `churn-64x1` and `fanout-1x64` did not
move, because the changes are on the quiet tick and `act` is where those spend
their time. `wide-256x10` moved by 5% to 7%.

**What #279's changes were.** A thread's `block_on` keeps one waker instead of
building one per call; the condition failures of a tick are recorded only for the
chains that met one; and a tick on which no source moved, no transition is held
and nothing is parked skips the observation fold and the decision pass instead of
walking every chain to confirm that nothing happened. Its targets, at most 10x
on a quiet tick and at most 30x on churn, are **missed** on every scenario, and
[the profile](bot-profile.md) itemises why. Its allocation target is met for a
quiet tick: **no heap allocation** (`--alloc-report`).

**Machine and toolchain.** Apple M5 Pro, 15 cores, macOS 27.0, rustc 1.99.0,
`opt-level = 3`, `lto = true`, `codegen-units = 1`. One machine: these are
absolute numbers for this host, not a cross-platform claim.

## These numbers are not the ones this file used to publish, and why

The table above is **larger than the one this file carried before 2026-10-05**,
which reported 72x to 256x. Two things changed, both on the bot's side of the
comparison, and neither is a rig change that flattered either engine:

1. **Every effect now writes a durable record.** An admitted bot is refused at
   assembly unless it is given an effect scope, and the crate will not dispatch an
   external effect through a journal that cannot outlive the process. A bot with
   no effect scope at all — which is what the previous run's rig built, and what
   the crate accepted then — fired effects with no ledger, no broker warrant and
   no record. That configuration is not a bot the crate will build today, so its
   ratios are not this crate's cost.
2. **The source-level digest that let a chain skip its poll was removed.** The
   `Observe::fingerprint` seam is deprecated and unused, and a source's value is
   compared against the substrate's baseline *inside* the poll, after the value
   has been produced. The previous run's rig source implemented `fingerprint`,
   so on 99 of every 100 ticks it skipped the poll entirely.

Both changes make the bot slower, and both are stated here rather than left for a
reader to reconcile. [The profile](bot-profile.md) is what says which of them the
current numbers are made of.

## What the two changes of 2026-10-05 bought, measured against each other

The rig was run three times in one session: on the commit the branch started
from, then on the tip, then on the starting commit again. The third run is the
control — this machine's baseline has moved by a factor of two between sessions
before, so two trees are only comparable when they are timed against each other
inside one. The baseline legs agreed across all three runs (31 to 43 ns/tick on
the 64-source scenarios, 127 to 132 on `wide-256x10`) and the control's two runs
agreed within 4.3%, 0.3% at best. Historical: these are that session's figures.

| scenario | before ns/tick | after ns/tick | bot | paired ratio |
|---|---:|---:|---:|---:|
| `poll-only-64x100` | 6 906.6 | 2 540.0 | **2.72x faster** | 216.98x → 82.57x |
| `steady-64x100` | 5 150.3 | 2 582.1 | **1.99x faster** | 161.54x → 84.03x |
| `churn-64x1` | 10 147.6 | 10 931.6 | unchanged | 252.32x → 256.42x |
| `fanout-1x64` | 2 545.4 | 2 329.5 | **1.09x faster** | 81.80x → 72.38x |
| `wide-256x10` | 20 617.9 | 12 726.5 | **1.62x faster** | 159.06x → 96.01x |

`churn-64x1` is marked unchanged rather than slower, and the paired ratio is why:
its `ns/tick` rose 7.7% but the baseline leg rose 6% in the same run, and the
control's own two intervals bracket the tip's. The two changes were an end to
rebuilding the tick's own scratch state every tick and a source-level digest that
let a chain skip the poll when its source had not moved, which is why the
scenarios where sources hold still gained the most. All five scenarios passed the
fairness gate in all three runs.
