# The 2 vCPU / 2 GB VPS profile

The matrix the [README](README.md) reports, run again under the hyperscale
axis's VPS profile (#269): `cargo run --locked -p lgwks_deps --no-default-features
--bin linux-container-tests -- vps` builds the bench uncapped, then runs every
door in a Linux container capped by cgroup. The cap, the doors and where each
record goes are data, [`vps/profile.toml`](vps/profile.toml); the records are in
[`vps/`](vps/).

## The cap, as the container read it

```
nproc                       4            (the VM's cores; the cap is the cgroup's)
/sys/fs/cgroup/cpu.max      200000 100000   = 2 CPUs
/sys/fs/cgroup/memory.max   2147483648      = 2 GiB, swap capped to the same
```

OrbStack on the M5 Pro host, `rust:1.99.0-bookworm`, release build, the
machine's cargo config. Taken 2026-10-11 13:22–13:38Z at host load 2.8, with
nothing else compiling: 947.8 s wall for the whole matrix.

## Closed loop: the facade's cost per task, ~1 µs body

| scenario | tasks | bound | facade p50 (s) | facade p99 (s) | base p50 (s) | base p99 (s) | ratio | 95% CI |
|---|---|---|---|---|---|---|---|---|
| quiet-async-bot | 256 | 8 | 0.000234 | 0.000266 | 0.000179 | 0.000579 | 1.31x | [1.24, 1.38] |
| high-fanout | 2048 | 32 | 0.001312 | 0.001915 | 0.000945 | 0.001020 | 1.39x | [1.39, 1.44] |
| at-capacity | 512 | 4 | 0.000642 | 0.000739 | 0.000508 | 0.001009 | 1.27x | [1.19, 1.30] |
| single-permit | 512 | 1 | 0.001446 | 0.001719 | 0.001266 | 0.001546 | 1.14x | [1.12, 1.17] |

Every ratio is inside #269's 2x target under the cap; on the uncapped
15-core host the same table reads 1.36x–4.49x ([results.md](results.md)).
Fewer cores mean less cross-core wake traffic for the baseline to win on.

| tasks | facade p50 | facade p95 | facade p99 | base p50 | base p95 | base p99 |
|---|---|---|---|---|---|---|
| 100 | 0.000101 | 0.000156 | 0.000167 | 0.000047 | 0.000066 | 0.000067 |
| 1,000 | 0.000649 | 0.000901 | 0.000949 | 0.000476 | 0.000667 | 0.000669 |
| 10,000 | 0.006092 | 0.006422 | 0.006450 | 0.004790 | 0.004957 | 0.004975 |
| 100,000 | 0.065839 | 0.110955 | 0.119972 | 0.055766 | 0.070260 | 0.073044 |

Peak RSS of the closed-loop process: 3,829,760 bytes.

## Saturation knees (SLO p99 ≤ 50 ms, 1 s per point)

| door | bound | side | knee offered/s | achieved/s | p99 at knee | refused at top |
|---|---|---|---|---|---|---|
| backpressure | 64 | facade | 5,000 | 4,977 | 10.5 ms | 0 |
| backpressure | 64 | baseline | 5,000 | 4,974 | 11.6 ms | 0 |
| backpressure | 1024 | facade | 20,000 | 18,376 | 86.4 ms | 0 |
| backpressure | 1024 | baseline | 5,000 | 4,750 | 72.0 ms | 0 |
| refuse | 64 | facade | none on this ladder | 0 | — | 306,419 |
| refuse | 64 | baseline | 5,000 | 4,968 | 10.8 ms | 306,199 |
| refuse | 1024 | facade | 5,000 | 4,752 | 56.7 ms | 300,544 |
| refuse | 1024 | baseline | 5,000 | 4,748 | 58.3 ms | 300,544 |

The refuse door at bound 64 has no facade knee: at the ladder's lowest point
the facade refused 1 arrival where the baseline refused 0, and a knee is
defined as zero refusals. That one refusal is the result, not rounded away.

## Overload and recovery (bound 64: 5 s at 5,000/s, 30 s at 20,000/s, 90 s at 5,000/s)

| side | overload p99 | drained in | p99 back to baseline |
|---|---|---|---|
| facade | 12,431.9 ms | 5.880 ms | after 508.8 ms |
| baseline | 13,237.2 ms | 5.749 ms | **did not return** within the 90 s phase |

## In-flight tiers: all of them fit under 2 GiB

| tier | side | reached | p99 | peak RSS |
|---|---|---|---|---|
| 100 | facade / baseline | 100 / 100 | 3 / 5 ms | 4 / 4 MB |
| 1,000 | facade / baseline | 1,000 / 1,000 | 5 / 4 ms | 6 / 6 MB |
| 10,000 | facade / baseline | 10,000 / 10,000 | 13 / 9 ms | 23 / 23 MB |
| 100,000 | facade / baseline | 100,000 / 100,000 | 86 / 40 ms | 193 / 197 MB |
| 1,048,576 | facade / baseline | 1,048,576 / 1,048,576 | 894 / 418 ms | 1,987 / 2,034 MB |

A million concurrently admitted tasks ran inside the 2 GiB cap on both sides
with no OOM kill; the facade's peak was 47 MB lower. Its p99 at the widest
tiers is about twice the baseline's: under two cores, the facade's per-task
bookkeeping is on the admission path that every one of a million tasks takes.

## Saturation with CPU bodies: the knee where the cores bind

The `saturation-cpu` door (`--saturation --window=1 --body-kind=cpu
--cpu-rounds=300000 --workers=2`, record
[`saturation-cpu.json`](vps/saturation-cpu.json)): every body burns 300,000
seeded hash rounds and records one work unit, so both sides do identical work
and the fairness gate holds unchanged. Taken 2026-10-26 (3.36 ns/round, a
1,008 µs body, declared capacity 1,984/s); the suite around it ran under
extreme host load, so the absolutes are load-depressed and the paired columns
are the finding.

| bound | facade knee offered/s | achieved/s | p99 at knee | baseline knee | baseline achieved/s |
|---:|---:|---:|---:|---:|---:|
| 64 | 496 | 496 | 6.0 ms | 496 | 496 |
| 1,024 | 496 | 496 | 5.8 ms | 496 | 496 |
| 10,000 | 1,984 | 1,947 | 44.0 ms | 1,984 | 1,966 |
| 16,384 | 1,984 | 1,981 | 14.8 ms | 1,984 | 1,977 |
| 100,000 | 1,984 | 1,981 | 16.9 ms | 1,984 | 1,979 |
| 131,072 | 1,984 | 1,977 | 14.5 ms | 1,984 | 1,975 |

The facade and the raw baseline declare the same knee at every bound, to the
arrival: under CPU contention the facade's bookkeeping does not move the
knee (p99 at knee 0.9–1.9x). No arrival refused on any CPU point; door peak
RSS 50.4 MB facade against 50.2 MB. The ladder is derived from the workers'
capacity, not the ceiling's: a CPU ceiling wider than the worker count never
binds.

## What this does not claim

- One run, one host's VM, OrbStack's scheduler under a cgroup quota; a real
  2-vCPU VPS also differs in cache, memory bandwidth and noisy neighbours.
- `nproc` reported 4 on 2026-10-11 and 2 on 2026-10-26 because the quota
  limits CPU time, not visible cores, so Tokio started one worker per visible
  core; a VPS with 2 visible cores would start two.
- The 2026-10-26 suite ran under extreme host load (53.10 at start) and was
  killed at the 1 M in-flight tier, so its timer rows are not republished and
  the CPU door's absolute knees are load-depressed; the paired
  facade-versus-baseline columns saw the same load and stand.
