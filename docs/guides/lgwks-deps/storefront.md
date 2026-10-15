# lgwks_deps: storefront features

## Storefront features

Each feature exposes its reviewed upstream capability through the storefront, so
you never declare it directly. `cargo add tokio` and `cargo add gpui` are refused
by the gate as second edges. Every feature is off by default.

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

The facade-only consumer matrix compiles GPUI examples on Linux, macOS, and
Windows, Candle Metal only on macOS, and the process-group probe only on Unix.
It does not treat those platform-bound combinations as all-target features.

```toml
[dependencies]
lgwks_deps = { version = "3", default-features = false, features = ["tokio"] }
```

The ML features are off by default, so the default build compiles none of
Candle's closure. Selecting `ml-candle` pulls `hf-hub` transitively through
`candle-transformers`, which makes the compiled tree network-capable even though
this repository's runtime loads a local checkpoint and does not call the hub.
`docs/candle-admission.md` records the authority and the transitive surface.
`gpui` is likewise outside the default build; `docs/bevy-admission.md` and
`docs/appcui-admission.md` cover the other two. Native terminal support does not
imply a full editor, backend execution, or measured platform parity.

If you want the tokio engine behind the bot facade, use `lgwks_bot::rt` rather
than `lgwks_deps::tokio`. The storefront path is for the case where you are
deliberately working below that facade.
