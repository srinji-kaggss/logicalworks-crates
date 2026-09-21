# Bot semantics on an ECS substrate — the mapping, and what it costs

Read this after `docs/async-sdk-shape.md`. That document says what a consumer of
the async surface is allowed to think about. This one answers a different
question: **what substrate executes a `BotSpec`, and does the four-verb model
extend onto it naturally?**

Status: design, with a measured spike behind §2–§3 and **step 2 of §10 landed**
as `lgwks_bot::ecs` (feature `ecs`). `bevy_ecs` is **admitted**
(`docs/bevy-admission.md`). The scheduler decision — §9, step 1 — remains open,
and is a Director call rather than an agent's.

## 1. Why an ECS at all

The four verbs are already an ECS in disguise, and naming that buys three
things:

| Current type | ECS role | What it buys |
|---|---|---|
| `Cap` / `Auth` | `Resource` — the capability proof | One authority per world; no ambient singleton |
| `Observe` impls (`Endpoint`, …) | `Component` + a system that writes it | Heterogeneous sources without `Box<dyn>` |
| `Evaluate::check` | **change detection** (`Changed<T>`) | The condition is a tick comparison, not a re-derivation |
| `Execute::execute_action` | a system that writes the effect | Effects are data, not callbacks |
| `BotSpec`, `ChainSpec` | a `Schedule` built from deserialized data | The spec *is* the ordering declaration |
| `Bot::tick()` | `Schedule::run(&mut World)` | One tick is one step, manually driven |

The last row is the one that matters for the DUM-E property: `Bot::tick()` is
already a manual step, and `Schedule::run(&mut World)` is the same shape. There
is no framework-owned main loop to surrender control to — `App::update()` is
available and `App::run()` is never called.

## 2. What the spike measured

`bevy_ecs` 0.19.1, `default-features = false, features = ["std"]`. A two-source
bot with a capability resource, a `Changed<T>` condition, and a
`Commands`-issued effect, stepped five times by hand.

**The mapping holds.**

```
diag: Changed<Status> matched: [2, 0, 2, 0, 2]
diag: Status values per tick: [[200,200],[200,200],[503,503],[503,503],[200,200]]
diag: spawn requests/tick   : [0, 0, 2, 0, 0]
total effects               : 2
ungranted world effects     : 0
flat-upstream effects       : 0
determinism                 : identical across rebuilds
```

- `Changed<T>` is a precise condition primitive: it matched on exactly the ticks
  where the observed value actually moved (ticks 2 and 4), not on every tick.
- The capability gate as a `Resource` correctly produced **zero** effects in a
  world without the grant — the equivalent of `Auth::check` denying.
- A flat upstream produced zero effects: a tick where nothing changed reports
  nothing changed. That is the property a naive "re-run every condition" loop
  does not have.
- Rebuilding the schedule and world from scratch reproduced the sequence exactly.

## 3. The one finding that shapes the design

**`Commands` do not flush unless a sync point is ordered after the writer.**

Deleting the single line `schedule.add_systems(ApplyDeferred)` takes `total
effects` from 2 to **0**, and *nothing else in the output changes*: the system
still ran, `Changed<T>` still matched, `spawn requests/tick` still reported
`[0,0,2,0,0]`. The effect simply never materialized. No panic, no error, no
diagnostic — a bot whose Execute verb silently does nothing.

The reason is sharper than "the sync point was missing", and a second spike
measured it as a four-way control, because the first phrasing points at the wrong
remedy. `ScheduleBuildSettings::auto_insert_apply_deferred` **defaults to
`true`**; it inserts the sync point only where some system is ordered *after* the
deferred writer. Repeat the effect three times over five manually stepped ticks,
so a correct run records 3:

| Schedule | Effects |
|---|---|
| writer is the last system, no sync point | **0** — the queue is dropped at the end of `Schedule::run` |
| writer, then an unrelated successor (auto-insert) | 3 |
| writer, then `add_systems(ApplyDeferred)` — unordered | **2** |
| `(writer, ApplyDeferred).chain()` — ordered | 3 |

Row 3 is the one worth staring at: adding `ApplyDeferred` *is* the remedy the
failure message suggests, and it still loses an effect, because an unordered sync
point can run before the writer and defer the effect by a tick — which on a
fixed-length run drops the last turn entirely. The hazard is not "the sync point
is absent"; it is **"a deferred effect lands at a time decided by the schedule
graph rather than by the caller"**, and the only fix that holds is to not defer.

Two consequences, and they are both requirements rather than observations:

