# lgwks_deps: library and CLI

There are two ways to use this crate, and they have different dependency costs.

```sh
cargo install lgwks_deps    # the `lgwks-deps` binary
cargo add lgwks_deps        # the storefront library
```

The binary is the gate. The library is the storefront. Current version:
`1.0.0`, MSRV Rust 1.98.0, edition 2024. Embedding the gate as a library is
[`embedding.md`](embedding.md).

## The default build carries no optional edge

Every storefront feature is default-off (`default = []`), so `cargo add
lgwks_deps` puts no third-party stack in your graph until you select one.

The Rust source scan is not here. It moved to `lgwks_ast` (#408), which already
parses Rust, so the gate's detectors read through the one parser rather than a
second one (`syn`) in this crate:

```sh
cargo install lgwks_ast --features tool
lgwks-ast scan [--json] [PATH]...
```

`lgwks-deps scan` still answers, so an old habit gets directions rather than an
unknown-verb error: it exits 2 and names the command above.

| What you are doing | Command |
|---|---|
| Running the gate | `cargo install lgwks_deps` |
| Embedding `check_dependencies` | `cargo add lgwks_deps` |
| Choosing a third-party stack | `cargo add lgwks_deps --features tokio` |
| Scanning Rust source | `cargo install lgwks_ast --features tool` |

## Storefront features

Each feature exposes its reviewed upstream capability through the storefront, so
you never declare it directly. `cargo add tokio` and `cargo add gpui` are refused
by the gate as second edges.

| Feature | Re-exports | Default |
|---|---|---|
| `tokio` | `lgwks_deps::tokio` | off |
| `tokio-time` | `lgwks_deps::tokio::time` | off |
| `tokio-sync` | `lgwks_deps::tokio::sync` | off |
| `tokio-macros` | `lgwks_deps::tokio` | off |
| `tokio-io` | `lgwks_deps::tokio::io` | off |
| `tokio-net` | `lgwks_deps::tokio::net` | off |
| `tokio-process` | `lgwks_deps::tokio::process` | off |
| `tokio-fs` | `lgwks_deps::tokio::fs` | off |
| `tokio-signal` | `lgwks_deps::tokio::signal` | off |
| `tokio-full` | `lgwks_deps::tokio` | off |
| `bevy-ecs` | `lgwks_deps::bevy_ecs` | off |
| `bevy-app` | `lgwks_deps::bevy_app` | off |
| `bevy-time` | `lgwks_deps::bevy_time` | off |
| `bevy-state` | `lgwks_deps::bevy_state` | off |
| `gpui` | `lgwks_deps::gpui` | off |
| `appcui` | `lgwks_deps::appcui` | off |
| `ml-candle` | `candle_core`, `candle_nn`, `candle_transformers` | off |
| `ml-candle-metal` | as `ml-candle`, plus Candle's macOS Metal backend | off |
| `ml-tokenizers` | `lgwks_deps::tokenizers` | off |
| `process-group-probe` | `lgwks_deps::process_group::exists`, deprecated: forwards to `lgwks_std::process::process_group_exists` | off |

The facade-only consumer matrix compiles GPUI examples on Linux, macOS, and
Windows, Candle Metal only on macOS, and the process-group probe only on Unix.
It does not treat those platform-bound combinations as all-target features.

For `tokio-macros`, alias `lgwks_deps::tokio` as `tokio` at the consumer crate
root before using Tokio's `#[tokio::main]` or `#[tokio::test]` macro.

```toml
[dependencies]
lgwks_deps = { version = "3", features = ["tokio"] }
```

Selecting `ml-candle` pulls `hf-hub` transitively through
`candle-transformers`, which makes the compiled tree network-capable even though
this repository's runtime loads a local checkpoint and does not call the hub.
`docs/candle-admission.md` records the authority and the transitive surface;
`docs/bevy-admission.md` and `docs/appcui-admission.md` cover the other two.

If you want the tokio engine behind the bot facade, use `lgwks_bot::rt` rather
than `lgwks_deps::tokio`. The storefront path is for the case where you are
deliberately working below that facade.

## The CLI

```sh
lgwks-deps check .              # audit this repository
lgwks-deps tiers                # the admission ladder, lowest rung first
lgwks-deps request <crate> <v>  # print an approval block to fill in
lgwks-deps init [PATH]          # fail-closed starting register
lgwks-deps freshness [PATH]     # resolved versus latest on crates.io
lgwks-deps vendor check [PATH]  # prove the lockfile is covered by the shared vendor tree
lgwks-deps debug [PATH]         # doctor the default lgwks_std debugger surface
```

`check` reads `cargo metadata --no-deps` rather than the transitive lockfile
closure, because only metadata preserves which package authored an edge
(`crates/lgwks-deps/src/lib.rs:15`). It refuses in both directions: an approval
with no authored Cargo edge is stale authority, and an authored edge with no
approval is unregistered.

`vendor check` binds the register to the bytes the offline build resolves, by
hash.

`debug` is the cargo-doctor path for the debugging lifecycle. It installs the
default `lgwks_std::trace` debugger in its own process, emits lifecycle events,
and inspects the repository manifest to prove the default `trace` surface still
includes `tracing-subscriber`. `--json` keeps the report on stdout and the
trace stream on stderr so agents can parse it without scraping terminal text.

The binary diagnoses. It never approves. Approval is a committed diff in
`contract/APPROVED.toml` with a human's name on it.
