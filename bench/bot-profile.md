# The `lgwks_bot` rig: where a tick goes

Part of the [measurement rigs](README.md); the headline is in
[bot-headline.md](bot-headline.md).

## The per-stage profile

The claim "98% of a tick is poll and change detection" came from a two-scenario
difference. It is measured directly, on the tick path itself, by the crate's own
`profile` feature (`Bot::tick_profiled` is `tick_async` with the per-stage
instrument armed; nothing here is a reimplementation of the tick). Same run and
tree as the headline: main `940e253cd` with #279's changes.

```
  scenario                     poll fingerprint   compare  schedule   decide      act      plain instrument
  poll-only-64x100         412.0      79.0      30.0     282.0      29.0      30.0      907.8      +89.2
  steady-64x100            401.0      79.0      36.0     278.0      60.0    1125.0     2075.3     +102.6
  churn-64x1              7904.0    7560.0    3090.0      45.0    3895.0  117593.0   131974.5    +8305.0
  fanout-1x64              519.0     148.0     312.0      40.0     465.0  113404.0   107720.8    +7175.3
  wide-256x10             3974.0    3043.0    1234.0     593.0    2274.0   43611.0    57188.2     -961.1

  shares of the measured tick
  scenario                     poll fingerprint   compare  schedule   decide      act
  poll-only-64x100         47.7%      9.2%      3.5%     32.6%      3.4%      3.6%
  steady-64x100            20.3%      4.0%      1.8%     14.1%      3.0%     56.8%
  churn-64x1                5.6%      5.4%      2.2%      0.0%      2.8%     83.9%
  fanout-1x64               0.5%      0.1%      0.3%      0.0%      0.4%     98.7%
  wide-256x10               7.3%      5.6%      2.3%      1.1%      4.2%     79.7%
```

**What each column is.** `poll` is the observation phase, and it carries each
source's own comparison of the value it read against the value the substrate
already holds. `fingerprint` is the admitted-input identity every value that
moves binds. `compare` is change detection outside the poll: the
`Changed<Revision>` query and the admission comparisons. `schedule` is the ECS
step itself, a **residual** — the stages nested inside it charge first, so it is
the dispatch of the two systems plus `observe_fold`'s commit bookkeeping.
`decide` is the condition walk. `act` is the effects the tick selected: the
ledger write, the warrant, the durable record and the action.

**Each figure is a mean over every tick of the window, so it includes the one
tick in a hundred on which a value moves.** The `fingerprint`, `compare` and
`decide` columns of `poll-only-64x100` (79, 30 and 29 ns) are that tick's cost
spread over a hundred; a tick on which nothing moves charges none of them,
because it skips the decision pass.

**The stage columns carry the instrument, and `plain` is what the same workload
costs with it off.** A tick reads the clock once per charge plus once to open the
window — seven reads — and one read costs **31 ns** on this host, so the
instrument adds about 209 ns/tick on a short tick. On the long scenarios the
difference is larger because the second window runs on a bot whose journal has
already grown; that is reported rather than subtracted, and it is why the
**shares**, not the absolutes, are what this table supports.

| scenario | poll + change detection | schedule, decision and effects |
|---|---:|---:|
| `poll-only-64x100` | 60.4% | 39.6% |
| `steady-64x100` | 26.1% | 73.9% |
| `churn-64x1` | 13.2% | 86.8% |
| `fanout-1x64` | 0.9% | 99.1% |
| `wide-256x10` | 15.2% | 84.8% |

On `poll-only-64x100`, which fires nothing, `act` was 242 ns of a 1 452 ns tick on
main `940e253cd`: the act stage's own walk over 64 chains with nothing selected.
It is 30 ns now, and what is left of that tick is the observation phase itself —
412 ns, 6.4 ns per source for the `revision()` read, the change-tick compare and
two declared-state reads — and the schedule residual, 282 ns: the dispatch of the
two systems plus, once in a hundred ticks, the commit bookkeeping of 64 moved
sources. That is why
the quiet ratio is 27.6x and not 10x: ten times the baseline's 32.3 ns tick is
323 ns, and the poll stage alone is above it. On every scenario that fires
effects `act` is the largest stage, 56.8% to 98.7%: the ledger, warrant and
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
