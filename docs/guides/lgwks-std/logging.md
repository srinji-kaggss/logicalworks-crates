# lgwks_std logging

Part of the [lgwks_std consumer guide](index.md).

## Logging

Library code in this workspace does not write to the terminal: `print_stdout` and
`print_stderr` are `forbid` in `[workspace.lints.clippy]` (`Cargo.toml:146` and
`Cargo.toml:147`), and `forbid` cannot be lowered from source by an `#[allow]`.
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
