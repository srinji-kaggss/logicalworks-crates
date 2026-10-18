# lgwks_std 0.6.5 / lgwks_bot 0.4.1 / lgwks_ast 0.2.1 / lgwks_deps 0.1.10 - 2026-09-20

Release: lgwks_std 0.6.5 / lgwks_bot 0.4.1 / lgwks_ast 0.2.1 / lgwks_deps 0.1.10 - 2026-09-20. Page 1 of 1. [Changelog index](../../CHANGELOG.md)

Documentation release. No API change and no behaviour change in any crate. The
published packages carry a rewritten public surface, which is what a reader
arriving from crates.io or docs.rs reads first.

### Shared Changed

- The root README is restructured as an index: a crate table, a quickstart, the
  use cases, the adjacent-crate comparison, and a documentation index into
  `docs/`. Dependency policy and the consumption contract moved to
  `docs/dependency-doctrine.md`, where the depth belongs.
- Every crate README rewritten for a reader arriving from crates.io. Each opens
  with a statement of what the crate is, then what it does, and the internal
  project vocabulary is gone.
- `docs/` rewritten in the same register. Each admission document states the
  decision, the alternatives measured against it, the authority for it, and the
  verification.
- The four `description` fields rewritten, which is the crates.io listing text.
  `lgwks_std`'s previously used internal shorthand and described the crate as
  async, which its default build is not.
- Manifest and `clippy.toml` comments de-jargoned, because those render on
  docs.rs and in the repository alongside the code.
- The README version-pin CI check now also matches the root README crate table,
  so the table cannot drift from the manifests.
- The root README quickstart is fixed. It did not compile: a tail expression
  returned `io::Error` where `main` promised `Box<dyn Error>`. The `lgwks_deps`
  call it also showed fails in a fresh project, because the gate reads a
  committed register. The snippet now covers `lgwks_std` and `lgwks_bot`, and the
  gate is shown as the CLI sequence it is, including the `cargo generate-lockfile`
  step the gate requires. The snippet is mirrored in
  `crates/lgwks-bot/examples/quickstart.rs`, which `cargo test --all-targets`
  compiles, and a `Docs` CI step fails if the two diverge.
- The root README license pointer no longer names a `LICENSE` file at the
  repository root, which does not exist. Each crate ships its own.
