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
  empty or partial graph. A capture that cannot be removed after a complete
  read is reported beside the result (`metadata::Collected`), never dropped and
  never in place of a valid graph; a refusal decided afterwards keeps it
  attached. A cleanup refusal names the failed step, the pid and each OS error.
  · why: #143 R14, #159 M1-M3, #193 · enforced by: `lgwks_deps::metadata::tests`,
  `metadata::tests::a_refusal_after_collection_keeps_the_unresolved_cleanup_attached`
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

- **INV-STD-SIM-2** A score and a refusal are different things. Raw cosine keeps
  its `[-1, 1]` domain and the shared `Similarity` contract reads it through the
  explicit, named `(raw + 1) / 2` mapping, so every implementation behind the
  trait satisfies its `[0.0, 1.0]` and identity laws. A weight total below `1.0`
  is a declared evidence deficit and is never renormalized or presented as
  identity. Any refused component withdraws the whole composed verdict at every
  threshold including `0.0`, is attributed to its component index, and its
  weight is not redistributed onto a surviving neighbour; all-zero effective
  evidence is an explicit `InsufficientEvidence`. The infallible `Similarity`
  and `Weighted::is_accepted` methods remain lossy for source compatibility and
  are not the authority-facing path. An edit or set budget is charged against
  the *normalized* unit — the lower-case-expanded scalar count — and a set
  budget is charged before dedup and before the quadratic scan. The lossy path
  heuristic is not reachable as an exact-match proof. · why: #160 S1/S2/S4 ·
  enforced by: `tests/similarity_evidence_contract.rs`
  (`cosine_trait_impl_stays_inside_the_documented_unit_interval`,
  `a_refused_component_is_not_accepted_at_threshold_zero`,
  `all_zero_weight_refuses_regardless_of_threshold`,
  `typed_refusals_carry_component_identity_through_composition`,
  `bounded_jaccard_refuses_before_the_quadratic_scan`,
  `budget_refusal_precedes_amplification_and_is_measurable`,
  `the_edit_budget_charges_the_normalized_unit_not_the_raw_scalar_count`,
  `the_heuristic_path_score_is_never_an_exact_match_proof`), `similarity.rs`
  (`every_evidence_error_variant_is_exercised_by_a_test`), and
  `tests/sim_similarity_sweep.rs` (`the_same_seed_replays_to_the_same_trace`)
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
  every stage and is never presented as proof of no effect. `timeout` bounds
  each phase of each hop; `deadline`, when set, bounds the whole call from the
  first lookup to the last body byte across every hop, and its expiry is the
  `Deadline` stage. · why: #163 N1/N2/N3, #190 review (ureq forwarded custom
  headers cross-origin), #191 · enforced by:
  `http::tests::a_deadline_bounds_the_whole_redirect_chain`,
  `http::tests::a_deadline_bounds_a_trickled_body`,
  `http::tests::a_cross_origin_redirect_carries_no_caller_header`,
  `http::tests::a_same_origin_redirect_keeps_ordinary_headers_but_not_credentials`,
  `http::tests::a_method_keeping_redirect_of_a_post_is_refused`,
  `http::tests::an_interruption_is_classified_the_same_way_at_every_stage`,
  `http::tests::body_timeout_preserves_stage_and_class`,
  `http::tests::eof_probe_timeout_preserves_stage_and_class`,
  `http::tests::legal_header_bytes_and_repeated_values_are_preserved`, and
  `http::tests::redirect_loop_refuses_at_the_configured_limit`
