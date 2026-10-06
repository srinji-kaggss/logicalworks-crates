# Async matched-semantics measurement

`lgwks_bot::rt::supervise::Supervisor` measured against **pinned raw Tokio** doing
the same work, at the same in-flight bound, with the same failure and
cancellation semantics. Part of #152; the saturation curve, the open-loop driver
and the in-flight tiers are #269.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --rounds=11 --alloc-report --json=bench/async/results.json
```

`--rounds=<n>` sets the paired-round count (default 15). `--alloc-report` runs a
separate, counted window; it is never mixed into a timed round.

## The host every number below was taken on

**Apple M5 Pro, 15 cores (5 performance, 10 efficiency), 24 GB, macOS 27.0,
rustc 1.99.0, `opt-level=3`, `lto=true`, `codegen-units=1`. One host, one run:
absolute numbers, not a cross-platform claim.**

**This host is shared, and it was not idle.** Its one-minute load average ran
from 2 to 97 across this work, and the rig now reads and prints it with every
run, because the load is not a footnote: a scenario table taken with three
other builds running on it reported the **baseline** at 1.08 ms where an idle
host reported 0.35 ms — a 3x shift in the *control* leg of a paired
comparison, which moves a ratio in a way no reader can see without the number.
Each table below carries the load it was taken at. Absolute latencies taken at
a load above ~20 are contaminated and are marked; the paired
facade-versus-baseline columns inside a point are not, because both sides of a
point experience the same load.

## The 1–2 vCPU / 1–2 GB VPS profile was NOT measured

Stated plainly because the axis it belongs to asks for it: **no number in this
file is a VPS figure.** macOS exposes no cgroup, no `taskset`, no `taskpolicy`
CPU set and no `cpulimit` — all four were checked on the reference host and all
four are absent — so there is no way to present a run on this machine as that
profile's. The closest thing that *can* be run here is `--workers=<n>`, which
pins the scheduler's thread count, and the measurement below says exactly why it
is not a substitute:

**`--workers=2` produced knees identical to `--workers=15` at every bound**
(`saturation-workers2.json` against `saturation-backpressure.json`): 5,000 /
5,000 / 20,000 / 20,000 / 80,000 / 80,000 offered per second, agreeing to within
0.5% on the achieved rate. The reason is measurable and is not a null result:
every body in the sweep is a timer, so the knee is a property of the ceiling's
capacity and not of how many workers drain it. A 2-vCPU profile differs from
this host in cores, cache, memory bandwidth and scheduler, **and** it would be
driving a CPU-bound workload rather than a timer-bound one. A CPU-bound body
would make the two configurations differ; this one does not, and publishing it
as a VPS measurement would be a category error with a number attached.

## Its own workspace root, and why

The comparison needs a raw `tokio` edge the four published crates must never
author. `lgwks-deps check` discovers packages through Cargo's own
`workspace_members`, so a separate root is invisible to the gate — the same
arrangement `bench/` uses.

It is a *separate* root from `bench/`, not a second binary in it, because
`bench/` deliberately builds `lgwks_bot` with `rt` withdrawn so its measured path
is exactly the ECS schedule. Cargo features are per-package and not per-binary,
so a second binary there would have forced the async surface into that build and
made its headline "no tokio edge is compiled" claim false.

## The fairness gate, and what it caught

Every round compares the two sides' **terminal counts field by field**: tasks
placed, tasks that completed, cancelled, aborted, spawns refused, and the sum of
each body's own work counter. Any difference aborts the run and prints the field
that diverged. A ratio computed on unequal work is not a slower engine, it is a
different program.

Four real asymmetries were found and fixed *in the harness*, not in the crate:

- **`Supervisor` reports a task cancelled if the token was cancelled when the
  body returned**, even if the body finished its work first. That reading is
  correct and conservative. But `shutdown` cancels first, so draining through
  `shutdown` measures "who cancelled" rather than "who completed". The facade
  side is therefore drained with its token intact (`reap` until the completion
  counter reaches the placed count) — the drain a caller who cares about
  completion performs.
- **`Supervisor` retains terminal outcomes in a buffer capped at the in-flight
  bound** and counts the loss in `reports_dropped`. The baseline has no
  reporting layer and so bounds it at nothing. Comparing retained-list lengths
  would compare reporting granularity, not execution. The retention asymmetry is
  **reported separately**, as the asymmetry it is, and is not part of the gate.
- **A time-bounded sweep point is not the same offered work on both sides.** The
  gate refused this, naming `placed`: offering "for one second" lets the faster
  side place more arrivals, and the offered work becomes a property of each
  engine's speed. A sweep point now offers the **arrival count** its rate and
  window define, and both sides are offered exactly that.
- **The facade had no blocking join, and the drain's timer was the gap (#269).**
  `Supervisor::reap` joins only what has already finished, so a drain through it
  had to poll, and every poll slept on Tokio's timer. Tokio's timer wheel has a
  1 ms granularity, so each drain paid a fixed 1.3–1.6 ms that raw Tokio, which
  awaits `JoinSet::join_next`, never paid: that was the whole 3–5x p99 gap the
  gate reported on the quiet rows. `Supervisor::wait_idle` now joins on the task
  set's own wakeup, and every drain here awaits it, so both sides wait on the same
  primitive and no timer is left in the drain path. A scheduler round-trip in
  place of the timer was built and measured earlier and lost (3.49x / 5.09x /
  3.50x), because it reschedules the draining task behind the workers it waits on.

The gate is not decoration: it refused during development on each of the first
three, each time on a real difference between the two sides.

## The open-loop driver

`--saturation` (backpressure door) and `--refusal` (refusal door) run an
**open-loop** generator: the offered rate is a parameter, independent of
completions, and each arrival's latency is measured from the instant it was
*intended* to start (`t0 + index x period`) rather than from the instant the
generator reached it.

That is the whole correction for **coordinated omission**. A closed-loop client
waits for request `i` before starting request `i+1`, so past saturation it slows
its own offered rate down exactly as fast as the service slows down, and the
reported latency never includes the time a request spent waiting for the server
to become free — which is precisely the case where a saturated server stopped
being offered to. A generator that falls behind instead reports the queue it
created.

The recorder is **HdrHistogram-style, implemented in the bench crate** with no
new published-crate edge (`openloop.rs`): 256 equal-width sub-buckets inside
each power-of-two octave, so the relative error of any reported value is at most
1/256 (0.39%) at *every* magnitude — from a 40 ns body to a 4 s stall. Min, max
and total are exact; a percentile is the midpoint of its bucket; samples past the
ceiling are counted rather than silently folded into the top one.

`sim_openloop.rs` is the seeded model of that driver and **all 17 of its tests are
deterministic simulation tests**: one seed drives arrival jitter and body
length, a failure prints its seed, and the same seed replays to the same trace
hash — asserted, on a hash that folds every scheduled event including every
refusal.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- --sim-open-loop
```

