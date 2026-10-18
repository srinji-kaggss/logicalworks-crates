# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (6/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 6 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — BREAKING: one way to build an `EffectKey`; every line of the crate and its tests through the guard (9-axis sweep) (continued)

**Breaking.** `EffectKey::new(run, action, attempt, flow, digest, environment,
epoch)` is removed. A key is built only as
`EffectIdentity::new(run, environment, flow).key(action, attempt, digest, epoch)`:
the three identity fields are the host's to supply once per run, and a
seven-argument constructor let a caller transpose two ids of the same width
with nothing to catch it. Every decoder (`broker`, `journal`, `retry`), the
bench and the examples build keys this way.

The source:

- `task.rs`: the host's reactor owns its `Runtime` directly (the wrapper and its
  `#[expect(dead_code)]` are gone); recorded and declared digests are chosen
  with `match`, not `unwrap_or_else`; every lock goes through the one
  `journal::owner::lock`.
- `rt/supervise.rs`: its private `lock_unpoisoned` copy is replaced by the same
  `journal::owner::lock`.
- `journal.rs`, `journal/frame.rs`: event counts and payload lengths widen
  through `SaturatingFrom`, never `try_from(..).unwrap_or(MAX)`.
- `ecs.rs`, `spec.rs`: the spec tests build their scope with the ECS tests'
  `test_effects` rather than a second copy of it.

The tests (`crates/lgwks-bot/tests`, and the `seed.rs` substrate `lgwks_ast` and
`lgwks_std` include):

- **No suppression and no default.** Every `#![allow(dead_code)]` is removed,
  along with every `unwrap_or`, `unwrap_or_default`, `unwrap_or_else` and
  `expect`. A value the test asserts is present comes through `ok_or(..)?`, and a
  conversion uses `?`. A wait measures `Instant::elapsed` against its budget,
  never a deadline whose overflow defaulted to "now". A clock before the epoch
  names how far before it. `thread::sleep` becomes `park_timeout`.
- **Seven kill-probe binaries are modules of `it`.** These are
  `durable_crash_group_commit`, `durable_crash_observation`,
  `journal_writer_fence`, `owner_epoch_takeover`, `process_escape`,
  `sim_clock_kill` and `task_resume`. Each support fixture now has one includer,
  so nothing in it is dead in one binary and used in another. A probe re-executes
  `it` through the one `probe_command`. The acceptance rows name `it::<module>::`.
- **One copy of each fixture.**
  - Six scratch-directory guards become one, `tests/support/scratch.rs`
    (`new`, `path`, `AsRef<Path>`, removed on `Drop`), shared by `it` and the
    measurement examples. Two of the six named their directory after
    `std::process::id()`: a reused pid reopened a leftover directory and read
    its stale pid files. The compile probe's leaked on any error between
    create and remove. `check-std-first.py`'s exemption for the pid-named one
    is gone with it.
  - `FakeGh` and the process probe no longer name a fixture `0` when the
    entropy source fails (`map_or(0, ..)`); the failure refuses the fixture.
  - Three `percentile`s become one (`None` for an empty sample, never a zero).
  - The spec sweep uses `spec_materialize`'s `Counter`, and a second copy of the
    in-memory effect scope is removed.
  - `sim_review_path`'s `Gh` binding is `FakeGh::binding`.
  - `wrong_identity_evidence` rewrites one key field through `EffectIdentity`,
    `at_epoch` and `with_attempt`, not a struct mirroring the key.
- **Every fixture compiles under exactly the features its callers need.**
  The parked-device instrument is `support/liveness.rs` (`rt`); the durable
  resume scaffolding is `support/resume.rs` (`script` + `ephemeral`); the
  compile probe's positive control and lint pass are `script`-only; `lgwks_ast`
  includes the seed substrate under `lang-rust`. `attempt_key_at` was one
  family's, and that family now moves the shared key with
  `EffectIdentity::at_epoch`.
- **The fault schedule holds only faults something injects.** `Faults` drew
  `disk_refuse`, `disk_tear`, `crash_at` and `concurrency`, and no family ever
  carried them out, so a schedule could claim coverage of a disaster that never
  happened. They are removed, along with the family that checked the crash tick
  of a crash nobody performed; the determinism family compares whole schedules
  through a derived `Eq`.
- **A replay that recorded nothing is a failure.** `sim::sweep` refuses a seed
  whose run left the trace empty, because its hash is the empty trace's
  whatever path it took. Three families had such seeds:
  `forced_refresh_matches_the_schedule` when nothing was armed,
  `a_refresh_that_never_lands_stays_marked` (which never touched its `Sim`),
  and `retry_budget_holds` on an empty list. Each now records what it decided.
  `two_tenants_on_one_digest_stay_isolated` checks a stranger that is never
  in the tenant set, so the claim holds for the draw that chooses every tenant.
- **No doubled blocks.** Sixteen `{ { let refusal = ..; return refusal; }; }`
  blocks are flattened, one of them in `crates/lgwks-std/src/http.rs`. A `for`
  statement carrying more than three `?` became a named per-iteration function.
- **Stable replay receipts.** `sim_review_path` and `sim_review_pr` hash through
  the substrate's FNV-1a `Trace`, not `DefaultHasher`, whose algorithm std leaves
  free to change between releases.
- **The seed substrate.**
  - `Trace::record_u64` and `Trace::record_count` wrote the same bytes, so they
    are one `record_number`.
  - `Trace::len` (only ever printed when it was zero) is removed.
  - `chance` and `is_empty` move to `sim/seed_helpers.rs`, which is included
    beside `seed.rs` by every target that calls them.
  - The seed bands move to `sim/band.rs`, so `sim_review_path` includes the
    bands and the generator and not the journal rig.

```
$ guard-files <every changed .rs file>
98 file(s) checked, 0 finding block(s)
$ cargo check --locked -p lgwks_bot --all-targets {--all-features | default | --no-default-features | --no-default-features --features process}
0 errors, 0 warnings in each
$ clippy: workspace, std/bot/ast all-features and no-default-features, deps tokio-full, bot process-only, bench/async — all -D warnings, all clean
$ lgwks-deps scan
OK  scan clean — 390 files, zero findings
$ ci_local.py --lane simulation-evidence
nextest_sim=2893 nextest_total=4307 nextest_percent=67.1697%; source_visible_sim=1680 source_visible_total=3351 source_visible_percent=50.1343%
$ cargo nextest run --workspace --locked -E 'not binary(storefront_consumers)'
4305 tests run: 4305 passed (1 leaky), 0 skipped        [215 s wall]
$ cargo nextest run -p lgwks_bot --all-targets --locked --features full --profile ci -E 'not test(saturation_r32_tier)'
3947 tests run: 3947 passed (3 slow, 3 leaky), 6 skipped [337 s wall]
$ cargo nextest run -p lgwks_bot --all-targets --locked --no-default-features
2667 tests run: 2667 passed, 0 skipped                   [238 s wall]
$ cargo nextest run -p lgwks_bot --locked --features full --test sim_review_path -E 'test(saturation_r32_tier)'
3 tests run: 3 passed (tiers 100 / 1,000 / 10,000)      [47 s wall]
$ cargo nextest run -p lgwks_std --all-targets --locked --features full
655 tests run: 655 passed, 0 skipped
```

The `full` lane's 337 s is over the five-minute gate budget; it was 331–385 s
before this change, and that is still open.

### lgwks_std — the nine-axis sweep: the ceiling assertion asserts, so the scan gate sees no unlogged refusal (9-axis sweep)