- **INV-STD-HTTP-2** The HTTP body ceiling is a measured bound, not an
  assertion. The body reader is handed one window of `min(remaining,
  READ_CHUNK_BYTES)` per call, never the declared body length; retained `Vec`
  capacity clamps to the declared ceiling at non-power-of-two sizes; retained
  header value bytes and the live heap held while a `Response` is alive are
  countable separately; the peak heap of a call is dominated by the engine's
  fixed buffering rather than by the body, so a preview of a body thousands of
  times the ceiling peaks within a fixed slack of a call whose body is at the
  ceiling; and an eager reader that reserves the declared length and drains the
  transport is measurably outside that bound. · why: #163 A5 · enforced by:
  `http::tests::the_read_window_is_a_fixed_chunk_and_capacity_is_clamped_to_the_ceiling`,
  `tests/sim_http.rs` (`seeded_ceiling_families_match_the_declared_outcome`,
  `a_seeded_torn_transport_is_never_a_complete_body`,
  `two_tenants_on_one_endpoint_stay_isolated`), and `tests/http_alloc.rs`
  (`the_read_path_retains_the_ceiling_not_the_body`).
- **INV-STD-ONLINE-1** Resolved reachability candidates share one monotonic
  connection budget and at most 64 addresses are attempted; each is offered an
  equal share of what remains, so a blackholed candidate cannot starve the
  ones after it. Synchronous
  `ToSocketAddrs` work is outside that budget and is not promised preemptible;
  the literal `is_online` endpoints share the same budget. The Boolean result
  remains a TCP heuristic, not application health. · why: #163 N4 · enforced
  by: `online::tests::address_candidates_share_one_remaining_budget`,
  `online::tests::a_blackholed_candidate_does_not_starve_the_next`,
  `online::tests::resolver_delay_is_outside_the_connection_budget` and
  `online::tests::a_whole_probe_fits_one_wall_clock_budget`
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
  allocation per token. A compiled `GlobPattern` carries no caller data and
  holds no interior mutability, so one pattern serves any number of concurrent
  callers; the mutable half is the caller-owned `GlobScratch`, which
  `is_match_with` takes by `&mut`. · enforced by: `glob::tests` work-growth,
  scratch capacity, Unicode, strict-error and exact double-star cases, plus
  `tests/glob_public.rs` and `tests/sim_shared_policy_tiers.rs`
  (`one_shared_matcher_evidence_policy_and_retry_policy_serve_every_tier`,
  `the_shared_values_are_reachable_through_an_arc_clone`)
- **INV-STD-SHARED-POLICY** One compiled matcher, one evidence policy and one
  retry policy serve 100, 1 000 and 10 000 concurrent callers with answers
  bit-identical to the single-threaded reference, zero divergence at every tier,
  and memory that does not scale with the caller count. Each caller owns its
  scratch; the shared values carry no caller data. A host that cannot reach a
  tier reports the requested tier and the level reached. A checked composition
  is `Send + Sync` because it is immutable and its components are, so the
  sharing claim is on the types rather than inferred from a run that did not
  crash; two tenants' policies over one input never cross. · why: the reviewer
  note on #154 item 7 and the hyperscale axis · enforced by:
  `tests/sim_shared_policy_tiers.rs`
  (`one_shared_matcher_evidence_policy_and_retry_policy_serve_every_tier`,
  `the_shared_values_are_reachable_through_an_arc_clone`),
  `glob::tests::a_compiled_pattern_is_shareable_across_threads_by_construction`,
  and `tests/sim_tenant_isolation.rs`
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
  establish application validity or schema identity. A retained fixture pins the
  schema and format and is read on every target whose format matches; a
  feature-unification probe selects an alternate pointer width and proves the
  descriptor and the emitted bytes move with it. · why: #167 · enforced by:
  `tests/wire_consumer.rs`, `tests/wire_fixture.rs`, `tests/sim_wire.rs` and
  `tests/wire_feature_unification.rs`
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
  `lgwks_std::fs::tests` and `tests/sim_fs_walk.rs`
  (`strict_refuses_the_first_unreadable_directory`,
  `tolerant_reports_unreadable_directories_and_keeps_the_rest`,
  `an_unresolvable_root_is_refused_by_every_entry_point`)