1. **The bot must not express effects through `Commands`.** An effect written
   through the deferred queue is visible at a schedule-graph-dependent moment.
   The spike shows the second half of the hazard too: even once flushed, the
   effect is first visible **one tick after it was issued** (requested on tick 2,
   counted on tick 3). Use an **exclusive system** (`&mut World`) at a named
   point, where the ordering is the caller's declaration rather than a
   consequence of graph construction.
2. **A missing sync point must be a build error, not a silent no-op.** Whatever
   schedule a `BotSpec` compiles to, `Schedule::initialize() -> Result<_,
   ScheduleBuildError>` and `ScheduleBuildSettings { ambiguity_detection:
   LogLevel::Error }` must both be run at `Bot::build()` time. An ambiguous
   schedule is currently a silent ordering bug; at `LogLevel::Error` it becomes a
   refusal. This is the ECS form of the estate's existing habit of failing at
   `build()` rather than at `tick()`.

Third finding, minor but worth recording: `World::entities().len()` is **not** a
live-entity count. `World::new()` alone reports 4, every `insert_resource` grows
it, and a `spawn` that reuses a free slot does not change it. Counting requires a
query. Anything that reports "N bots running" must count by query.

## 4. The design rule the spike taught

**Identity and mutable state must be separate components.**

The first version fused `url` and `status` into one `Endpoint` component. Every
poll then marked the whole component changed, so `Changed<Endpoint>` degenerated
to "always true" and the condition stopped meaning anything. Splitting into
`Endpoint` (identity, never mutated after spawn) and `Status` (the observed
value, the only thing the observer writes) is what makes change detection a real
condition primitive.

This generalizes into a rule for every domain in `lgwks_bot/src/domain/`:
**a component is either identity or observed value, never both.** The corollary
is the Bevy guidance that variation belongs in an *enum payload* component, not
in a bespoke component set per bot — archetype count stays bounded and queries
stay linear.

**Amendment, measured while implementing step 2.** The observed value cannot be
the component the spike used. That spike's `Status` was a `Copy` `u16`; a real
observer produces `Box<dyn Any>` from the verb erasure, and `bevy_ecs` 0.19.1
declares `Component: Send + Sync + 'static` unconditionally — there is no
`non_send` feature to relax it (`NonSend` survives for **resources** only, which
is why `World::insert_non_send` exists).

So the value lives in a `NonSend` resource and the component is a `Revision(u64)`
marker the observe system bumps **only when the value actually differs**. That
keeps §4's property — the condition is still a tick comparison, not a
re-derivation — while keeping `lgwks_bot`'s non-`Send` contract intact rather
than tightening it to fit the substrate. It also means the ECS path needs
`PartialEq` on an observer's `Output`, which `Bot::builder` does not: a value that
cannot be compared cannot be detected as changed. That bound belongs on
`EcsBuilder::observe` and nowhere else.

## 5. Determinism on this substrate

Bevy supplies the mechanism but not the guarantee, and it is worth being exact
about which is which.

- **`ExecutorKind::SingleThreaded`, set explicitly per schedule.** This is the
  single highest-leverage decision. The multithreaded executor's `can_run` admits
  any system whose conflicting-system bitset is disjoint from the running set, so
  which of two non-conflicting systems runs first is not fixed. For a bot polling
  sources, parallel scheduling buys throughput the workload does not need and
  costs interleaving reproducibility.
- **Ambiguity detection is opt-in** and defaults to `LogLevel::Ignore`. Turn it
  to `Error`. It runs at schedule build, not at runtime.
- **`Time<Virtual>` as the clock root**, with `Time<Fixed>`'s overstep accumulator
  providing the tick. `Time<Virtual>` supports `pause()`, `set_relative_speed()`,
  and `set_max_delta()`, so a test can run 10,000 ticks without touching
  wall-clock. This is the virtual clock `docs/distributed-boundaries.md` records
  as missing.
- **Not supplied, and must not be assumed:** there is no first-party
  `bevy_replay` or `bevy_determinism`. Entity-ID and archetype-ID allocation are
  history-dependent, and `bevy_platform`'s `HashMap` has a fixed hasher —
  reproducible *hashing*, documented **arbitrary iteration order**. Sort
  deterministically before emitting or comparing anything. Determinism remains
  the estate's responsibility, exactly as `docs/async-sdk-shape.md` §4 says.

## 6. gpui — the honest verdict

**gpui is not an acceleration layer for non-UI work.** This is not a judgement
call; it follows from what the crate exposes.

