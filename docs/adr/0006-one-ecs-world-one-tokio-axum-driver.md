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

- None of ADR 0005's steps landed: `EcsBot::world()` is public, no `core_boundary` test, no wake
  handle, no admitted or settled run event reaches the world.
- #734 made `rt::Runtime` the front door and #735 built `rt::Estate` from `Host`, `Supervisor` and
  `Runtime`, neither touching the world: the "tokio and `rt` as the glue" ADR 0005 rejected.
- A process can hold several runtimes: `rt::Runtime`, the host's lazy reactor, the estate's own, and
  one per `rt::block_on` call. ADR 0005's boundary was `review only` until R1, which never landed.

## Decision

1. **One ECS world is the core.** A source moving, an HTTP request arriving, a run admitted or
   settled, an effect dispatched or settled, a timer firing: each is an event written to that world.
   Systems decide; effects leave through the journal (`docs/effect-kernel.md`).
2. **One driver, built on tokio and axum.** A process holds one tokio runtime, owned by the driver.
   axum is the driver's HTTP edge: the world's request system answers each request from its
   tenant's state (R4). `Host`, `Supervisor`, `Estate` and every verb run on that one driver and
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

- **An owned reactor in place of tokio (ADR 0005).** Rebuilds every low-level async function.
- **tokio and `rt` as the glue, as #734 and #735 left it.** The world becomes one consumer among several.
- **A runtime per owner.** Several reactors compete for the cores and none carries the world's events.

## Consequences

- R1 and R2 remove public items (`EcsBot::world`, `Host`'s and `Estate`'s runtimes): major bumps, no shim; axum is a registered `lgwks_deps` edge, default-on (ADR 0003).

## Correspondence

Supersedes ADR 0005. Builds on `docs/bevy-admission.md`, `docs/bot-on-ecs.md`, `docs/effect-kernel.md`.

## Verification

R1: `tests/it/core_boundary.rs` holds decision 4; `a_planted_driver_import_in_the_core_is_refused` is the falsifier.

R2 landed 2026-10-28. `rt::runtime::driver` builds the one runtime; `Host`, `Estate` and `rt::block_on` take its
handle. `one_driver_tests::one_process_holds_one_driver` counts one runtime or none across an estate, a host and a
tick; `sim_one_driver` draws that schedule from a seed across four tenants.

R3 landed 2026-10-29. `ecs::RunFeed` journals a run's `admitted`, then `settled`, rungs; `Host` and `Supervisor` feed
it; `RunFeed::bind` fires the bound system once per settled run. `run_events.rs`: 100 host runs and 100 tasks leave 100
of each, a reopen fires none, a `SIGKILL` after 50 settlements reopens to 50 fires then 0; `sim_run_events`, 512 seeds.
`run_events_cost` p50/p95/p99: 1/1/2 µs, then 3/4/9 µs.

R4 landed 2026-10-29. `rt::edge::serve` runs axum on the driver; `ecs::RequestFeed`, the world's request system,
answers `/t/{tenant}/…` from that tenant's state in 64 lock-striped partitions, on the worker that read it. A queue to
the world thread was built first and measured 143k req/s to axum's 167k, so it did not land (decision 8).
`tests/it/edge.rs` drives two tenants over HTTP; `sim_requests` holds isolation, no lost update over 16 threads, a
panicking handler and the tenant ceiling over 1,000 seeds.
`edge-load`, 256k posts, 16 tenants, 3 rounds, world vs axum (same ceiling) vs Loco 1.2.0: c=64 166–168k,
153–165k, 149–150k req/s; c=1024 161–163k, 158–159k, 145k; CPU 4.0–4.1, 4.4–4.7, 6.3–6.7 s; RSS 8.2, 8.6, 19.2 MB.

R5 landed 2026-10-29. `Observe::cadence` declares `Tick`, `Every(period)` or `Wake`; `WakeHandle::for_source`
wakes one source, coalesced; `rt::edge::serve` drives the world beside axum on its wakes and deadlines.
`sim_wake`, 256 seeds: 0 ticks over a virtual day once idle, one wake polls one source, same seed same hash.
`edge-load` world (sampling every 1 s, woken on rejections) vs axum, 3 rounds: c=64 163–166k vs 161–164k req/s,
c=1024 157–161k vs 154–157k; CPU 4.1–4.3 vs 4.5–4.7 s; RSS 8.4–8.7 vs 8.5–8.9 MB at c=64; errors 0.

R6 landed 2026-10-29: `crates/drishti` imports drishti 1318a20, its nine commits named by SHA in the commit since the push gate refuses a commit with no `Design-Plan:`, and splits its six oversize docs; `nextest -p drishti` passes 105 (107 with `ui`): 6 bundle, 7 on the `drishti` binary's `script!` flows (`tests/cli.rs`), 21 seeded sims against independent models (`tests/sim/units.rs`); G01–G15 stay blocked.

Decisions 6 and 7 stay `review only` until the `script` drift test lands (owner: the executing agent,
2026-10-26). The `doc-governance` lane holds this record's shape.
