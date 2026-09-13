# Changelog — logicalworks-crates

All notable changes to the four estate crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
`0.x`, so any minor may carry breaking changes, which are then listed
explicitly under that crate.

## [Unreleased]

### Added

- `lgwks_std::retry` (core, zero-dep): `RetryPolicy` value type — attempts,
  exponential backoff with caller-supplied jitter, total deadline.
  Pure data: no I/O, no threads, no clock reads. O(1) saturating `delay`.
- `lgwks_std::http::Options::idempotency_key`: attach an `Idempotency-Key`
  header; the client never invents the key.
- `lgwks_std::ron::FromSliceError::source`: forwards the wrapped UTF-8 / RON
  error instead of dropping the chain.
- Examples that compile in CI: `lgwks_std` quickstart (core-only),
  `lgwks_ast` parse tour, `lgwks_deps` checkout audit.
- `lgwks_ast` feature-matrix acceptance: `--no-default-features` asserts an
  empty grammar table selects nothing (no more vacuous green).
- Repo bar: root `CHANGELOG.md`, `SECURITY.md`, `CONTRIBUTING.md`,
  `docs/distributed-boundaries.md`, per-crate docs.rs `all-features`
  metadata, `lgwks_ast` LICENSE, discoverability metadata for `lgwks_deps`
  / `lgwks_ast`, shared `[workspace.lints]`, crate docs on every `pub mod`
  listing, README quickstarts with current versions.

### Changed

- `lgwks_std::time::ParseError` `Display` now carries the `at` offset on every
  variant (previously dropped on four of six). Messages changed; match on
  variants, not strings.
- `lgwks_bot::error` documents that `String` causes at the domain boundary are
  intentional (typed envelope, escaped payload), narrowing INV-BOT-ERROR-TYPED
  wording without weakening enforcement.
- `lgwks_bot::cap` documents that `Cap::new` accepts any name by convention
  and enforcement is by grant equality; prefer the shipped constants.
- `lgwks_bot` documents both domain paths (`lgwks_bot::net` and
  `lgwks_bot::domain::net`) as stable, and `BotSpec` as validate-only (no
  `from_spec` materializer, no `bot!` macro — by decision, see below).
- `lgwks_std::http` documents the pooling boundary: one attempt per call, no
  connection reuse, retries are caller policy via `retry`.

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
