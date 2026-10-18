# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (5/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 5 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — per-tenant capacity: one tenant cannot starve another (#268) (continued)

- **New: `rt::tenancy`.** `TenancyPolicy` (per-tenant ceiling, per-tenant queue
  bound, supervisor-wide waiting bound, per-tenant weights, each clamped to a
  declared ceiling: `MAX_QUEUE_PER_TENANT`, `MAX_TOTAL_QUEUE`, `MAX_TENANT_WEIGHT`,
  `MAX_WEIGHTED_TENANTS`), `TenancyError`, `SpawnRefused`, and the pure scheduler
  `DeficitRoundRobin` with its `Arrival`, `TryArrival`, `Grant` and `GrantOutcome`.
- **New on `Supervisor`:** `with_tenancy`, `spawn_for`, `run_process_for` (with
  `RunForError`), `tenant_capacity` and `tenants_waiting`. Untenanted calls on a
  tenanted supervisor are charged to one implicit tenant; a supervisor built
  with `new` behaves exactly as before.
- **Fixed before release: a late abandonment miscounted the tenant's queue.** An
  owner that left between the round's liveness check and the delivery had its
  abandonment corrected inside the pump *before* its own report added it, and the
  count saturates at zero, so the report overcounted: `queued_of` read low and a
  tenant could hold five live waiters past a queue bound of four. The shell now
  marks that waiter withdrawn and the owner's report counts nothing;
  `DeficitRoundRobin::note_grant_abandoned` is gone. Found by the new
  `tests/it/sim_tenancy_model.rs` on its first run (seed 9); pinned by the
  white-box `a_late_abandonment_leaves_the_live_count_exact`, which drives the
  shell's own functions in the order the locks force and failed (2 of 3) before
  the fix.
- **Tests.** `tests/it/sim_tenancy_model.rs` model-checks the shipped scheduler
  under an executor that follows the shell step for step — seventeen properties,
  each over all 256 seeds twice. `tests/it/tenancy.rs` measures the live
  supervisor: ten thousand loud submissions parked in the loud tenant's own queue
  and released one controlled completion at a time, against the quiet tenant's
  p99 alone; and a fail-at-once flood four times the neighbour's volume against
  the neighbour's admission time alone (the median of 41 interleaved rounds
  within 10% of the median alone, no sleep before either timed window).
  INV-BOT-151.

### lgwks_std tests — `fs::capability` is swept with hostile seeds

`fs::capability` had 28 unit tests and no seeded sweep. It is the sandbox
boundary that `Dir` promises, so `tests/it/sim_fs_capability.rs` now draws
adversarial inputs per seed and watches a parent directory the handle was
never given, which makes an escape observable rather than assumed absent:

- names spelled from `..`, `.`, `/`, NUL, newline and multi-byte text are
  refused with `InvalidInput` by all eight entry points, exactly when they are
  not one component, and valid names land inside and nowhere else;
- symlinks aimed up, absolute, and through `sub/../..` are reported as
  symlinks with their targets verbatim, and are never opened, truncated or
  descended under the default policy;
- every created file and directory carries no group, world or special bits;
- a held handle keeps writing into the directory it opened after that
  directory's name is moved onto one to four successive replacements;
- 2–16 tenants writing the same names concurrently each read back only their
  own bytes;
- a `try_clone` handle keeps resolving and creating after the original and
  the other clones are dropped in a seeded order.

The `lgwks-deps` sim substrate doc no longer claims `lgwks_std` shares
`seed.rs`: the crate sits below `lgwks_bot` and keeps its own generator.

```
$ cargo nextest run -p lgwks_std --locked --features full
691 tests run: 691 passed, 0 skipped        (36 in sim_fs_capability)
```


### tests — the simulation substrate's own contracts are swept, not assumed

Every `sim_*` family rests on the seed substrate, the sweep and the scratch
guard; nothing checked them. `lgwks_bot/tests/it/sim_substrate.rs` sweeps:

- the sweep refuses exactly the seeds that recorded nothing, swept per band,
  per seed, and against an all-empty and an all-recording control;
- the generator replays exactly and `below`/`between` stay in bounds;
- `chance(p)` is calibrated within five standard deviations;
- the trace frames each record, so re-splitting the same bytes changes the
  receipt;
- concurrent scratch guards never share a directory and vanish on drop.

`lgwks_deps/tests/it/sim_seed_draws.rs` sweeps `pick` reaching every element,
a draw consuming one step whatever the table (empty included), and `receipt`
refusing only an empty trace.

```
$ cargo nextest run --workspace --locked
4339 tests run: 4339 passed (1 leaky), 0 skipped
simulation-evidence: source_visible 1712/3383 = 50.61% (was 50.13%)
```


### lgwks_deps tests — one seed substrate for every workspace simulation

The six `sim_*` policy families drew from their own LCG and hashed their replay
receipts through `std`'s `DefaultHasher`, whose algorithm std leaves free to
change between releases, so a receipt was not stable across toolchains and a
seed meant a different stream here than in `lgwks_bot`, `lgwks_ast` and
`lgwks_std`. They now include `lgwks_bot`'s `sim/seed.rs` and
`sim/seed_helpers.rs` like every other crate:

- `tests/support/sim.rs` is the crate's extension of the shared `Rng`: `pick`,
  `pick_named`, one `coin` (the substrate's `chance(500)`, replacing three
  copies that each read a different bit), and `receipt`, which refuses a run
  whose trace is empty rather than hashing nothing.
- Every receipt is an FNV-1a `Trace` of labelled records.
- Bit-twiddled draws (`>> 20 & 0x3ff` in a retry loop, `>> 33 % 3`, `>> 41 & 1`,
  `next_u64().checked_rem(3)`) are `between`, `below` and `coin`.

```
$ cargo nextest run -p lgwks_deps --locked -E 'not binary(storefront_consumers)'
307 tests run: 307 passed, 0 skipped
$ lgwks-deps scan
OK  scan clean — 390 files, zero findings
```


### lgwks_bot — BREAKING: one way to build an `EffectKey`; every line of the crate and its tests through the guard (9-axis sweep)
