# Releasing

This document describes how a version of these crates reaches crates.io, and the
conditions that must hold before an upload.

## 1. Publish order

The crates form a chain, and Cargo refuses to publish a crate whose path
dependency version is not already on the registry:

```
lgwks_std  ──►  lgwks_deps  ──►  lgwks_ast
                     └────────►  lgwks_macros  ──►  lgwks_bot
```

`lgwks_deps` and `lgwks_bot` both depend on `lgwks_std`; `lgwks_ast` depends on
`lgwks_deps` (feature `script`); `lgwks_macros` depends on `lgwks_deps` (feature
`macro`); and `lgwks_bot` depends on `lgwks_deps` and, for its default `script`
feature, on `lgwks_macros`. So:

```sh
cargo publish -p lgwks_std    --locked
cargo publish -p lgwks_deps   --locked   # needs lgwks_std
cargo publish -p lgwks_ast    --locked   # needs lgwks_deps
cargo publish -p lgwks_macros --locked   # needs lgwks_deps
cargo publish -p lgwks_bot    --locked   # last: needs all of the above
```

`cargo publish --workspace --dry-run --locked` packages and verifies the whole
chain in dependency order before any of it is on the registry, which a per-crate
`--dry-run` cannot do for a crate whose dependency's new version is not
uploaded yet. The order is a real constraint: publishing `lgwks_deps` before
`lgwks_std` fails with `candidate versions found which didn't match: <older
versions>` (`required by package lgwks_deps vX.Y.Z`), and `lgwks_ast` 2.0.0
stopped the same way until `lgwks_deps` 4.0.0 was on the registry.

`cargo publish --dry-run` reaches the upload stage without a token and stops
there, so it says nothing about authentication: confirm credentials first
(`ls ~/.cargo/credentials.toml`, or `test -n "$CARGO_REGISTRY_TOKEN"`), or the
train stops mid-chain. A partially published chain is recoverable (finish the
rest once the token is available), but uploaded versions cannot be withdrawn.

## 2. Gates that must pass first

CI runs the following gates, and the commands below are CI's rather than the
workspace ones: `.cargo/config.toml` sets `rustc-workspace-wrapper =
"clippy-driver"`, so every `cargo check` here is a clippy run, but a
workspace-wide clippy does not compile every feature combination. CI's
per-crate `--all-features` matrix reaches feature-gated code that a workspace
build skips. `Language::C` in `lgwks_ast` reached CI that way.

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --no-default-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-std/Cargo.toml  --all-targets --all-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-bot/Cargo.toml  --all-targets --all-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-ast/Cargo.toml  --all-targets --all-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-deps/Cargo.toml --all-targets --features tokio-full -- -D warnings
./scripts/doc-lanes.sh                   # the docs lane, in all four feature lanes
cargo run -p lgwks_deps -- check .       # the two workspace gates
./scripts/lgwks-std-package-smoke.sh
```

The doc gate is one call because *which* intra-doc links break depends on which
features are on; it runs the Docs job's four lanes under `RUSTDOCFLAGS='-D
warnings'`. The **contract-drift** job cross-checks `crates/lgwks-std/Cargo.toml`
against the list under `crates/lgwks-std/README.md`'s `## Dependency
philosophy`; a dependency added without a bullet fails it.

## 3. Cut the release commit

All version bumps land in a single commit: the `[package] version` fields,
their `[workspace.dependencies]` requirements in the root `Cargo.toml`, the
version table in the root `README.md`, and `Cargo.lock` (`cargo update
--workspace`). The nested workspaces under `bench/` and
`crates/lgwks-deps/tests/fixtures/` keep their own `Cargo.lock`, each pinning
the moved crates by path, and CI checks them `--locked`: move each in the same
commit, and prove it with `cargo metadata --locked --offline --manifest-path
<dir>/Cargo.toml`. The `CHANGELOG.md` entry follows, per crate, and records the
behavioural changes a consumer will encounter, not only the signatures.

Version position: every crate is at `1.x` since 2026-10-04, so semantic
versioning applies as written. A removed or renamed public item, or a new
variant on an enum that is not `#[non_exhaustive]`, takes a **major** bump. New
public items, and new variants on `#[non_exhaustive]` enums, take a **minor**
bump. Fixes that change no public item take a **patch**. A refusal made
stricter is a minor bump, not a patch: code that compiled may stop compiling,
and the changelog says so (`lgwks_macros 1.1.0`, #265).

## 4. Tag and release: after the upload, not before

Tags are `lgwks_<crate>-v<version>`, one per crate that moved:

```sh
git tag lgwks_std-v0.6.4
git push origin lgwks_std-v0.6.4
gh release create lgwks_std-v0.6.4 --title "lgwks_std 0.6.4" --notes-file <notes>
```

There is no publish-on-tag workflow: `.github/workflows/` has no `cargo
publish` step, so nothing is uploaded by pushing a tag. A tag or a GitHub
release created before the upload is a claim the registry does not support.

## 5. Prove it from a stranger's side

After the last tag, build every published crate from crates.io at its newest
tag, with the README's own quickstart as the program:

```sh
cargo run -p lgwks_deps --bin published-consumer
```

It prints one receipt line per consumer (eight: `lgwks_std` default, core and
full; `lgwks_ast`; `lgwks_deps` default and `tokio-full`; `lgwks_bot` default
and full) and exits non-zero when one fails or a tag names a version crates.io
does not serve as its newest. The release is done at `published consumers: all
passed`.
