# lgwks_std

Everyday Rust primitives with no async runtime required: hex, base64,
timestamps, UUIDs, hashing, glob matching, regex, JSON, HTTP, and structured
logging, as single-import calls, each behind one audited dependency stack.

Pick exactly what you need. The default build is `core` plus `trace`; every
other capability is one feature with one vetted stack beneath it.

`trace` is default-on. Structured logging is the replacement for terminal
output in library code, so it has to be reachable without selecting a feature
first. It includes the `tracing` facade and the default subscriber bootstrap
(`LGWKS_LOG`/`RUST_LOG` filtering, compact, pretty and JSON-lines output) and
keeps `tracing-attributes` off, so the `syn` macro stack stays out.

**Want a zero-dependency build?**

```sh
cargo add lgwks_std --no-default-features --features core
```

That is the only configuration in which this crate pulls nothing at all, and it
is verified by command rather than asserted. `cargo tree -p lgwks_std
--no-default-features --features core -e normal` prints one line: `lgwks_std`
itself. The package is `lgwks_std` (underscore); its directory is
`crates/lgwks-std` (hyphen).

## Install

```sh
cargo add lgwks_std                                    # core + trace
cargo add lgwks_std --features json,http               # pick what you need
cargo add lgwks_std --no-default-features --features core   # zero deps
cargo run -p lgwks_std --example quickstart            # 10-line tour (this repo)
```

## Usage

The core modules (`hex`, `time`, `encoding`, `glob`) need no feature: call
`hex::encode`, `time::now_rfc3339`, `encoding::base64::encode` and
`glob::matches`. The default `trace` feature gives `trace::install_default` and
the `info!` and `warn!` level macros. The opt-in features each add one vetted
stack (`json` is serde and serde_json, `ron` is serde and ron, `http` is ureq
and iri-string, `fs-raw` and `process` share rustix): `id::Uuid` (`random`),
`hash::blake3` (`hash`), `pattern::Regex` (`pattern`) and `json::from_str`.

> **A derive on a JSON or RON type needs `#[serde(crate = ...)]`.** Point it at
> `lgwks_std::json::serde` or `lgwks_std::ron::serde`; the RON path needs
> neither `json` nor a direct `serde` dependency. The policy forbids consumers
> from declaring `serde`, so if the compiler reports `cannot find serde in this
> scope`, add the attribute rather than running `cargo add serde`;
> `lgwks-deps check` refuses that edge.

## Feature map

One manifest key, three alternative lines. Use exactly one of them:

```toml
[dependencies]
# Zero external deps: defaults off, core selected. The default build is core
# plus trace, so a bare `lgwks_std = "3"` is NOT zero-dependency.
lgwks_std = { version = "3", default-features = false, features = ["core"] }

# Pick what you need, on top of the default core + trace.
# lgwks_std = { version = "3", features = ["hash", "json"] }

# Everything.
# lgwks_std = { version = "3", features = ["full"] }
```

The feature table, module reference, JSON and RON decoder notes, entropy target
rules, walk threat models, time profile and glob text semantics are in the
[reference](../../docs/guides/lgwks-std/reference.md) and the
[time and glob notes](../../docs/guides/lgwks-std/time-and-glob.md).
Boundaries: [distributed-mesh boundaries](../../docs/distributed-boundaries.md), [bounded background work](../../docs/guides/lgwks-bot/background-work.md), [what a failed tick means](../../docs/guides/lgwks-bot/failures.md).

## Dependency philosophy

Every direct dependency is a vetted leaf or single-purpose stack registered by
semantic capability and owner. `lgwks-deps check` audits authored edges from
Cargo metadata; Cargo.lock preserves the exact transitive provenance.

- **blake3** — 3 zero-dep leaves (arrayvec, cfg-if, constant_time_eq)
- **regex** — 4 BurntSushi-internal crates, zero external deps
- **serde** — derive stack (proc-macro2, quote, syn)
- **serde_json** — 2 leaves beyond serde (itoa, ryu)
- **ron** — 1 leaf beyond serde (bitflags)
- **rkyv** — 5 djkoloski crates, zero external deps
- **getrandom** — zero deps in std-only mode
- **ureq** — blocking HTTP client, rustls-only TLS stack plus small leaves
- **iri-string** — zero-dep URI validation leaf at default features
- **ed25519-dalek** — detached seals for the `sign` feature (alloc-only, no rand_core; key generation reads `lgwks_std::random`)
- **rustix** — safe POSIX syscall surface for the `fs-raw` and `process` primitives; Unix-only, optional
- **tracing** — default-on structured event facade for library diagnostics.
- **tracing-subscriber** — default debugger bootstrap. `install_default("service")`
  installs env-filtered compact output, and `LGWKS_LOG_FORMAT=json` switches the
  same stream to JSON lines. `attributes` stays off, so `#[instrument]` and its
  proc-macro stack do not enter this crate.

`core` alone carries zero external dependencies; the default build is `core` plus `trace`. Every other feature flag is one capability and one stack.

## Minimum supported Rust version

Rust **1.98**, the `rust-version` in the manifest. The MSRV moves forward only
when code or dependency requirements demand it.

## The other crates

`lgwks_bot` and `lgwks_deps` depend on `lgwks_std`; `lgwks_ast` stands alone;
`lgwks_macros` carries the `script!` syntax and is used through `lgwks_bot`.

| Crate | What it gives you |
|---|---|
| [`lgwks_bot`](https://docs.rs/lgwks_bot) | `script!`: orchestration with bounded fan-out, deadlines, retries under a shared budget and tenant-keyed steps |
| [`lgwks_ast`](https://docs.rs/lgwks_ast) | Parse many languages into one AST type, with bounded traversal and typed diagnostics |
| [`lgwks_deps`](https://docs.rs/lgwks_deps) | The audited storefront for third-party stacks, plus `lgwks-deps check` and `lgwks-deps debug` to prove dependency and debugger wiring |

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents. License: Apache-2.0, Copyright 2026 Logical Works
Incorporated.