- `gpui` 0.2.2 is on crates.io and is **already admitted**: `contract/APPROVED.toml`
  records `crate = "gpui"`, `tier = "boundary"`, `owner = "lgwks_deps"`,
  `capability = "ui.gpu-desktop"`, wired as the storefront feature `gpui`.
- But the capability name is accurate. Layout is **taffy** (CPU, pinned `=0.13.0`),
  text shaping is **cosmic-text** (CPU), element diffing is CPU, and the GPU
  surface is a `Scene` of paint primitives submitted through `blade-graphics`.
  **There is no exposed compute, buffer, or shader-dispatch API.**
- `default = ["font-kit", "wayland", "x11", "windows-manifest"]`, and even
  `test-support` enables wayland+x11. Adopting it headless drags in
  wayland/x11/blade/objc2/font-kit for zero speedup.

So: gpui is the right dependency for *a bot console* — a real dashboard over a
running bot — and the wrong one for making the bot faster. Saying otherwise would
be selling a UI toolkit as a compute layer.

**The salvageable part is the scheduler, not the GPU.** GPUI's `TestDispatcher`
is the best determinism idea in the landscape: `TestSchedulerConfig { seed,
randomize_order: true, allow_parking: false }`, a virtual clock
(`advance_clock`, `advance_clock_to_next_timer`, `run_until_parked`), and seeded
interleaving so seeds *sweep* real schedules and replay failures exactly. That is
strictly stronger than "single-threaded".

**And it is not reachable as a dependency.** The `scheduler` crate is not
published on crates.io — the crate of that name there is an unrelated 2016 Linux
affinity binding — so zed's is available only as a path or git dependency on the
monorepo.

That leaves the scheduler decision in §9.

## 7. The control plane — bors, and why Zuul is the better reference

bors is worth studying because it is the canonical **dumb obedient bot**: a tiny
legible state machine, a narrow command grammar, and no cleverness. Its lessons
transfer directly, and they are all about durability rather than concurrency.

**Adopt as invariants:**

1. **Derive queue membership; never store it.** bors's `Patch` has no "queued"
   flag — "queued" is a *query* (`open AND no active-batch link`). A stored enum
   is a second source of truth that drifts on crash. In `lgwks_bot` terms: a
   chain's readiness is a predicate over observed facts, not a field.
2. **One idempotent reconciler; events only wake it.** The webhook and the timer
   call the same guarded transition. Recompute ordering from durable rows each
   tick; never hold the queue in memory.
3. **Checkpoint after the side effect, in a single write.** bors pushes to
   staging and *then* writes `{commit, state: running}` in one UPDATE. A crash
   before the write leaves the item "not started", which is the safe default.
4. **The durable log is the external system.** bors re-reads CI status from
   GitHub rather than trusting local rows. Local state caches *intent*, never
   *truth*.
5. **Retry is replay of recorded intent.** `bors retry` re-runs the stored
   `(actor, command)` pair — it re-executes a recorded authorization rather than
   re-deriving one. This is the same property `docs/async-sdk-shape.md` §4 calls
   journaling, arrived at from a different direction.
6. **Authorize at the parse boundary, over the whole command list** — parse
   first, reduce to one required level, authorize once, all-or-nothing. bors
   reduces `[:try, {:activate_by, ..}]` to a single `:reviewer` level before any
   effect. This is `Auth`/`GrantSet`, and the ordering (parse → reduce →
   authorize → act) is the part worth copying.
7. **Unknown input parses to nothing; it is not an error.** bors's fallthrough
   returns `[]`. A bot that errors on unrecognized text is a bot that can be
   wedged by a stranger's comment.

**And one structural correction:** bors is a *degenerate case of Zuul*. Zuul adds
declared pipelines with named managers (`independent` / `dependent` / `serial`),
**windows as speculative rate limiting** with TCP-style auto-tuning, enumerated
reporting outcomes, and cross-project `Depends-On`. If the estate ever grows past
one queue, Zuul's pipeline model is the reference, not bors's single queue.

**Do not copy:** batching. bors amortizes CI by testing several changes at once
and pays for it with bisection on failure. A bot whose tick is cheap does not
need that trade, and every batch is a place where a failure cannot be attributed.

## 8. The data plane — the frontier

A bot's canonical workload is an observe→evaluate→execute loop over a set of
sources, which is a crawler with an action attached. Four reference
implementations were studied (spider-rs, Scrapy, Heritrix, Nutch) plus Mercator's
1999 architecture, which is still the canonical shape.

