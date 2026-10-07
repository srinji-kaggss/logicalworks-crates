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

## Logging

Library code in this workspace does not write to the terminal: `print_stdout` and
`print_stderr` are `forbid` in `[workspace.lints.clippy]` (`Cargo.toml:137` and
`Cargo.toml:138`), and `forbid` cannot be lowered from source by an `#[allow]`.
No file under `crates/*/src` or `crates/*/examples` calls `println!` or
`eprintln!`. `lgwks_std::trace` is the replacement, and `trace` is default-on
precisely so it is reachable without selecting a feature first.

`trace` is a re-export plus a default debugger bootstrap, not a second logging
facade. It exposes the level macros, the span macros, `Level`, `Event`, `Span`,
`Value`, `Instrument`, and `field` from `tracing` itself, so a subscriber you
build against `tracing` works unchanged. It also exposes
`install_default("service-name")`, which installs a process-global subscriber
with `LGWKS_LOG`/`RUST_LOG` filtering, compact output by default, and JSON lines
when `LGWKS_LOG_FORMAT=json`.

```rust
use lgwks_std::trace::{info, install_default, warn};

/// A typed failure. The error travels as a value; the event travels to whatever
/// subscriber the application installed once at the process edge.
#[derive(Debug, PartialEq, Eq)]
pub enum PortError {
    /// The text was not a number at all.
    NotANumber,
    /// The number was out of range for a port.
    OutOfRange(u32),
}

/// Parse a listening port. Emits structured events and returns a typed error.
///
/// `info!` and `warn!` are no-ops until the process installs a subscriber.
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

pub fn main() -> Result<(), lgwks_std::trace::DebugInstallError> {
    install_default("port-service")?;
    let _ = parse_port("8080");
    Ok(())
}
```

Two facts about that feature that the name does not carry:

- **`attributes` is off.** `#[instrument]` is not available from
  `lgwks_std::trace`, because enabling it would pull `tracing-attributes` and
  then `syn`. `crates/lgwks-std/Cargo.toml` records the decision and names the
  alternative: a consumer who wants `#[instrument]` adds `tracing` with
  `attributes` themselves, as their own audited edge.
- **`trace` is a debugging path, not a network exporter.** The default
  subscriber writes local structured output to stderr and keeps stdout clean for
  data. OpenTelemetry is the schema/export boundary; OTLP export belongs in the
  deployment adapter that owns network destinations and credentials.

Returning a typed error and emitting an event are different concerns. A library
returns information; the application installs the debugger once and decides
whether a human, an agent, or an exporter sees it.

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

### Glob matching

`glob::matches` is an anchored text matcher over caller-supplied paths. It is
case-sensitive, does not normalize Unicode, treats leading dots and backslashes
as ordinary characters, and counts Unicode scalar values rather than bytes or
grapheme clusters. `/` is never consumed by `?`, `*`, or a class (even a
negated one); only `**` crosses separators. Ranges sort by Unicode scalar
value. It does not accept native `OsStr`, normalize Windows separators, walk
directories, or implement full POSIX shell expansion.

The compatibility entry point keeps the former permissive dialect: an
unmatched `[` is literal and `**` may appear inside a component, as in
`a**b`. New callers can compile strictly and distinguish malformed patterns
from ordinary no-match results:

Classes use `[abc]`; an initial `!` or `^` negates the class, and an interior
`x-y` denotes an inclusive range ordered by Unicode scalar value. A hyphen at
either edge is literal. An initial `]` is a member when a later `]` closes the
class; without that closer, strict compilation returns an unclosed-class
error and the legacy dialect treats the opening `[` literally. Strict
compilation also rejects descending ranges. Backslash has no escape meaning;
put `*` or `?` in a class to match it literally. In strict syntax, `**` must
occupy a complete slash-delimited component. Legacy syntax keeps embedded
`**` during migration.

```rust
use lgwks_std::glob::{GlobPattern, GlobScratch};

let pattern = GlobPattern::compile("src/**/[a-z]?.rs")?;
let mut scratch = GlobScratch::new();
assert!(pattern.is_match_with("src/a1.rs", &mut scratch));
assert!(pattern.is_match_with("src/sub/a1.rs", &mut scratch));
# Ok::<(), lgwks_std::glob::PatternError>(())
```

`GlobPattern::compile` rejects unclosed classes, descending ranges, and `**`
that does not occupy a complete path component. The recognized component forms
include `**/`, `/**`, and `/**/`. Use
`GlobPattern::compile_with_dialect(pattern, GlobDialect::Legacy)` for a named
migration path. The compiled tokens and class ranges use O(M) storage; reused
scratch holds scalar indexing and two rolling rows in O(N), with no row
allocation per token. `glob` is not a drop-in replacement for the upstream
`glob::Pattern`: its accepted legacy forms differ, and it does not provide
directory traversal.

`crates/lgwks-std/README.md` carries the replacement matrix: which module stands
in for which third-party crate.

## Where to go next

- [lgwks_bot](../lgwks-bot/index.md) for capability-gated automation, which
  depends on this crate with `json` and `http` enabled.
- [lgwks_ast](../lgwks-ast/index.md) for the parser and the diagnostic derive.
- [lgwks_deps](../lgwks-deps/index.md) if you want the admission gate on your own
  workspace.
