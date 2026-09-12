# lgwks_bot — automation bots with a four-verb execution model

A capability-gated bot framework built on four fixed verbs: **Observe**,
**Evaluate**, **Execute**, **Query**. Bots are built from `(condition, action)`
chains that bind observed sources to side effects. The framework validates
capabilities at build time and dispatches at tick time.

The four verbs are **async**. `Bot::tick` polls every source concurrently on the
estate's zero-dependency `lgwks_std::task` executor — no tokio, no `futures`, no
`async-trait` — and `Bot::block_on_tick` drives the same computation for
synchronous callers. Blocking domain work (file reads, HTTP) runs on a
`lgwks_std::task::spawn_blocking` thread so it overlaps its siblings instead of
stalling them.

## Quick start

```rust
use lgwks_bot::{Auth, Bot, Cap, GrantSet};
use lgwks_bot::verb::{Observe, Execute};

// 1. Implement Observe on your source — the verb is async
struct PrWatcher { /* ... */ }
impl Observe for PrWatcher {
    type Output = PrState;
    fn required_caps(&self) -> &[Cap] { &[Cap::net()] }
    async fn poll(&self, call: (Auth, ())) -> Result<PrState, lgwks_bot::BotError> {
        call.0.check(self.required_caps())?;
        /* ... */
    }
    fn domain_id(&self) -> &str { "gh::pr_status" }
}

// 2. Implement Execute on your action — also async
struct SlackNotify { /* ... */ }
impl Execute for SlackNotify {
    type Input = PrState;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[Cap::notify()] }
    async fn run(&self, call: (Auth, &PrState)) -> Result<(), lgwks_bot::BotError> {
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

Authority is proof-carrying past build: every `poll`, `run`, and `query` takes
an `(Auth, input)` tuple, and only `GrantSet::issue` can mint the `Auth` half.
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
| GitHub | `domain::gh` | `bot.net` | Observe, Query |
| Network | `domain::net` | `bot.net` | Observe, Execute, Query |
| Chat | `domain::chat` | `bot.net` | Observe, Execute, Query |
| Filesystem | `domain::fs` | `bot.fs` | Observe, Execute, Query |
| Data store | `domain::data` | `bot.fs` | Observe, Execute, Query |
| System | `domain::sys` | `bot.sys` | Observe, Execute, Query |
| Notifications | `domain::notify` | `bot.notify` | Execute |
| Flow | `domain::flow` | inherited | Execute (pipeline, branch, fan-out) |
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

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