- **INV-FS-5** Walk output uses absolute paths rooted at the canonicalized
  input root; descendants reached through symlinks retain the logical alias,
  while canonical targets identify visits. Reports expose the applied depth
  and symlink policy, and completeness means complete within that policy.
  Bounded walks charge entries and path bytes before retention and mark any
  budget-limited prefix incomplete. Completeness does not depend on
  filesystem iteration order, except that following symlinks without sorting
  charges an aliased directory through whichever alias is met first; which
  entries an incomplete report retains, in the directory where a budget ran
  out, does. Strict failures preserve path, stage and
  the original I/O source. `available_space` is an advisory snapshot, never a
  reservation or write guarantee. · why: #166 · enforced by:
  `lgwks_std::fs::tests`, the public API doctest, and `tests/sim_fs_walk.rs`,
  which checks 48 seeded on-disk trees per family against a model of the
  admitted set, preorder and budget charge
  (`completeness_does_not_depend_on_sorting`,
  `a_tolerant_budget_prefix_is_bounded_and_marked`,
  `following_links_visits_each_directory_once_and_terminates`)

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

- **INV-FS-6** A capability listing is bounded by `ListLimits` (entries and
  name bytes), charged before a name is kept, and says when it was truncated.
  It returns only UTF-8 names, the one name type every `Dir` method accepts,
  and counts the rest as unaddressable; a listing with either is not complete.
  · why: #192 · enforced by:
  `lgwks_std::fs::capability::tests::a_listing_is_bounded_by_entries_and_name_bytes`,
  `lgwks_std::fs::capability::tests::a_non_utf8_name_is_counted_not_returned`

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
  refusal and the rendered report walk the same node set, root included. A
  rendered `InvalidSyntax` refusal points at the earliest retained recovery
  node; `AstMetrics` has no `Default`, so every value came from a walk.
  · why: #165 A1–A4, #194 · enforced by:
  `lgwks_ast::tests::an_invalid_syntax_diagnostic_points_at_the_earliest_recovery_node`,
  `lgwks_ast::tests::duplicate_and_permuted_candidates_parse_once_and_preserve_ambiguity`,
  `lgwks_ast::tests::incomplete_candidate_inspection_is_not_reported_as_unique`,
  `lgwks_ast::tests::inspection_metrics_name_complete_exact_and_over_limit_walks`,
  `lgwks_ast::tests::syntax_diagnostics_stop_at_the_declared_bound`,
  `lgwks_ast::tests::a_truncated_syntax_report_keeps_the_earliest_errors_in_source_order`,
  `lgwks_ast::tests::the_refusal_and_the_report_count_the_same_recovery_nodes`,
  `tests/content_detection.rs`, and `tests/sim_diagnostics.rs`, which checks 64
  seeded malformed sources per family against a line/column model
  (`the_refusal_keeps_the_earliest_recovery_nodes_in_source_order`,
  `a_syntax_refusal_points_at_the_earliest_recovery_node`)

## Docs

- **INV-DOC-1** Bare `//!` intra-doc links break when `lib.rs` also doc-comments the
  `mod`; use reference definitions. · enforced by: rustdoc `-D warnings` in CI
- **INV-DOC-2** A documentation claim about a capability is stated at the level
  the evidence supports, names the test that observes it, and names its owning
  issue when the property is not yet exercised. A claim resolved to a file and
  a line, a green job, or a passing unit suite is not evidence of the
  behavioural property; and an admitted dependency edge, a build check and an
  external observation are three different things. · why: #155 and #170 found
  prose describing a journal with no lock, a registry that accepted duplicates,
  a 33-grammar matrix that has 28, a lint split that does not partition its
  corpus, and a portability verdict that rested on a three-OS *build* · enforced
  by: the `doc-citations` lane, which runs
  python3 scripts/check-doc-citations.py (citation layer: a cited line must exist
  and still read as a person last checked it). The claim layer is
  docs/std-ast-deps-closure-matrix.md: every one of the twenty lgwks_std
  modules, lgwks_ast and lgwks_deps carries a state from the fixed vocabulary
  exercised / present / unexercised-gap / assurance-gap /
  admitted-not-implemented, plus the test that exists

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
- **INV-BOT-17** A `BotSpec` materializes into a runnable bot only through the same
  `assemble`/`build` path a native bot uses, so the two produce the same operation
  trace. Authority comes only from the caller's `GrantSet`: a spec cannot grant
  itself reach. Admission is all-or-nothing and reports every presently knowable
  unmet need in one `NeedSet` (unknown source or action domain, a constructor that
  rejects its target, a missing capability, an unknown condition), each attributed
  to its chain and action; a duplicate registry identifier and an unsupported spec
  version are typed refusals; and a refused materialization polls nothing and
  executes nothing. · why: #87 step 3 · enforced by:
  `tests/spec_materialize.rs` and `tests/sim_spec_materialize.rs`
