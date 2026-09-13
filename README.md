# logicalworks-crates

Source of truth for the estate's shared Rust foundation crates, extracted from
[Braid](https://github.com/srinji-kaggss/Braid) and released from this workspace:

| Crate | Version | Docs | Role |
|---|---|---|---|
| `lgwks_std` | 0.6.3 | [docs.rs](https://docs.rs/lgwks_std) | Core functions: hex, time, id, hash, glob, pattern, json, ron, wire, fs, http, online, process, task, retry, leb128, encoding |
| `lgwks_bot` | 0.3.2 | [docs.rs](https://docs.rs/lgwks_bot) | Async, runner, and actor surface: Observe, Evaluate, Execute, Query verbs plus the curated runtime |
| `lgwks_ast` | 0.1.3 | [docs.rs](https://docs.rs/lgwks_ast) | The estate's code-observability lane: multi-language AST parsing plus the shared typed-diagnostic derive |
| `lgwks_deps` | 0.1.7 | [docs.rs](https://docs.rs/lgwks_deps) | The third-party storefront: opt-in dependency features, admission, audit, freshness (`lgwks-deps check`) |

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
fn main() {
    // std+ core: zero-config primitives, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339();
    println!("now: {now} hex: {}", lgwks_std::hex::encode(b"hi"));

    // bot: capability-gated actors (needs grants to build, not just to run).
    let bot = lgwks_bot::Bot::builder("demo")
        .build(&lgwks_bot::GrantSet::empty())
        .expect("empty bot builds with no grants");
    let _ = bot;

    // deps: audit your own tree the way CI does.
    let root = std::path::Path::new(".");
    match lgwks_deps::check_dependencies(root) {
        Ok((_register, refusals)) => assert!(refusals.is_empty()),
        Err(e) => eprintln!("gate refused: {e}"),
    }
}
```

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

Surprises, documented once so no one re-discovers them: `online` is
feature-gated (not in `core`); `tempfile` is dev-only (no production tempdir
path); typed errors derive through `lgwks_ast` (`extern crate lgwks_ast as
thiserror`), not `lgwks_std`; `lgwks_bot`'s `join!`/`select!`/`try_join!`
re-exports need the `macros` feature and there is deliberately no `#[tokio::main]`
equivalent — enter through `Runtime::block_on`.

## The deps law

Cognitive load is three surfaces plus one grandfathered parser:

- **`lgwks_std`** — the std+ core.
- **`lgwks_bot`** — async, runners, and the actor roles.
- **`lgwks_deps`** — everything else: the **storefront**. Install it and select
  the dependency features you want; capability features are default-off. The
  reviewed `scan` gate-tool feature is the explicit default-on exception.
- **`lgwks_ast`** — standalone. Already adopted; grandfathered, not a lane to
  grow.

Every new third-party dependency is an optional, feature-gated edge of
`lgwks_deps` — e.g.
`lgwks_deps = { version = "0.1.7", default-features = false, features = ["gpui"] }` —
registered with `owner = "lgwks_deps"`. Do NOT `cargo add tokio` / `serde` /
`regex` / `syn` directly: each maps to an estate path
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
