# ADR 0006: one ECS world, one driver on tokio and axum

Status: **accepted**, 2026-10-26. Supersedes ADR 0005.

Stakeholders: the Director; the executing agent of each step below; a consumer of `lgwks_bot` outside Logical Works, when one exists
Concerns: lgwks_bot drifted from its intent of ECS driving everything; the tree builds several runtimes where one is meant; ADR 0005 planned to rebuild the async layer that tokio already provides
Requirements: R1, R2, R3, R7, R11
Viewpoint: the module-dependency viewpoint — boxes are `lgwks_bot` modules and driver crates, arrows are `use` edges, and the rule is which arrows may exist and how many runtimes a process holds
Elements and relations: the ECS world in `ecs/`, the core modules (`verb`, `cap`, `gate`, `effect`, `journal`, `error`, `clock`, `spec`), the one driver (`rt`, built on tokio and axum), `task`, `script`, `domain`; relations: `driver wakes world`, `world emits an effect through the journal`, `core does not import the driver`, `one driver per process`

## Context

The Director, 2026-10-26: "I already know lgwks does not align to my original intent of ecs driving
everything"; "i do not know why you guys keep doing such massive spec drifts"; "no, its one ecs world,
with one driver based on tokio and axum so you dont have to rebuild every low level async function";
"script as the higher level orchestration and runtime framework so AI can code in a deeply non
technical and business logic format and code compiles to everything needed. So bot was meant to be
just the core boilerplate"; "if u use it, you dont have to think of shitty things like idp, and
security and speed optimizations ... its like loco.rs on drugs"; "its also meant to provide
protection against this very spec drift we see"; "tokio and axum are only for pieces where
rebuilding does not make sense, its not an all things escape hatch either"; "from 9 axis, all are imp
with speed being the biggest blindspot".

Read from the code on 2026-10-26:

- None of ADR 0005's steps landed. `EcsBot::world()` is still public (`ecs/bot.rs`), there is no
  `core_boundary` test, no wake handle, and no admitted or settled run event reaches the world.
- After ADR 0005 was accepted, #734 made `rt::Runtime` the orchestration front door and #735 assembled
  `rt::Estate` from `Host`, `Supervisor` and `Runtime`. Neither touches the ECS world. That is the
  design ADR 0005 rejected ("tokio and `rt` as the glue").
- A process can hold several runtimes at once: `rt::Runtime`, the host's lazily built reactor
  (`task::host`), the estate's own runtime, and the current-thread runtime `rt::block_on` builds per call.
- ADR 0005's Verification called its boundary `review only` until R1 landed. R1 never landed, so
  nothing refused the drift. The missing gate is the cause.

## Decision

1. **One ECS world is the core.** A source moving, an HTTP request arriving, a run admitted or
   settled, an effect dispatched or settled, a timer firing: each is an event written to that world.
   Systems decide; effects leave through the journal (`docs/effect-kernel.md`).
2. **One driver, built on tokio and axum.** A process holds one tokio runtime, owned by the driver.
   axum is the driver's HTTP edge: a request becomes an event in the world and its response is the
   effect the world settles. `Host`, `Supervisor`, `Estate` and every verb run on that one driver and
   never build a runtime of their own.
