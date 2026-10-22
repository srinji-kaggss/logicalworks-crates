# ADR 0002 evidence: the scan route, measured

The measurement section of [ADR 0002](../0002-scan-routed-through-lgwks-ast.md),
kept whole here so the record stays inside the repository's 120-line limit. The
record's decision, alternatives and consequences are unchanged; this file is the
evidence they rest on and is re-measured, not rewritten.

Taken 2026-10-15 on `main` at `eb6d4f4d9` plus the change: release builds, a Mac
shared with other sessions (load average 1.7 to 4.3), one reader per process so a
peak RSS (`/usr/bin/time -l`) is that reader's own.

The gate itself, the old binary against the new, wall time and peak RSS:

| corpus | old `lgwks-deps scan` | new `lgwks-ast scan` |
|---|---|---|
| this repository, 696 files (3 runs each) | 0.60 to 0.68 s, 35 MB | 1.63 to 1.65 s, 11 MB |
| bevy, 1,900 files | 1.46 s, 59 MB | 3.07 s, 13 MB |

**The port is 2.1x to 2.7x slower in wall time** and holds 3.2x to 4.5x less memory,
which no longer grows with the corpus. The cause is the detectors, which walk the
tree several times with per-node allocation: the scan costs 3x the checked parse
alone (below). An index pass or fewer walks would recover most of it and is not
done here. The gate lane takes 1.6 s, far inside its budget.

`rust_readers`, 5 rounds over every file, per-file latency:

| corpus, reader | accepted | p50 / p99 / max | one round | peak RSS |
|---|---|---|---|---|
| this repository (696 files, 12.2 MB), `try_parse` | 696 | 510 us / 4.2 ms / 9.0 ms | 474 ms | 10.0 MB |
| this repository, `scan` | 696 | 1.6 ms / 14.1 ms / 26.3 ms | 1,517 ms | 10.5 MB |
| bevy (1,900 files, 23.6 MB), `try_parse` | 1,871 | 232 us / 4.2 ms / 7.4 ms | 941 ms | 11.6 MB |
| bevy, `scan` | 1,871 | 704 us / 13.3 ms / 36.6 ms | 2,967 ms | 12.9 MB |

ADR 0001 measured the `syn` reader at 1,080 MB over a 107 MB corpus (the vendored
tree it used is gone from `main`, so that row is cited, not repeated).

Hostile nesting, one process per depth (`rust_readers nest`, all exit 0):

| depth | 10,000 | 100,000 | 1,000,000 |
|---|---|---|---|
| `syn` (ADR 0001) | abort, exit 134 | abort | abort |
| `try_parse` | `AstTooDeep` in 3.2 ms | 32 ms | 328 ms |
| `scan` | `AstTooDeep` in 3.2 ms | 31 ms | 330 ms |

`scan_bounds` holds these rows, and holds that a tree at the depth bound is
scanned on a 512 KiB stack in four nesting shapes.
