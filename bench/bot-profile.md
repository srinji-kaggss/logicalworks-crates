# The `lgwks_bot` rig: where a tick goes

Part of the [measurement rigs](README.md); the headline is in
[bot-headline.md](bot-headline.md).

## The per-stage profile

The claim "98% of a tick is poll and change detection" came from a two-scenario
difference. It is measured directly, on the tick path itself, by the crate's own
`profile` feature (`Bot::tick_profiled` is `tick_async` with the per-stage
instrument armed; nothing here is a reimplementation of the tick). Same run and
tree as the headline: main `f1dc7434c` with the quiet-tick schedule skip.

```
  scenario                     poll fingerprint   compare  schedule   decide      act      plain instrument
  poll-only-64x100         405.0      78.0      32.0     243.0      31.0      30.0      855.7     +100.2
  steady-64x100            394.0      77.0      39.0     240.0      63.0    1113.0     2032.8      +93.0
  churn-64x1              7978.0    7618.0    3364.0      45.0    4049.0  117642.0   132137.9    +8134.3
  fanout-1x64              558.0     154.0     341.0      37.0     485.0  116819.0   113166.6    +6448.8
  wide-256x10             3835.0    3012.0    1296.0     549.0    2365.0   43409.0    55900.4     -651.8

  shares of the measured tick
  scenario                     poll fingerprint   compare  schedule   decide      act
  poll-only-64x100         49.4%      9.5%      4.0%     29.6%      3.9%      3.7%
  steady-64x100            20.4%      4.0%      2.0%     12.5%      3.3%     57.8%
  churn-64x1                5.7%      5.4%      2.4%      0.0%      2.9%     83.6%
  fanout-1x64               0.5%      0.1%      0.3%      0.0%      0.4%     98.7%
  wide-256x10               7.0%      5.5%      2.4%      1.0%      4.3%     79.7%
```

**What each column is.** `poll` is the observation phase, and it carries each
source's own comparison of the value it read against the value the substrate
already holds. `fingerprint` is the admitted-input identity every value that
moves binds. `compare` is change detection outside the poll: the
`Changed<Revision>` query and the admission comparisons. `schedule` is the ECS
step itself, a **residual** — the stages nested inside it charge first, so it is
the dispatch of the two systems plus `observe_fold`'s commit bookkeeping, except
on a tick that polled nothing and holds nothing, which settles in place and
dispatches neither.
`decide` is the condition walk. `act` is the effects the tick selected: the
ledger write, the warrant, the durable record and the action.

**Each figure is a mean over every tick of the window, so it includes the one
tick in a hundred on which a value moves.** The `fingerprint`, `compare` and
`decide` columns of `poll-only-64x100` (78, 32 and 31 ns) are that tick's cost
spread over a hundred; a tick on which nothing moves charges none of them,
because it skips the decision pass.

**The stage columns carry the instrument, and `plain` is what the same workload
costs with it off.** A tick reads the clock once per charge plus once to open the
window — seven reads — and one read costs **30 ns** on this host, so the
instrument adds about 206 ns/tick on a short tick. On the long scenarios the
difference is larger because the second window runs on a bot whose journal has
already grown; that is reported rather than subtracted, and it is why the
**shares**, not the absolutes, are what this table supports.

| scenario | poll + change detection | schedule, decision and effects |
|---|---:|---:|
| `poll-only-64x100` | 62.9% | 37.2% |
| `steady-64x100` | 26.4% | 73.6% |
| `churn-64x1` | 13.5% | 86.5% |
| `fanout-1x64` | 0.9% | 99.1% |
| `wide-256x10` | 14.9% | 85.1% |

On `poll-only-64x100`, which fires nothing, `act` was 242 ns of a 1 452 ns tick on
main `940e253cd`: the act stage's own walk over 64 chains with nothing selected.
It is 30 ns now, and what is left of that tick is the observation phase itself —
405 ns, 6.3 ns per source for the `revision()` read, the change-tick compare and
one declared-state read — and the schedule residual, 243 ns, which the quiet
ticks no longer spend on dispatch and which carries, once in a hundred ticks, the
commit bookkeeping of 64 moved sources. The skip itself was timed against the
code it replaced in one binary (the headline's paired table): 985.4 to 849.9
ns/tick on this scenario. That is why
the quiet ratio is 26.4x and not 10x: ten times the baseline's 32.4 ns tick is
324 ns, and the poll stage alone is above it. On every scenario that fires
effects `act` is the largest stage, 57.8% to 98.7%: the ledger, warrant and
record each dispatched effect is written through are the price of the
guarantees, and the 30x churn target cannot be met without removing them.

## Heap allocations: a quiet tick makes none

The counting allocator (`--alloc-report`, one shared instrument with
`bench/async`) measures, on a warmed bot and outside every timed window:

| scenario | allocs/tick | bytes/tick | mean bytes | baseline allocs/tick |
|---|---:|---:|---:|---:|
| `poll-only-64x100` | 4.2 | 484.2 | 115.3 | 0.0 |
| `steady-64x100` | 8.0 | 1 972.8 | 247.9 | 0.0 |
| `churn-64x1` | 878.0 | 216 320.1 | 246.4 | 0.0 |
| `fanout-1x64` | 345.0 | 168 370.1 | 488.0 | 0.0 |
| `wide-256x10` | 347.4 | 95 412.3 | 274.7 | 0.0 |

The baseline's zero is the control: a hand-rolled loop over the same workload
allocates nothing, so a non-zero count on that side would mean the counter was
picking up something other than the bot. The per-tick trace of `steady-64x100`
(a value moves every 100 ticks) reads `98:0 99:0 100:558 101:0 102:0`: **zero
allocations on every tick on which nothing moves**, and 558 on the tick where 64
sources move, about 8.7 per moved chain. Before #279 the trace read 1 on every
quiet tick — `block_on`'s `Arc<ThreadWaker>` — and before the change ticks 20.
The means above are therefore the moving ticks spread over a hundred. Attributing
the 558 to allocation sites needs an allocation-site profiler, which this
repository cannot host — `unsafe` is denied by the crate's lint contract and a
`bevy_ecs` edge in `bench/` is a dependency the register does not carry — so the
split is reported as measured by window and not guessed.
