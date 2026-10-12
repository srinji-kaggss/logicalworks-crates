# lgwks_deps: storefront features

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
lgwks_deps = { version = "3", default-features = false, features = ["tokio"] }
```

The ML features are default-off, so the default build compiles none of
Candle's closure. Selecting `ml-candle` pulls `hf-hub` transitively through
`candle-transformers`, which makes the compiled tree network-capable even though
this repository's runtime loads a local checkpoint and does not call the hub.
`docs/candle-admission.md` records the authority and the transitive surface.
`gpui` is likewise outside the default build; `docs/bevy-admission.md` and
`docs/appcui-admission.md` cover the other two.

If you want the tokio engine behind the bot facade, use `lgwks_bot::rt` rather
than `lgwks_deps::tokio`. The storefront path is for the case where you are
deliberately working below that facade.
