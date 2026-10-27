# 4.2 Hyperscale — more than a million concurrent, correct

Part of [production readiness](../production-readiness.md), §4, nine axes.

**⚠️ — measured at a million concurrent, through a saturation curve on this
host, and under a 2 CPU / 2 GiB cgroup; the facade's closed-loop cost is
1.02x–1.17x raw Tokio at p50 and 0.93x–1.41x at p99. No real 1–2 vCPU machine
has run it: the cgroup run is one run in a VM on the development Mac.**

Every number is from `bench/async`, is this host, this toolchain and this run,
and is read with its condition beside it. Host: **Apple M5 Pro, 15 cores, 24 GB,
macOS 27.0, rustc 1.99.0.** That host is **shared** and its one-minute load
average ran from 2 to 97 across the work, so the rig now reads and prints the
load with every run: a scenario table taken with three other builds running on
it reported the *baseline* at 1.08 ms where an idle host reported 0.35 ms, a 3x
shift in the control leg of a paired comparison. Each table below carries its
load, and the paired facade-versus-baseline columns are not contaminated by it
because both sides of a point see the same load.

**The knee, at six in-flight bounds** (backpressure door, load 79.13, 6:48.56
wall, peak RSS 250,953,728 bytes). Offered rate against a derived per-bound body
cost so every ceiling's declared capacity is 20,000 arrivals/s; knee read against
`max(50 ms, one further service time)`:

| bound | facade knee offered/s | achieved/s | p99 at knee | baseline knee | baseline achieved/s |
|---:|---:|---:|---:|---:|---:|
| 64 | 5,000 | 4,987 | 6.5 ms | 5,000 | 4,979 |
| 1,024 | 5,000 | 4,763 | 59.7 ms | 5,000 | 4,754 |
| 10,000 | 20,000 | 13,340 | 504.9 ms | 20,000 | 13,329 |
| 16,384 | 20,000 | 10,968 | 831.5 ms | 20,000 | 11,009 |
| 100,000 | 80,000 | 13,331 | 5,008.0 ms | 80,000 | 13,339 |
| 131,072 | 80,000 | 10,589 | 6,551.5 ms | 80,000 | 10,593 |

**The facade and the raw baseline declare the same knee at every bound**, to
within 0.5% on achieved rate at five of the six. At a service time of 3.2 ms or
more the ceiling is the binding constraint, not the accounting.

**Past the knee it refuses, and it refuses at exactly the declared bound.**
`try_spawn` at 320,000 arrivals/s: bound 16,384 admitted **exactly 16,384**,
bound 100,000 admitted **exactly 100,000**, bound 131,072 admitted **exactly
131,072**, both sides, to the same number; 59% of arrivals refused and counted in
`Stats::refused`; peak RSS unchanged at the bound's worth of state; the p99 of
what *was* admitted still one service time.

**A million concurrent, reached.** 1,048,576 tasks **concurrently admitted** —
the tier is the bound and every body parks until the whole tier is admitted, so
the peak is observed rather than inferred — all completed, on both sides:

| tier | facade peak RSS | facade RSS per in-flight task | baseline RSS/task | facade p50 | baseline p50 |
|---:|---:|---:|---:|---:|---:|
| 10,000 | 27,115,520 B | 2,712 B | 3,267 B | 16.3 ms | 12.2 ms |
| 100,000 | 210,501,632 B | 2,105 B | 2,672 B | 100.8 ms | 72.0 ms |
| 1,048,576 | 2,137,751,552 B | **2,039 B** | 2,595 B | 1,007.7 ms | 726.7 ms |

**Peak RSS per in-flight task at the million tier: 2,039 bytes**, against the raw
baseline's 2,595 — memory favours the facade, by 21%. Admission latency does not:
1.39x the baseline's at both wide tiers.

**Overload and recovery** (load 19.88): a 30-second overload at twice the knee
built a 229,554-arrival queue and took the served p99 from 6.7 ms to 11.4 s, with
**nothing refused and nothing lost** — offered = admitted = completed on both
sides, checked by the rig's conservation gate. The facade drained in 0.023 ms
against the baseline's 4.801 ms; both sides' served p99 was back inside its own
baseline at the first recovery window, 500.7 ms and 505.1 ms after the overload
stopped. The recovery figure is a **bound at one 500 ms window's resolution**, not
a point estimate.