**Frontier structure.** Mercator's design — per-host queues under a global
priority queue over hosts — is right, and for a reason worth stating: politeness
is a per-host property and priority is a global one, so one flat queue cannot
express both. A bounded per-host queue under a global host priority queue also
gives backpressure for free: a host whose queue is full stops being selected
rather than growing without limit.

**Politeness is a scheduling policy, not a fixed delay.** Scrapy's `AutoThrottle`
and Mercator both adapt; Mercator specifically uses priority decay plus
weighted-random selection rather than a fixed re-crawl interval. Note a
correction to the common summary of that paper: Mercator has **no
interval-adaptation rule** — the adaptive criteria live in US patent 6,263,364,
not the paper. Do not cite the paper for behaviour it does not describe.

**Determinism primitives worth stealing, none of which come from a crawler:**

- **Heritrix's comparator rule, stated plainly in `WorkQueue::compareTo`: *"at
  this point, the ordering is arbitrary, but still must be consistent/stable
  over time."*** Any comparator that can tie must be strengthened until it
  cannot. This is the same rule as `docs/async-sdk-shape.md` §4's "deterministic
  event ordering at every collection that is iterated", arrived at independently.
- **FoundationDB splits three RNG streams** — `deterministicRandom()`,
  `nondeterministicRandom()`, and `debugRandom()` — so that a debug draw cannot
  shift the main stream. It uses `boost::mt19937_64` rather than `std::`
  distributions specifically because those differ between libstdc++ and libc++,
  i.e. for cross-toolchain reproducibility of the stream. For this estate: a
  log line, a metric, or a `HashMap` iteration must never perturb the scheduling
  stream.
- **Kafka's `read_committed` LSO** is the right shape for a replayed frontier: a
  replaying reader should see what has been **decided**, not what has been
  **written**.
- **Temporal's `GetVersion` rule**: a version marker must be called *forever*,
  even after the branch it guarded is deleted, because the marker event exists in
  history. The persistence corollary: never remove a tie-break field from a
  persisted record.

### 8.1 The finding that changes the design

**A single-bit seen-set is structurally insufficient, and no reference crawler is
durable-before-fetch.**

The two failure modes are asymmetric, which is why ordering alone cannot fix
them: fetch-then-mark loses the *fact of the fetch* (the URL is dropped and
unrecoverable without re-seeding), while mark-then-fetch loses the *fetch itself*.
A mark that means only "known" cannot be re-driven.

The fix is a **two-phase mark**: a durable `Pending` recorded *before* the fetch,
a durable `Done` after it, with recovery re-driving everything left `Pending`.
The seen-set gains a state machine rather than a flag.

This is not matching a reference implementation. It is exceeding all four:

| Crawler | Durability before fetch |
|---|---|
| Heritrix | `checkpointIntervalMinutes = -1` — **checkpointing is off by default**. The recovery journal writes through a 32 KiB `BufferedOutputStream` whose `writeLine` never flushes, so a hard kill loses the tail. `importRecoverFormat` catches `EOFException` with the comment *"expected in some uncleanly-closed recovery logs; ignore."* |
| Scrapy | `requests.seen` has **no `flush()` and no `fsync` anywhere**; `close()` only closes the handle. The OS page cache is the entire durability story. |
| Nutch | `generate.update.crawldb` is `false` by default, so nothing is marked at generate time. Worse, on `fetcher.timelimit` expiry `Fetcher::emptyQueues()` **drops the remaining queue without writing it** — not even as failed statuses. |
| Mercator | Paper-level; no durable-frontier claim to evaluate. |

Also worth copying from Heritrix: `Checkpoint.VALIDITY_STAMP_FILENAME = "valid"`.
A validity stamp is what makes a **torn checkpoint detectable** rather than
silently half-loaded — the failure mode `Bot::tick` would otherwise inherit.

### 8.2 Reconciling bors and the frontier

§7 says *checkpoint after the side effect, in a single write*. §8.1 says *record
`Pending` before the fetch*. These look opposed and are not; the difference is
where the durable log lives.

bors can write after the effect because its durable log **is the external
system** — the merge and push to GitHub are themselves the record, so there is
nothing to write first. A crawler has no such external log; the frontier is the
only record, so the intent must be durable before the effect.

The unifying rule: **where an external system is the durable log, checkpoint
after the effect and re-read truth from it. Where the local record is the only
log, it needs a state machine — `Pending` then `Done` — and recovery re-drives
the ambiguous middle.** The failure to avoid is identical in both cases: a record
that cannot distinguish "done" from "never started".

