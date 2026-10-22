# The §2–§3 spike, re-run on `bevy_ecs` 0.20

`docs/bevy-admission.md` §6 attaches a condition to every version bump: the
spike recorded in `docs/bot-on-ecs.md` §2–§3 is re-run, not skipped as a
`cargo update`. This page is the re-run for the 0.19.1 → 0.20.0 bump (2026-10-22),
and the record the register's `review` field cites until those two pages are
split under the 120-line law and can carry it themselves.

Spike source: a throwaway crate outside the repository,
`bevy_ecs = { version = "0.20", default-features = false, features = ["std"] }`,
five manually stepped ticks, `Changed<Status>` and a `Commands`-issued effect,
`ScheduleBuildSettings { ambiguity_detection: LogLevel::Error }` set before
`initialize`. Every row below was printed by one run of that crate.

## 1. The deferred-effect control

Three effects are issued, on ticks 1, 3 and 4; a correct schedule materialises
all three. Materialised and issued counts:

| Schedule | 0.19.1 | 0.20.0 |
|---|---|---|
| writer is the last system, no sync point | 0 | **(3, 3)** |
| writer, then an unrelated successor (auto-insert) | 3 | (3, 3) |
| writer, then `add_systems(ApplyDeferred)`: unordered | 2 | **refused at build** |
| `(writer, ApplyDeferred).chain()`: ordered | 3 | (3, 3) |

Two behaviours changed, both in the direction the admission wanted:

- **The silent loss is gone.** On 0.19.1 the queue was dropped at the end of
  `Schedule::run` when nothing was ordered after the writer; 0.20 applies the
  queue there, so a bot whose Execute verb defers can no longer do nothing
  quietly.
- **The unordered sync point is now a refusal, not a loss.** On 0.19.1 adding
  `ApplyDeferred` beside its writer without an order ran and lost the last
  effect. With `ambiguity_detection: LogLevel::Error`, 0.20 refuses the
  schedule at build — `Systems with conflicting access have indeterminate run
  order` — which is exactly the failure mode the admission's second condition
  asked for.

Both conditions of admission remain in force. They are the design that holds on
either version, and they are what the code does: effects are written by
exclusive systems at named points, and `Schedule::initialize` plus ambiguity
detection run at `Bot::build()` time. `initialize` now takes `&mut World` in
0.20; `lgwks_bot::ecs` already calls it that way, so the bump needed no source
change.

## 2. `Changed<T>` precision

Two components move to 503 at tick 2 and back to 200 at tick 4. Changed
entities per tick: **`[2, 0, 2, 0, 2]`** — identical to the 0.19.1 measurement.
The condition is still a tick comparison that matches exactly when the value
moved.

## 3. `World::entities().len()` is still not a live count

`empty=4 after_insert_resource=4 after_spawn=4 after_despawn=4`. The 0.19.1
spike reported that `insert_resource` grew the count; on 0.20 it does not. The
usable rule is unchanged and the stronger form of it now holds: the number is a
slot-allocation detail, never a live-entity count, and anything that reports
"N bots running" must count by query.

## 4. Cost

`cargo tree -p lgwks_deps --features bevy-ecs -e normal --prefix none | sort -u
| wc -l` → **89 packages** (102 with dev-dependencies). `bevy_reflect` remains
absent from the compiled tree. 0.20 adds `bevy_ecs_macro_logic` to the macro
machinery, which brings `mutants`, `unsynn`, `variadics_please`,
`convert_case`, `disqualified` and `unicode-xid` as proc-macro-only packages;
no renderer, windowing, audio or GPU driver enters the graph.

## 5. What this does not claim

Only the `bevy_ecs` surface the workspace compiles. `bevy_app`, `bevy_time` and
`bevy_state` are storefront edges with no workspace consumer; their 0.20 builds
are proven by the `feat-deps-gpui`/`feat-*` lanes and the storefront fixture,
not by this spike. No frontier, fastest or cross-platform claim follows.
