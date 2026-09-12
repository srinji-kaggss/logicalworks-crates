# lgwks_bot — automation bots with a four-verb execution model

A capability-gated bot framework built on four fixed verbs: **Observe**,
**Evaluate**, **Execute**, **Query**. Bots are built from `(condition, action)`
chains that bind observed sources to side effects. The framework validates
capabilities at build time and dispatches at tick time.

Three of the four verbs are **async** (`Evaluate` stays synchronous and pure,
because it has no side effect to gate). `Bot::tick` polls every source
concurrently on the estate's zero-dependency `lgwks_std::task` executor — no
`futures`, no `async-trait` — and `Bot::block_on_tick` drives the same
computation for synchronous callers. Blocking domain work (file reads, HTTP)
runs on a `lgwks_std::task::spawn_blocking` thread so it overlaps its siblings
instead of stalling them.

The crate is also the estate's **async and runner surface** (default feature
`rt`): an explicitly owned `Runtime` (multi-threaded on native targets and
current-thread on WASM), bounded fan-out, timers, channels, and opt-in
`net`/`process`/`fs`/`signal` drivers. The engine is sourced from the
`lgwks_deps` storefront (`feature = "tokio"`), so no other crate authors a
`tokio` edge. `--no-default-features` withdraws it entirely and leaves the
actor surface on the synchronous executor.

## Quick start

```rust
use lgwks_bot::{Auth, Bot, Cap, GrantSet};
use lgwks_bot::verb::{Observe, Execute};

// 1. Implement Observe on your source — the verb is async
struct PrWatcher {
    caps: Vec<Cap>,
    /* ... */
}
impl Observe for PrWatcher {
    type Output = PrState;
    fn required_caps(&self) -> &[Cap] { &self.caps }
    async fn poll(&self, call: (Auth, ())) -> Result<PrState, lgwks_bot::BotError> {
        call.0.check(self.required_caps())?;
        /* ... */
    }
    fn domain_id(&self) -> &str { "gh::pr_status" }
}

// 2. Implement Execute on your action — also async
struct SlackNotify {
    caps: Vec<Cap>,
    /* ... */
}
impl Execute for SlackNotify {
    type Input = PrState;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &self.caps }
    async fn execute_action(&self, call: (Auth, &PrState)) -> Result<(), lgwks_bot::BotError> {
        call.0.check(self.required_caps())?;
        /* ... */
    }
    fn domain_id(&self) -> &str { "notify::slack" }
}

// 3. Build with capability grants
let bot = Bot::builder("ci-watcher")
    .observe(PrWatcher::new("owner/repo"))
    .on(|pr: &PrState| pr.checks_changed, SlackNotify::new("#deploys"))
    .build(&GrantSet::empty().grant(Cap::net()).grant(Cap::notify()))?;

// 4. Tick — polls sources concurrently, evaluates conditions, fires matching actions
let fired = bot.block_on_tick()?;   // sync callers
// let fired = bot.tick().await?;   // already-async callers
```

## The four verbs

| Verb | Trait | Purpose |
|------|-------|---------|
| **Observe** | `verb::Observe` | Watch a source — poll, listen, stream. Async; produces a value each tick. |
| **Evaluate** | `verb::Evaluate<T>` | Gate on a condition. Boolean over observed state. Synchronous and pure. Closures implement this automatically. |
| **Execute** | `verb::Execute` | Perform a side effect. Async and capability-gated. The action half of the chain. |
| **Query** | `verb::Query` | Read without side effects. Async, direct call, no chain required. |

No fifth verb exists. New domains add implementations of these four, not new
verbs.

## Async execution model

- `Bot::tick` is an `async fn`; it polls all sources in bounded concurrent waves
  (`MAX_IN_FLIGHT_POLLS = 32`) with `lgwks_std::task::join_all`, then fires
  matching actions sequentially in declaration order so side effects stay
  deterministic.
- Futures are **local** (not `Send`): `lgwks_std::task` drives a bot on the
  calling thread, and domains may hold thread-local state. Embedders that need
  multi-threaded execution run the bot on a dedicated thread.
- A blocking domain offloads its syscall with `lgwks_std::task::spawn_blocking`
  — one OS thread per in-flight call — so the executor is never stalled. The
  wave cap bounds simultaneous blocking threads by `MAX_IN_FLIGHT_POLLS`.

## Capability system

Every domain declares the capabilities it requires. The bot builder validates
`required ⊆ granted` before construction — a bot that asks for `bot.net` without
a grant fails at build time, not at runtime.

Authority is proof-carrying past build: every `poll`, `execute_action`, and
`query` takes an `(Auth, input)` tuple, and only `GrantSet::issue` can mint the
`Auth` half.
The framework issues a fresh proof per domain on every `tick`; each callee
checks coverage before acting, so a narrower proof presented to a broader
domain is denied (no confused deputies). `Evaluate` takes no proof — it is
pure boolean logic with no side effect to gate. This is explicit, auditable
authority, not a sandbox: in-process code can always dial out directly.

Shipped capabilities:

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

let spec = BotSpec::from_json(r#"{
    "name": "larry",
    "chains": [{
        "source": "gh::pr_status",
        "target": "owner/repo",
        "on": [["checks_changed", {"domain": "notify::slack", "target": "#deploys"}]]
    }]
}"#)?;

let json = spec.to_json()?;
```

## Async runtime surface

Beyond the bot executor, `lgwks_bot` exposes the estate's async runtime. It is
gate-enforced: `tokio` appears in `contract/APPROVED.toml` with
`owner = "lgwks_deps"`, and `lgwks-deps check` refuses any other workspace crate
that declares `tokio` directly (`INV-DEP-EDGE-OWNED`). `--no-default-features`
compiles the crate with no tokio at all.

On `wasm32-wasip1`, the `rt` feature uses a current-thread runtime;
`Builder::worker_threads(Some(_))` returns `Unsupported`. The native-only
`net`/`process`/`fs`/`signal` driver features are outside the WASM domain.

| Module | Feature | Contents |
|---|---|---|
| `rt::runtime` | `rt` | `Runtime`, `Builder`, `Handle`, free `block_on` |
| `rt::task` | `rt` / `sync` | `spawn`, `JoinHandle`, `JoinError`, `JoinSet`, `spawn_blocking`; `join_all_bounded` requires `sync` |
| `rt::time` | `time` | `sleep`, `timeout`, `interval`, `Instant`, `Elapsed` |
| `rt::sync` | `sync` | `mpsc`, `oneshot`, `broadcast`, `watch`, `Mutex`, `RwLock`, `Semaphore`, `Notify`, `Barrier` |
| `rt::net` | `net` | `TcpListener`, `TcpStream`, `UdpSocket`, `lookup_host` |
| `rt::process` | `process` | `Command`, `Child` and its pipes |
| `rt::fs` | `fs` | async filesystem (blocking-threadpool wrapper) |
| `rt::signal` | `signal` | OS signal streams (Unix/Windows) |

`select!`, `join!`, and `try_join!` are re-exported at the crate root. There is
deliberately **no attribute macro**: a re-exported proc-macro expands to
`::tokio` paths a consumer without a `tokio` edge cannot resolve. Enter through
`Runtime::block_on` instead.

```rust
use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time::{sleep, Duration};

let runtime = lgwks_bot::Runtime::new().expect("runtime");
let results = runtime.block_on(async {
    join_all_bounded(4, (0..16).map(|i| async move {
        sleep(Duration::from_millis(1)).await;
        i * i
    }))
    .await
});
assert_eq!(results.len(), 16);
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