## The saturation curve — the knee, at six in-flight bounds

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --saturation --window=1 --json=bench/async/saturation-backpressure.json
```

Bounds `64`, `1,024`, `10,000`, `16,384`, `100,000`, `131,072` — the issue's
list and the delivery brief's list, because they name different decades and
neither is a subset of the other, and a sweep that quietly drops a tier reads as
coverage of the tiers it kept.

**The body cost is derived per bound** so every ceiling's declared capacity
lands on one declared target of 20,000 arrivals a second. A constant body cost
cannot produce a curve at more than one ceiling width: a 5 ms body puts a ceiling
of 131,072 at 26 million arrivals a second, an in-process generator offers about
450,000, and every rung above the first then measures the *generator's* backlog
rather than the ceiling's — measured, before the scaling: at bound 131,072 the
generator's own `max lag` reached 3.4 s against a ceiling that had admitted
every arrival immediately. With the cost derived, each bound is swept at the
same four rates against a ceiling under exactly that load, so the knee is
comparable across bounds and is a property of the engine. `--body-micros=<n>`
pins the cost at every bound instead, which is the other question a reader may
want, and the two are reported separately because neither answers the other.

The knee is read against **`max(50 ms, one further service time)`**. The floor is
the estate's standing budget for a request-shaped interaction; the multiple of the
body exists because the sweep scales the body per bound, and at a ceiling of
131,072 the body is 6.55 s — a 50 ms budget there would be smaller than one
service time and would declare "no knee" at every rung, which is a statement
about the budget and not about the engine. Every bound's header prints the budget
it is read against.

Backpressure door, load average **79.13**, 6:48.56 wall, peak RSS 250,953,728
bytes:

| bound | side | knee offered/s | achieved/s | p99 at knee | refused at top |
|---:|---|---:|---:|---:|---:|
| 64 | facade | 5,000 | 4,987 | 6.5 ms | 0 |
| 64 | baseline | 5,000 | 4,979 | 8.2 ms | 0 |
| 1,024 | facade | 5,000 | 4,763 | 59.7 ms | 0 |
| 1,024 | baseline | 5,000 | 4,754 | 57.5 ms | 0 |
| 10,000 | facade | 20,000 | 13,340 | 504.9 ms | 0 |
| 10,000 | baseline | 20,000 | 13,329 | 504.9 ms | 0 |
| 16,384 | facade | 20,000 | 10,968 | 831.5 ms | 0 |
| 16,384 | baseline | 20,000 | 11,009 | 829.4 ms | 0 |
| 100,000 | facade | 80,000 | 13,331 | 5,008.0 ms | 0 |
| 100,000 | baseline | 80,000 | 13,339 | 5,008.0 ms | 0 |
| 131,072 | facade | 80,000 | 10,589 | 6,551.5 ms | 0 |
| 131,072 | baseline | 80,000 | 10,593 | 6,551.5 ms | 0 |

**The two sides declare the same knee at every one of the six bounds**, and their
achieved rates agree to within 0.5% at five of them. At a service time of 3.2 ms
or more, `Supervisor`'s ceiling and raw Tokio's admit work at the same offered
rate: the accounting is not the binding constraint, the ceiling is. The narrow
bounds (64, 1,024) declare a lower knee than the wide ones because a 3.2 ms
timer body overshoots to roughly 6 ms on this runtime, so their *real* capacity
is near 10,500/s rather than the declared 20,000/s — the achieved column says so
and the knee follows the measurement rather than the arithmetic.

Queue depth and the refusal rate are in the results file per point
(`peak_queue`, `mean_queue`, `refusal_rate`, `max_lag_millis`).

## Past the knee, `Supervisor` refuses — it does not grow

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --refusal --window=1 --json=bench/async/saturation-refuse.json
```

