# logicalworks-crates — invariants

Read before the first edit. Each entry: rule · why · enforced by. Add an entry in
the same PR as any Director correction or incident fix. Long-form: `AGENTS.md`,
`docs/dependency-doctrine.md`.

## Surfaces and dependencies

- **INV-DEP-1** Three surfaces (`lgwks_std`, `lgwks_bot`, `lgwks_deps`) plus the
  finished, standalone `lgwks_ast`. Never grow `lgwks_ast`; never add a new owner or
  top-level crate for third-party code. · enforced by: `lgwks-deps check .`
- **INV-DEP-2** Every authored external edge is an optional, default-off feature of
  `lgwks_deps` registered in `contract/APPROVED.toml` with owner, capability, source,
  requirement, consumers and kinds (INV-DEP-EDGE-OWNED). Exception: the `scan`
  gate-tool feature is default-on. · enforced by: `lgwks-deps check .`
- **INV-DEP-3** Never depend directly on tokio, futures, async-trait, pollster, syn,
  proc-macro2, regex, uuid, chrono, walkdir, glob, base64, hex, percent-encoding,
  serde_json, ureq, reqwest or ast-grep-*; use the workspace path. One `tokio` edge,
  owned by `lgwks_deps`. · enforced by: `lgwks-deps check .`
- **INV-DEP-4** `lgwks_std` never routes through `lgwks_deps` (cycle). · enforced by: cargo
- **INV-DEP-5** Never widen consumers or edit a pinned version to clear a gate
  refusal. · enforced by: review
- **INV-DEP-6** `lgwks_std::random` is feature-gated; code built in the feature
  matrix must not assume it. Test identities use nanos + `AtomicU64`. · enforced by:
  feature-matrix CI
- **INV-DEP-7** A path dependency is internal only when its resolved manifest
  directory is a workspace member's; sharing a member's name is not
  membership, and an unlocatable path fails closed to external. · why: #143
  R13 · enforced by: `lgwks_deps::metadata::tests`, `lgwks-deps check .`
- **INV-DEP-8** Cargo metadata collection has an elapsed deadline and a
  per-stream retained-byte cap sampled every poll quantum; this is not a hard
  temporary-disk quota, and a descendant may retain an inherited capture
  descriptor after the direct child exits. Termination and capture-removal OS
  errors remain visible, and any incomplete collection is a refusal, never an
  empty or partial graph. · why: #143 R14, #159 M1-M3 · enforced by:
  `lgwks_deps::metadata::tests`
- **INV-DEP-9** Each selected Bevy runtime feature (`bevy-app`, `bevy-time`,
  `bevy-state`) exposes its promised public import path; a deselected path is
  absent from the facade. · why: #169 · enforced by:
  `crates/lgwks-deps/tests/storefront_consumers.rs`
- **INV-DEP-10** Every declared workspace member resolves to exactly one package
  record and one unique manifest directory before dependency classification;
  duplicate or missing identity records and member/path name mismatches are
  schema refusals, never filtered into an empty or misattributed graph. · why:
  #158 A3 · enforced by: `lgwks_deps::metadata::tests`
- **INV-DEP-11** Dependency and invariant registers share one fail-closed TOML
  subset: repeated entry keys, unsupported string syntax, invalid decoded
  vocabularies, and impossible dates are refused with source identity and
  position; valid unique fields have order-independent meaning, and parsed
  approvals cannot be mutated outside the crate. · why: #157 ·
  enforced by: `contract::tests::invariant_register_uses_the_shared_duplicate_key_refusal`
## lgwks_bot — durable execution

Each of these was a shipped defect. Treat the list as the spec.

