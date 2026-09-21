# logicalworks-crates

Four small, independently versioned Rust crates: a zero-dependency core, a
runtime for long-running automation, a multi-language AST front end, and an
audited dependency storefront.

Each is usable on its own — take one and ignore the rest. They share a workspace
and a release process, not a dependency graph; only `lgwks_bot` depends on
`lgwks_std`, and nothing depends on `lgwks_ast`.

| Crate | Version | Docs | What it gives you |
|---|---|---|---|
| `lgwks_std` | 0.6.4 | [docs.rs](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: JSON/RON/wire codecs, a blocking HTTP client, retry policies, structured logging, time and calendars, hashing, ids, globs, process control |
| `lgwks_bot` | 0.4.0 | [docs.rs](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs (Observe, Evaluate, Execute, Query), capability-gated authority, change-triggered execution, and supervision that bounds background work |
| `lgwks_ast` | 0.2.0 | [docs.rs](https://docs.rs/lgwks_ast) | Parse many languages into one AST type. tree-sitter grammars behind cargo features, bounded traversal, and typed diagnostics for tools that report on code |
| `lgwks_deps` | 0.1.9 | [docs.rs](https://docs.rs/lgwks_deps) | One audited place to opt into third-party stacks, plus `lgwks-deps check` to prove no dependency entered your build unreviewed |

All four crates are Apache-2.0, Logical Works Incorporated. Versions move
independently from one repo and one tag; the table lists the current published
versions and the next bump publishes here.

> **Names:** package `lgwks_std` (underscore) == directory `crates/lgwks-std`
> (hyphen) == `use lgwks_std::...`. The one exception is the gate binary
> `lgwks-deps` (hyphen) built from library `lgwks_deps` (underscore):
> `cargo install lgwks_deps` for the CLI, `cargo add lgwks_deps` for the
> storefront library.

## Quickstart

```sh
cargo new demo && cd demo
cargo add lgwks_std --features json,http
cargo add lgwks_bot
cargo add lgwks_deps --no-default-features
```

```rust
use std::io::Write;

// Output goes through an explicit locked handle. The workspace lint table makes
// `print_stdout`/`print_stderr` a hard error, and this is also where a broken
// pipe (`demo | head`) becomes an ordinary `Err` instead of a panic.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339();
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: capability-gated actors (needs grants to build, not just to run).
    // `build` returns a typed error; the caller decides what to do with it.
    let bot = lgwks_bot::Bot::builder("demo").build(&lgwks_bot::GrantSet::empty())?;
    let _ = bot;

    // deps: audit your own tree the way CI does. A non-empty refusal list is a
    // refused build, not a warning.
    let root = std::path::Path::new(".");
    let (_register, refusals) = lgwks_deps::check_dependencies(root)?;
    writeln!(out, "{} refusal(s)", refusals.len())
}
```

## Use cases

The jobs people arrive with, and which crate does each one.

**Automation that runs unattended**

- A bot that watches an endpoint, a file, or a queue and acts when something
  *changes* — not on every timer tick. → `lgwks_bot`
- A webhook- or cron-driven job that retries with backoff and is safe to kill
  mid-flight. → `lgwks_bot` for the loop, `lgwks_std::retry` for the policy
- Long-running background work you can cancel cleanly on shutdown, without
  hunting for a leaked task or a loop that never ends. → `lgwks_bot::rt::supervise`

**Giving an agent real authority, safely**

- An LLM-driven or rule-driven agent that must not be able to do more than you
  granted it. Every verb checks a capability proof before it acts, so "what can
  this agent touch?" is answerable from its grant set rather than from a reading
  of the code. → `lgwks_bot`
  *(You bring the model. This is the execution substrate, not a model client.)*

**HTTP and data without an async runtime**

- Calling an HTTP API from a CLI, a build script, or a synchronous service, where
  pulling in a whole async runtime is the wrong trade. → `lgwks_std::http` is
  blocking, rustls-only, and needs no tokio
- Parsing JSON that may be truncated, streamed, or hostile. → `lgwks_std::json`
  reports the offset it stopped at, so a partial body is a value rather than an
  error you have to guess your way out of
- Config in RON, or a compact binary wire format. → `lgwks_std::ron`, `lgwks_std::wire`

**Tools that read source code**

- Building a linter, a codemod, a repository audit, or an "explain this file"
  tool across several languages. → `lgwks_ast` gives one AST type and one
  diagnostic type across every grammar you enable
- Reporting a parse failure with a real span. → `lgwks_ast` diagnostics carry
  file, range, and severity

**Logging you can query**

- Structured, levelled logs from a library without `println!` and without
  committing your consumers to a subscriber. → `lgwks_std`, feature `trace`
  (default on)

**A dependency graph you can defend**

- Answering "what third-party code is in our build, and who approved it?" with a
  command instead of an archaeology project. → `lgwks_deps`, and `lgwks-deps check`

### What these crates are not

- **Not an agent framework.** `lgwks_bot` runs *your* logic and gates its
  authority; it does not call a model for you.
- **Not an async runtime.** `lgwks_bot::rt` is a curated surface over tokio,
  which owns the engine. Parity and the deliberate divergences are documented in
  [`docs/async-parity.md`](docs/async-parity.md).
- **Not a logging facade.** `lgwks_std::trace` re-exports `tracing`; bring your
  own subscriber.
- **Not a parser generator.** `lgwks_ast` wraps tree-sitter grammars; it does not
  produce them.

### Where these sit next to what you'll find first

Anyone searching these problems lands on a specific set of crates before they
land here. Naming them is more useful than not:

| If you're looking for | You'll find first | How this differs |
|---|---|---|
| An LLM agent framework | **Rig**, ADK-Rust, AutoAgents, Metalcraft, neuron, anda | These own model integration: providers, prompts, tool schemas, orchestration. `lgwks_bot` owns none of that and does not compete with them — it is the layer underneath: what the agent is *allowed* to do, and a loop that stops when told. Use both. |
| Zero-trust agents | Symbiont, ZeroTrustAgent, agentmesh | Same thesis, narrower boundary — see the next section. |
| Background job processing | **fang**, backie, apalis, tokio-cron-scheduler | Those are queue- and database-backed workers. `lgwks_bot` has no queue and no database: it is bounded in-process supervision, for work that belongs to the process rather than to a broker. |
| Multi-language parsing | **tree-sitter-language-pack**, rust-code-analysis, ast-grep | Same grammars. `lgwks_ast` narrows to one `Language` enum, one AST type, and typed diagnostics with spans — selectable by cargo feature, so you compile only the grammars you ship. |
| Blocking HTTP without an async runtime | `ureq`, `reqwest::blocking` | `lgwks_std::http` is that idea with rustls-only TLS and no tokio — plus JSON, retry, and time beside it, so a CLI depends on one crate instead of five. |

### The zero-trust vocabulary, and which boxes this ticks

Agent security guidance has settled on a shared vocabulary — *deny by default*,
*runtime gating*, *tool allowlists*, *task-scoped permissions*. Two of those are
what `lgwks_bot`'s gate actually implements, in those words:

- **Runtime gating** — "the agent runtime blocks any unauthorized tool call
  before the action executes." Every verb calls
  `Auth::check(&self.required_caps())?` *before* it touches anything, so the
  refusal lands before the effect rather than after it.
- **Deny by default** — a proof covering no capability authorizes nothing.
  There is a test for exactly that case: a vacuous proof against `bot.net` is
  denied.

It does **not** implement short-lived credentials, MCP tool routing, human
approval gates, or isolated enclaves. Those are absent, not partially built.

### The in-process gap

Google's own summary of Rust background work draws the line here: use
`tokio::spawn` for "quick, non-durable fire-and-forget tasks that do not survive
server restarts", and a Postgres or Redis queue for anything real.

That leaves out what most bots actually are — in-process, long-lived, and
needing to stop cleanly on shutdown. `lgwks_bot::rt::supervise` is that middle:
bounded concurrency, tracked rather than detached, observable, and cancellable
without waiting on a task that will never notice.

<details>
<summary>How this list was built — and what it changed</summary>

Not invented. On 2026-09-21: 33 candidate phrases through Google's autocomplete,
10 queries through the live results pages, and the AI Overviews and AI Mode
replies that Google generated for the agent, background-work, and zero-trust
queries. Findings that changed the framing above:

- **"rust workflow automation" and "rust task orchestration" return no
  autocomplete suggestions at all.** Those are the phrases we would have reached
  for, and nobody types them. The demand is under *background job processing*,
  *scheduler*, *webhook*, and *ai agent*.
- **"rust automation framework" is a trap.** The results are testing frameworks
  (stainless), RPA and acceptance testing (botwork), and GUI automation
  (uiautomation, RustAutoGUI). Someone typing it is not looking for this.
- **"rust automation" and "rust cron job" are worse** — they resolve mostly to
  the video game *Rust* and to job listings.
- **The zero-trust agent cluster is real and active.** Symbiont ships as "a
  security-first, AI-native agent framework built entirely in Rust"; ZeroTrustAgent
  and agentmesh are live; there is published research on zero-trust identity for
  agentic AI. That is the neighbourhood `lgwks_bot`'s capability gate belongs in,
  and it is what the comparison table above answers.
- **Google's AI Overview supplied the vocabulary for both sections above.** For
  agent security it lists *deny by default*, *runtime gating*, *tool allowlists*,
  and *task-scoped permissions*. `lgwks_bot` implements the first two in those
  terms, so the section uses the industry's words rather than inventing its own —
  a reader arriving from that vocabulary can tell in one line whether this is
  what they want.
- **The in-process gap is Google's observation, not ours.** Its background-work
  summary offers exactly two options: `tokio::spawn` fire-and-forget, or a
  Postgres/Redis queue. That is what made the missing middle legible enough to
  build for.

Across every cluster the recurring modifiers were `example`, `tutorial`,
`benchmark`, `comparison`/`vs`, `lightweight`, and `wasm`. That is why this
README and the crate READMEs lead with a runnable example and a comparison
rather than with an architecture description.

</details>

## Which crate do I want?

| I need... | Reach for | Feature |
|---|---|---|
| Hex, base64, timestamps, UUIDs, hashing, glob, LEB128 | `lgwks_std` core | default, zero deps |
| Retry budgets (attempts, backoff, deadlines) | `lgwks_std::retry` | default, zero deps |
| Regex, JSON, RON, binary wire, HTTP client, reachability | `lgwks_std` | `pattern`, `json`, `ron`, `wire`, `http`, `online` |
| `block_on`, bounded `join_all`, `spawn_blocking` (sync) | `lgwks_std::task` | default, zero deps |
| Capability-gated automation (Observe/Evaluate/Execute/Query) | `lgwks_bot` | default (sync) |
| Async timers, channels, sockets, processes, signals | `lgwks_bot::rt` | `rt`, `time`, `sync`, `net`, `process`, `fs`, `signal` |
| Parse Rust/Python/TS/JS/Go/Java/Swift in a tool | `lgwks_ast` | default (7 grammars) |
| Rust source lint scan (zero-gate detectors) | `lgwks_deps` | `scan` (default for CLI) |
| Raw `tokio` engine without the bot facade | `lgwks_deps` | `tokio*` with `default-features = false` |
| GPU desktop UI (Zed's GPUI) | `lgwks_deps` | `gpui` with `default-features = false` |
| Native terminal UI (AppCUI) | `lgwks_deps` | `appcui` with `default-features = false`, version 0.1.9 or newer |

Surprises, documented once so no one re-discovers them: `online` is
feature-gated (not in `core`); `tempfile` is dev-only (no production tempdir
path); typed errors derive through `lgwks_ast` (`extern crate lgwks_ast as
thiserror`), not `lgwks_std`; `lgwks_bot`'s `join!`/`select!`/`try_join!`
re-exports need the `macros` feature and there is deliberately no `#[tokio::main]`
equivalent — enter through `Runtime::block_on`.

## Dependency policy

Cognitive load is three surfaces plus one grandfathered parser:

- **`lgwks_std`** — the core. Everyday primitives, no async runtime required.
- **`lgwks_bot`** — the runtime, the four verbs, and the capability gate.
- **`lgwks_deps`** — everything third-party: install it and select the
  dependency features you want; capability features are default-off. The
  reviewed `scan` gate-tool feature is the explicit default-on exception.
- **`lgwks_ast`** — standalone, and nothing here depends on it. Take it or
  leave it.

Every new third-party dependency is an optional, feature-gated edge of
`lgwks_deps` — e.g.
`lgwks_deps = { version = "0.1.7", default-features = false, features = ["gpui"] }` —
registered with `owner = "lgwks_deps"`. Do NOT `cargo add tokio` / `serde` /
`regex` / `syn` directly: each maps to a curated path here
(`lgwks_bot::rt`, `lgwks_std::json`, `lgwks_std::pattern`,
`lgwks_deps::scan`), and `lgwks-deps check` refuses the second edge.
`lgwks_std`'s core feature stack and `lgwks_ast`'s parser are grandfathered:
`lgwks_std` cannot route through `lgwks_deps`, because `lgwks_deps` depends on
`lgwks_std` and the reverse would be a cycle.

Downstream derive owners note: `lgwks_std::json` re-exports `serde` derives,
so annotate types with `#[serde(crate = "lgwks_std::json::serde")]` — never add
a direct `serde` edge to get the derive to resolve.

## Consumption contract

1. **Core functions → `std` + `lgwks_std`.** If the capability exists in
   `lgwks_std`, depending on the upstream crate directly is duplication, not
   choice — it becomes an `lgwks_std` module (CONSOLIDATE) or is removed
   (ELIMINATE).
2. **Async, runners, actor roles → `lgwks_bot`.** Capability-gated
   `(condition, action)` chains over the shipped domains; a bot missing a grant
   fails at build time, not at runtime.
3. **Multi-language parsing → `lgwks_ast`.** Language identification, grammar
   selection, and bounded tree traversal for code tools. Consumers do not
   depend on `ast-grep` directly; a language's grammar is a cargo feature, so
   each tool compiles exactly the languages it selects.
4. **Everything else → an `lgwks_deps` storefront feature.** Register the edge
   in `contract/APPROVED.toml` (owner, capability, source, requirement,
   consumers, kinds) and expose it as an optional feature. `lgwks-deps check`
   refuses unregistered edges, drifted requirements or sources, and stale unused
   approvals — that is the duplication protection.

`BotSpec` JSON is validate-only: specs never materialize into bots without an
explicit `Bot::builder` call carrying grants. There is deliberately no `bot!`
proc-macro — the builder chain is the DSL, and codegen would hide the
per-call `Auth::check` auditors read while dragging `syn` into every consumer.

Distributed mesh boundaries (retries execute caller-side via
`lgwks_std::retry`, single-attempt HTTP, no pooling, no mTLS material, no
consensus/WAL/gossip, no tracing facade) are declared in
[`docs/distributed-boundaries.md`](docs/distributed-boundaries.md) — read it
before putting these crates on a mesh data path.

## Checks

```sh
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p lgwks_deps -- check .
./scripts/lgwks-std-package-smoke.sh
```

## Releasing

Versions bump here; the tagged commit publishes to crates.io. Process:
`CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`.
