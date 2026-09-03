# lgwks_bot — automation bots with a four-verb execution model

A capability-gated bot framework built on four fixed verbs: **Observe**,
**Evaluate**, **Execute**, **Query**. Bots are built from `(condition, action)`
chains that bind observed sources to side effects — no scheduling, no
orchestration engine, no runtime. The framework validates capabilities at build
time and dispatches at tick time.

## Quick start

```rust
use lgwks_bot::{Bot, Cap, GrantSet};
use lgwks_bot::verb::{Observe, Execute};

// 1. Implement Observe on your source
struct PrWatcher { /* ... */ }
impl Observe for PrWatcher {
    type Output = PrState;
    fn required_caps(&self) -> &[Cap] { &[Cap::net()] }
    fn poll(&self) -> Result<PrState, lgwks_bot::BotError> { /* ... */ }
    fn domain_id(&self) -> &str { "gh::pr_status" }
}

// 2. Implement Execute on your action
struct SlackNotify { /* ... */ }
impl Execute for SlackNotify {
    type Input = PrState;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[Cap::notify()] }
    fn run(&self, state: &PrState) -> Result<(), lgwks_bot::BotError> { /* ... */ }
    fn domain_id(&self) -> &str { "notify::slack" }
}

// 3. Build with capability grants
let bot = Bot::builder("ci-watcher")
    .observe(PrWatcher::new("owner/repo"))
    .on(|pr: &PrState| pr.checks_changed, SlackNotify::new("#deploys"))
    .build(&GrantSet::empty().grant(Cap::net()).grant(Cap::notify()))?;

// 4. Tick — polls sources, evaluates conditions, fires matching actions
let fired = bot.tick()?;
```

## The four verbs

| Verb | Trait | Purpose |
|------|-------|---------|
| **Observe** | `verb::Observe` | Watch a source — poll, listen, stream. Produces a value each tick. |
| **Evaluate** | `verb::Evaluate<T>` | Gate on a condition. Boolean over observed state. Closures implement this automatically. |
| **Execute** | `verb::Execute` | Perform a side effect. Capability-gated. The action half of the chain. |
| **Query** | `verb::Query` | Read without side effects. Direct call, no chain required. |

No fifth verb exists. New domains add implementations of these four, not new
verbs.

## Capability system

Every domain declares the capabilities it requires. The bot builder validates
`required ⊆ granted` before construction — a bot that asks for `bot.net` without
a grant fails at build time, not at runtime.

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

BSD-3-Clause — Copyright 2026 Logical Works Incorporated
