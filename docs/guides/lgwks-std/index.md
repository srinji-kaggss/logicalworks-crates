# lgwks_std consumer guide

Everyday Rust primitives with no async runtime required, each behind one
audited dependency stack. The package is `lgwks_std` (underscore) in the
directory `crates/lgwks-std` (hyphen), used as `use lgwks_std::...`.

Current version: `0.6.6`. MSRV: Rust 1.98.0, edition 2024
(`crates/lgwks-std/Cargo.toml`).

## Install

```sh
cargo add lgwks_std                                     # core + trace
cargo add lgwks_std --features json,http                # pick what you need
cargo add lgwks_std --no-default-features --features core   # zero external deps
```

## Feature selection

The default is `core` plus `trace`. That is two features, and the second one
matters: it is what makes the default build pull four crates rather than zero.

| Feature | Modules | External deps |
|---|---|---|
| `core` (default) | `encoding`, `fs`, `glob`, `hex`, `leb128`, `retry`, `task`, `time` | none |
| `trace` (default) | `trace` | `tracing`, `tracing-core`, `pin-project-lite`, `once_cell` |
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
lgwks_std = { version = "0.6.6", default-features = false, features = ["core"] }
```

`default-features = false` removes `trace`. `features = ["core"]` puts the core
modules back. A bare `lgwks_std = "0.6.6"` is **not** zero-dependency: it is
`core` plus `trace`.

This is checkable rather than assertable. From a clean project:

```sh
cargo tree -e normal | grep lgwks_std
```

Against this repository, `cargo tree -p lgwks_std --no-default-features
--features core -e normal` prints a single line, `lgwks_std v0.6.6`, and nothing
beneath it. The default build prints `tracing`, `tracing-core`,
`pin-project-lite`, and `once_cell` under it.

## Logging

Library code in this workspace does not write to the terminal: `print_stdout` and
`print_stderr` are `forbid` in `[workspace.lints.clippy]` (`Cargo.toml:113` and
`Cargo.toml:114`), and `forbid` cannot be lowered from source by an `#[allow]`.
No file under `crates/*/src` or `crates/*/examples` calls `println!` or
`eprintln!`. `lgwks_std::trace` is the replacement, and `trace` is default-on
precisely so it is reachable without selecting a feature first.

`trace` is a re-export, not a second facade
(`crates/lgwks-std/src/trace.rs:47`). It exposes the level macros, the span
macros, `Level`, `Event`, `Span`, `Value`, `Instrument`, and `field` from
`tracing` itself, so a subscriber you build against `tracing` works unchanged.
The crate chooses no subscriber: bring your own.

```rust
use lgwks_std::trace::{info, warn};

/// A typed failure. The error travels as a value; the event travels to whatever
/// subscriber the *application* installed, which this crate never chooses.
#[derive(Debug, PartialEq, Eq)]
pub enum PortError {
    /// The text was not a number at all.
    NotANumber,
    /// The number was out of range for a port.
    OutOfRange(u32),
}

/// Parse a listening port. Emits structured events and returns a typed error.
///
/// `info!` and `warn!` are no-ops when no subscriber is installed, which is the
/// point: a library emits and does not decide where the output goes.
pub fn parse_port(raw: &str) -> Result<u16, PortError> {
    let parsed: u32 = raw.parse().map_err(|_| {
        warn!(input = raw, "port is not a number");
        PortError::NotANumber
    })?;
    let port = u16::try_from(parsed).map_err(|_| {
        warn!(parsed, "port is out of range");
        PortError::OutOfRange(parsed)
    })?;
    info!(port, "parsed a listening port");
    Ok(port)
}
```

Two facts about that feature that the name does not carry:

- **`attributes` is off.** `#[instrument]` is not available from
  `lgwks_std::trace`, because enabling it would pull `tracing-attributes` and
  then `syn`. `crates/lgwks-std/Cargo.toml` records the decision and names the
  alternative: a consumer who wants `#[instrument]` adds `tracing` with
  `attributes` themselves, as their own audited edge.
- **`trace` is a logging path, not an output path.** `event_enabled!` and the
  macros are no-ops without a subscriber, so a library that returns a typed error
  and emits an event does both, and neither is a print.

Returning a typed error and emitting an event are different concerns. A library
returns information; the application decides whether a human sees it.

## Notes that save a support round

**`online` is not part of `core`.** It is its own feature, with no external
dependency, and it does not come with the default build.

**There is no production temporary-directory path.** `tempfile` is a
`[dev-dependencies]` edge (`crates/lgwks-std/Cargo.toml`), so it is not in your
dependency graph.

**Typed errors derive through `lgwks_ast`, not here.** `lgwks_std` does not own
`thiserror`. A crate in this workspace that wants `#[derive(Error)]` names
`lgwks_ast` as `thiserror`:

```rust
extern crate lgwks_ast as thiserror;

#[derive(thiserror::Error, Debug)]
enum Diagnostic {
    #[error("parse refused: {0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

The derive expands to absolute `::thiserror::__private<N>::...` paths resolved in
the consuming crate, so that `extern crate` line is what makes the expansion
resolve.

**`serde` derives need `#[serde(crate = "lgwks_std::json::serde")]`.** The JSON
facade re-exports serde, and the dependency policy forbids declaring `serde`
directly. The attribute points the derive expansion at the re-export. If you see
`cannot find serde in this scope`, add the attribute rather than running
`cargo add serde`.

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

`crates/lgwks-std/README.md` carries the replacement matrix: which module stands
in for which third-party crate.

## Where to go next

- [lgwks_bot](../lgwks-bot/index.md) for capability-gated automation, which
  depends on this crate with `json` and `http` enabled.
- [lgwks_ast](../lgwks-ast/index.md) for the parser and the diagnostic derive.
- [lgwks_deps](../lgwks-deps/index.md) if you want the admission gate on your own
  workspace.