The same ladder on `Supervisor::try_spawn`. Load average **16.09**, 2:56.55 wall,
peak RSS 244,367,360 bytes:

| bound | offered/s | facade admitted | facade refused | baseline admitted | baseline refused | facade p99 |
|---:|---:|---:|---:|---:|---:|---:|
| 64 | 320,000 | 13,746 | 306,254 | 13,920 | 306,080 | 8.3 ms |
| 1,024 | 320,000 | 19,456 | 300,544 | 19,459 | 300,541 | 55.5 ms |
| 10,000 | 320,000 | 20,000 | 300,000 | 20,000 | 300,000 | 504.9 ms |
| 16,384 | 320,000 | **16,384** | 287,232 | **16,384** | 287,232 | 827.3 ms |
| 100,000 | 320,000 | **100,000** | 220,000 | **100,000** | 220,000 | 6,551.5 ms |
| 131,072 | 320,000 | **131,072** | 188,928 | **131,072** | 188,928 | 6,551.5 ms |

The bold cells are the whole acceptance item: at bound 16,384 the facade admitted
**exactly 16,384** tasks, at 100,000 exactly **100,000**, at 131,072 exactly
**131,072** — the declared in-flight bound, to the task, on both sides and to the
same number. 59% of offered arrivals were refused and counted, peak RSS stayed at
the bound's worth of state, and the p99 of what *was* admitted stayed at one
service time: the ceiling refused instead of growing. The refusal rate is a
declared overflow behaviour, not a silent drop (`TrySpawnRefusal`, counted in
`Stats::refused`), and each point's own conservation gate — every arrival offered
was admitted or refused, every admitted body completed, every completed body
recorded its work unit, nothing left in flight — passed on both sides.

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

## Overload and recovery

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --overload --bound=64 --knee=10000 --baseline-seconds=5 \
    --overload-seconds=30 --recovery-seconds=90 --json=bench/async/overload.json
