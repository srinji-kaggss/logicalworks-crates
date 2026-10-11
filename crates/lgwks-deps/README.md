# lgwks_deps

A third-party dependency storefront, with an admission gate that audits what the
storefront is for.

The storefront packages each third-party stack as an optional cargo feature, so
a consumer selects a capability rather than a crate. The gate then checks that
every external dependency authored by a workspace package names its semantic
owner, capability, source, requirement, allowed consumers, and allowed
dependency kinds. Code that needs a library outside `std` and `lgwks_std` does
not pass the gate until a human registers it in `contract/APPROVED.toml`.

## Install

The binary is the gate; the library is the storefront.

```sh
cargo install lgwks_deps    # the `lgwks-deps` binary
cargo add lgwks_deps        # the storefront library
```

Every storefront feature is default-off, so the library brings no third-party
stack until you select one. The Rust source scan moved to `lgwks_ast`, which
already parses Rust (`cargo install lgwks_ast --features tool`, then
`lgwks-ast scan`, #408); `lgwks-deps scan` exits 2 and names that command.

## Storefront

Each feature exposes a reviewed upstream capability through the storefront, so
a consumer never declares it directly. Do not add `tokio`, `gpui`, or Bevy
crates directly; select the storefront feature and import its public path. The
explicit admission gate detects unregistered direct edges when it runs; the
library does not intercept arbitrary Cargo invocations.

| Feature | Re-exports | Notes |
|---|---|---|
| `tokio` | `lgwks_deps::tokio` | The bypass path. Prefer `lgwks_bot::rt` unless you are deliberately working below the bot facade. |
| `tokio-time` | `lgwks_deps::tokio::time` | Enables Tokio timers. |
| `tokio-sync` | `lgwks_deps::tokio::sync` | Enables Tokio synchronization primitives. |
| `tokio-macros` | `lgwks_deps::tokio` | Enables Tokio's macros. Alias `lgwks_deps::tokio` as `tokio` at the consumer root before using `#[tokio::main]` or `#[tokio::test]`. |
| `tokio-io` | `lgwks_deps::tokio::io` | Enables asynchronous reader/writer traits and their extensions. |
| `tokio-net` | `lgwks_deps::tokio::net` | Enables Tokio sockets. |
| `tokio-process` | `lgwks_deps::tokio::process` | Enables Tokio child-process support. |
| `tokio-fs` | `lgwks_deps::tokio::fs` | Enables Tokio filesystem APIs. |
| `tokio-signal` | `lgwks_deps::tokio::signal` | Enables Tokio signal handling. |
| `tokio-full` | `lgwks_deps::tokio` | Combines the listed Tokio capabilities. |
| `bevy-ecs` | `lgwks_deps::bevy_ecs` | ECS substrate; no renderer, window, audio, or multithreaded scheduler. |
| `bevy-app` | `lgwks_deps::bevy_app` | Bevy `App` runtime, layered on `bevy-ecs`. |
| `bevy-time` | `lgwks_deps::bevy_time` | Bevy time resources and plugin, layered on `bevy-ecs`. |
| `bevy-state` | `lgwks_deps::bevy_state` | Bevy state runtime, layered on `bevy-ecs`. For `States` derives, alias `lgwks_deps::bevy_state` as `bevy_state` at the consumer crate root; the upstream derive resolves that manifest crate name. |
| `gpui` | `lgwks_deps::gpui` | Zed's GPU desktop UI framework, retaining its normal platform renderer. macOS builds require a usable Xcode Metal toolchain (`metal` and `metallib`); Linux builds require the platform development stack GPUI selects. External consumer checks run on Linux, macOS, and Windows. |
| `appcui` | `lgwks_deps::appcui` | Native terminal UI. Downstream code uses `use lgwks_deps::appcui; use appcui::prelude::*;` so upstream macros resolve their crate alias without a direct dependency. |
| `ml-candle` | `candle_core`, `candle_nn`, `candle_transformers` | ML inference: tensor compute and transformer models. |
| `ml-candle-metal` | as `ml-candle`, plus Candle's macOS Metal backend | macOS-only. |
| `ml-tokenizers` | `lgwks_deps::tokenizers` | The matching tokenizer stack. |
| `process-group-probe` | `lgwks_deps::process_group::exists` (deprecated) | Forwards to `lgwks_std::process::process_group_exists`, the one process-group backend; kept for 1.x compatibility. |

The ML features are default-off, so the default build compiles none of
Candle's closure. Selecting `ml-candle` pulls `hf-hub` transitively through
`candle-transformers`, which makes the compiled tree network-capable even though
the runtime here loads a local checkpoint and does not call the hub. Authority,
transitive-surface limits, and verification are recorded in
[`docs/candle-admission.md`](../../docs/candle-admission.md).

The `gpui` feature is not compiled by the default build either. Its
authority and verification are recorded in
[`docs/bevy-admission.md`](../../docs/bevy-admission.md); AppCUI's in
[`docs/appcui-admission.md`](../../docs/appcui-admission.md). Native terminal
support does not imply a full editor, backend execution, or measured platform
parity.

## Gate

```sh
lgwks-deps check .              # audit this repository
lgwks-deps tiers                # the admission ladder, lowest rung first
lgwks-deps request <crate> <v>  # print an approval block to fill in
lgwks-deps init [PATH]          # fail-closed starting register
lgwks-deps freshness [PATH]     # resolved versus latest on crates.io
lgwks-deps vendor check [PATH]  # prove the lockfile is covered by the shared vendor tree
lgwks-deps debug [PATH]         # doctor the default lgwks_std debugger surface
```

`check` runs as the first CI lane. The binary diagnoses; it never approves.
Approval is a committed diff with a human's name on it. The register syntax,
the `tier` control and `vendor check` are in
[`docs/guides/lgwks-deps/register.md`](../../docs/guides/lgwks-deps/register.md).

`[policy] enforce = false` is an adoption posture, **not an off switch**. It is
a supported posture over a tree the gate admits: `check` still reports every
refusal it finds, and a tree with no refusals still exits 0. What it no longer
does is make a violating tree pass. A register that reads `enforce = false`
while the tree actually carries dependency-edge violations is itself refused, by
a named `AdoptionModeRefusals` refusal that states how many violations the
posture stood down, and `check` exits 2 — identically to `enforce = true`. The
verdict is therefore a function of the refusals alone; flipping that one token
cannot change a build. See issue #204.

`debug` is the cargo-doctor path for observability: install the default
`lgwks_std::trace` debugger, emit lifecycle events, and refuse if the SDK
debugger no longer rides the default trace surface. `--json` writes the report
to stdout and leaves trace events on stderr.

## The other crates

Five crates ship from this repository and share a release process, not a
dependency graph. The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
lists them and indexes the design documents.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
