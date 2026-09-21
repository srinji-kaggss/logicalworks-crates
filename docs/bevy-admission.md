# Bevy ECS admission

This document records the admission of four Bevy crates into the `lgwks_deps`
storefront, the alternatives measured against it, and the conditions attached to
the decision.

Register entries: `contract/APPROVED.toml`, `bevy_ecs`, `bevy_app`, `bevy_time`,
`bevy_state`, all `tier = "boundary"`, `owner = "lgwks_deps"`, approved
2026-09-20. Storefront features: `bevy-ecs`, `bevy-app`, `bevy-time`,
`bevy-state`.

Status: admitted, and now consumed. `lgwks_bot::ecs` is the first
implementation (see `docs/bot-on-ecs.md` §10–§11 for what it lands and, just
as importantly, what it does not).

## 1. Why one substrate and not three

Three consumers in the constellation need the same three primitives:

| Consumer | Needs |
|---|---|
| `lgwks_bot` (four-verb actors) | stable identity per source, "run this when that value changed", declared action ordering |
| drishti (incremental derivation) | invalidate only what a change actually affects; reuse valid work |
| GPU framework (`gpu-framework-first-pass`) | *"Changed demand: invalidate affected dependencies; reuse only valid keyed work"*; *"No demand: sleep"* |

The GPU framework's own architecture document states the requirement as an
intent-preservation invariant and a six-step scheduling policy in which step 2 is
literally *"invalidate affected dependencies; reuse only valid keyed work."*
That is change detection. The three consumers were separately re-deriving it;
admitting one substrate is cheaper than three approximations of it.

**What the workspace already had, and why it is not enough.** `lgwks_std::task`
is a zero-dependency executor with the property this workspace values most: no
`spawn`, so no task can leak. But it is deliberately
single-threaded, non-incremental, and has no query engine. It can await a bounded
set of futures and nothing more. It stays exactly as it is.

## 2. Measured cost

Taken from this workspace, not estimated:

```
cargo tree -p lgwks_deps --features bevy-ecs --prefix none | sort -u | wc -l
→ 60 packages
```

```
bevy crates actually pulled:
  bevy_ecs  bevy_ecs_macro_logic  bevy_ecs_macros  bevy_macro_utils
  bevy_platform  bevy_ptr  bevy_tasks  bevy_utils
```

`bevy_reflect` is **not** in the compiled tree (`grep -c bevy_reflect` → 0). It
appears in `Cargo.lock` (Cargo locks optional packages regardless of feature
activation), which is why the lockfile shows 15 bevy entries while only 8
compile. Reading the lock as the cost would overstate it by nearly half.

An independent measurement of `bevy_ecs` + `bevy_app` with `default-features =
false` on an M5 Pro: 64 lockfile packages, **7–9 s cold build**, 131 MB release
`target/`. No renderer, no windowing, no audio, and no GPU driver in the graph.

`bevy_ecs` requires rustc 1.95; the toolchain for this workspace is pinned to 1.98.0
(`rust-toolchain.toml`), so there is no MSRV conflict.

## 3. What was deliberately excluded

- **`bevy_reflect`.** It is the expensive half of `bevy_ecs`'s default feature
  set. This workspace does not need runtime type reflection, and the measurement
  above confirms it is genuinely absent rather than merely untested.
- **`multi_threaded`.** `MultiThreadedExecutor::can_run` admits any system whose
  conflicting-system bitset is disjoint from the running set, so the relative
  order of two *non-conflicting* systems is not fixed. A bot polling sources
  gains no throughput from parallel scheduling and would pay for it in
  reproducibility. `SingleThreadedExecutor` is set explicitly per schedule, at
  the point the schedule is built.
- **`bevy_render` and everything above it.** The GPU framework chose wgpu as its
  only foundation. Bevy's renderer is also wgpu-based, but adopting it would
  import an opinionated render graph, an asset pipeline, and a shader
  permutation system that the framework's architecture explicitly declines to
  inherit. Rendering stays the GPU framework's decision, not this crate's.
- **`bevy_asset`, `bevy_scene`, `bevy_remote`.** Two were evaluated and deferred
  in §5.

## 4. The hazard that admission must be conditioned on

Measured on `bevy_ecs` 0.19.1 with the configuration used here. A system
that ran, matched its `Changed<T>` filter, and entered its effect branch
produced **zero** effects, with no panic, no error, and no diagnostic, because
`Commands` are not flushed unless `ApplyDeferred` is added explicitly. Deleting
one line took the count from 2 to 0 while changing nothing else in the output.
Even once flushed, the effect was first visible one tick *after* it was issued.

Two conditions follow, and they are conditions of admission rather than
suggestions:

1. Effects are expressed through **exclusive systems** (`&mut World`) at named
   points, not through the deferred command queue.
