# lgwks_bot

Capability-gated automation bots.

A bot framework built on four fixed verbs (Observe, Evaluate, Execute, Query)
that run as systems on a [Bevy ECS](https://bevy.org) schedule. Three properties
distinguish it from a task runner:

- **Authority is proof-carrying.** Every effect takes an `(Auth, input)` tuple,
  and only `GrantSet::issue` mints the `Auth` half. Capabilities are checked at
  build time *and* on every call. The grants are a **snapshot**, not a live
  lease — see [Authority is a snapshot](#authority-is-a-snapshot).
- **A condition is change detection, not re-evaluation.** A chain fires on the
  tick its source value *moves*. A source that holds still fires nothing, and
  the framework tells you which sources moved (`bot.revisions()`).
- **A nondeterministic schedule is refused at build.** Ambiguity detection runs
  as an error, so an ordering the engine cannot fix is a build failure rather
  than a misordering discovered at 3am.

```sh
cargo add lgwks_bot
```

**Version boundary.** `crates/lgwks-bot/Cargo.toml` reads `0.4.2`, but the
`session`, `language`, `semantic`, `interface`, and `frontier` modules are on
`main` and are **not** in the published `lgwks_bot-v0.4.2` tag — that tag's
`lib.rs` exports `cap`, `domain`, `error`, `gate`, `json`, `rt`, `spec`, and
`verb`, and nothing else. This README, like the rustdoc beside it, describes
`main`. Check `CHANGELOG.md` for the release that carries a symbol before
depending on it, and do not assume an installed `0.4.2` has these.

## Quick start

```rust
use lgwks_bot::{Auth, Bot, BotError, Cap, GrantSet};
use lgwks_bot::verb::{Execute, Observe};

/// A source. `poll` runs each tick; the framework fires the chain only when the
/// returned value differs from the previous tick's.
struct QueueDepth {
    caps: Vec<Cap>,
}

impl Observe for QueueDepth {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(42) // your real read goes here
    }

    fn domain_id(&self) -> &str {
        "queue::depth"
    }
}

/// An effect. `execute_action` runs only when the chain's condition holds.
struct PageOnCall;

impl Execute for PageOnCall {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        // your real effect goes here
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "notify::page"
    }
}

let grants = GrantSet::empty().grant(Cap::net());

let mut bot = Bot::builder("queue-watch")
    .observe(QueueDepth { caps: vec![Cap::net()] })
    .on(|depth: &u32| *depth > 10, PageOnCall)
    .build(&grants)?;

// One tick: poll every source, fire every chain whose condition now holds.
// `tick` is synchronous — the systems drive the non-`Send` verb futures
// themselves, so there is nothing for a caller to await.
let fired = bot.tick()?;
assert_eq!(fired, 1);
# Ok::<(), BotError>(())
```

`bot.tick()` returns the number of actions fired. A built bot is `mut`
because a tick advances its world.

## Relationship to tokio

It runs on tokio rather than replacing it. The async engine is
sourced from the `lgwks_deps` storefront, and `lgwks_bot` exports it as
`rt::*` so a consumer never names `tokio` directly. If what you want is
futures, timers, sockets, and channels, `rt::` gives you exactly that.

What tokio does not give you is the layer above: what to poll, when, in what
order, and with what authority. Concretely:

| You would otherwise | Here |
|---|---|
| Re-read every source each tick and diff it yourself | `Changed<Revision>` on the source entity, maintained by the schedule |
| Thread an `Auth`/permission check through every call site by hand | `(Auth, input)` on every verb; only `GrantSet::issue` mints `Auth` |
| Discover a nondeterministic system order in production | `ScheduleBuildSettings { ambiguity_detection: Error }` refuses it at `build()` |
| Define your own notion of "an action" per project | Four verbs, closed. No fifth exists |
| Hope a config file's meaning matches the code's | `BotSpec` is the serializable contract, with typed parse errors |

The honest boundary: this is **not a sandbox**. In-process code can always dial
out directly, and capability gating is auditable authority, not isolation.

Against other bot frameworks, the distinction is the same one restated: most
are either a hosted GUI workflow editor (n8n, Zapier, Make) or an actor library
with no notion of authority (ractor, actix). This one is a compiled,
capability-gated execution model with a serializable spec, in Rust.

## The four verbs

| Verb | Trait | Purpose |
|------|-------|---------|
| **Observe** | `verb::Observe` | Watch a source: poll, listen, stream. Async; produces a value each tick. |
| **Evaluate** | `verb::Evaluate<T>` | Gate on a condition. Boolean over observed state. Synchronous and pure. Closures implement this automatically. |
| **Execute** | `verb::Execute` | Perform a side effect. Async and capability-gated. The action half of the chain. |
| **Query** | `verb::Query` | Read without side effects. Async, direct call, no chain required. |

No fifth verb exists. New domains implement these four rather than adding verbs.

## Execution model

A `Bot` owns a `bevy_ecs::World` and a `Schedule` with exactly two systems,
`observe` then `fire`. Both are **exclusive systems** (`fn(&mut World)`), which
is what lets them drive the verbs' deliberately non-`Send` futures directly.

- **`observe`** polls every source in bounded concurrent waves (32 in flight),
  compares each result with the remembered one, and bumps that source's
  `Revision(u64)` marker component *only when the value moved*.
- **`fire`** walks the sources in declaration order, selects those matching
  `Changed<Revision>`, evaluates each chain's condition, and runs the actions
  whose conditions hold, also in declaration order, so side effects stay
  deterministic.

Two consequences worth knowing before you rely on `tick`:

- **A failing poll fires nothing; a failing action does not undo anything.**
  Every source is polled before any effect runs, so a poll that fails fires
  *nothing* for that tick and returns the first error. Actions then run in
  sequence, so an action that fails returns the first error *after* the actions
  before it have already taken effect. There is no rollback. See
  [Failure](#failure).
- **Values live in a `NonSend` resource; entities carry only a revision.** Bevy
  requires `Component: Send + Sync + 'static` with no opt-out, and this crate's
  futures are not `Send` on purpose, so a polled value cannot be a component.
  See `docs/bevy-admission.md` §4 for the measurement and the rejected
  alternative.

Futures are **local** (not `Send`): a bot is driven on the calling thread and
domains may hold thread-local state. `rt::task::LocalSet` is available when you
need to spawn such a future.

### Failure

`tick` returns `Err(BotError)` in two situations, and they mean different things.

**A poll failed.** No `Revision` was written, so no condition was evaluated and
no action ran. The tick had no effect, and the source values are exactly as they
were. This is the strong case, and it is the one an all-or-nothing reading is
tempted to generalise from — it does not generalise.

**An action failed.** Actions run in declaration order, so the actions *before*
the failure already ran and their effects are live. `tick` reports the first
error and does not roll anything back, because an external effect cannot be
rolled back. `Err` here means "this run did not finish", not "nothing happened".

Two consequences follow, and neither is fixed by retrying blindly:

- **The unattempted work is lost, not queued.** Revisions are committed in the
  observe phase, before any action runs, so the next tick sees an unchanged
  source and does not re-fire the chain. The actions after the failure are
  simply not attempted again.
- **A retry may duplicate.** An action that failed after its request was sent
  fails as `BotError::EffectIndeterminate`, which says the effect may be live.
  That variant exists precisely so this is readable from the type rather than
  inferred from a cause string: `BotError::DomainError` means the effect did not
  happen and a retry is a retry, while `EffectIndeterminate` means a retry is a
  possible duplicate — a second merge, message, or process launch. Consumers
  that retry should match on the variant and treat these two differently.

Delivering exactly-once across an external effect needs durable intent and an
outcome-unknown record, which this crate does not yet provide. Until it does,
treat a failed tick as a partial run to be reconciled rather than a no-op to be
retried.

## Capability system

Every domain declares the capabilities it requires. The builder validates
`required ⊆ granted` before construction. A bot asking for `bot.net` without a
grant fails at build time, not at runtime.

Authority stays proof-carrying past build: every `poll`, `execute_action`, and
`query` takes an `(Auth, input)` tuple, and only `GrantSet::issue` can mint the
`Auth` half. The framework mints a fresh proof for each call, from the grant set
it retained at build, and each callee checks coverage before acting, so a
narrower proof presented to a broader domain is denied (no confused deputies).
`Evaluate` takes no proof, because it is pure boolean logic with no side effect
to gate.

### Authority is a snapshot

A built bot owns a *clone* of the grant set it was built with. `GrantSet` has no
`revoke`, and an `Auth` holds the capabilities it was issued with, so:

- Changing or dropping the `GrantSet` you passed to `build` does not narrow the
  bot. It keeps the authority it was admitted with for its whole life.
- An `Auth` in hand stays valid for the capabilities it covers. It is a
  capability-membership proof inside this process — not a signature, not an
  identity, and not a lease with an expiry.
- Narrowing a running bot is therefore a decision made *outside* this API: build
  it from a narrower set, or stop calling `tick`. There is no in-process
  revocation, and nothing here should be described as one.

This is a real limitation and it is stated as one. Live revocation needs a
linearization point, a token generation, and a defined answer for effects
already in flight; none of those exist here yet.

| Capability | Constant | Description |
|------------|----------|-------------|
| `bot.net` | `Cap::NET` | Network access: HTTP, WebSocket, API calls |
| `bot.fs` | `Cap::FS` | Filesystem access: read, write, watch paths |
| `bot.sys` | `Cap::SYS` | System access: process control, environment |
| `bot.notify` | `Cap::NOTIFY` | Notification delivery: Slack, email, webhooks |

Custom capabilities use `Cap::new("your.domain.cap")`.

## Shipped domains

Nine domains ship with the crate, each implementing one or more verbs:

| Domain | Module | Capabilities | Verbs |
|--------|--------|-------------|-------|
| GitHub | `domain::gh` | `bot.net` | Observe, Execute, Query |
| Network | `domain::net` | `bot.net` | Observe, Query |
| Chat | `domain::chat` | `bot.net` | Observe, Query |
| Filesystem | `domain::fs` | `bot.fs` | Observe, Query |
| Data store | `domain::data` | `bot.fs` | Observe, Query |
| System | `domain::sys` | `bot.sys` | Observe, Execute, Query |
| Notifications | `domain::notify` | `bot.notify`, `bot.net` | Execute |
| Flow | `domain::flow` | inherited | Execute (pipeline) |
| Evaluators | `domain::eval` | — | Evaluate (changed, threshold) |

## Serializable specs

`BotSpec` is the serializable contract: what an AI emits, what a manifest
contains. It round-trips through JSON:

```rust
use lgwks_bot::BotSpec;

// `r##"…"##`, not `r#"…"#`: the payload contains `"#deploys"`, and the `"#` in
// that channel name would otherwise terminate the raw string early.
let spec = BotSpec::from_json(
    r##"{
    "name": "larry",
    "chains": [{
        "source": "gh::pr_status",
        "target": "owner/repo",
        "on": [["checks_changed", {"domain": "notify::slack", "target": "#deploys"}]]
    }]
}"##,
)?;

let json = spec.to_json()?;

// The two halves of the round-trip have different error types, deliberately:
// `from_json` reports typed field and size errors (`BotError`) because untrusted
// input can be wrong in domain ways, while `to_json` can only fail in the
// serializer (`json::Error`), because a `BotSpec` in hand is already valid.
# Ok::<(), Box<dyn std::error::Error>>(())
```

`BotSpec` is validate-only: there is no `from_spec` materializer. A spec that
validates still builds through `Bot::builder`, so capability grants stay
explicit at the call site. The builder chain is the DSL. There is deliberately
no `bot!` proc-macro: it would drag `syn` into every consumer (the dependency
policy restricts `syn` to the `lgwks_deps` gate tool) and hide the per-call
`Auth::check` that auditors read.

## Async runtime surface

`lgwks_bot` also provides the asynchronous runtime surface (default feature
`rt`). The `tokio` edge appears in `contract/APPROVED.toml` with
`owner = "lgwks_deps"`, and `lgwks-deps check` refuses any other workspace crate
that declares `tokio` directly (`INV-DEP-EDGE-OWNED`). `--no-default-features`
compiles the crate with no tokio at all.

On `wasm32-wasip1`, the `rt` feature uses a current-thread runtime;
`Builder::worker_threads(Some(_))` returns `Unsupported`. The native-only
`net`/`process`/`fs`/`signal` driver features are outside the WASM domain.

| Module | Feature | Contents |
|---|---|---|
| `rt::runtime` | `rt` | `Runtime`, `Builder`, `Handle`, free `block_on` |
| `rt::task` | `rt` / `sync` | `spawn`, `spawn_local`, `LocalSet`, `JoinHandle`, `JoinError`, `JoinSet`, `spawn_blocking`, `yield_now`; `join_all_bounded` requires `sync` |
| `rt::time` | `time` | `sleep`, `sleep_until`, `timeout`, `timeout_at`, `interval`, `Instant`, `Elapsed` |
| `rt::sync` | `sync` | `CancellationToken`, `mpsc`, `oneshot`, `broadcast`, `watch`, `Mutex`, `RwLock`, `Semaphore`, `Notify`, `Barrier`, `OnceCell` |
| `rt::io` | `io` | `AsyncRead`/`AsyncWrite`/`AsyncBufRead` and their extensions, `BufReader`, `BufWriter`, `duplex`, `copy` |
| `rt::net` | `net` | `TcpListener`, `TcpStream`, `UdpSocket`, `lookup_host` |
| `rt::process` | `process` | `Command`, `Child` and its pipes |
| `rt::fs` | `fs` | async filesystem (blocking-threadpool wrapper) |
| `rt::signal` | `signal` | OS signal streams (Unix/Windows) |

`select!`, `join!`, and `try_join!` are re-exported at the crate root. There is
deliberately **no attribute macro**: a re-exported proc-macro expands to
`::tokio` paths a consumer without a `tokio` edge cannot resolve. Enter through
`Runtime::block_on` instead.

### Cancellation

`rt::sync::CancellationToken` is provided by this crate, built on
`tokio::sync::watch` rather than re-exported from `tokio-util`. A child token is cancelled when its
parent is; cancelling a child never touches the parent. The parent link points
*up*, so a token you hold can always be cancelled even if an intermediate token
in its chain has been dropped.

This completes the background-work contract the crate already enforced: every
task is tracked in a `JoinSet` and observes a `CancellationToken`.

```rust
use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::rt::task::JoinSet;

# let runtime = lgwks_bot::Runtime::new()?;
# runtime.block_on(async {
let token = CancellationToken::new();
let mut tasks = JoinSet::new();

let worker = token.clone();
tasks.spawn(async move {
    worker.cancelled_owned().await;
    // stop
});

token.cancel();
while let Some(joined) = tasks.join_next().await {
    joined?;
}
# Ok::<(), lgwks_bot::rt::task::JoinError>(())
# })?;
# Ok::<(), std::io::Error>(())
```

### Bounded fan-out

```rust
use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time::{Duration, sleep};

let runtime = lgwks_bot::Runtime::new()?;
let results = runtime.block_on(async {
    join_all_bounded(4, (0..16).map(|index| async move {
        sleep(Duration::from_millis(1)).await;
        index * index
    }))
    .await
});
assert_eq!(results.len(), 16);
# Ok::<(), std::io::Error>(())
```

### Supervision

`rt::supervise::Supervisor` is the way to run background work, and it exists so
that a caller never has to reason about a leak or a runaway loop.

A task cannot leak. `spawn` returns no handle, so there is nothing for a caller
to drop. There is no unbounded
constructor and no internal queue: the ceiling is taken at `new`, and the permit
is acquired *before* the spawn, so waiting is real backpressure rather than
buffering. `try_spawn` refuses instead of growing, and counts the refusal.
Finished tasks are reaped at every entry point, which matters because a
`JoinSet` retains a completed task's slot until it is joined. `Drop` cancels and
aborts, so there is no `close()` to forget.

A loop cannot run away. `repeat` cannot be written without a `Budget`, and every
iteration *races* the token rather than checking it between iterations, so a
cancel drops a body that is still awaiting rather than waiting for it to finish.
`Budget::Ongoing` is the unbounded case, and it is cancellation-bounded rather
than free-running.

A caller can tell how a task ended. Every task produces exactly one
`TaskOutcome`, carrying the `TaskId` the supervisor assigned in spawn order, and
`Stats` counts the four outcomes separately: `succeeded`, `cancelled`, `aborted`,
`panicked`. `Stats::completed` is their sum — a resource count, not a success
count — so a task that panicked before producing its result can never be read as
one that finished. The report buffer is capped at the in-flight ceiling, and
`Stats::reports_dropped` counts what a caller that never drains it missed.

```rust
use lgwks_bot::rt::supervise::{Budget, Supervisor};

# let runtime = lgwks_bot::Runtime::new()?;
# runtime.block_on(async {
let mut supervisor = Supervisor::new(4); // at most 4 in flight, ever

supervisor
    .spawn_repeating(Budget::Ongoing, |tick| async move {
        let _tick = tick;
    })
    .await;

// Refuses rather than growing past the ceiling.
assert!(supervisor.try_spawn(|_token| async {}).is_ok());

// Cancel, drain, join — and hand back the evidence.
let report = supervisor.shutdown().await;
assert_eq!(report.stats().spawned, 2);
assert_eq!(report.stats().completed, 2);
// One terminal outcome per task, each naming the task it is about.
assert_eq!(report.outcomes().len(), 2);
# });
# Ok::<(), std::io::Error>(())
```

Invariants (enforced by `crates/lgwks-bot/tests/rt_async_tier.rs` and
`rt::supervise`'s unit tests):

- **INV-RT-SINGLE-ENTRY** — only `lgwks_deps` authors a `tokio` edge.
- **INV-RT-BOUNDED-FANOUT** — `join_all_bounded` never exceeds its limit, still
  runs every input, and returns results in input order.
- **INV-RT-PANIC-ISOLATION** — a panicking *input* is resumed on the awaiting
  task (it does not abort the process); a panicking *spawned task* becomes a
  `JoinError`.
- **INV-RT-DROP-DETACHES** — dropping a `JoinHandle` detaches the task; only
  `abort` cancels it.
- **INV-RT-EXPLICIT-OWNER** — no hidden global reactor; the `Runtime` is owned.
- **INV-RT-SUPERVISED** — a `Supervisor` retains at most its in-flight bound
  however many times it is spawned into, refuses rather than growing, and stops
  every task it owns when it is dropped or shut down.

## The other crates

Four crates ship from this repository. They share a release process, not a
dependency graph: `lgwks_bot` and `lgwks_deps` depend on `lgwks_std`, and
`lgwks_ast` stands alone.

| Crate | What it gives you |
|---|---|
| [`lgwks_std`](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: codecs, a blocking HTTP client, retry, structured logging, time, hashing, ids |
| [`lgwks_ast`](https://docs.rs/lgwks_ast) | Parse many languages into one AST type, with bounded traversal and typed diagnostics |
| [`lgwks_deps`](https://docs.rs/lgwks_deps) | The audited storefront for third-party stacks. `lgwks_bot`'s async engine is selected through it, so this crate's tokio edge is the workspace's only one |

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents.

## License

MPL-2.0 — Copyright 2026 Logical Works Incorporated

Deliberately not the workspace's Apache-2.0. The bot is the artefact the rest of
the estate embeds, so it carries file-level copyleft while its three siblings
stay permissive. A proprietary consumer is still permitted — MPL-2.0 §3.3 — and
only modification of the MPL-covered files carries an obligation. See
[`LICENSING.md`](../../LICENSING.md).
