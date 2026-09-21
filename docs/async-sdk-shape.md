# The async surface as an SDK — shape, invariants, and migration

Read this before adding anything to `lgwks_bot::rt` or `lgwks_std::task`. It is
the design contract for the estate's async tier: what a consumer is allowed to
have to think about, and what the crate owes them instead.

Status: design. Nothing here is implemented yet beyond what is marked *shipped*.

## 1. The one rule

A consumer of the async surface never names any of these:

`tokio` · `Pin` · `Send` · `'static` · `JoinHandle` · `JoinError` · `abort` ·
`Arc` (as task plumbing) · `select!` · `Future` extension traits

Not "usually doesn't" — *never*. Each name in that list is a piece of
scheduling machinery. Cognitive load is the sum of the machinery a caller must
hold in their head to state their intent. The SDK's whole job is to pay that
cost once, in one place, so callers pay it zero times.

The test for any new API: **does the caller's code read as a description of
what they want done, or of how the runtime should do it?** If the latter, the
API is wrong.

## 2. The constraint that shapes everything

Withoutboats' *scoped task trilemma*: a sound API can provide **at most two** of

1. **Concurrency** — children proceed concurrently with the parent.
2. **Parallelizability** — children can proceed in parallel (multiple cores).
3. **Borrowing** — children can borrow the parent's data without `Arc`.

This is not a Rust implementation gap; it follows from `std::mem::forget` being
safe. It is why `tokio::spawn` requires `'static + Send`, why `moro` is
nightly-only and unsafe, and why `std::thread::scope` blocks.

**Consequence for the design: do not promise all three.** An API that appears to
offer them and quietly drops one is worse than one that names the trade. The SDK
exposes two verbs, each honest about which two horns it has:

| Verb | Concurrency | Parallel | Borrows | Cost |
|---|---|---|---|---|
| `scope.spawn(fut)` | yes | yes | no — `'static` | caller owns or `Arc`s its data |
| `(a, b).join()` | yes | no — one task | **yes** | a blocking branch stalls siblings |

Choosing between them is a real decision the caller makes *once*, at the call
site, and the type system tells them which one they got. That is the reduction
in cognitive load: not hiding the trade, but making it a single, legible choice
instead of a running background concern.

## 3. The public surface

### 3.1 Entry — *shipped*

```rust
let runtime = Runtime::new()?;          // owned, no ambient global
let answer = runtime.block_on(async { 2 + 2 });
```

No attribute macro. A re-exported proc-macro expands to `::tokio` paths a
consumer without a tokio edge cannot resolve. Entry stays explicit.

### 3.2 Structured scope — *to build*

```rust
let total = scope(|s| async move {
    let a = s.spawn(async { fetch_prices().await });   // Task<Vec<Price>>
    let b = s.spawn(async { fetch_fx().await });       // Task<FxTable>
    let (prices, fx) = (a, b).join().await;
    reconcile(prices, fx)
})
.await?;
```

Guarantees, each of which is an invariant with a test:

- **INV-RT-NO-ESCAPE** — the scope handle cannot leave the closure. The handle
  is not a value the caller can store. (Swift enforces this with `inout` +
  `mutating`; we enforce it by construction — the handle is only ever passed
  into the closure.)
- **INV-RT-NO-LEAK** — when `scope` returns, every child has finished, been
  cancelled, or been reaped. It cannot return early on a momentarily-empty set.
  A bare `JoinSet` loop does **not** satisfy this: it returns the moment the set
  happens to be empty, which is not the same as "no more children will be
  spawned". The scope must track *outstanding* children (a count incremented on
  spawn and decremented on completion) and wait on a notify, rather than
  observing set emptiness.
- **INV-RT-NO-ORPHAN-JOIN** — a `Task` need not be awaited for the scope to wait
  for it. Forgetting a task is not a leak; it is a discarded result.
- **INV-RT-FAIL-FAST** — the first child error cancels its siblings and surfaces
  at the scope boundary. `Scope::supervisor()` opts out and collects all errors.

`Task<T>` awaits to `T`, not `Result<T, JoinError>`. A panicking child resumes
the panic on the awaiter — matching what `join_all` already documents. A
`JoinError` in a caller's signature is machinery leaking upward.

### 3.3 Readiness handoff — *to build*

The highest-value idea in the landscape that no Rust crate has. From trio's
`nursery.start()` + `TaskStatus.started(value)`:

```rust
let listener = scope(|s| async move {
    let listener = s.start(bind(addr)).await?;   // fails HERE if bind fails
    serve(listener).await
})
.await?;
```

Without it, "start a service and wait until it is up" is
spawn → sleep → hope. With it, a failure during startup is an ordinary error at
the call site, not a group failure arriving later from an unrelated direction.
This is what makes "go do this thing" read as straight-line code.

### 3.4 Declarative work — *partly shipped*

`BotSpec` is already documented as *"what an AI emits and what a manifest
contains"* — a serializable, capability-gated contract the runtime executes.
That is the "tell it to go do something" surface, and it exists.

The gap is that it covers **observation chains**, not general work. The
generalization is a `PlanSpec`: the same shape (serde, `deny_unknown_fields`,
capability-validated at build) for arbitrary steps rather than source→action
tuples. Do not design this until the scope work lands — `PlanSpec` should be
expressed *in terms of* `scope`, not alongside it.

## 4. Determinism

**Determinism is not an engine property.** It is four things, and none of them
require leaving tokio:

1. A scheduler whose ready-queue order is a function of the seed, not of thread
   arrival time.
2. A virtual clock replacing wall-clock reads.
3. Seeded entropy replacing OS entropy.
4. Deterministic event ordering at every collection that is iterated.