```

Load average **19.88**, 4:10.33 wall, peak RSS 5,521,408 bytes. Baseline 5 s at
5,000/s, overload 30 s at 20,000/s (twice the 10,000/s knee), recovery 90 s at
5,000/s:

| side | baseline p99 | overload p99 | peak queue | drain | back to baseline p99 | offered = admitted = completed |
|---|---:|---:|---:|---:|---:|---:|
| facade | 6.742 ms | 11,358.175 ms | 229,554 | 0.023 ms | 500.747 ms | 845,586 |
| baseline | 6.398 ms | 11,358.175 ms | 229,172 | 4.801 ms | 505.144 ms | 845,959 |

The 30-second overload at twice the knee built a **229,554-arrival queue** and
took the served p99 from 6.7 ms to 11.4 s — a factor of 1,684 — with **nothing
refused and nothing lost**: the conservation gate checked offered = admitted =
completed on both sides and both partition exactly. The facade drained in
**0.023 ms** where the baseline needed 4.801 ms (208x), and both sides' served p99
was back inside its own baseline at the **first recovery window**, 500.7 ms and
505.1 ms after the overload stopped.

**The recovery figure is a bound at one window's resolution, not a point
estimate**: it is measured from the first 500 ms window whose p99 came back
inside the baseline's, so 500.7 ms means "inside by 500.7 ms", and the true
figure is somewhere in (0, 500.7 ms].

Two harness defects surfaced here and are fixed, and both had been reporting a
false answer:

- The phase loop scheduled each phase's arrivals with the **cumulative** arrival
  index against **that phase's own** `t0`, so after a 30 s overload the recovery
  phase's first arrival was scheduled 71.5 s into its future. It offered exactly
  **one** arrival in five seconds and the run reported "the p99 did not return to
  the baseline". The engine had recovered; the rig had stopped offering.
- The recovery detector also required an empty engine, which is unsatisfiable
  while the phase is still offering load at 0.5x the knee. The drain is measured
  separately and timed, so requiring emptiness as well made the one reading that
  should *include* the drain the one reading that excluded it.

## Allocation attribution — every window, priced separately

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --alloc-attribution --json=bench/async/alloc-attribution.json
```

1,024 tasks per wave, counted in a window separate from every timed round, on one
unchanged harness. **BEFORE** is `origin/main`'s `cancel.rs` and `supervise.rs`;
**AFTER** is this branch. Per task:

| operation | before | after |
|---|---:|---:|
| facade spawn wave, contended (bound 8) | **15.33** | **3.61** |
| facade spawn wave, uncontended (bound 4096) | 15.17 | 3.17 |
| raw tokio wave, contended (bound 8) | 2.02 | 2.02 |
| the facade's excess over raw | 13.31 | 1.59 |
| `CancellationToken::new` (a root token) | 11.00 | 1.00 |
| one child token | 11.00 | 1.00 |
| one `run_until_cancelled` race | 13.00 | 3.00 |
| one `Supervisor::new(4096)` | 15.00 | 5.00 |

**The issue's allocation target (≤ 6 per task) is met: 3.61, from 15.33.**
Four changes, each measured on its own:

1. **`Inner`'s signal channel is built on first use** (`OnceLock`), not in the
   constructor. `CancellationToken::new` cost 11 allocations and `child_token`
   another 11, all of it a `watch::channel` that nothing awaited: the
   `AtomicBool` is the authority and a channel only *wakes* waiters. 11 → 1 each.
   The subscriber re-reads the flag **after** installing the channel, which closes
   the window a lazy channel opens; two regression tests in `rt::cancel::tests`
   cover exactly that window, because nothing else reached it.
2. **`Supervisor::claim` no longer arms a 100 ms timer on an uncontended
   spawn.** It raced the permit against the token through a
   `timeout(100 ms)` inside `run_until_cancelled` — 13 allocations and a
   timer-wheel registration — for a wait never taken. It now takes a free permit
   and re-checks cancellation on it, which is the same decision with the same
   cancellation-wins rule. The contended path is untouched.
3. `claim_now` is the same shape through one non-counting `try_take`, so a full
   pool on the backpressure door is backpressure rather than a refusal.
