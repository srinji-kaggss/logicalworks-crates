# Saturation and overload

The knee at six in-flight bounds, and what twice the knee for 30 seconds does. Part of the [async measurement](README.md).

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
