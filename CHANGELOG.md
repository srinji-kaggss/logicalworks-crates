# Changelog

All notable changes to the four crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
`0.x`, so any minor may carry breaking changes, which are then listed
explicitly under that crate.

## [lgwks_std 0.6.5 / lgwks_bot 0.4.1 / lgwks_ast 0.2.1 / lgwks_deps 0.1.10] - 2026-09-20

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

## [lgwks_bot 0.4.0 / lgwks_std 0.6.4 / lgwks_deps 0.1.9 / lgwks_ast 0.2.0] - 2026-09-20

Four crates move together. Two are breaking and take the minor position:
`lgwks_bot` (the ECS substrate and a synchronous `tick`) and `lgwks_ast` (a
renamed enum variant). `lgwks_std` and `lgwks_deps` carry compatible additions
and take patches.

**Breaking.** The four verbs now execute as systems on a `bevy_ecs` schedule,
and that substrate is the only path. There is no feature flag that selects it,
because a default-off implementation is a candidate rather than an architecture.

### lgwks_bot Changed

- `Bot::tick` is **synchronous**. The exclusive systems drive the non-`Send`
  verb futures themselves, so there is nothing for a caller to await.
  `bot.tick().await?` becomes `bot.tick()?`, and the bot binding is now `mut`.
- A condition fires on the tick its source value **moves**
  (`Changed<Revision>`), not on every tick it holds. A source that never changes
  fires nothing.
- A tick is **all-or-nothing**: every source is polled before any effect runs,
  so a failing poll fires nothing and returns the first error. The previous
  contract fired the chains declared before the failure.
- A schedule that cannot be ordered deterministically is refused at `build()`
  (`ambiguity_detection: LogLevel::Error`), not misordered at tick.

### lgwks_bot Removed

- `Bot::block_on_tick` — a pure alias for `tick` once `tick` became
  synchronous. Two equivalent entry points for one job is one too many.
- `Bot::chains()` — replaced by `Bot::source_domains()`, which answers what a
  caller actually wanted ("what is this bot watching") without exposing erased
  observer objects.
- The `Chain`, `ChainEntry`, and `ObserveBuilder::entries` types. `ChainEntry`
  was public only because `chains()` returned it.

### lgwks_bot Added

- `Bot::fired`, `Bot::world`, `Bot::revisions`, `Bot::source_domains`: the
  change-detection state a caller needs to reason about a tick.
- `rt::sync::CancellationToken` (+ `DropGuard`). Every background task must
  listen to one, and the type was previously absent, so the requirement named
  something unobtainable. Built on `tokio::sync::watch`, not `tokio-util`: no
  new third-party edge.
- `rt::supervise` — `Supervisor`, `Budget`, `Outcome`, `repeat`. Background work
  that cannot leak and cannot run away. A task cannot leak because nothing is
  detached (no handle to drop), there is no unbounded constructor and no
  internal queue (a bounded permit is acquired *before* the spawn, and
  `try_spawn` refuses rather than grows), every entry point reaps finished tasks
  so the retained set tracks the live bound rather than the lifetime total, and
  `Drop` cancels and aborts. A loop cannot run away because `repeat` cannot be
  written without a `Budget`, and every iteration *races* cancellation instead
  of checking it between iterations, so a cancel interrupts a body that is
  still awaiting rather than waiting for it to finish.
- `rt::task::LocalSet` and `rt::task::spawn_local`. Every verb is deliberately
  non-`Send`, and `spawn` requires `Send`, so the crate's own futures could not
  be spawned at all.
- `domain::net::NetState::BODY_PREVIEW` is now public. Its cap was documented in
  prose beside a public field, which is how a documented limit drifts from the
  real one; the field's doc links the constant instead, so they cannot disagree.
- `rt::io` (feature `io`): `AsyncRead`/`AsyncWrite`/`AsyncBufRead` and their
  extensions, `BufReader`, `BufWriter`, `duplex`, `copy`. The storefront gained
  `tokio-io` for it, because `io-util` was previously reachable only through the
  whole networking stack. `net`, `process`, and `fs` now imply `io`.

### lgwks_std 0.6.4 Added

- `trace` (default-on): structured, levelled logging via `tracing`, with
  `tracing` registered in `contract/APPROVED.toml` under `owner = "lgwks_std"`.
  Library code in this workspace does not print to the terminal and `tracing`
  is the replacement, but no crate could reach it.
- Measured cost: four crates (`tracing`, `tracing-core`, `pin-project-lite`,
  `once_cell`). `attributes` is off, so `#[instrument]` is unavailable and no
  `syn` proc-macro stack enters the foundation; no subscriber is bundled.
  `--no-default-features --features core` is still genuinely zero-dependency.

### lgwks_deps 0.1.9 Added

- `check --json`: one JSON object on stdout, nothing on stderr, exit code
  unchanged. Keys are always `root`, `enforce`, `admitted`, `approvals`,
  `refusals[]`, `error`.
