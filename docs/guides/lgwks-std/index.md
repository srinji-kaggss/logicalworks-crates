# lgwks_std consumer guide

Everyday Rust primitives with no async runtime required, each behind one
audited dependency stack. The package is `lgwks_std` (underscore) in the
directory `crates/lgwks-std` (hyphen), used as `use lgwks_std::...`.

Current version: `1.0.0`. MSRV: Rust 1.98.0, edition 2024
(`crates/lgwks-std/Cargo.toml`).

## Install

```sh
cargo add lgwks_std                                     # core + trace
cargo add lgwks_std --features json,http                # pick what you need
cargo add lgwks_std --no-default-features --features core   # zero external deps
```

## Feature selection

The default is `core` plus `trace`. That is two features, and the second one
matters: it is what makes the default build pull the tracing facade and the
subscriber bootstrap rather than zero external crates.

| Feature | Modules | External deps |
|---|---|---|
| `core` (default) | `encoding`, `fs`, `glob`, `hex`, `leb128`, `retry`, `task`, `time` | none |
| `trace` (default) | `trace` | `tracing`, `tracing-subscriber` stack |
| `random` | `random`, `id` | `getrandom` |
| `hash` | `hash` | `blake3` |
| `pattern` | `pattern` | `regex` |
| `json` | `json` | `serde`, `serde_json` |
| `ron` | `ron` | `serde`, `ron` |
| `wire` | `wire` | `rkyv` |
| `http` | `http` | `ureq`, `iri-string` |
| `online` | `online` | none |
| `fs-raw` | `fs` | `rustix` |
| `process` | `process` | `rustix` |
| `full` | all of the above | all of the above |

Features compose. Pick the ones you use and let the rest stay compiled out.

### The zero-dependency configuration

There is exactly one configuration of this crate that pulls nothing at all, and
it needs both halves:

```toml
[dependencies]
lgwks_std = { version = "2", default-features = false, features = ["core"] }
```

`default-features = false` removes `trace`. `features = ["core"]` puts the core
modules back. A bare `lgwks_std = "2"` is **not** zero-dependency: it is
`core` plus `trace`.

This is checkable rather than assertable. From a clean project:

```sh
cargo tree -e normal | grep lgwks_std
```

Against this repository, `cargo tree -p lgwks_std --no-default-features
--features core -e normal` prints a single line, `lgwks_std v0.7.0`, and nothing
beneath it. The default build prints the tracing facade and subscriber stack
under it.

## More pages

- [Logging](logging.md): why library code never prints, and `trace`.
- [Notes and glob matching](details.md): support-round notes and the glob dialects.

## Modules

| Module | What it does |
|---|---|
| `encoding` | base64 and percent-encoding |
| `fs` | recursive directory walking with sandbox enforcement |
| `glob` | shell-style glob matching |
| `hex` | hex encode and decode |
| `leb128` | LEB128 variable-length integers |
| `retry` | retry budgets: attempts, exponential backoff with caller jitter, deadlines |
| `task` | single-threaded executor: `block_on`, concurrent `join_all`, off-thread `spawn_blocking` |
| `time` | RFC 3339 timestamps, calendar math |
| `random`, `id` | OS entropy, UUID v4 |
| `hash` | BLAKE3 |
| `pattern` | compiled regex |
| `json`, `ron`, `wire` | serialization |
| `http` | blocking HTTPS client, rustls-only TLS |
| `online` | TCP reachability probing |

## Where to go next

- [lgwks_bot](../lgwks-bot/index.md) for capability-gated automation, which
  depends on this crate with `json` and `http` enabled.
- [lgwks_ast](../lgwks-ast/index.md) for the parser and the diagnostic derive.
- [lgwks_deps](../lgwks-deps/index.md) if you want the admission gate on your own
  workspace.