- **INV-BOT-1** Journal before acknowledge: a live settlement is journaled before it is
  acknowledged. · why: cef8059b (#112)
- **INV-BOT-2** A settlement binds to the generation it names; a transition binds to
  the payload it was opened under; a dispatch digest binds to the admitted input, not
  a revision. · why: ac2fbc11 (F03), d5da9cbb (F04), 3534e9e4 (#114)
- **INV-BOT-3** An abandoned entry and a recovered unknown are barriers, never
  skipped past. · why: fe1a7fde (F02), 98872468 (#113)
- **INV-BOT-4** A known effect outcome is kept even when its record fails to land;
  outcome receipts must be durable and bound. · why: bb3e152e (#111), 65236222, 9eafabed
- **INV-BOT-5** Observation fingerprints commit only together with their values;
  detached fingerprints are retired. · why: 78fa72d8 (#110), bb3b8802
- **INV-BOT-6** Stale effect controllers are fenced (generation/lease fencing).
  · why: d0610245 (#137)
- **INV-BOT-7** A journal read failure is an error, never `NoSuchWork`. Errors are
  not absence. · why: 7f3dd387 (#139)
- **INV-BOT-8** A state watch that returns to a previously landed value fires again
  (edge, not level, identity). · why: 3291773c (#138)
- **INV-BOT-9** Process ownership covers descendants; cleanup reaps the tree. A
  process spec deadline counts from enable time. · why: bd9a372b (#117), 2f88bad5 (#132)
- **INV-BOT-10** External handoffs are admitted on the real dispatch path, not a side
  path; locator ladder eligibility is enforced. · why: fff9d335 (#115), da0a3471 (#116)
- **INV-BOT-11** Builders that can be dropped unused are `#[must_use]` and state their
  authority. · why: bb2ee99f (#94)
- **INV-BOT-12** A supervised Unix process-group signal is sent only while its
  unreaped leader pins the group id; the supervisor holds the native-task
  permit through the bounded termination attempt and never signals that numeric
  id after reaping. · why: #143 R09/R10 identity-reuse and ownerless-cleanup
  findings · enforced by: `tests/process_ownership.rs`, `tests/rt_process.rs`,
  and `rt::supervise::tests`
- **INV-BOT-13** Cleanup that remains pending after its process task ends transfers
  its group identity and admission permit to the supervisor's bounded cleanup
  owner; present/error observations retain both, and only observed absence may
  release capacity and emit an attributed terminal receipt. · why: #143 R10
  ownerless-cleanup finding · enforced by: `rt::supervise::tests` and
  `tests/rt_process.rs`
- **INV-BOT-14** Shipped journals refuse appends and opens beyond their explicit
  event/byte ceilings without deleting or partially replaying committed or
  unresolved evidence. · why: #143 R06 · enforced by:
  `journal::tests::memory_journal_refuses_history_beyond_its_declared_limit`,
  `file::tests::scanning_refuses_a_complete_event_beyond_the_limit`,
  `file::tests::batch_admission_refuses_history_over_the_event_limit_without_writing`,
  and `file::tests::open_refuses_an_over_limit_file_without_truncating_it`

## lgwks_std

- **INV-STD-SIM-1** The documented `Geometry::score` accepts both `[f64; 4]`
  and `BoundingBox`. · why: #160 S3 · enforced by:
  `tests/similarity_public_api.rs`
- **INV-STD-RETRY** `RetryPolicy` remains a pure, allocation-free policy:
  `delay(0)` is the backoff after the first failure, exponential scaling
  reaches the exact `max_delay` cap for every retry index without work
  proportional to that index, and jitter applies the documented inclusive
  modulo formula across the complete `Duration` range. The one-word entropy
  mapping is modulo-biased and does not promise uniformity. Zero attempts have
  an effective floor of one at construction and use, while deadline equality
  refuses an attempt, including the initial one. · why: #164 · enforced by:
  `lgwks_std::retry::tests`
- **INV-HEX-1** `hex::decode_into` requires exact destination length and validates
  the entire input before writing, so every refusal leaves the destination
  unchanged. · enforced by: `hex::tests::decode_into_validates_exact_length_and_preserves_output_on_failure`
- **INV-ENCODING-1** Percent escape errors report original-input byte offsets;
  UTF-8 errors name offsets in decoded bytes and never present them as source
  coordinates. · enforced by: `encoding::tests::percent_refuses_a_non_hex_escape`
  and `encoding::tests::percent_refuses_escapes_that_decode_to_invalid_utf8`
- **INV-ID-1** UUID v4 masks apply to generated IDs only; parsing and raw-byte
  construction preserve arbitrary UUID values, and malformed hex reports both
  group start and invalid character offsets. · enforced by: `id::tests`
- **INV-LEB128-1** Integer decoders accept only minimal encodings and return the
  consumed prefix length; trailing input remains with the caller. · enforced by:
  `leb128::tests::distinguishes_prefix_trailing_bytes_from_nonminimal_and_truncated_input`
- **INV-TIME-1** RFC 3339 parsing validates offset component bounds and refuses
  leap-second labels the `SystemTime` profile cannot preserve; checked Unix
  conversion and canonical formatting report range failures instead of
  manufacturing the epoch or extended-year RFC text. Civil-to-day conversion
  narrows only after the complete mathematical count is computed. · why: #153
  T1–T5 · enforced by: `tests/sim_time_profile.rs`
  (`invalid_numeric_offsets_name_the_field_and_byte`,
  `leap_second_labels_are_explicitly_unsupported`,
  `checked_unix_conversion_never_substitutes_epoch`,
  `rfc_year_boundaries_refuse_extended_output_without_clamping`,
  `calendar_roundtrips_endpoints_neighbors_and_overflow_transition`) and
  `time::parse::tests::conversion_seam_preserves_platform_range_failure`
- **INV-STD-HTTP-1** HTTP failures retain a machine-readable class and observed
  stage; a body or EOF-probe timeout is never EOF, preview completion, or proof
  of no effect. Response header bytes and multiplicity survive, while the
  String view is explicitly lossy and map-ordered. Redirects are explicit and
  bounded to ten hops; target provenance redacts userinfo, query and fragment.
  A redirect hop to another origin (scheme, host, effective port) carries none
  of the caller's headers; a same-origin hop carries them all but
  `Authorization`, `Cookie` and `Proxy-Authorization`; a 307/308 of a POST is
  refused rather than replaying the body. Idempotency keys
  remain singular and receiver-defined. `EINTR` is its own failure class at
  every stage and is never presented as proof of no effect. · why: #163
  N1/N2/N3, #190 review (ureq forwarded custom headers cross-origin) ·
  enforced by:
  `http::tests::a_cross_origin_redirect_carries_no_caller_header`,
  `http::tests::a_same_origin_redirect_keeps_ordinary_headers_but_not_credentials`,
  `http::tests::a_method_keeping_redirect_of_a_post_is_refused`,
  `http::tests::an_interruption_is_classified_the_same_way_at_every_stage`,
  `http::tests::body_timeout_preserves_stage_and_class`,
  `http::tests::eof_probe_timeout_preserves_stage_and_class`,
  `http::tests::legal_header_bytes_and_repeated_values_are_preserved`, and
  `http::tests::redirect_loop_refuses_at_the_configured_limit`
- **INV-STD-ONLINE-1** Resolved reachability candidates share one monotonic
  connection budget and at most 64 addresses are attempted; each is offered an
  equal share of what remains, so a blackholed candidate cannot starve the
  ones after it. Synchronous
  `ToSocketAddrs` work is outside that budget and is not promised preemptible;
  the literal `is_online` endpoints share the same budget. The Boolean result
  remains a TCP heuristic, not application health. · why: #163 N4 · enforced
  by: `online::tests::address_candidates_share_one_remaining_budget`,
  `online::tests::a_blackholed_candidate_does_not_starve_the_next` and
  `online::tests::resolver_delay_is_outside_the_connection_budget`
- **INV-GLOB-1** Glob matching is anchored and operates on Unicode scalar
  values without normalization: `?` and classes consume one scalar, `/` is
  excluded from `?`, `*`, and all classes (including negated classes), `*`
  stays within a segment, and `**` may cross separators. Matching is
  case-sensitive; leading dots are ordinary; backslash is literal; `**` in a
  component and unmatched `[` are accepted only by the named legacy dialect.
  Strict compilation reports malformed classes, descending ranges, and
  component-invalid `**` as typed errors. Compilation is O(M); each token
  transition is O(N) over the finite Unicode scalar alphabet; reusable
  scratch retains one scalar index and two rolling rows in O(N), with no row
  allocation per token. · enforced by: `glob::tests` work-growth, scratch
  capacity, Unicode, strict-error and exact double-star cases, plus
  `tests/glob_public.rs`
- **INV-CODEC-1** JSON and RON text/slice decoders preserve input borrowing
  where their decoders support it; escaped text that needs allocation is not
  reported as borrowed. RON writer failures distinguish serialization from I/O
  and preserve the underlying cause without claiming unobserved byte progress.
  · why: #162 · enforced by:
  `json::tests::unescaped_string_fields_borrow_from_text_and_slice`,
  `json::tests::escaped_string_cannot_be_returned_as_a_borrowed_str`,
  `ron::tests::unescaped_string_fields_borrow_from_text_and_slice`,
  `ron::tests::writer_preserves_serialization_and_io_failures`, and
  `tests/serde_facade_consumers.rs`
- **INV-WIRE-1** `lgwks_std::wire` is a feature-unified rkyv archive facade, not a
  canonical semantic encoding or versioned envelope. The effective byte order,
  alignment, and archived pointer width are observable via
  `wire::format_descriptor`; callers bind those properties and their own schema
  version before persisting or exchanging bytes. Structural validation does not
  establish application validity or schema identity. · why: #167 · enforced by:
  `tests/wire_consumer.rs`
- **INV-PATTERN-SAFE** A single regex search costs worst-case `O(m * n)`, but
  complete greedy match, split, and replacement iteration may cost `O(m * n^2)`;
  iterator laziness does not promise prefix-only search work. Checked patterns
  enforce source-pattern, compiled-size, nesting, input-byte, and
  replacement-output-byte ceilings before avoidable allocation, preserve
  engine match semantics, and never return a partial replacement. Pattern text
  is escaped in `Debug` and `Display`.
  · why: issue #168 · enforced by:
  `pattern::tests::configured_limits_refuse_input_and_amplified_output`,
  `pattern::tests::configured_pattern_compile_size_and_nesting_limits_are_enforced`,
  `pattern::tests::greedy_adversary_and_literal_control_keep_exact_match_workloads`,
  `pattern::tests::bounded_replacement_expands_exactly_like_the_engine`, and
  `tests/pattern_external.rs`
- **INV-FS-2** A successful strict directory walk has no known omissions; a
  tolerant walk returns each known omission alongside its entries, and an
  unresolved root is always refused. Path-based identity rechecks are
  best-effort only and do not promise race-safe containment against hostile
  concurrent replacement. · why: #143 R15/R16 · enforced by:
  `lgwks_std::fs::tests`
- **INV-FS-5** Walk output uses absolute paths rooted at the canonicalized
  input root; descendants reached through symlinks retain the logical alias,
  while canonical targets identify visits. Reports expose the applied depth
  and symlink policy, and completeness means complete within that policy.
  Bounded walks charge entries and path bytes before retention and mark any
  budget-limited prefix incomplete. Strict failures preserve path, stage and
  the original I/O source. `available_space` is an advisory snapshot, never a
  reservation or write guarantee. · why: #166 · enforced by:
  `lgwks_std::fs::tests` and the public API doctest

- **INV-FS-3** Handle-relative access resolves every name below one admitted
  directory from that directory's descriptor, so a name replaced mid-walk
  cannot redirect the walk off the tree it was admitted to. Every such call
  takes exactly one path component: `.`, `..`, empty and anything containing
  `/` are refused before the syscall, because those are the spellings that move
  out of the admitted directory. A symlink is never followed implicitly;
  following is a stated [`SymlinkPolicy`], and a final link is removable
  without touching its target. · why: #174 R15/R16 · enforced by:
  `lgwks_std::fs::capability::tests::an_opened_subdir_survives_its_path_being_repointed`,
  `lgwks_std::fs::capability::tests::dotdot_cannot_walk_out_of_the_admitted_directory`,
  `lgwks_std::fs::capability::tests::open_entry_does_not_follow_a_symlink_by_default`,
  `lgwks_std::fs::capability::tests::remove_file_unlinks_a_symlink_not_its_target`

- **INV-FS-4** A capability that creates names grants the owner and nobody
  else: a created file is mode 0o600 and a created directory 0o700 before the
  umask, because a umask can only clear bits and a mode naming group or world
  stays granted wherever the umask leaves it. A descriptor returned to a caller
  is close-on-exec, including one duplicated by `try_clone`, so a directory
  capability does not survive an `exec` into a child spawned mid-walk. A
  listing is rewound before it is read, so it is a property of the directory
  rather than of whatever that descriptor did earlier, and it omits `.` and
  `..`. An open request that the kernel answers inconsistently across platforms
  is refused before the syscall: `truncate` without write access, and neither
  read nor write, and `create` combined with following a final symlink. · why:
  #185 adversarial review · enforced by:
  `lgwks_std::fs::capability::tests::a_created_file_is_not_writable_by_group_or_world`,
  `lgwks_std::fs::capability::tests::a_created_directory_is_not_accessible_by_group_or_world`,
  `lgwks_std::fs::capability::tests::a_cloned_dir_is_closed_across_exec`,
  `lgwks_std::fs::capability::tests::listing_a_dir_that_already_created_a_child_is_not_empty`,
  `lgwks_std::fs::capability::tests::an_empty_directory_lists_nothing`,
  `lgwks_std::fs::capability::tests::a_readless_write_request_is_refused_rather_than_truncating`,
  `lgwks_std::fs::capability::tests::create_refuses_to_follow_a_final_symlink`

  Known limit, stated rather than left to be discovered:
  `fs::capability::Dir::entry_names` is Linux-only. `getdents64` is the only
  syscall that lists a descriptor, the BSDs expose no equivalent, and calling
  `readdir(3)` would need `unsafe` under `unsafe_code = forbid`. It reports
  `Unsupported` elsewhere. Every other `Dir` operation is `*at(2)` and portable.

## lgwks_ast

- **INV-AST-1** Checked AST inspection charges nodes before descending and
  retains pending traversal state proportional to active depth, not sibling
  fan-out; the byte and node ceilings remain separate from parser allocation.
  · why: #143 R17 · enforced by: `lgwks_ast::tests::a_small_node_budget_does_not_retain_a_wide_sibling_frontier`
- **INV-AST-2** Content detection parses each distinct compiled candidate once;
  only invalid syntax is negative evidence, while parser or budget refusal
  leaves detection incomplete. Bounded AST metrics identify partial walks, and
  checked syntax diagnostics preserve recovery kind and original-source byte
  spans under a fixed ceiling that keeps the earliest in source order. The
  refusal and the rendered report walk the same node set, root included.
  · why: #165 A1–A4 · enforced by:
  `lgwks_ast::tests::duplicate_and_permuted_candidates_parse_once_and_preserve_ambiguity`,
  `lgwks_ast::tests::incomplete_candidate_inspection_is_not_reported_as_unique`,
  `lgwks_ast::tests::inspection_metrics_name_complete_exact_and_over_limit_walks`,
  `lgwks_ast::tests::syntax_diagnostics_stop_at_the_declared_bound`,
  `lgwks_ast::tests::a_truncated_syntax_report_keeps_the_earliest_errors_in_source_order`,
  `lgwks_ast::tests::the_refusal_and_the_report_count_the_same_recovery_nodes`, and
  `tests/content_detection.rs`

## Docs

- **INV-DOC-1** Bare `//!` intra-doc links break when `lib.rs` also doc-comments the
  `mod`; use reference definitions. · enforced by: rustdoc `-D warnings` in CI

## Governance as code

- **INV-GOV-1** The gate is defined once in `scripts/gate-lanes.toml`; CI runs
  the same lane commands and `scripts/check-gate-parity.py` refuses drift. ·
  enforced by: `gate-parity` lane
- **INV-BOT-15** One owner serializes journal writes, and an ambiguous write is
  never reported as a clean failure. A capacity-one request slot preserves
  ordering; a `FileView` gives lock-free fence checks; and when a waiter is
  dropped mid-write the owner poisons the handle with the reason instead of
  letting a later append proceed on an unknown outcome. A poisoned handle is
  recovered by reopening, which replays to the same facts and never a second
  effect. · enforced by: `journal::file::a_stalled_device_does_not_stop_the_
  task_waiting_on_it`, `journal::file::a_dropped_waiter_poisons_the_handle_and_a_
  reopen_does_not_duplicate`, and the `tests/sim_journal` torn-tail, replay and
  chain families
- **INV-BOT-16** The journal's event cap is a reported bound, not a hidden one.
  A scale measurement that had to clamp to the cap records the requested level,
  the level reached and the ceiling together, so a reader is never told a
  concurrency number nobody ran. · enforced by: `tests/sim_scale::tier_r*`
  (`tier-requested`, `tier-reached`, `tier-ceiling` in the trace)
- **INV-GOV-2** A product requirement is superseded, never edited: a changed
  normative sentence with no Supersession log entry fails the gate. · enforced
  by: `python3 scripts/check-requirements.py` (`requirements` lane)

## Open questions for the Director

- For INV-BOT-1..10: which crash/recovery test exercises each one? The ones without a
  named test are where the next regression will come from.
- Partial answer, 2026-09-23: INV-BOT-2, INV-BOT-3, INV-BOT-4 and the
  journal-before-acknowledge half of INV-BOT-1 are exercised under a real
  process kill against a real file store by
  `tests/durable_crash_observation.rs` (rows #100, #101, #102, #104, #106 of
  the #109 register). The still-unnamed remainder — INV-BOT-5's poll path
  under a real store (#99), INV-BOT-9's descendant tree (#107 T21/T22), and
  INV-BOT-10's real frame (#108) — is where the next regression will come
  from.
