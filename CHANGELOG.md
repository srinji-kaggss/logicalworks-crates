# Changelog — logicalworks-crates

All notable changes to the four estate crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
`0.x`, so any minor may carry breaking changes, which are then listed
explicitly under that crate.

## [lgwks_deps 0.1.8] — release candidate

### lgwks_deps Added

- Default-off `appcui` storefront feature, pinned to 0.5.1, exposing native
  terminal widgets and input-driven drawing through the estate owner.
- Admission records the Director's AppCUI selection and estate-use directive;
  the authorized commit is the policy record, not a cryptographic signature.
- Native feature CI and re-export macro doctest. Publication remains separate.

### Shared verification Changed

- Package-smoke disposal uses the operating-system Trash instead of irreversible
  removal; missing Trash tooling fails loudly and retains the owned artifact.

## [lgwks_std 0.6.3] — 2026-09-13

### Added

- `retry` (core, zero-dep): `RetryPolicy` value type — attempts, exponential
  backoff with caller-supplied jitter, total deadline. Pure data: no I/O, no
  threads, no clock reads. O(1) saturating `delay`.
- `http::Options::idempotency_key`: attach an `Idempotency-Key` header; the
  client never invents the key.
- `ron::FromSliceError::source`: forwards the wrapped UTF-8 / RON error
  instead of dropping the chain.
- Quickstart example (`examples/quickstart.rs`), compiled and run in CI.

### Changed

- `time::ParseError` `Display` now carries the `at` offset on every variant
  (previously dropped on four of six). Messages changed; match on variants,
  not strings.
- `http` documents the pooling boundary: one attempt per call, no connection
  reuse, retries are caller policy via `retry`.

## [lgwks_bot 0.3.2] — 2026-09-13

### Changed

- `error` documents that `String` causes at the domain boundary are
  intentional (typed envelope, escaped payload), narrowing
  INV-BOT-ERROR-TYPED wording without weakening enforcement.
- `cap` documents that `Cap::new` accepts any name by convention and
  enforcement is by grant equality; prefer the shipped constants.
- Both domain paths (`lgwks_bot::net` and `lgwks_bot::domain::net`) documented
  as stable; `BotSpec` documented as validate-only (no `from_spec`
  materializer, no `bot!` macro — by decision, see below).
- `rt` intra-doc links repaired (absolute `crate::` paths) under the new
  `broken_intra_doc_links` deny lane.

## [lgwks_ast 0.1.3] — 2026-09-13

### Added

- Shipped LICENSE (was declared but missing from the package).
- Parse-tour example (`examples/parse.rs`, `lang-rust`-gated).
- Feature-matrix acceptance: `--no-default-features` asserts an empty grammar
  table selects nothing (no more vacuous green).
- Discoverability metadata (keywords) and docs.rs `all-features` config.

## [lgwks_deps 0.1.7] — 2026-09-13

### Added

- Checkout-audit example (`examples/check.rs`) exercising
  `check_dependencies_against` the way CI does.
- Discoverability metadata (categories, keywords) and docs.rs
  `all-features` config.

### Changed

- README leads with the storefront (was gate-first): install-and-select
  snippets with `default-features = false`, tokio-via-facade guidance,
  CLI-vs-library split.

## Shared repo bar (all four crates, 2026-09-13)

- Root `CHANGELOG.md`, `SECURITY.md`, `CONTRIBUTING.md`,
  `docs/distributed-boundaries.md` (mesh transport/identity/time/
  replication/backpressure/observability/schema boundaries, each with an
  owner).
- Shared `[workspace.lints]` (`missing_docs` deny, `unsafe_code` forbid,
  `broken_intra_doc_links` deny) with per-crate opt-in; new CI lanes: docs
  builds (all-features + no-default), per-crate clippy matrix,
  README-version drift guard.
- Crate docs on every `pub mod` listing; README quickstarts pinned at current
  versions.

### Decisions (no code)

- **Declarative API stands; no proc-macro.** Measured: ~26 small heterogeneous
  verb impls; a macro would save tens of lines while dragging `syn` into every
  consumer (banned outside the `lgwks_deps` gate tool), hiding the audited
  `Auth::check` lines, and repeating the already-rejected `thiserror`-in-bot
  tradeoff. Revisit only past ~30 shipped domains with a machine-checkable
  cross-consistency invariant. The `#[serde(crate = ...)]` attribute stays:
  four one-line attributes beat a derive-wrapper macro plus its docs.

## [0.6.2] `lgwks_std` — 2026-09-13

- Keyed BLAKE3 MAC, HTTP custom headers, process-group kill primitive.
- `serde_json` `Deserializer` re-export for prefix-tolerant parsing.

## [0.3.1] `lgwks_bot` — 2026-09-13

- Storefront-owned async runtime surface (`tokio` edge owned by `lgwks_deps`).

## [0.1.6] `lgwks_deps` — 2026-09-13

- Storefront `tokio` engine features; gate-tool `scan` default.

## [0.1.2] `lgwks_ast` — 2026-09-13

- Typed-error derive moved here from `lgwks_std`; `thiserror` owned by this
  lane so the std+ stack stays lean.
