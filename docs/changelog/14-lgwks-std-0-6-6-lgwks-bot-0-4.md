# lgwks_std 0.6.6 / lgwks_bot 0.4.2 / lgwks_ast 0.2.2 / lgwks_deps 0.1.11 - 2026-09-20

Release: lgwks_std 0.6.6 / lgwks_bot 0.4.2 / lgwks_ast 0.2.2 / lgwks_deps 0.1.11 - 2026-09-20. Page 1 of 1. [Changelog index](../../CHANGELOG.md)

Documentation release. No API change in any crate. This supersedes 0.6.5 / 0.4.1 /
0.2.1 / 0.1.10, whose rustdoc carried internal project vocabulary.

### Shared Changed

- The Rust doc comments in all four crates are rewritten. `docs.rs` renders the
  `lib.rs` and module `//!` documentation as the crate front page, not the
  README, so the previous release published a front page a reader arriving from
  crates.io could not act on. 55 Rust files changed.
- Removed throughout: internal project names and shorthand, references to named
  companion repositories, references to an internal governance file, and
  citations to internal issue numbers that a public reader cannot resolve.
- Em dashes across `crates/**/*.rs`: 316 to 56. The 20 that remain on
  doc-comment lines are the ``- `item` — description`` form in feature and
  module lists, which reads as a definition list rather than as prose
  punctuation. The 36 on code lines are the separator in error-message format
  strings, which are program output rather than prose; they are left unchanged
  so that no caller matching on message text is affected.
- No changed line outside a comment except in `lgwks_deps`, listed below. The
  intra-doc link targets, code fences, and fenced example content are byte
  identical to the previous release, so no documented example changed behaviour.

### lgwks_std Fixed

- The `retry` module documentation linked to `crate::random`, which is behind
  the `random` feature. The link resolves under `--all-features`, so it built on
  `docs.rs`, and it failed under `--no-default-features`. Found while verifying
  this release; predates it. The module is now named in code font instead of
  linked. The `docs` CI job gained a `lgwks_std` no-default-features doc build,
  which is the lane that would have caught it.

### lgwks_deps Changed

- The CLI help and admission-ladder text no longer carry internal vocabulary.
  The `about` line now reads `lgwks-deps: dependency admission for the core
  surface`, and the ladder heading reads `The core admission ladder
  (INV-DEP-EDGE-OWNED)`. This is the only user-visible output change in the
  release; no command, flag, exit code, or register format changed.
- Approval fixtures in the contract tests use `reviewer` rather than an internal
  role name. Test-only data.