- **INV-GOV-2** A product requirement is superseded, never edited: a changed
  normative sentence with no Supersession log entry fails the gate. · enforced
  by: `python3 scripts/check-requirements.py` (`requirements` lane)
- **INV-BOT-18** A supervised process reports only what it observed. A captured
  stream retains at most its declared non-zero ceiling, keeps draining past it
  so a chatty child cannot block, and reports the exact byte total with a
  truncation flag; the retained buffer is sized once to the ceiling. A run
  carries its exit status or signal, whether a deadline stopped it, and the
  process-group cleanup receipt; a refusal before the fork (`Refused`,
  `NotStarted`) is distinguishable from a failure after it (`AfterStart`); and
  an exit of zero is reported as an exit of zero, never as a completed task.
  Concurrent verb calls on one `sys::Process` share one bounded slot pool,
  claimed before the fork, so a burst of calls never forks past the ceiling.
  On a non-Unix target the supervised process surface is a typed pre-fork
  refusal (`io::ErrorKind::Unsupported`, `DispatchCertainty::Refused`) rather
  than a second spawn path. The drop-time group kill repeats only while the
  unreaped leader pins the group id, so a member forked at the instant of the
  kill can still outlive the leader on macOS/BSD (Linux aborts such a fork);
  no bounded fix preserves INV-BOT-12, because after the leader is reaped the
  numeric id may be reused and the cleanup owner is observation-only by design.
  · why: #151 sys part, T05/T19/T20/T35 · enforced by:
  `tests/sys_process_binding.rs` (including
  `concurrent_calls_on_one_process_share_its_ceiling`), `tests/sim_process.rs`,
  `tests/sys_process_portable.rs` (non-Unix), and `rt::supervise::tests`
- **INV-BOT-19** After a delivered group signal, an `EPERM` from a further
  `killpg` against the still-present, unreaped group is an observation that the
  group is present, not a refused termination: cleanup stays pending and is
  settled by the post-reap signal-zero probe, never reported as a failed kill.
  · why: macOS/BSD report `EPERM` for a group whose leader is an unreaped
  zombie; reading it as failed reported a deadline stop as a cleanup failure ·
  enforced by:
  `rt::supervise::tests::an_unsignalable_present_group_stays_pending_rather_than_failed`
  and `rt::supervise::tests::an_unexpected_signal_error_is_a_failed_cleanup`
