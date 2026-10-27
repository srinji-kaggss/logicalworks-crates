# The open-loop driver and the refusal door

Coordinated-omission-correct arrivals, the flood split, and `Supervisor` refusing past the knee. Part of the [async measurement](README.md).

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

`sim_openloop.rs` is the seeded model of that driver and **17 of its tests are
deterministic simulation tests**: one seed drives arrival jitter and body
length, a failure prints its seed, and the same seed replays to the same trace
hash — asserted, on a hash that folds every scheduled event including every
refusal. Four more pin the CPU door's own arithmetic: the hash replays per
(seed, rounds) and diverges across seeds, pinned rounds declare their
calibrated cost, the calibration is a positive finite cost, and the kind flag
parses strictly (the calibration reads a wall clock, so that one test asserts
a range, not a value).

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- --sim-open-loop
```

## Body kinds — timer, then CPU

Timer bodies park: `--saturation` and `--refusal` run `--body-kind=timer` by
default, so the scheduler is the binding constraint and the cores idle. CPU
bodies burn: `--body-kind=cpu --cpu-rounds=N` replaces the park with N fixed,
seeded splitmix64 hash rounds per task (`support.rs`), so the cores are the
binding constraint and the knee measures CPU contention. Both sides burn the
same rounds from the same seed and each body records exactly one work unit,
so the conservation gate (`work_units == completed`) and the cross-side
fairness gate hold unchanged; the hash state is spent through `black_box`,
not counted.

The rounds are fixed, not derived per bound: a bound-derived CPU body would
burn seconds per task at the wide bounds. The declared cost is the rounds'
calibrated cost and the ladder is derived from the workers' capacity, not the
ceiling's: a CPU ceiling wider than the worker count never binds. Mismatched
flags are refused (`--cpu-rounds` with the timer kind, `--body-kind=cpu`
without `--cpu-rounds`, `--cpu-rounds` beside `--body-micros`): each would run
one door under another door's name.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --saturation --window=1 --body-kind=cpu --cpu-rounds=300000 --workers=2 \
    --json=bench/async/saturation-cpu.json
```

The recorder survives the deep queues a CPU sweep builds past its knee: every `u64` has a
bucket, and samples past the 34 s ceiling land in the top one and count as clamped.

## The flood split — a neighbour's cost under a fail-at-once flood

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- --flood-split --rounds=41
```

The wall-clock half of INV-BOT-151 (`flood.rs`). Each paired round runs the
neighbour alone and under four attacker submissions per neighbour task, back
to back in alternating order, on a 64-permit pool with a 32-permit ceiling,
and prints the neighbour's own throughput and its own arrival-decision time as
per-mille of the arm alone (p50/p95/p99/min/max). It asserts nothing: the gate
is `sim_tenancy_flood`, which checks the claim exactly in virtual time, and a
ratio of two arms on a shared host reads the host (#375). Two runs on a quiet
macOS arm64 host read throughput p50 1018‰ / 986‰ and decision p50
1053‰ / 1010‰, 0.70 s wall, 4.2 MB peak RSS.

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
