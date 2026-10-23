# ADR 0003: the bot core is an event-driven ECS, and drivers sit below it

Status: **accepted**, 2026-10-23. Plan: design-gate `b97012375d58a4a3`. Immutable: this file is
never edited; a change is a new ADR that supersedes it.

## Decision

1. The core of `lgwks_bot` is one ECS world. A source moving, an effect dispatched or settled, a
   run admitted or finished, a timer firing: each is an event written to that world. Systems react
   to events, and effects leave through the journal (`docs/effect-kernel.md`).
2. tokio and `rt` are drivers below the core. They carry events in and effects out, and the core
   never imports them. They are not the glue, and the code does not make them the glue.
3. A poll is an adapter. A source may declare that it is woken instead of polled; the poller is one
   driver that wakes a source on a timer.
4. Engines are admitted today and ours later. `bevy_ecs` stays behind `ecs/`, and each engine is
   replaced when ours beats it on a matched workload with printed numbers.
5. The repository is unstable. Until a consumer outside Logical Works exists, a breaking change
   bumps the major version and ships no shim, re-export or deprecation period (Director,
   2026-10-23). The README says so, and the note comes off when nothing breaks every other day.

## What the tree does today

Read from the code on 2026-10-23, not from the docs.

- `bevy_ecs` is named by ten files, all in `crates/lgwks-bot/src/ecs/`. `EcsBot::world()` hands out a
  bevy `World` and has no caller.
- The tick is pulled. `tick_async` observes (polls every source in bounded waves on the caller's
  executor), decides (one `Schedule::run`), acts, and reports. Nothing wakes it: a caller calls it,
  and every tick visits every source.
- The verbs run on `lgwks_std::task` or the caller's executor. `fs::Path::poll` uses
  `lgwks_std::task::spawn_blocking`, and `--no-default-features` builds with no tokio
  (`docs/bot-on-ecs.md` section 12).
- `script!`, `task::Host`, `rt::supervise` and `rt::estate` are a second stack on tokio. None of
  them uses the ECS. So `rt` sits beside the core today, not under it and not between its parts.

## Why `bevy_ecs`

`docs/bevy-admission.md` and `docs/bot-on-ecs.md` hold the answer and it is not re-derived here.
Three consumers (the bot, drishti and the GPU framework) need the same primitive, "run this when
that value changed", and one substrate costs less than three approximations of it. Measured there: 60 packages and 7 to 9 s cold;
`Changed<T>` matched only the ticks where the value moved; effects run in exclusive systems and an
ambiguous schedule is refused at build; the executor is single-threaded for reproducibility. Gaps
stated there stay true: no invalidation graph, no replay, determinism is this workspace's job, and
`bevy_ecs` is pre-1.0 (`^0.19`).

## The order

Each step lands alone and green, with its own design-gate plan that cites this ADR.

| # | Change | Falsifier |
|---|---|---|
| R1 Seal | Remove `EcsBot::world()`. A test `core_boundary` fails the build if a file outside `ecs/` names bevy, or if a core module (`ecs`, `verb`, `cap`, `gate`, `effect`, `journal`, `error`, `clock`, `spec`) names `rt`, `task`, `script`, `domain` or tokio. Any edge it finds today is cut in the same change. | A planted `use crate::rt` in `gate.rs` fails the test. |
| R2 Wake | A cloneable, bounded wake handle. A source declares `Every` (today's behaviour, the default) or `Wake`. The core ticks on a wake or a deadline and polls only the woken sources. The dirty set coalesces into a full visit on overflow and never drops a wake. | Idle runs 0 ticks and 0 polls over a stated virtual span. One wake polls exactly that source. 100, 1,000, 10,000 and 100,000 wakes stay under the bound. Two tenants never wake each other. Same seed, same trace hash. |
| R3 Run events | `Host` and `Supervisor` append `admitted` and `settled` to the journal and wake the core, so the tokio stack reaches the ECS through the log and not through a hook. | 100 runs leave 100 `admitted` and 100 `settled` rows. A chain bound to `settled` fires 100 times. Kill and restart replays the record without refiring. |
| R4 Drishti in | Merge `srinji-kaggss/drishti` as a workspace member with its history. Split its oversize `.md` files under the 120-line law (`docs/decisions.md` is 169 lines, `docs/architecture.md` 157). It consumes the same ECS substrate `docs/bevy-admission.md` already names it for. | Its 18 library and 6 bundle tests pass from the workspace, every lane stays green, and its 15 blocked proof gates stay blocked: none is claimed by the move. |
| R5 Measure | Print before and after for R2 with the existing `profile` feature: idle wakeups, event-to-effect p50, p95 and p99, peak RSS. | The numbers are in the PR. No figure is claimed before it is printed. |

R1 first, because R2 and R3 rely on the boundary it enforces. R4 has no code dependency on R2 and
can land in either order.

## Direction: everything ours

Nothing here is greenfield on day 0, and every admitted engine ends up replaced. The seams above
are where the replacements go in.

1. **The reactor.** R2's wake handle is the whole driver interface. `lgwks_std::task` is already an
   owned executor; an owned readiness loop over epoll, kqueue and IOCP replaces tokio under `rt`
   when it matches it on a matched workload with printed numbers.
2. **The ECS.** R1's seal limits the swap to `ecs/`. The contract an owned ECS must meet is the
   measured behaviour in `docs/bot-on-ecs.md` sections 2, 3 and 5: `Changed<T>` fires only when the
   value moved, effects land at a point the caller names, a schedule that cannot be ordered is
   refused at build, and one seed replays one order.
3. **The rest of the storefront.** Each `contract/APPROVED.toml` entry is replaced the same way.

A replacement that loses its matched workload stays out. No date is set.

## Not decided here

- The names of any new crate (runtime, server, app framework). None is created by this ADR.
- The server and app half of the framework. `codegraph` finds no router, extractor, middleware or
  HTTP server in the repository today, so it is built, not moved.
