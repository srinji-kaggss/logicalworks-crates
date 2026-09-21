# lgwks_bot — capability-gated automation bots

A bot framework built on four fixed verbs — **Observe**, **Evaluate**,
**Execute**, **Query** — that run as systems on a [Bevy ECS](https://bevy.org)
schedule. Three properties distinguish it from a task runner:

- **Authority is proof-carrying.** Every effect takes an `(Auth, input)` tuple,
  and only `GrantSet::issue` mints the `Auth` half. Capabilities are checked at
  build time *and* on every call, so a grant revoked after construction cannot
  fire.
- **A condition is change detection, not re-evaluation.** A chain fires on the
  tick its source value *moves*. A source that holds still fires nothing, and
  the framework tells you which sources moved (`bot.revisions()`).
- **A nondeterministic schedule is refused at build.** Ambiguity detection runs
  as an error, so an ordering the engine cannot fix is a build failure rather
  than a misordering discovered at 3am.

```sh
cargo add lgwks_bot
```

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

## Why this and not just tokio

**It is not an alternative to tokio — it runs on it.** The async engine is
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
| **Observe** | `verb::Observe` | Watch a source — poll, listen, stream. Async; produces a value each tick. |
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
  whose conditions hold — also in declaration order, so side effects stay
  deterministic.

Two consequences worth knowing before you rely on `tick`:

- **A tick is all-or-nothing.** Every source is polled before any effect runs,
  so a poll that fails fires *nothing* and returns the first error.
- **Values live in a `NonSend` resource; entities carry only a revision.** Bevy
  requires `Component: Send + Sync + 'static` with no opt-out, and this crate's
  futures are not `Send` on purpose, so a polled value cannot be a component.
  See `docs/bevy-admission.md` §4 for the measurement and the rejected
  alternative.

Futures are **local** (not `Send`): a bot is driven on the calling thread and
domains may hold thread-local state. `rt::task::LocalSet` is available when you
need to spawn such a future.

## Capability system

Every domain declares the capabilities it requires. The builder validates
`required ⊆ granted` before construction — a bot asking for `bot.net` without a
grant fails at build time, not at runtime.

Authority stays proof-carrying past build: every `poll`, `execute_action`, and
`query` takes an `(Auth, input)` tuple, and only `GrantSet::issue` can mint the
`Auth` half. The framework issues a fresh proof per domain on every tick, and
each callee checks coverage before acting, so a narrower proof presented to a
broader domain is denied (no confused deputies). `Evaluate` takes no proof — it
is pure boolean logic with no side effect to gate.

| Capability | Constant | Description |
|------------|----------|-------------|
| `bot.net` | `Cap::NET` | Network access — HTTP, WebSocket, API calls |
| `bot.fs` | `Cap::FS` | Filesystem access — read, write, watch paths |
| `bot.sys` | `Cap::SYS` | System access — process control, environment |
| `bot.notify` | `Cap::NOTIFY` | Notification delivery — Slack, email, webhooks |

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

`BotSpec` is the serializable contract — what an AI emits, what a manifest
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
explicit at the call site. The builder chain is the DSL — there is deliberately
no `bot!` proc-macro: it would drag `syn` into every consumer (the estate bans
`syn` outside the `lgwks_deps` gate tool) and hide the per-call `Auth::check`
that auditors read.

## Async runtime surface

`lgwks_bot` is also the estate's async and runner surface (default feature
`rt`). It is gate-enforced: `tokio` appears in `contract/APPROVED.toml` with
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

`rt::sync::CancellationToken` is the estate's own, built on `tokio::sync::watch`
rather than admitted from `tokio-util`. A child token is cancelled when its
parent is; cancelling a child never touches the parent. The parent link points
*up*, so a token you hold can always be cancelled even if an intermediate token
in its chain has been dropped.

This is the other half of a rule the crate already enforced: `AGENTS.md`
requires every background task to be tracked in a `JoinSet` **and** to listen to
a `CancellationToken`.

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

Invariants (enforced by `crates/lgwks-bot/tests/rt_async_tier.rs`):

- **INV-RT-SINGLE-ENTRY** — only `lgwks_deps` authors a `tokio` edge.
- **INV-RT-BOUNDED-FANOUT** — `join_all_bounded` never exceeds its limit, still
  runs every input, and returns results in input order.
- **INV-RT-PANIC-ISOLATION** — a panicking *input* is resumed on the awaiting
  task (it does not abort the process); a panicking *spawned task* becomes a
  `JoinError`.
- **INV-RT-DROP-DETACHES** — dropping a `JoinHandle` detaches the task; only
  `abort` cancels it.
- **INV-RT-EXPLICIT-OWNER** — no hidden global reactor; the `Runtime` is owned.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
