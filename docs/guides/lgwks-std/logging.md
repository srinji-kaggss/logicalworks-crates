# lgwks_std logging

Part of the [lgwks_std consumer guide](index.md).

## Logging

Library code in this workspace does not write to the terminal: `print_stdout` and
`print_stderr` are `forbid` in `[workspace.lints.clippy]` (`Cargo.toml:155` and
`Cargo.toml:156`), and `forbid` cannot be lowered from source by an `#[allow]`.
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

The shape is a function that parses a listening port. It returns a typed
failure (not a number, or out of range for a port) as a value, and it emits a
`warn!` event with the offending input on each failure and an `info!` event with
the port on success. The events are no-ops until the process installs a
subscriber once at its edge with `install_default("port-service")`; the error
travels to the caller and the events travel to whatever the application chose.

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