**Allocations, per supervised task**, 1,024 tasks at bound 8, one harness:
**15.33 before, 3.61 after**, against a raw baseline's 2.02 — the issue's ≤ 6
target met, by cutting an eagerly-built `watch` channel per cancellation token
and a 100 ms timer armed on every uncontended spawn. The remaining 3.61 is
itemised in `bench/async/README.md` with the guarantee each allocation pays for.

**The latency target is met, and it moved by a different change.** The issue
asks for ≤ 2x the baseline at p99 on every scenario. `bench/async --rounds=15`
at main `22f1d526` (#553, load 6.86) reads p99 1.41x / 1.02x / 1.09x / 0.93x
(quiet / fanout / capacity / single-permit), p50 1.17x / 1.02x / 1.06x / 1.07x,
and 3.46 allocations per task. The 4.49x this section carried was the drain's
1 ms Tokio timer tick, whose granularity cost every drain a fixed 1.3–1.6 ms that
raw Tokio never paid; `Supervisor::wait_idle` (#320, `b1b1a3434`) joins on the
task set's own wakeup instead. The allocation cut above had moved the paired
ratios not at all, which is how the gap was found to be the timer.

**The 2 CPU / 2 GiB profile, run in a container.** macOS exposes no cgroup, no
`taskset`, no `taskpolicy` CPU set and no `cpulimit` (all four checked on the
reference host, all four absent), and `--workers=2` produced identical knees at
every bound because every open-loop body is a timer. So `linux-container-tests --
vps` runs every `bench/async` door in a Linux container on the same machine
(OrbStack, `rust:1.99.0-bookworm`, release build) under `cpu.max 200000 100000`
and a 2 GiB `memory.max` with swap capped to the same; the cap and the doors are
data in `bench/async/vps/profile.toml`. Taken 2026-10-11 at host load 2.8,
947.8 s for the whole matrix ([`bench/async/vps.md`](../../bench/async/vps.md)):

| door under the cap | facade | baseline |
|---|---|---|
| closed loop, p50 ratio (quiet / fanout / capacity / single) | 1.31x / 1.39x / 1.27x / 1.14x | 1 |
| backpressure knee, bound 64 | 5,000/s, p99 10.5 ms | 5,000/s, p99 11.6 ms |
| backpressure knee, bound 1,024 | 20,000/s offered, 18,376/s achieved, p99 86.4 ms | 5,000/s, p99 72.0 ms |
| refuse door, bound 64 | no knee: refused 1 arrival at the lowest rung | refused 0 there |
| overload at bound 64, p99 back to baseline | after 508.8 ms | did not return in the 90 s phase |
| 1,048,576 in flight, peak RSS | 1,987 MB, no OOM kill | 2,034 MB, no OOM kill |
| CPU-body knee (300,000 hash rounds, 2 workers) | same knee as baseline at every bound; p99 at knee 0.9–1.9x | 1 |

The CPU row is the 2026-10-26 `saturation-cpu` door
([`bench/async/vps.md`](../../bench/async/vps.md)), same knee both sides; absolutes load-depressed.

*Not covered, and named:*

- **No real 1–2 vCPU machine has run it.** The cgroup run is one run under OrbStack's
  scheduler in a VM on the development Mac; a VPS differs in cache, memory bandwidth and
  noisy neighbours; the timer doors park, and the CPU-body curve above is the first that burns.
- `Bot::tick` scaling is now measured to 16,384 sources (#640): 54 663 /
  238 706 / 1 167 019 / 8 471 300 ns/tick at 256 / 1 024 / 4 096 / 16 384, a
  log-log slope of 1.206; `act` carries the superlinear term (§4.9).
- The closed-loop p99 ratios are point readings (the paired interval is on p50
  only; the p50 spread between runs is about 0.1x, and a second run read p99
  1.05x / 1.01x / 1.12x / 1.05x). Every figure is one host, and nothing here is a
  cross-platform claim.

*To close the axis:* the same matrix on a real 1–2 vCPU box. "Runs for weeks" is now a measured knee and a measured recovery rather than a design intent, and it is still not a measured week.
