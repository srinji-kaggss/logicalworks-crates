# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (14/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 14 of 20. [Changelog index](../../CHANGELOG.md)

### The saturation curve, the open-loop driver and the in-flight tiers (#269) (continued)

`bench/async` gained an open-loop driver: offered rate as a parameter, each
arrival's latency measured from its **intended** start rather than from when the
generator reached it, and a HdrHistogram-style log-bucketed recorder implemented
in the bench crate with no new published-crate edge. Latency is bucketed at
1/256 relative error at every magnitude, so a 40 ns body and a 4 s stall are both
representable; min, max and total are exact and samples past the ceiling are
counted rather than folded in.

Measured on an Apple M5 Pro (15 cores, 24 GB, macOS 27.0, rustc 1.99.0):

- **A saturation curve at six in-flight bounds** — 64, 1,024, 10,000, 16,384,
  100,000, 131,072 — with the knee declared on both `Supervisor` and raw Tokio.
  **The two declare the same knee at every bound**, agreeing to within 0.5% on
  achieved rate at five of six. The body cost is derived per bound so every
  ceiling's declared capacity lands on one declared target; a constant body cost
  put the wide ceilings above what an in-process generator can offer, and every
  rung above the first then measured the generator's backlog rather than the
  ceiling's.
- **Past the knee `Supervisor` refuses rather than growing.** At 320,000
  arrivals/s, a bound of 16,384 admitted **exactly 16,384** tasks, 100,000
  admitted **exactly 100,000**, 131,072 admitted **exactly 131,072** — the
  declared bound to the task, on both sides — refusing and counting 59% of the
  offer, with the p99 of what was admitted still one service time.
- **1,048,576 tasks concurrently admitted** and all completed, on both sides:
  peak RSS **2,039 bytes per in-flight task** for the facade against the
  baseline's 2,595, 2.14 GB resident for the process.
- **Overload and recovery**: 30 s at twice the knee built a 229,554-arrival queue
  and took the served p99 from 6.7 ms to 11.4 s with nothing refused and nothing
  lost (offered = admitted = completed, gated); the facade drained in 0.023 ms
  against 4.801 ms, and both sides were back inside their own baseline p99 at the
  first recovery window, 500.7 ms and 505.1 ms.
- **17 seeded simulation tests** for the driver's schedule arithmetic, all with a
  trace-hash replay assertion, the ladder bracketing its knee at every declared
  bound and body cost, both sides offered the same arrivals, the derived body
  putting every ceiling at the declared target, and the knee budget separating
  the regimes at 1x and 16x capacity.

**The 1–2 vCPU / 1–2 GB VPS profile is NOT measured** and is stated as such in
`bench/async/README.md` and `docs/production-readiness.md` §4.2. macOS exposes no
cgroup, no `taskset`, no `taskpolicy` CPU set and no `cpulimit`; the closest
runnable thing, `--workers=2`, produced knees identical to `--workers=15` because
every body in the sweep is a timer, so a thread count is not a vCPU count and
this workload would not separate them.

### Acceptance evidence is now executable (#271)

Every T01–T36 falsifier row in `docs/orchestration-acceptance.spec.md` names
the tests that address it, in `docs/acceptance/t-rows.toml`. That claim was
previously unfalsifiable; `scripts/acceptance-receipts.py` now runs exactly
those tests through one anchored nextest filter and records each row's outcome
in a SQLite database under the state directory, one row per revision, row, test,
platform and feature set.

The receipt can lower a row's claimed state and never raise one, so a green run
cannot promote `present` to `exercised`, and it exits non-zero when a named test
is absent, failing, or a row claims more than the run shows. The one promotion,
`accepted`, requires the receipt's revision to be the head being rendered. The
spec's per-row table is rendered from the database for one exact revision between
`<!-- acceptance-table: start -->` markers, `--check` fails when the committed
table disagrees with it, `--export` writes one JSON artifact for a CI upload, and
`--test` runs the generator's own fifteen regression cases. No new dependency:
the database is Python's standard-library `sqlite3`.

CI builds the receipt from what the suite already ran. The `ci` nextest profile
writes a JUnit report per shard, and a job with no Rust toolchain merges the four
with `--from-junit` in about 0.2 s, rather than executing the 215 named tests a
second time.

Every row now also answers to its own id in a test name: 15 new row-addressed
tests across `crates/lgwks-bot/tests/it/t_rows.rs` and
`crates/lgwks-bot/tests/it/sim_t_rows.rs`, so
`cargo nextest list --workspace -E 'test(/_t07$/)'` says which tests address a
row without reading the map. Six of them are seeded sweeps replay-checked against
their trace hash. They are new tests rather than renames precisely so that every
existing citation of an existing test keeps resolving. No crate's public API
changed.

### lgwks_ast — the parse had no time bound, and the walk was quadratic (#277)

Two bounds the crate already claimed, and the numbers behind them.