3. **tokio and axum only where rebuilding makes no sense.** tokio's reactor, timers and I/O and
   axum's router and extractors are used as they are, behind `rt`; ADR 0005's owned readiness loop
   is withdrawn. They are not an escape hatch: orchestration, admission, scheduling, state and
   anything on the hot path stay ours in the world, and each driver piece we keep is beaten on a
   matched workload or stays. Zoned and calendar time is jiff through `lgwks_deps` (#740) beside
   the owned `lgwks_std::time`; the async wait is the driver's timer, which jiff does not provide.
6. **`script` is the framework above the core.** `bot` is the core boilerplate: the world, the
   journal, the gate. `script` is the one orchestration and runtime framework an AI agent writes:
   business logic in a non-technical form that compiles to everything it needs, with identity,
   security and speed already decided by the framework. It is the one embedding surface, not one
   of several (script, an API framework and an SDK are not three doors).
7. **`script` is the drift guard.** What `script` code may say is the spec; code written through
   it cannot leave the architecture this record fixes, the way `core_boundary` holds the core.
8. **Speed is the axis that decides.** Every step prints event-to-effect p50, p95 and p99 and peak
   RSS before and after, against the alternative it replaces; a slower step does not land.
4. **The core does not import the driver.** `ecs/`, `verb`, `cap`, `gate`, `effect`, `journal`,
   `error`, `clock` and `spec` name no tokio, axum, `rt`, `task`, `script` or `domain` item. A test
   holds that, so the next drift fails the build instead of passing review.
5. The repository stays unstable as ADR 0005 decision 5 set it: a breaking change bumps the major
   version and ships no shim.

### The order

Each step lands alone and green.

| # | Change | Falsifier |
|---|---|---|
| R1 Seal | Remove `EcsBot::world()`. `tests/it/core_boundary.rs` fails if a file outside `ecs/` names bevy, or a core module names tokio, axum, `rt`, `task`, `script` or `domain`. Every edge it finds is cut in the same change. | A planted `use crate::rt` in `gate.rs` fails the test. |
| R2 One driver | `rt` owns the one tokio runtime. `Host`, `Estate`, `Supervisor` and `rt::block_on` take its handle; the host's lazy reactor and the estate's private runtime are removed. | A process that builds an estate, runs a host and ticks a bot holds exactly one tokio runtime, counted by its worker threads. |
| R3 Events in | `Host` and `Supervisor` write `admitted` and `settled` into the world through the journal and wake it. | 100 runs leave 100 `admitted` and 100 `settled` events; a system bound to `settled` fires 100 times; kill and restart replays without refiring. |
| R4 axum edge | axum, registered under `lgwks_deps`, is the driver's HTTP ingress: each request is a world event, each response a settled effect, per tenant. | Two tenants' requests on one route never read each other's state; a load test prints p50, p95, p99, error rate and peak RSS. |
| R5 Wake | Sources declare `Every` or `Wake`; the world ticks on a wake or a deadline and polls only woken sources. | Idle runs 0 ticks over a stated virtual span; one wake polls exactly one source; same seed, same trace hash. |
| R6 Drishti | As ADR 0005 R4. | As ADR 0005 R4. |

## Alternatives rejected

- **An owned reactor in place of tokio (ADR 0005).** Rebuilds every low-level async function for no
  gain the Director asked for; tokio is the driver.
- **tokio and `rt` as the glue, as #734 and #735 left it.** The world becomes one consumer among
  several and stops driving anything.
- **A runtime per owner, as today.** Several reactors compete for the same cores and none of them
  carries the world's events.

## Consequences

- R1 and R2 remove public items (`EcsBot::world`, the private runtimes behind `Host` and `Estate`):
  major bumps, no shim.
- axum becomes a registered `lgwks_deps` edge, on by default under ADR 0003.
- The `--no-default-features` build without tokio, which ADR 0005 protected, is now a driver choice
  to be re-decided in R2, not a constraint on it.

## Correspondence

Supersedes ADR 0005. Builds on `docs/bevy-admission.md`, `docs/bot-on-ecs.md` and
`docs/effect-kernel.md`; consistent with ADR 0003 and held to ADR 0004.

## Verification

`tests/it/core_boundary.rs` holds decision 4 (R1, landed with this record:
`the_core_names_no_driver_and_only_ecs_names_bevy` reads every production file,
`a_planted_driver_import_in_the_core_is_refused` is the falsifier).

Decision 2 is held by R2, landed 2026-10-28. `rt::runtime::driver` builds the one runtime on first
use; `Host`, `Estate` and `rt::block_on` take its handle, and the host's reactor and the estate's
runtime are gone. `rt::runtime::one_driver_tests::one_process_holds_one_driver` builds an estate,
runs a host, ticks a bot and blocks on futures, and counts the runtimes `Builder::build` made on
that thread: one or none. `sim_one_driver::sim_every_door_in_any_order_shares_one_driver` draws
that schedule from a seed across four tenants. Counting builds replaces counting worker threads,
because a build is what makes a runtime. The estate smoke binary (`src/bin/estate.rs`) runs on the
estate's driver. Tests that need one thread build their own current-thread runtime by name
(`Builder::current_thread`, crate-private). The driver stays inside `rt`, behind the `rt` feature;
R2 changes no feature, so a build without `rt` compiles what it compiled before.

Decisions 6, 7 and 8 stay `review only` until the `script` drift test and R5's printed numbers
land, owner the executing agent of each step, dated 2026-10-26. The `doc-governance` lane holds
this record's shape.
