# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (6/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 6 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_std Added (continued)

- `similarity::CheckedSimilarity`, the checked scoring seam, and
  `similarity::CheckedEvidence`, the authority-facing composition that carries
  component identity, the refusal, and applicability through to the acceptance
  decision (#160 S2). Any refused component withdraws the whole verdict at
  every threshold including `0.0`; surviving weights are not renormalized, and
  all-zero effective evidence is `EvidenceError::InsufficientEvidence`.
- `similarity::EvidenceError`, the typed refusal shared by every checked
  scorer: `DimensionMismatch`, `ZeroMagnitude`, `NonFinite`, `InputTooLong`,
  `CollectionTooLong`, `InsufficientEvidence`, and `Composition` (#160 S2).
- `similarity::{Evidence, ComponentOutcome, EvidenceVerdict}` (#160 S2).
- `similarity::BoundedJaccard`, a set scorer that charges its element budget
  before dedup and before the quadratic scan, so hostile input is refused
  without paying the work it was trying to cause (#160 S4). `Jaccard` remains
  available and is documented as unbounded.
- `similarity::is_exact_path_match`, the exact path comparison that the lossy
  `PathSimilarity` heuristic is not (#160 S4).
- `similarity::EditDistance::normalized_length`, the unit the scorer's budget
  actually charges: the lower-case-expanded scalar count, which differs from the
  raw count because `İ` expands to two (#160 S4).
- `glob::GlobPattern::token_count` and
  `glob::GlobScratch::{scalar_capacity, row_capacity, row_count,
  storage_bytes}`, so a caller can report pattern storage, scalar indexing, and
  rolling rows separately rather than as one RSS figure (#154).

- `tests/fixtures/wire/consumer_record_v1.hex` is a retained archive that pins
  an application schema and the effective rkyv format (pointer width 32,
  little-endian, `u32` alignment 4). `tests/wire_fixture.rs` reads it on every
  target whose format matches and reproduces it byte for byte, so a format or
  schema drift is caught on every OS rather than inferred from the host.
  `tests/wire_feature_unification.rs` builds a separate consumer crate that
  selects `rkyv/pointer_width_16`, `rkyv/big_endian` and `rkyv/unaligned` and
  proves `wire::format_descriptor()` and the emitted bytes move with the
  unified features, and `tests/sim_wire.rs`
  checks 64 seeded nested values for byte-repeatability, round-trip and refusal
  of truncated or misaligned archives. (#167)
- The HTTP body ceiling is measured, not merely asserted (#163).
  `tests/http_alloc.rs` builds a throwaway consumer crate with a counting global
  allocator and reports separately the retained body bytes, retained header
  bytes, the live heap held while a `Response` is alive, the peak heap during a
  call, and an eager reader's peak. At a 3 003-byte ceiling the retained body is
  324 bytes and the peak is ~267 KB whether the body is 3 KB or 3 MB, while an
  eager reader of the same 3 MB reports a 3 MB peak — so the same metric
  discriminates the mutant the ceiling exists to stop. `tests/sim_http.rs`
  checks 48 seeded scripts per family against a model: ceiling outcomes at
  non-power-of-two sizes, a transport torn before its declared length,
  and two tenants polling one endpoint concurrently without header bleed.
  `http::tests::the_read_window_is_a_fixed_chunk_and_capacity_is_clamped_to_the_ceiling`
  records that the body reader is handed one `READ_CHUNK_BYTES` window and that
  retained capacity clamps to the ceiling. No public behaviour changes; the
  tests pin INV-STD-HTTP-2.
- `online::tests::a_whole_probe_fits_one_wall_clock_budget` observes, from
  outside the injected dial, that one probe's resolved candidates share a single
  wall-clock budget rather than restarting per address. (#163)

### lgwks_std Fixed

- `similarity`: a refused component can no longer become an acceptance. The
  infallible `Weighted::is_accepted` still maps a refusal to `0.0` for source
  compatibility and is documented as lossy; `CheckedEvidence::verdict` is the
  path that retains it (#160 S2).
- `similarity`: the `Similarity` implementation for `EditDistance` no longer
  reports two identical over-limit inputs as `0.0` through the trait's identity
  contract when the checked form refuses them (#160 S1).

### Documentation

- **#155/#170 — documentation claims reconciled to the code at this revision.**
  No public API changed; this entry records sentences that described code the
  tree does not contain, or overstated what it does.
  - `journal/file.rs` and `registry.rs` module docs described pre-repair
    behaviour: a journal with no writer exclusion, and a registry where a
    duplicate identifier silently first-wins. Both now describe what ships
    (`FileJournal::open` takes an exclusive advisory lock before scanning and
    refuses a second opener; `validate` and both `build_*` paths refuse
    duplicate identifiers). The journal doc now states the lock's three real
    limits — advisory, lifetime/host-scoped, filesystem-dependent — so it is
    not read as a distributed lease.
  - `docs/production-readiness.md`: 33 grammars → 28; "13 jobs across three
    operating systems" → 20 job definitions with 14 ubuntu / 2 macos / 1
    windows / 1 `matrix.os` (AppCUI), plus the distinction between a *build*
    receipt and an *executed containment* receipt; the lint-ceiling arithmetic
    (25 + 4 + 50 = 79 over a 75-entry corpus) → 69 clippy + 5 rustc + 1
    rustdoc; "the journal grows without bound" → the real
    `MAX_JOURNAL_BYTES` / `MAX_JOURNAL_EVENTS` refusal and why a hard refusal is
    not a long-running-service availability proof.
  - `docs/async-parity.md`: removed a recommendation (`join_all_bounded` /
    `Supervisor::spawn` for non-`Send` futures) that does not compile — both
    require `Send + 'static` — and added §5a separating tracked lifetime,
    cancellation request, actual termination, queue/byte bounds and retained
    authoritative state.
  - `README.md`, `llms.txt`, all four crate READMEs, `CODEBOOK.md`,
    `AGENTS.md`, `GOVERNANCE.md`, `docs/orchestration-acceptance.spec.md` and
    three stale guide version pins: crate count four → five, the
    zero-dependency claim stated at the level it is true at, the real
    dependency graph, the ban list restated as a ban on *new* edges, and T22
    marked as having candidate tests while remaining unaccepted.
  - Added `docs/std-ast-deps-closure-matrix.md`: all twenty `lgwks_std`
    modules, `lgwks_ast` and `lgwks_deps`, each with a state from a fixed
    vocabulary and the test that exists on the named revision. Recorded as
    `INV-DOC-2`. Four rows are marked `assurance-gap` (constant-time hashing,
    fallible thread admission, entropy failure meaning, feature-isolated
    ergonomics) — unproven, not disproven, and deliberately not green.

### lgwks_bot Added
