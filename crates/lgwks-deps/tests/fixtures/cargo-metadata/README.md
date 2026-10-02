# `cargo-metadata` — a real locked workspace, one dimension at a time

Each member package depends on the single local `engine` package and varies
exactly one authored dimension from `baseline`:

| member | dimension varied |
|---|---|
| `baseline` | baseline: no features, defaults on, not optional, no target, no rename |
| `feature` | enables the `extra` feature |
| `default_off` | `default-features = false` |
| `optional` | `optional = true` |
| `target_scope` | declared under `[target.'cfg(unix)'.dependencies]` |
| `renamed` | `package = "engine"` under the local key `alias_engine` |

`baseline.json` is the **raw, unedited** output of:

```sh
cd crates/lgwks-deps/tests/fixtures/cargo-metadata
cargo generate-lockfile          # writes Cargo.lock, committed
cargo metadata --no-deps --format-version 1 --locked > baseline.json
```

`baseline.json` is retained so the decode tests run without invoking Cargo, and
`tests/metadata_dimensions.rs` additionally re-runs the command and compares the
freshly decoded edges to the retained ones. The fixture is path-only, so the
whole thing resolves, locks and compiles offline.

`target_scope` is scoped to `cfg(unix)`, so the re-run comparison only asserts it
on Unix hosts; the retained JSON always carries it.
