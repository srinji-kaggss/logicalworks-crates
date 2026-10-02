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

`baseline.json` is Cargo's output with one edit: the absolute fixture root in
every path-bearing field (`id`, `src_path`, `manifest_path`, dependency `path`,
`target_directory` and `workspace_root`) is replaced by the fixed token
`__FIXTURE_ROOT__`, so the retained bytes are identical on every checkout
instead of naming the machine that captured them. Regenerate it with:

```sh
cd crates/lgwks-deps/tests/fixtures/cargo-metadata
cargo generate-lockfile          # writes Cargo.lock, committed
cargo metadata --no-deps --format-version 1 --locked \
  | sed "s#$(pwd)#__FIXTURE_ROOT__#g" > baseline.json
```

`tests/metadata_dimensions.rs` substitutes the real fixture directory for the
token when it loads the file, so the decode runs without invoking Cargo, and it
additionally re-runs the command and compares the freshly decoded edges to the
retained ones. The fixture is path-only, so the whole thing resolves, locks and
compiles offline.

`target_scope` is scoped to `cfg(unix)`, so the re-run comparison only asserts it
on Unix hosts; the retained JSON always carries it.