2. `Schedule::initialize()` and `ScheduleBuildSettings { ambiguity_detection:
   LogLevel::Error }` run at `Bot::build()` time, so a schedule that cannot be
   ordered deterministically is a refusal at build, not a silent misordering at
   tick.

Full evidence in `docs/bot-on-ecs.md` §2–§3.

**A third condition, found by implementing rather than by spiking.**
`Component: Send + Sync + 'static` is unconditional in `bevy_ecs` 0.19.1, and
there is no `non_send` feature to relax it. `NonSend` survives for **resources**
only. This collides with `lgwks_bot`'s deliberate non-`Send` verb erasure (its
futures are not `Send` on purpose, so that a domain may hold thread-local state)
and a `Box<dyn Any>` produced by that erasure cannot be a component.

Both workarounds were considered and one was taken:

- **Taken.** Keep the value in a `NonSend` resource and put a `Revision(u64)`
  marker component on the entity, bumped only when the value differs. The
  condition is still `Changed<Revision>`, a tick comparison, so the property
  the admission was for is preserved, and the crate's non-`Send` contract is
  untouched.
- **Rejected.** Tighten the erasure to `Box<dyn Any + Send + Sync>` and require
  `Send + Sync` on every observer's output. It would fit the substrate better and
  cost every existing and future observer a bound it does not otherwise need, to
  solve a problem the substrate introduced.

The cost of the choice is a `PartialEq` bound on an observer's `Output`, but only
on the ECS path: change detection needs to tell "the same value again" from "a
new value", and a type that cannot be compared cannot be detected as changed.
`Bot::builder` takes no such bound, because it re-evaluates every tick.

## 5. What CodeGraph showed about adjacent components

The Bevy index (1,932 files, 51,020 nodes, 230,384 edges) was queried for the
mechanisms the three consumers need. Three findings changed the shape of this
admission.

**Adopted alongside `bevy_ecs`:**

- **`bevy_time`** — `Time<Virtual>` (`crates/bevy_time/src/virt.rs:75`) is a real
  virtual clock: `pause`, `set_relative_speed`, `max_delta`, `effective_speed`,
  and a `Time<Fixed>` overstep accumulator in `fixed.rs`. This is precisely the
  virtual time source `docs/distributed-boundaries.md` records as missing, and
  it is the reason a test can run 10,000 ticks without touching the wall clock.
- **`bevy_state`** — `NextState` (`state/resources.rs:181`), `State<S>`,
  `PreviousState`, `StateTransitionEvent`, `apply_state_transition`
  (`state/freely_mutable_state.rs:49`), plus `SubStates` and `ComputedStates`.
  This is a declared state machine with generational transitions and enter/exit
  schedules, which is the control-plane shape bors arrives at by hand
  (`docs/bot-on-ecs.md` §7). Adopting it means the transition is data rather
  than a chain of conditionals.

**Rejected after inspection:**

- **`bevy_asset`, for dependency tracking.** It has a dependency graph
  (`dependency_load_state`, `DependencyLoadState` in `src/server/mod.rs`), but it
  is an *asset loading* graph, tied to `AssetId` and the asset server. It is not
  a general value-dependency graph and using it as one would fight it.

**Absent, and this is the honest gap:**

**Bevy has no general-purpose invalidation graph.** Querying `invalidate` and
`Dependency` across all 62 crates returns only two things: the *schedule* DAG
(`crates/bevy_ecs/src/schedule/graph/mod.rs:23`) and the asset-loading graph
above. There is no "this derived value depends on that source value" structure
anywhere in the engine.

So the substrate provides the **primitive** (per-component `Tick` stamps and
`ComponentTicks`, with `set_if_neq` as the precise-notification path) and not
the **graph**. drishti's incremental derivation and the GPU framework's demand
closure must be built on top of `Changed<T>`, and that is real work this
admission does not do for them. Stating it plainly matters more than the
admission does: a consumer that assumes a dependency graph exists will not find
one.

## 6. Risks accepted

- **Pre-1.0 churn.** Bevy breaks between minor versions. 0.20 is currently an RC
  and is **not** pinned; the admission is `^0.19`. Every version bump is a
  reviewed change with the spike re-run, not a `cargo update`.
- **`bevy_ecs` is not minimizable further.** It unconditionally pulls
  `bevy_tasks`, `bevy_utils`, and `bevy_platform`. Those four plus the macro
  crates are the floor, and 60 packages is what that floor costs.
- **Two determinism properties remain this workspace's responsibility, not Bevy's.** There is
  no first-party `bevy_replay` or `bevy_determinism`; entity- and archetype-ID
  allocation is history-dependent; and `bevy_platform`'s `HashMap` has a fixed
  hasher but documented **arbitrary iteration order**. Anything emitted or
  compared must be sorted first.