The estate already owns the most important piece and has not noticed:
**`lgwks_std::task` has no `spawn`.** With no detach, there is no way to leak a
task, and `join_all` returns results in input order regardless of completion
order. It is a structured executor by construction, in zero dependencies.

Two things it must gain:

- **A wake-set ready queue.** Today every child is polled with the same `cx`, so
  any one child's wake repolls every pending child — unconditionally, whether or
  not that child's event fired. Measured cost, standalone replica of
  `join_all_boxed`'s algorithm, one chatty source among `m` quiet ones:
  **21.6× wasted polls at m=31** (the current `MAX_IN_FLIGHT_POLLS`), rising to
  **170.9× at m=511**. Giving each child its own waker and repolling only woken
  children makes it O(woken) instead of O(pending). This is what makes wide
  fan-out possible without tokio.

  Precision on that number: the wasted polls are measured against a workload
  whose quiet children do not wake themselves, which is what a future waiting on
  an unfired timer or socket actually does — it registers its waker with the
  event source and returns `Pending`. The Future contract requires exactly that,
  so the waste accrues for well-behaved futures. A future that returns `Pending`
  *without* registering a waker is relying on the current design's spurious
  rescans to make progress at all; such a future would hang under a wake-set
  queue, and it is already incorrect today. The fix therefore tightens the
  contract rather than loosening it — worth stating in `join_all`'s docs when
  this lands.
- **A clock and an entropy source behind traits**, with real and virtual
  implementations, so the same code runs reproducibly under a seed.

`docs/distributed-boundaries.md` records virtual time as missing. It remains
missing; this is the design that closes it.

### What determinism does *not* buy

Making the runtime deterministic does **not** make an AI agent deterministic.
The nondeterminism lives in the model's sampling and in the tools it calls —
neither of which the runtime owns. What journaling buys is that the agent's
*decisions* can be replayed by reading them back rather than re-deriving them.
Do not sell determinism as agent reproducibility to anyone.

## 5. Failure and cancellation

- **Fail-fast by default.** trio, Kotlin `coroutineScope`, and
  `asyncio.TaskGroup` all fail fast; Kotlin's `supervisorScope` is the explicit
  opt-out. Match that default.
- **Cancellation is an expression, not bookkeeping.** `scope.cancel(reason)`
  terminates the group and yields the reason, in the spirit of `moro`'s
  `scope.cancel(v)`.
- **`Drop` cannot await.** Services need an explicit shutdown; the runtime's
  `shutdown_timeout` bounds the blocking pool only. Say so in the docs rather
  than implying a grace period that does not exist.

## 6. What is already right — do not break it

- **The facade is the asset.** `lgwks_bot::rt` hiding tokio behind estate types
  is what makes a future engine swap cheap. `AGENTS.md` designates `lgwks_bot`
  as the async surface and `lgwks_deps` as the sole owner of the tokio edge.
  Keep the public signatures engine-agnostic.
- **No `spawn` in `lgwks_std::task`.** Adding one would introduce the entire
  leak problem the estate currently cannot have.
- **Bounded fan-out.** `join_all_bounded` never exceeding its limit is a
  property worth keeping in every new primitive.

## 7. Non-goals

- **No new async runtime.** `glommio` and `tokio-uring` are Linux-only;
  `compio` (0.19.x, 18 breaking releases) falls back to kqueue on macOS, which
  is what tokio already does — no local gain for a pre-1.0 API. `async-std` is
  discontinued (its README says "use smol instead").
- **No new logging dependency.** `log`/`tracing`/`env_logger` are not estate
  capabilities (`docs/distributed-boundaries.md`, Observability). Library code
  returns information; it does not print it.
- **No `futures-concurrency` dependency.** It would pull `futures-core`,
  `futures-lite` and `pin-project` to provide a `(a, b).join()` this SDK can
  offer natively on primitives the estate already owns. Study its API; do not
  import its closure.
- **No `tokio-util` admission.** The obvious shortcut for §3.2 is
  `tokio_util::task::TaskTracker` + `util::CancellationToken`. `tokio-util`
  0.7.19 is present in `Cargo.lock` only as a transitive dependency; it is not
  an admitted storefront edge, so both types are unavailable to estate code.
  Admitting it would be defensible — it is well maintained and is exactly the
  kind of edge `lgwks_deps` exists to carry — but the semantics needed here are
  an outstanding-child counter, a notify, and a shutdown flag. That is a module,
  not a storefront edge, and building it keeps the estate's `forbid`-level
  guarantees (bounded, no detach, no `Drop`-as-async-shutdown) visible in one
  place instead of spread across a third-party surface. Revisit if the scope
  grows features beyond these three.
- **No dependency at all for determinism — but not for the reason it looks like.** Build the clock, seeded RNG, and wake-set scheduler as `lgwks_std`
  modules. That is the estate's own ELIMINATE doctrine: a capability that is
  a few hundred lines becomes a module, not a storefront edge.

## 8. Migration order

Each step must leave `cargo test --workspace --all-targets` green.

1. Get the tree compiling (the lint contract landed before the code satisfied it).
2. `scope` + `Task` + `Scope::supervisor`, on top of `tokio::task::JoinSet`
   (already re-exported by `rt::task`) plus a ~40-line outstanding-child counter
   and a `tokio::sync::Notify`. No new dependency — see §7 on `tokio-util`.
3. Wake-set ready queue in `lgwks_std::task`, with the fan-out benchmark as the
   acceptance test.
4. Clock + entropy traits with real and virtual implementations.
5. Readiness handoff (`scope.start`).
6. `PlanSpec`, expressed in terms of `scope`.

Step 2 is additive: `spawn`/`JoinSet`/`join_all_bounded` stay as they are and
are re-expressed in terms of `scope` once it exists. Parallel seams, not
demolition.
