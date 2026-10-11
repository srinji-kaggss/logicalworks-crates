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

## The host every number was taken on

**Apple M5 Pro, 15 cores (5 performance, 10 efficiency), 24 GB, macOS 27.0,
rustc 1.99.0, `opt-level=3`, `lto=true`, `codegen-units=1`. One host, one run:
absolute numbers, not a cross-platform claim.**

**This host is shared, and it was not idle.** Its one-minute load average ran
from 2 to 97 across this work, and the rig now reads and prints it with every
run, because the load is not a footnote: a scenario table taken with three
other builds running on it reported the **baseline** at 1.08 ms where an idle
host reported 0.35 ms — a 3x shift in the *control* leg of a paired
comparison, which moves a ratio in a way no reader can see without the number.
Each table carries the load it was taken at. Absolute latencies taken at
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

## Where each measurement is

| Document | Covers |
|---|---|
| [The fairness gate](fairness.md) | Why the rig is its own workspace, the gate that refuses a ratio on unequal work, and the negative control that proves it can refuse. |
| [The open-loop driver and the refusal door](openloop.md) | Coordinated-omission-correct arrivals, the flood split, and `Supervisor` refusing past the knee. |
| [Saturation and overload](saturation.md) | The knee at six in-flight bounds, and what twice the knee for 30 seconds does. |
| [In-flight tiers and the concurrency ladder](inflight.md) | That many tasks concurrently admitted, against throughput through 64 slots. |
| [Allocation attribution](allocations.md) | Every allocation window priced separately, and why the latency target is a different target. |
| [Closed-loop results and the workload matrix](results.md) | The scenario table, every §3 matrix row with its receipt, and the committed records. |

## What this says, stated before the reader draws a conclusion

**At a service time of 3.2 ms or more, `Supervisor` and raw Tokio admit work at
the same offered rate at every in-flight bound from 64 to 131,072**, and past the
knee both refuse at exactly the declared bound with the served p99 unchanged.
The facade's cost is a **fixed per-task** cost — 3.61 allocations and a fixed
amount of bookkeeping per spawn — and it is amortised away as the body lengthens
and as the ceiling widens. The [closed-loop table](results.md) is the regime where that
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
