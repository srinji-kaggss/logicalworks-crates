# Choosing a crate

**Two qualifications on "zero-dependency core", because at the crate level it is
not true.** Only `--no-default-features --features core` pulls nothing at all;
the *default* feature set includes `trace`, which is `tracing` +
`tracing-subscriber`. `lgwks_std`'s README carries this caveat and the quickstart
below assumes the default set.

**The dependency graph, stated exactly.** `lgwks_std` and `lgwks_ast` depend on
nothing in this workspace. `lgwks_bot` depends on `lgwks_std`, and optionally on
`lgwks_macros` (behind its `script` feature). `lgwks_deps` depends on
`lgwks_std` — twice, under two separate entries, both unconditional (one for
`json`, one for `random`). So it is not true that only `lgwks_bot` depends on
`lgwks_std`, and it is true that nothing depends on `lgwks_ast`.

| I need... | Reach for | Feature |
|---|---|---|
| Hex, base64, timestamps, UUIDs, hashing, glob, LEB128 | `lgwks_std` core | default, zero deps |
| Retry budgets (attempts, backoff, deadlines) | `lgwks_std::retry` | default, zero deps |
| A seeded, replayable stream for deterministic simulations | `lgwks_std::seeded` | default, zero deps |
| Edit-distance / Jaccard / cosine scoring, weighted composition | `lgwks_std::similarity` | default, zero deps |
| Directory walking with depth, order and symlink policy | `lgwks_std::fs` | default, zero deps |
| Regex, JSON, RON, binary wire, HTTP client, reachability | `lgwks_std` | `pattern`, `json`, `ron`, `wire`, `http`, `online` |
| `block_on`, bounded `join_all`, `spawn_blocking` (sync) | `lgwks_std::task` | default, zero deps |
| Capability-gated automation (Observe/Evaluate/Execute/Query) | `lgwks_bot` | default (sync) |
| Async timers, channels, sockets, processes, signals | `lgwks_bot::rt` | `rt`, `time`, `sync`, `net`, `process`, `fs`, `signal` |
| Parse Rust/Python/TS/JS/Go/Java/Swift in a tool | `lgwks_ast` | default (7 grammars) |
| Rust source lint scan (zero-gate detectors) | `lgwks_ast` | `scan`; `tool` for the `lgwks-ast scan` CLI (#408) |
| Raw `tokio` engine without the bot facade | `lgwks_deps` | `tokio*` |
| GPU desktop UI (Zed's GPUI) | `lgwks_deps` | `gpui` |
| Native terminal UI (AppCUI) | `lgwks_deps` | `appcui`, version =0.5.1 per `contract/APPROVED.toml` |
| Detached ed25519 seals that verify offline from the public key | `lgwks_std::sign` | `sign` (ed25519-dalek, alloc-only, no rand_core) |

Three behaviours are worth knowing before you depend on them, and each is
documented where it applies. `online` is feature-gated and is not part of
`core`. `tempfile` is a development-only edge, so there is no production
temporary-directory path. Typed errors derive through `lgwks_ast`
(`extern crate lgwks_ast as thiserror`) rather than `lgwks_std`. The
`join!`/`select!`/`try_join!` re-exports in `lgwks_bot` require the `macros`
feature, and there is deliberately no `#[tokio::main]` equivalent: entry is
through `Runtime::block_on`.
