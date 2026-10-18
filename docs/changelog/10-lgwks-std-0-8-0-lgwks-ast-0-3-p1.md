# lgwks_std 0.8.0 / lgwks_ast 0.3.0 / lgwks_deps 0.2.0 / lgwks_bot 0.6.0 / lgwks_macros 0.1.0 - 2026-09-30 (1/2)

Release: lgwks_std 0.8.0 / lgwks_ast 0.3.0 / lgwks_deps 0.2.0 / lgwks_bot 0.6.0 / lgwks_macros 0.1.0 - 2026-09-30. Page 1 of 2. [Changelog index](../../CHANGELOG.md)

Every crate that moves takes the minor position, because each carries a break
against what crates.io holds. `lgwks_std` changes `http`, `time` and `glob`
signatures. `lgwks_ast` changes the return type of `try_detect_content`.
`lgwks_deps` makes the public fields of its metadata records private.
`lgwks_bot` re-exports `lgwks_std::json` and depends on `lgwks_deps`, so it
moves with both of them. `lgwks_macros` is new and must be uploaded before
`lgwks_bot`, which depends on it.

### lgwks_std Added

- `fs`: `walk_dir_bounded` and `walk_dir_tolerant_bounded` under
  `WalkLimits` (entries, per-directory entries, path bytes, omissions), charged
  before retention; `WalkReport::policy`, `is_complete_within_policy`,
  `budget_exhausted` and `into_parts`; `WalkFailure` keeps the path, stage and
  original I/O error; `OmissionStage` gains `RootResolution` and
  `ResourceBudget`. Walk output paths are absolute under the canonicalized
  root, and a descendant reached through a symlink keeps its logical path.
- `fs::capability::Dir` (feature `fs-raw`): handle-relative access beneath one
  opened directory (`openat`/`statat`/`unlinkat`/`mkdirat`/`readlinkat`), one
  path component per call, no implicit symlink following, owner-only modes
  (0o600 / 0o700) and close-on-exec descriptors. `entry_names` is Linux-only;
  on non-Unix targets every call reports `Unsupported`.
- `hex::decode_into` validates the whole input and the exact destination length
  before writing, so a refusal leaves the destination unchanged.
- `online::probe_resolved` probes caller-resolved addresses under one budget.
  Each candidate is offered an equal share of what remains, so a blackholed
  address no longer starves the next.
- `glob::GlobPattern` (`compile`, `compile_with_dialect`, `is_match`,
  `is_match_with` + reusable `GlobScratch`) with typed `PatternError`s.
- `time::format::unix_parts_lossy` / `from_unix_parts_lossy`, the explicit
  names for the old infallible behaviour.

### lgwks_std Fixed

- `similarity::Geometry::score` accepts `BoundingBox` as documented, not only
  `[f64; 4]`.
- `retry::RetryPolicy` backoff reaches the exact `max_delay` cap for every
  retry index (it overflowed past attempt 31) without work proportional to the
  index; jitter covers the full `Duration` range.
- Percent-decoding errors report original-input byte offsets; UUID and LEB128
  decoding refuse malformed and non-minimal input with offsets.
- Glob matching is O(N) per token over Unicode scalars, not the claimed
  O(M×N) with byte semantics.
- A signal (`EINTR`) during an HTTP exchange is `FailureKind::Interrupted` at
  every stage, never a transport outage and never proof of no effect; an
  `HTTPS://` URL's TLS failures are filed under the TLS stage.

### lgwks_ast Breaking

- `try_detect_content` returns `Result<ContentDetection, ParseError>`
  (`NoMatch`, `Unique(Language)`, `Ambiguous`) instead of
  `Result<Option<Language>, _>`, so two clean candidates are no longer reported
  as no match. Each distinct compiled candidate is parsed once, and a parser or
  budget refusal is an `Err`, never negative evidence.
- `AstMetrics` gains `complete`, `node_limit` and `stop_reason`
  (`InspectionStopReason`), so a partial walk is visible.

### lgwks_ast Added

- `diagnostic`: positioned `Diagnostic`s (`Pos`, `Span`, `Severity`, `render`)
  and `diagnostics` / `recovery_count` over a tree.
- `tree_diagnostics` returns at most `MAX_SYNTAX_DIAGNOSTICS` (32)
  `SyntaxDiagnostic`s with recovery kind and original-source byte spans; a
  truncated report keeps the earliest, in source order.
- `Language::of_shebang` resolves extensionless scripts, reading at most
  `MAX_SHEBANG_BYTES`.

### lgwks_deps Breaking

- The metadata records' public fields are private behind getters:
  `consumer()`, `package()`, `requirement()` and `name()`.

### lgwks_deps Added

- Features `bevy-app`, `bevy-time` and `bevy-state` now export
  `bevy_app`, `bevy_time` and `bevy_state`; before, they were enableable and
  unreachable.
- Cargo metadata collection returns a `CleanupObligation` when it cannot
  confirm the child is gone; `retry_cleanup` retries it, and dropping it makes
  a bounded last attempt to kill and reap the child instead of leaking it.

### lgwks_bot

- Moves with `lgwks_std` 0.8 and `lgwks_deps` 0.2 (it re-exports
  `lgwks_std::json`). The `lgwks_bot` and `lgwks_macros` entries below ship
  in this release.

### lgwks_bot Added
