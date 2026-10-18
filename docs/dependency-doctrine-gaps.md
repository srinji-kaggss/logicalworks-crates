# Dependency doctrine: gaps, procedure and enforcement

Part of the [dependency doctrine](dependency-doctrine.md). This page is §6 to §8.

## 6. Where no equivalent exists

These are documented gaps. The policy names them explicitly so that a missing
capability is neither assumed to be present nor added as an unregistered edge:

| Capability | Path |
|---|---|
| `thiserror`, `anyhow` | Implement `std::error::Error` manually; keep typed error enums at library boundaries (the workspace's own crates do this). |
| `log`, `env_logger` | Nothing to add: `lgwks_std::trace` is the logging path, and it is default-on. `print_stdout`/`print_stderr` are `forbid` (§6.1), so there is no print path to route through. |
| `clap`, `argh` | Parse `std::env::args` directly; CLI parsing is not a capability of this workspace. |
| `toml`, `serde_yaml`, `csv` | Not provided. Use `json`/`ron` for data; the gate parses TOML line-wise on purpose to avoid a TOML dependency. |
| `sha2`, `hmac`, `aes-gcm`, `argon2`, `ed25519` | Not provided (BLAKE3 only). Crypto is a BOUNDARY tier: register it with the concrete reason. |
| `rand` distributions | `lgwks_std::random` is OS entropy only; derive the distribution you need or register `rand`. |
| `bytes`, `byteorder` | Use slices and `from_le_bytes`/`to_le_bytes`. |

### 6.1 The print ban is a lint, not a style preference

An earlier revision of this section said machine output stays parseable via
`eprintln!` / `stderr`. **That is superseded and was never enforceable.**
`print_stdout` and `print_stderr` are `forbid` in `[workspace.lints.clippy]`, and
`forbid` cannot be lowered from source, so library code has no print path at
all, and no `#[allow]` can carve one. The lint is stricter than a source-level
convention would be: it grants no exemption for `main.rs`, `src/bin/`,
`examples/`, or `benches/`. It is committed and machine-enforced, and it is the
artifact that governs.

The consequence for the row above is not "where do logs go" but *whether there
is a log at all*. A diagnostic the caller will need belongs in the return type:
carry only facts the operation actually observed, rather than narrating them to
a stream the caller may not be reading. `lgwks_std::ron::to_writer` preserves
the original I/O error and reports no byte count because `write_all` does not
expose how much of a failing write the writer accepted.

A **binary** still has to produce output, and writes to an explicit handle:

```rust
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "check: admitted")?;
    Ok(())
}
```

An explicit `writeln!` to a locked handle is not what `print_stdout` matches, and
it forces the caller to confront the case `println!` hides: a broken pipe. A
program whose consumer went away (`mycmd | head`) must treat
`io::ErrorKind::BrokenPipe` as a clean exit, not a panic.

When a gap forces a new edge, the edge is not the end of the conversation: the
register entry must say which owner carries it and what `std` cannot do. If the
capability is a general primitive, the correct answer is a new `lgwks_std`
module (ELIMINATE), not a consumer-local crate.

## 7. Adding a dependency

The procedure in short form:

```sh
cargo run -p lgwks_deps -- tiers            # find the rung
cargo run -p lgwks_deps -- request foo 1.2 # print a block to fill in
# edit contract/APPROVED.toml, add the Cargo.toml edge, update closure tests
cargo run -p lgwks_deps -- check .          # must print OK
```

An approval with no authored Cargo edge is refused as stale authority
(`UnusedApproval`); an authored edge with no approval is refused as
unregistered (`UnregisteredEdge`). Both directions are enforced.

## 8. Enforcement

- `lgwks-deps check` runs as the first CI job, locally and on the build server.
  The same `check_dependencies` API is available to embedders, so the verdict
  cannot drift between a developer's machine and CI.
- The gate is fail-closed: a missing register, unparseable metadata, or
  unreadable lock is a refusal, never a pass.
- `enforce = false` is an adoption posture, not an off switch. It remains a
  supported posture over a tree the gate admits, and a clean tree still exits 0
  under it. What it cannot do is stand down violations: a register reading
  `enforce = false` while the tree carries dependency-edge violations is itself
  refused as `AdoptionModeRefusals`, naming the count it stood down, and `check`
  exits 2 — the same code it exits under `enforce = true`. The verdict is a
  function of the refusals alone, so the reviewable one-token diff that adoption
  mode depends on cannot change a build's result. (Issue #204.)
- `deps_are_approved_leaves` in `crates/lgwks-deps/src/lib.rs` pins the exact
  allowed dependency names for `lgwks_std` and for the gate itself, so a new
  edge cannot be added by editing a manifest alone.