## 9. The open decision

The ECS admission is **made** — `bevy_ecs`, `bevy_app`, `bevy_time`, and
`bevy_state` are registered in `contract/APPROVED.toml` and `lgwks-deps check .`
accepts them (`docs/bevy-admission.md`). Measured in this working tree rather
than estimated: **60 packages** for `bevy-ecs`, 63 for `bevy-app`, 64 for
`bevy-time`, 62 for `bevy-state`, with `bevy_reflect` **absent** from the
compiled tree. Pinned `^0.19`; 0.20 is an RC and is not taken.

**The scheduler remains open.** Two honest options, and the standing
instruction — *if OSS code naturally does it, modify and improve it, don't
rebuild* — favours the first:

- **VENDOR (ladder rung 7).** Take zed's `scheduler` crate as audited source into
  `vendor/` and extend it for the estate's clock and RNG. The `vendor/` tree
  already exists (294 crates) and `lgwks-deps vendor` is the rung-7 tool. This
  gets the seeded-interleaving scheduler without taking gpui's UI closure.
- **ELIMINATE, against the instruction.** Write the seeded scheduler as an
  `lgwks_std` module. Cheaper at the boundary, but it is rebuilding something
  that already exists and works.

This is the one place where the "don't rebuild" instruction and the dependency
doctrine point at different rungs, so it is a Director call rather than an
agent's.

## 10. Migration order

Each step must leave `cargo test --workspace --all-targets` green.

0. **Finish getting the tree green.** ✅ Done. `cargo test --workspace
   --all-targets --locked` → 208 passed, 0 failed; clippy `-D warnings` clean;
   `cargo fmt --all -- --check` exit 0; `lgwks-deps check .` exit 0 with 27
   approvals; `lgwks-std-package-smoke.sh` passed.
1. **The scheduler decision** from §9 — vendored zed `scheduler`, or an
   `lgwks_std` module. **Still open, and deliberately not taken here.** It is a
   Director call because the doctrine and the "don't rebuild" instruction point
   at different rungs.
2. **A `Spec -> Schedule` bridge alongside the current executor.** ⚠️ **Landed in
   part**, as `lgwks_bot::ecs` (feature `ecs`). What exists: `EcsBot`, a parallel
   builder, `SourceId`/`Revision` components, `Grants`/`Fired`/`TickError`
   resources, non-`Send` chain and value storage, the `observe` and `fire`
   exclusive systems, and the build-time validation of step 3. `Bot` and
   `Bot::tick()` are untouched; this is a seam, not a replacement.

   **What is not done, stated plainly: the `from_spec` gap is still open.** The
   builder takes *verbs*, not a [`BotSpec`]. Materializing a `World` from wire
   data needs a `domain_id -> constructor` registry — `"gh::pr_status"` has to
   become a concrete `Observe` — and no such registry exists anywhere in the
   estate. `experience/invariants/sdk.yaml`'s *"a validated BotSpec cannot be
   materialized into a runnable Bot through this SDK alone"* is therefore still
   true, on both substrates. Closing it is a separate piece of work, and it is
   the same missing registry the `Lambda`/`Workers` comparison arrived at from
   the other direction.
3. **Build-time schedule validation** — ✅ landed with step 2, since it has
   nowhere else to live: `Schedule::initialize()` plus `ambiguity_detection:
   LogLevel::Error`, surfaced as `BotError` from `build()`, with a control test
   that the same two systems *ordered* do validate.
4. **`Time<Virtual>` clock root**, replacing wall-clock reads, with real and
   virtual implementations.
5. **Effects move to exclusive systems**, and the deferred path is removed so
   §3's silent-no-op failure cannot recur.
6. **bors invariants land on the control plane** — derived readiness, one
   reconciler, checkpoint-after-effect, replay of recorded intent.

Steps 2–5 are additive; the current `lgwks_std::task` executor and `Bot::tick()`
stay working throughout. Parallel seams, not demolition.

## 11. Running the substrate's tests

`ecs` is default-off, so the workspace gate does not compile the module — a
substrate whose tests never run is a substrate whose guarantees are claims:

```sh
cargo test  -p lgwks_bot --features ecs
cargo clippy -p lgwks_bot --all-features --all-targets -- -D warnings
```

Five tests, one per guarantee, including the semantic delta pinned against the
plain executor: on the same five-tick script and the same condition, `Bot` fires
**2** and `EcsBot` fires **1**. That test exists so the difference between "the
value is true" and "the value moved" cannot be quietly forgotten.