- `tokio-io` storefront feature; `tokio-net` now builds on it.
- Both JSON printers are built through `lgwks_std::json`. `freshness --json` had
  been escaping only the double quote, so any string containing a backslash
  produced invalid JSON.

### lgwks_deps 0.1.9 Fixed

- `check --json` exited **0** for a repository with no register. The gate
  passing without reading its own contract, which the fail-closed rule exists to
  prevent. Reachable only through the new flag. The exit code checks the error
  arm first, and `admitted` is `error.is_none() && refusals.is_empty()` so a
  payload cannot report admission beside a failed read.

### lgwks_ast 0.2.0 Breaking

- `Language::C` is renamed **`Language::CLang`**. The workspace forbids
  `clippy::min_ident_chars`, and `forbid` cannot be lowered from source, so the
  variant name itself had to change. An `#[allow]` there is a hard E0453, not a
  suppression. The lint fires on the macro's input token, so an alias elsewhere
  would not have helped. `Language::name()` still reports `"c"`, which is the
  stable identity findings and callers match on, so only Rust code naming the
  variant is affected. Nothing in the workspace referenced it.

### lgwks_ast Changed

- `Language` and its four lookup tables are now generated from a single row per
  grammar, so a language cannot be added by halves: the variant, its slot in
  `Language::ALL`, and its arm in each table are gated together. A grammar that
  is compiled out has no variant for a table arm to mention, which is what keeps
  the enum exhaustive without a wildcard.
- `CustomLang` gained explicit extension handling (`with_extensions`,
  `of_path`) and traversal metrics that keep `nodes` saturating, `max_depth`
  monotone, and `has_syntax_issues` sticky.
- `examples/parse.rs` extended.

### Shared Changed

- The bot README is compiled: `#[cfg(doctest)] #[doc =
  include_str!("../README.md")]` runs every Rust block in it. Two defects fell
  out on the first run. A `r#"…"#` literal terminated early by a `"#deploys"`
  payload, and a round-trip whose error types were documented as one type when
  `from_json` returns `BotError` and `to_json` returns `json::Error`.
- crates.io metadata and the GitHub repository description and topics no longer
  use internal vocabulary ("estate", "std+", "lane"), which matched no search.

## [lgwks_deps 0.1.8] - 2026-09-17

### lgwks_deps Added

- Default-off `appcui` storefront feature, pinned to 0.5.1, exposing native
  terminal widgets and input-driven drawing through the storefront owner.
- Admission records the AppCUI selection and its use directive; the authorized
  commit is the policy record, not a cryptographic signature.
- Native feature CI and re-export macro doctest. Publication remains separate.

### Shared verification Changed

- Package-smoke disposal uses the operating-system Trash instead of irreversible
  removal; missing Trash tooling fails loudly and retains the owned artifact.

## [lgwks_std 0.6.3] - 2026-09-13

### Added

- `retry` (core, zero-dep): `RetryPolicy` value type: attempts, exponential
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

## [lgwks_bot 0.3.2] - 2026-09-13

### Changed

- `error` documents that `String` causes at the domain boundary are
  intentional (typed envelope, escaped payload), narrowing
  INV-BOT-ERROR-TYPED wording without weakening enforcement.
- `cap` documents that `Cap::new` accepts any name by convention and
  enforcement is by grant equality; prefer the shipped constants.
- Both domain paths (`lgwks_bot::net` and `lgwks_bot::domain::net`) documented
  as stable; `BotSpec` documented as validate-only (no `from_spec`
  materializer, no `bot!` macro, by decision; see below).
- `rt` intra-doc links repaired (absolute `crate::` paths) under the new
  `broken_intra_doc_links` deny lane.

## [lgwks_ast 0.1.3] - 2026-09-13

### Added

- Shipped LICENSE (was declared but missing from the package).
- Parse-tour example (`examples/parse.rs`, `lang-rust`-gated).
- Feature-matrix acceptance: `--no-default-features` asserts an empty grammar
  table selects nothing (no more vacuous green).
- Discoverability metadata (keywords) and docs.rs `all-features` config.

## [lgwks_deps 0.1.7] - 2026-09-13

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

## [0.6.2] `lgwks_std` - 2026-09-13

- Keyed BLAKE3 MAC, HTTP custom headers, process-group kill primitive.
- `serde_json` `Deserializer` re-export for prefix-tolerant parsing.

## [0.3.1] `lgwks_bot` - 2026-09-13

- Storefront-owned async runtime surface (`tokio` edge owned by `lgwks_deps`).

## [0.1.6] `lgwks_deps` - 2026-09-13

- Storefront `tokio` engine features; gate-tool `scan` default.

## [0.1.2] `lgwks_ast` - 2026-09-13

- Typed-error derive moved here from `lgwks_std`; `thiserror` owned by this
  job so the core stack stays lean.
