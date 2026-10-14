# Tasks and the harness-owned world

Each task's contract and hidden oracle clauses, and the instrumented `support/` crate the solutions call. Part of the [authoring benchmark](README.md).

## The tasks and their hidden oracles

Each `tasks/<task>/prompt.md` states the signature and the contract in
API-neutral words. Each `tasks/<task>/oracle.rs` is an integration test copied in
as `tests/oracle.rs`; it asserts on the harness instrumentation, one
`#[test]` per contract clause.

- **`aggregate`** — `solve(ids, fetch, deadline)`. Clauses: empty input → `Ok(0)`
  (1); at most 4 fetches in flight (2); a failure names its id (3); no fetch
  starts after the failure is observed (4); the overall deadline → `Deadline`
  (5); dropping the future leaves no fetch live (6).
- **`pipeline`** — `solve(stage, deadline)`. Clauses: success publishes the
  combined artifact (1); a failed `fetch_a` stops combine and publish and names
  the stage (2); the same for `fetch_b` (3); a slow stage → `Deadline` and no
  publish (4); dropping the future leaves no stage live (5).
- **`recovery`** — `recover(world, store_dir, deadline)`. Clauses: the work
  completes and the total is the sum (1); a second call finishes it without
  re-running a completed unit (2); the effect is applied exactly once across any
  number of calls (3); the overall deadline → `Deadline` with no unit left live
  (4); dropping the future leaves no unit live (5). The correct solution needs a
  host with a durable run store installed, a *derived* run identity that both
  attempts agree on, a task helper both attempts run, and a read-before-apply on
  the effect ledger.

  **`recovery` has no old-API arm, and the reason is architectural.** The old
  `lgwks_bot::rt` surface has no durable run store, no `remember`, no run
  identity and no resume, so it keeps no record of a completed unit to consult
  and cannot express clause 2 at all. A cell asking for one would measure a
  model guessing at a capability the sheet does not offer. The omission is
  recorded in every run's `summary.json` under `protocol.skipped_cells` rather
  than left as a missing row.

- **`capture`** — `solve(argv, head_limit, deadline)`. Clauses: a small output
  returns its exact head and total (1); a 70 MB flood stays within the
  four-kibibyte ceiling with the exact total and the truncated flag (2); an
  unstartable program is a typed `Spawn` refusal (3); a 300 ms deadline stops
  the whole group (4); a completed run leaves no orphaned grandchild (5);
  dropping the future leaves no descendant and no new zombie, promptly (6).
  Unix-only: every clause forks `sh` grandchildren and reads the process
  table, under per-process markers so sibling trials never match each other.

  **`capture` has no old-API arm, for the same architectural reason in the
  other direction.** The old sheet offers no supervised process spawn, so the
  task cannot be written from it. The omission is recorded under
  `protocol.skipped_cells` like `recovery`'s.
- **`tenant-cache`** — `solve(tenant, resource, token)`. Clauses: the same
  request resolves the same bytes twice (1); two tenants on one resource stay
  isolated (2); a wrong token is refused and caches nothing (3); another
  tenant's token is refused (4); a thousand mixed calls across ten resources
  and both tenants all resolve their own value (5); dropping mid-flight leaves
  the cache usable (6).
- **`durable-retry`** — `solve(effect_dir, key, deadline)`. Clauses: a fresh
  call applies once and returns `applied:{key}` (1); the status is never
  `Unknown` (2); waiting past the deadline is typed (3); dropping mid-wait
  applies nothing twice (4); `SIGKILL` mid-effect (Unix-only, in a child
  running the phase-1 test) followed by an in-process recovery applies exactly
  once in total and returns `recovered:{key}` (5).

The oracles are deterministic: no counter is asserted on a wall-clock sleep. The
only waits are short ones that let a future begin, plus the 200 ms the drop
clause allows a cancelled body to be counted out. The `recovery` oracle's
interruption drops the first attempt's future once the requested number of unit
bodies has recorded — dropping rather than killing is deliberate, and is stated
at the call site: a killed process leaves the same durable evidence, and
arranging one would change what is measured from "does a resume work" to "does
this runner manage a subprocess".

## The harness-owned world: `support/`

`ai_task_support` is the instrumented world the solutions call. It is owned by
the harness, never by the solution.

- `Fetcher` (`Clone + Send + Sync + 'static`): `async fn fetch(&self, id) ->
  Result<u64, FetchError>`, returning `id * 3` after an async delay drawn from a
  seeded plan, failing for configured ids. Instrumentation via atomics: `live`
  (decremented by a drop guard, so a cancelled or aborted fetch counts out too),
  `max_live`, `started`, `started_after_first_failure`, `completed`, `failed`.
- `Stage` (`Clone + Send + Sync + 'static`): `async fn run(&self, name)` for
  `StageName::{FetchA, FetchB, Combine, Publish}`, with configurable failing and
  slow stages and the same live-count instrumentation. `Artifact::value()`;
  `Published: From<Artifact>`.
- `recovery::World`: the `recovery` task's durable units. `async fn unit(&Scope,
  u32) -> Result<u64, UnitError>` counts its own body *inside* the `remember`
  closure, so a replayed record does not increment the count — a replay and a
  re-run return the same value, so only the count separates them.
  `recovery::Ledger` is the effect: `applied(name)` is the read,
  `apply(name)` counts every call including duplicates, and it refuses nothing —
  a harness that refused a duplicate would make the no-duplicate clause
  unfalsifiable, which is what that clause exists to detect.
- Constructors for the oracle to configure fault plans: `failing(..)`,
  `delay(..)`, `uniform_delay(..)`, `slow(..)`.

The delay plan is a seeded SplitMix64, so a run replays exactly. Sleeping is
`lgwks_bot::rt::time::sleep` throughout; there is no `std::thread::sleep`.
