# Releasing

This document describes how a version of these crates reaches crates.io, and the
conditions that must hold before an upload.

## 1. Publish order

The crates form a chain, and Cargo refuses to publish a crate whose path
dependency version is not already on the registry:

```
lgwks_std  ──►  lgwks_deps  ──►  lgwks_bot
           └─►  lgwks_ast            (standalone)
```

`lgwks_deps` and `lgwks_bot` both depend on `lgwks_std`; `lgwks_bot` also
depends on `lgwks_deps`. So:

```sh
cargo publish -p lgwks_std  --locked
cargo publish -p lgwks_ast  --locked     # order with deps does not matter
cargo publish -p lgwks_deps --locked
cargo publish -p lgwks_bot  --locked     # last: needs both of the above
```

Evidence that this is a real constraint rather than caution. Publishing
`lgwks_deps` before `lgwks_std` fails with:

```
candidate versions found which didn't match: <older versions>
location searched: crates.io index
required by package `lgwks_deps vX.Y.Z`
```

`cargo publish --dry-run` reaches the upload stage without a token and stops
there, so it proves packaging and verification but says nothing about
authentication. Credentials are confirmed before the sequence starts; otherwise
the train stops mid-chain:

```sh
ls ~/.cargo/credentials.toml          # or: test -n "$CARGO_REGISTRY_TOKEN"
```

A partially published chain is recoverable (finish the remaining crates once
the token is available), but the versions already uploaded cannot be withdrawn.

## 2. Gates that must pass first

CI runs the following gates, and the commands below are CI's rather than the
workspace ones. The distinction matters: `.cargo/config.toml` sets
`rustc-workspace-wrapper = "clippy-driver"`, so every `cargo check` here is a
clippy run, but a workspace-wide clippy does not compile every feature
combination. CI's per-crate `--all-features` matrix reaches feature-gated code
that a workspace build skips. `Language::C` in `lgwks_ast` reached CI that way.

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --no-default-features -- -D warnings

# per-crate, feature-complete
cargo clippy --locked --manifest-path crates/lgwks-std/Cargo.toml  --all-targets --all-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-bot/Cargo.toml  --all-targets --all-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-ast/Cargo.toml  --all-targets --all-features -- -D warnings
cargo clippy --locked --manifest-path crates/lgwks-deps/Cargo.toml --all-targets --features tokio-full -- -D warnings

# docs, with the link denial the docs lane uses, in all four lanes
./scripts/doc-lanes.sh

# the two workspace gates
cargo run -p lgwks_deps -- check .
./scripts/lgwks-std-package-smoke.sh
```

The doc gate is one call rather than four commands because *which* intra-doc
links break depends on which features are on. The script runs the four lanes the
Docs job runs — every feature, no features, the default set, and each
`lgwks_std` feature alone — and a link between two optional modules resolves
under some of them and not others. `RUSTDOCFLAGS='-D warnings'` is what makes a
broken link an error; without it the crate builds green and the break surfaces
after the push.

The **contract-drift** job is easy to miss and easy to trip: it cross-checks
`crates/lgwks-std/Cargo.toml` against the bulleted list under
`crates/lgwks-std/README.md`'s `## Dependency philosophy`. Adding a dependency
to the manifest without adding a bullet fails that job.

## 3. Cut the release commit

All version bumps land in a single commit: the four `[package] version` fields,
their `[workspace.dependencies]` requirements in the root `Cargo.toml`, the
version table in the root `README.md`, and `Cargo.lock` (`cargo update
--workspace`). The `CHANGELOG.md` entry follows, per crate, and records the
behavioural changes a consumer will encounter, not only the signatures.

Version position: this workspace is `0.x`, so **the minor position is the
breaking one**. A removed or renamed public item takes a minor bump; compatible
additions take a patch. `lgwks_bot 0.4.0` and `lgwks_ast 0.2.0` in the 2026-09-20
train were both breaking; the other two took patches.

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