- **INV-BOT-21** A structural inspection reads and parses the subject's bytes
  and never executes them: no compile, import, build-script evaluation,
  dependency install, shell invocation or dynamic-library load of the subject,
  and the subject's instructions remain data. It parses through `lgwks_ast`
  (never a second parser) and walks with budgets on **separate** axes — source
  bytes, nodes, depth, work, retained findings and emitted output — refusing
  before avoidable amplification. Its verdict is typed: a supported and
  complete clean scope is `Clean`, and an unsupported grammar or rule set, an
  unconfirmable declared version, a budget exhaustion or parse recovery, and an
  infrastructure failure are distinct non-clean arms. Rule support is reported
  per rule and is separate from grammar support; a `Clean` result claims only
  that the configured rules did not match, never that the subject is safe. The
  one operation is reachable through the same registry/admission path every
  other domain uses — a [`Query`](crate::verb::Query) over supplied bytes
  (`domain::inspect::Inspector`) and an [`Observe`](crate::verb::Observe)
  source that reads the artifact under `bot.fs` (`domain::inspect::Subject`) —
  and as a `Host`-run `Task`, and every door returns the operation's own report;
  the artifact-read source is admitted, or refused, exactly like any other
  capped domain.
  · why: #150 (R8) · enforced by:
  `tests/inspect_non_execution.rs` (independent filesystem, process-liveness and
  TCP-listener observers over a hostile corpus),
  `a_parser_fault_is_an_infrastructure_failure_not_a_clean_report`,
  `an_eager_traversal_mutant_fails_the_node_budget_oracle`,
  `a_subject_executing_mutant_fails_the_non_execution_oracle`,
  `both_entry_points_produce_the_identical_inspection`,
  `a_spec_naming_the_inspection_source_without_bot_fs_is_an_admission_need`,
  `two_tenants_inspecting_the_same_artifact_stay_isolated`,
  `host_bounded_admission_holds_at_every_tier`,
  `retained_counters_grow_with_the_input`,
  `a_match_longer_than_the_preview_budget_is_truncated_with_its_full_span_kept`,
  `tests/inspect.rs::every_budget_has_its_own_refusal`,
  `tests/inspect.rs::invalid_syntax_is_incomplete_and_never_a_clean_report`,
  `tests/inspect.rs::a_declared_language_version_is_undecidable_not_clean`,
  `tests/inspect.rs::the_report_round_trips_and_preserves_identity_spans_and_coverage`,
  `sim_seeded_subjects_agree_across_every_entry_point`,
  `sim_same_seed_same_trace_hash`,
  `sim_seeded_multitenant_reports_stay_isolated`,
  and `tests/sim_inspect.rs` (`sim_seeded_fragments_match_the_rule_model`,
  `sim_same_seed_same_trace`, `sim_node_budget_tiers_refuse_deterministically`)
- **INV-BOT-20** A task run on a `Host` takes at most one admission permit per
  host for its whole tree: a nested `host.run` from inside a body that already
  holds that host's permit is charged to the parent, so nesting at any depth
  completes at an admission ceiling of one, while sibling top-level runs each
  take their own permit and stay bounded. Waiting for a permit is cancellable by
  the host's stop and charged to the run's deadline; a run the host refuses
  before admission is `Refused`, never `Cancelled` or `Failed`. The step trail
  is a bounded ring whose overflow is counted and never changes the
  disposition, output or located error, and every report says no external
  effect is known. · why: #87 step 1 (T01–T04, T36) · enforced by:
  `tests/task_front_door.rs` and `tests/sim_task.rs`
- **INV-BOT-40** The committed record can be replayed without materializing it:
  `FileJournal::replay` streams frames from its own read-only descriptor,
  retaining at most one event, applies the same frame validation and event
  ceiling `open` does, and yields exactly the acknowledged history. The
  materialized `events()` view and the stream agree on every seed. · why: #122
  item 2 · enforced by: `tests/sim_journal_liveness.rs`
  (`streaming_replay_r00..r15`)
- **INV-BOT-41** A durable journal reserves room for the whole external handoff
  — intent, preparation and settlement — before the first rung is written, so
  it never leaves an attempt admitted and unable to settle. A journal with no
  settlement room refuses the handoff with `CapacityExceeded` and writes
  nothing. · why: #122 item 2 / #156 · enforced by:
  `ecs::tests::an_external_handoff_reserves_settlement_capacity_before_any_rung`
- **INV-BOT-42** Applied-key membership is answered by index, not by scanning a
  growing vector: the contract identity keeps the latest key per action and the
  event identity keeps `(action, digest)` membership, so a run that applied N
  distinct keys does O(N) work rather than the removed `Vec`'s Θ(N²)
  membership-before-push. Both indexes retain one entry per action / per
  distinct event for the controller's life, the same horizon the vector had, so
  the #101 redelivery and #129 new-episode semantics are preserved. · why: #122
  item 2 / #156 · enforced by:
  `ecs::tests::applied_membership_answers_at_scale_where_a_scan_would_be_quadratic`
