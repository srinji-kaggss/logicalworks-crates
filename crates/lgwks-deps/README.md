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
cargo install lgwks_deps                        # the `lgwks-deps` binary
cargo add lgwks_deps --no-default-features      # the storefront library
```

`default-features = false` matters for library consumers. The default build
carries the `scan` gate-tool feature, which pulls `syn` and `proc-macro2` for
the CLI's source detectors. Disabling defaults keeps a parser out of a runtime
dependency graph.

## Storefront

Each feature re-exports the upstream crate, so a consumer never declares it
directly. Do not run `cargo add tokio` or `cargo add gpui`; `lgwks-deps check`
refuses the second edge.

| Feature | Re-exports | Notes |
|---|---|---|
| `tokio` | `lgwks_deps::tokio` | The bypass path. Prefer `lgwks_bot::rt` unless you are deliberately working below the bot facade. |
| `gpui` | `lgwks_deps::gpui` | Zed's GPU desktop UI framework, retaining its normal platform renderer. macOS builds require a usable Xcode Metal toolchain (`metal` and `metallib`); Linux builds require the platform development stack GPUI selects. |
| `appcui` | `lgwks_deps::appcui` | Native terminal UI. Downstream code uses `use lgwks_deps::appcui; use appcui::prelude::*;` so upstream macros resolve their crate alias without a direct dependency. |
| `ml-candle` | `candle_core`, `candle_nn`, `candle_transformers` | ML inference: tensor compute and transformer models. |
| `ml-candle-metal` | as `ml-candle`, plus Candle's macOS Metal backend | |
| `ml-tokenizers` | `lgwks_deps::tokenizers` | The matching tokenizer stack. |
| `scan` (default) | — | The gate's Rust source detectors. The one reviewed default-on feature. |

The ML features are default-off, so the default `scan` build compiles none of
Candle's closure. Selecting `ml-candle` pulls `hf-hub` transitively through
`candle-transformers`, which makes the compiled tree network-capable even though
the runtime here loads a local checkpoint and does not call the hub. Authority,
transitive-surface limits, and verification are recorded in
[`docs/candle-admission.md`](../../docs/candle-admission.md).

The `gpui` feature is not compiled by the default `scan` build either. Its
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
lgwks-deps scan [PATH]...       # source detectors, one verdict binary
```

`check` runs as the first CI lane. The binary diagnoses; it never approves.
Approval is a committed diff with a human's name on it, and the register format
is documented in `contract/APPROVED.toml`.

`vendor check` is the physical counterpart of the register. The register states
which edges are owned, the tree in `vendor/` states which bytes the offline
build resolves, and the subcommand binds the two by hash. The tree sits at the
workspace root so every repository here resolves one physical copy. It is
outside all package directories, so `cargo package` never ships it.

An approval with no authored Cargo edge is refused as stale authority. An
authored edge with no approval is refused as unregistered. Both directions are
enforced.

## The other crates

Four crates ship from this repository. They share a release process, not a
dependency graph: `lgwks_bot` and `lgwks_deps` depend on `lgwks_std`, and
`lgwks_ast` stands alone.

| Crate | What it gives you |
|---|---|
| [`lgwks_std`](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: codecs, a blocking HTTP client, retry, structured logging, time, hashing, ids |
| [`lgwks_bot`](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs, capability-gated authority, change-triggered execution, supervised background work. Its async engine is one of the stacks this storefront ships |
| [`lgwks_ast`](https://docs.rs/lgwks_ast) | Parse many languages into one AST type, with bounded traversal and typed diagnostics |

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