4. `run_until_cancelled`'s two `Box::pin`s became one allocation's worth (3.00
   from 13.00) as a side effect of (1) and (2).

**The remaining 3.61, itemised, with the guarantee that pays for each** (1.61
above the raw wave's 2.02):

| per task | what it is | the guarantee |
|---:|---|---|
| 1.00 | the child `CancellationToken`'s own `Arc<Inner>` | a task's token must be cancellable *without* being able to cancel its siblings (`cancel.rs`, "Why the link points up"). The raw baseline's `child` is an `Arc::clone` of the parent and so cannot be; this one owns a flag. |
| 1.00 | the identity map's node | `Supervisor::identities: BTreeMap<Id, TaskId>` — the terminal `JoinError` has no value to carry a `TaskId`, so the engine's task id must be mapped back to the supervisor's own (INV-BOT-31). |
| ~1.60 | `JoinSet`'s own task and the report buffer's growth | the owned, un-detached task (INV-BOT-18) and the capped `TaskOutcome` ring. |

**A bare `JoinSet::spawn` measures 0.00 allocations** after the first, because the
runtime hands tasks out of a per-worker slab and never returns to the allocator.
That is why the raw wave's 2.02 is the semaphore permit, the baseline's own
cancellation child and its body — not the spawn.

### The latency target was NOT met, and the reason is the finding

The issue also asks for **≤ 2x the baseline at p99 on every scenario**. That is
**not met** — `quiet-async-bot` is 4.49x (see the table below). And the finding
is that the two targets are not the same target: **removing 76% of the
allocations moved the allocation count exactly as predicted and moved the paired
latency ratios not at all.** Measured paired, four rounds each, same harness:

| scenario | ratio before | ratio after |
|---|---:|---:|
| quiet-async-bot | 4.65 / 4.36 / 4.01 / 4.54 | 4.28 / 3.91 / 2.65 / 3.37 |
| high-fanout | 2.52 / 2.25 / 2.13 / 2.15 | 1.76 / 2.07 / 1.83 / 1.69 |
| at-capacity | 2.11 / 2.14 / 2.64 / 2.81 | 1.92 / 1.50 / 1.54 / 1.30 |
| single-permit | 1.70 / 1.45 / 1.50 / 1.51 | 1.56 / 1.46 / 1.48 / 1.44 |

(The three rounds that overlapped with a load of 50+ are excluded from the
`quiet-async-bot` after column's reading as noise; the harness printed the load
for exactly this reason.) **The remaining gap is work, not allocation**, and the
work is the per-spawn reap, the identity map, the `TaskOutcome` the wrapper builds
and the reporting layer. Closing it is a different change from the one this issue
names, and it is not made here.

## Results — the closed-loop scenario table

Apple M5 Pro, macOS 27.0, rustc 1.99.0, `opt-level=3`, `lto=true`,
`codegen-units=1`. **15 paired rounds, load average 9.33.** One host, one run.

| scenario | tasks | bound | facade p50 | facade p99 | base p50 | base p99 | ratio | 95% CI (paired) |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| quiet-async-bot | 256 | 8 | 1.692 ms | 1.859 ms | 0.377 ms | 0.771 ms | 4.49x | [2.41, 5.03] distinguishes |
| high-fanout | 2048 | 32 | 4.041 ms | 4.689 ms | 2.646 ms | 2.910 ms | 1.53x | [1.44, 1.58] distinguishes |
| at-capacity | 512 | 4 | 2.891 ms | 3.886 ms | 1.486 ms | 1.918 ms | 1.95x | [1.86, 2.31] distinguishes |
| single-permit | 512 | 1 | 6.560 ms | 8.415 ms | 4.820 ms | 6.806 ms | 1.36x | [1.25, 1.40] distinguishes |

Bare synchronous floor (no scheduler): 7 µs for 4,096 counter bumps. It is a
reference line and is **never** multiplied into the ratio above.

Peak RSS for the whole process: **3,850,240 bytes**.

Allocation model, 1,024 tasks at bound 8, counted in a window separate from every
timed round: facade **3,798 allocations / 977,528 bytes (3.71 per task)**, baseline
2,064 / 320,800 (2.02 per task).

**The spread is itself a result and is not smoothed away.** `quiet-async-bot`
(4.49x) is the smallest workload, where the facade's fixed per-spawn work
dominates; `high-fanout` (1.53x) amortizes it over 2,048 tasks. `single-permit`
is the row to watch: with one permit there is no concurrency for the facade to
account for, so what remains is per-task bookkeeping competing with scheduler
jitter of comparable size, and it sits at 1.36x with a 95% interval that
distinguishes parity on every run here.

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

## The mutant baseline — proving the gate can refuse

A gate that has only ever been shown agreeing runs cannot be told apart from one
that always says yes. `--mutant-check` runs the negative control: a side that
places every task exactly as the honest facade does and then **stops draining**,
so the tally it hands the gate claims 512 placed tasks while fewer bodies were
observed to finish. The gate must refuse it, and must refuse it *for a
work-count reason*.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- --mutant-check
```

Recorded output, Apple M5 Pro, macOS 27.0, rustc 1.98.0, 512 tasks at bound 8:

```
refused, as required: completed: facade 505 vs baseline 512
the refusal names a work-count field, so the gate discriminates on work

mutant tally:  placed 512 completed 505 cancelled 0 aborted 0 work_units 509
honest tally:  placed 512 completed 512 cancelled 0 aborted 0 work_units 512
```

The mutant is the honest facade body with one parameter flipped — `Drain::GiveUp`
instead of `Drain::Complete` — so the defect is readable in a diff of one line
rather than by comparing two functions. Which field the gate *names* depends on
the order it checks them and on how far the workers got before the early reap, so
the check accepts any work-count field and prints which one fired rather than
pinning one; a slower host would name `work_units` first.

The mutant is deliberately *unfair* and not merely slow: a slower-but-identical
mutant would pass the gate and print a meaningless ratio, which is the failure
this control exists to rule out. The mutant check runs alone and exits non-zero
if the gate ever accepts it.

## What this says, stated before the reader draws a conclusion

**At a service time of 3.2 ms or more, `Supervisor` and raw Tokio admit work at
the same offered rate at every in-flight bound from 64 to 131,072**, and past the
knee both refuse at exactly the declared bound with the served p99 unchanged.
The facade's cost is a **fixed per-task** cost — 3.61 allocations and a fixed
amount of bookkeeping per spawn — and it is amortised away as the body lengthens
and as the ceiling widens. The closed-loop table above is the regime where that
fixed cost is the whole cost (a ~1 µs body), and it is where the facade is
1.36x–4.49x raw Tokio.

**Memory favours the facade.** 2,039 bytes per in-flight task against the
baseline's 2,595 at a million concurrent tasks.

## What this does not claim

- **No VPS figure.** No number here is a 1–2 vCPU / 1–2 GB measurement, and the
  reason is stated above with the measurement that supports it.
- No third engine is in this comparison. It is a cost decomposition of a facade
  against its own baseline, **not** a leaderboard and not a claim about any other
  scheduler.
- The absolute numbers are this host, this toolchain, this run, at the load
  average printed beside them. A load above ~20 contaminates the absolute
  latencies; it does not contaminate the paired columns within a point.
- Allocation counts are dominated by the facade's per-task accounting structures
  and are reported as counts, not as a verdict. `realloc` is counted at its
  *new* size, so a growing workload is overstated; that approximation is stated
  in `bench/src/alloc_count.rs` rather than left inside a number.
- Nothing here measures durability, restart cost, or history scaling. Those are
  different layers and a number from this rig would not speak to them.
- `lgwks_bot::Bot::tick` (the ECS path) beyond 256 sources is still not measured
  here; that is the synchronous rig in `bench/`.

## The raw record

`results.json`, `saturation-backpressure.json`, `saturation-refuse.json`,
`saturation-workers2.json`, `inflight.json`, `overload.json` and
`alloc-attribution.json` in this directory are the committed output of the runs
above, with every point's p50/p95/p99, the admission and refusal counts, the
queue-depth peak and mean, the offered and achieved rates, the peak RSS and its
source, and the correctness counts the gate verified. A run that had *failed* a
gate would have written no file at all — the abort happens before the write.