- **INV-BOT-43** The durable write is performed by the storage owner thread, not
  the thread that awaits the append: a parked device leaves the runtime and an
  unrelated ready task free to progress, and a release from an independent
  thread is what lets the append finish. A dropped waiter still poisons the
  handle. · why: #122 item 4 / #156 · enforced by: `tests/journal_liveness.rs`
  (`a_slow_store_lets_the_runtime_and_the_release_progress`,
  `a_cancelled_append_leaves_the_runtime_and_the_handle_live`)
- **INV-BOT-44** A real process kill while an append is in flight, against a
  store that has not answered, leaves a clean journal: the reopen reports no
  committed event and no torn tail, and the retry lands exactly once. The kill
  is a real `SIGKILL` of a child that handed the storage owner the append and
  then parked. · why: #122 item 2 / #156 · enforced by:
  `tests/durable_crash_observation.rs`
  (`a_real_kill_mid_append_leaves_no_duplicate_and_no_lost_receipt`)
- **INV-BOT-45** Concurrent tenant appends over separate files stay isolated,
  lose nothing and duplicate nothing, and one acknowledged append's latency
  tails stay bounded on the shipped path where the write and `sync_all` run on
  the storage owner. The tiered sweep runs 100, 1,000 and 10,000 concurrent
  tenant journals; a tier the host cannot reach is clamped to the level the
  process really can, and the requested, reached and ceiling levels are recorded
  together with the p50/p95/p99 append latency and the peak RSS, so no reader is
  told a concurrency number nobody ran (the INV-BOT-16 rule). · why: #122 item 2
  / #156 · enforced by: `tests/journal_scale.rs`
  (`concurrent_tenant_appends_scale_with_isolation`,
  `append_latency_tails_are_bounded`) and `tests/sim_journal_liveness.rs`
  (`tenant_tiers_replay_at_the_level_the_sim_can_drive`)
- **INV-BOT-46** A registry identifier declared twice in one role has one
  meaning: refused. `DomainRegistry::validate` names the identifier, role and
  both positions before any build, and the raw `source`/`action` lookups never
  resolve an ambiguous identifier to its first declaration, so dispatch never
  depends on declaration order. One identifier used once per role stays valid.
  · why: #122 item 1 · enforced by: `tests/registry.rs`
  (`an_ambiguous_identifier_is_not_resolved_by_declaration_order`,
  `a_duplicate_source_identifier_is_refused_with_both_positions`,
  `refusal_is_independent_of_declaration_order`)
- **INV-BOT-47** An append whose reply was lost but whose record committed is
  reconciled by readback and settled, never resent and never stalled on; an
  append that may have committed and did not reports the occurrence as certain
  and lands its record on the retry without re-entering the action.
  · why: #118 item 1 · enforced by: `tests/ambiguous_commit.rs`
  (`a_committed_outcome_with_a_lost_reply_settles_without_resending`,
  `an_unknown_outcome_that_did_not_commit_reports_occurrence_and_records_on_retry`)
- **INV-BOT-70** The task front door's nine-axis evidence: a drawn scale of
  concurrent `Host::run` never exceeds the admission ceiling and returns every
  permit (100/1,000/10,000 tiers with recovery); two hosts with different
  tenants over one shared task name and step path produce distinct step keys,
  reports and budgets under concurrency; dropping or cancelling a suspended run
  releases every permit and leaves nothing in flight; and task names, inputs
  and host limits are accepted or refused exactly on their declared boundaries.
  · why: #203 nine-axis review · enforced by:
  `tests/sim_task_axes.rs` (`saturation_conserves_permits`,
  `saturation_reaches_100_1000_and_10000_with_recovery`,
  `two_tenants_stay_isolated`, `a_dropped_run_releases_everything`,
  `names_inputs_and_limits`) and `examples/compare_orchestration.rs`

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
