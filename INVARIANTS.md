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
- **INV-DEP-12** A source class is not an approved origin: an admission compares
  the Cargo origin (a complete registry source, a Git repository plus its
  admitted revision/reference policy, or an external path authority), so a
  substitution inside an approved class is `OriginDrift`, never a pass. A legacy
  class-only entry is exact for crates.io — both its Git and sparse spellings —
  and insufficient for a Git or path edge; an unknown scheme is neither
  authorable nor an ordinary admitted origin. · why: #158 A1 · enforced by:
  `tests/origin_binding.rs`, `tests/sim_origin.rs`, and
  `lgwks_deps::tests::an_approved_git_origin_admits_only_that_repository`,
  `lgwks_deps::tests::a_git_revision_policy_change_is_an_origin_drift`,
  `lgwks_deps::tests::an_approved_registry_origin_refuses_a_different_registry`,
  `lgwks_deps::tests::a_class_only_registry_approval_admits_crates_io_only`,
  `lgwks_deps::tests::a_class_only_git_approval_is_insufficient_for_exact_origin`,
  `lgwks_deps::tests::an_approved_path_origin_refuses_a_different_path`,
  `lgwks_deps::tests::an_unknown_scheme_is_not_an_admitted_origin`,
  `lgwks_deps::tests::multiple_approvals_report_the_relevant_failed_dimension`,
  `metadata::tests::a_sparse_registry_source_is_classified_as_a_registry`
- **INV-DEP-13** An approval may author admitted-capability policy: `features`
  (the complete set of upstream features the edge may enable), `required_features`
  (a subset that must be enabled), `uses_default_features` and `optional` (the
  exact authored bit) and `target` (the exact scope; `""` is unconditional). A
  dimension an entry authors is enforced exactly — an enabled feature outside the
  set, a missing required feature, a flipped bit, a changed scope is a typed
  `Refusal::{FeatureDrift, DefaultFeaturesDrift, OptionalityDrift, TargetDrift}`;
  a dimension an entry does not author is grandfathered rather than refused.
  Mandatory and allowed features are distinguished, so ordering is never
  significant. · why: #158 A2 · enforced by: `tests/feature_policy.rs`,
  `tests/sim_dependency_policy.rs`, and
  `lgwks_deps::tests::a_class_only_registry_approval_admits_crates_io_only`
- **INV-DEP-14** Cargo package identity is byte-exact: an approval admits an
  observed package name only when it is that name, or a name the entry lists in
  its explicit `aliases`. There is no implicit `-`/`_` fold or case fold, so two
  distinct packages whose spellings fold alike cannot share one authority. An
  alias names exactly one package (collision-checked at load, including against
  another package's real name), and a `package =` rename stays a local spelling
  of the upstream identity rather than a second one. · why: #158 A2 · enforced by:
  `tests/identity_binding.rs`,
  `contract::tests::lookup_is_exact_and_only_an_explicit_alias_is_tolerated`,
  `contract::tests::aliases_collide_rather_than_share_authority`
- **INV-DEP-15** A `check` receipt binds the subject root, the contract identity
  and schema version (a stable digest of the register text), the exact metadata
  subject (a stable digest over every direct edge's identity), the policy mode
  (`--contract` diagnosis versus committed enforcement) and the assurance scope;
  the `--json` form exposes the same under stable keys. The digest is an identity
  fingerprint, not an adversarial integrity claim. · why: #158 A6 · enforced by:
  `tests/check_cli.rs` (`the_human_receipt_binds_contract_subject_and_mode`,
  `the_json_receipt_has_stable_identity_fields`,
  `the_receipt_changes_when_its_subject_changes`)

## lgwks_bot — durable execution

Each of these was a shipped defect. Treat the list as the spec.

- **INV-BOT-30** One declared clock governs every deadline this crate
  evaluates, and pausing it never disables the wall-clock watchdog.
  `rt::clock::Clock` is the authority; `rt::time::Deadline` names the clock that
  governs a deadline rather than an opaque process-local `Instant`. A
  caller-advanceable clock saturates at its declared ceiling and refuses a caller
  advance when already there, so a deadline computed from a wrapped clock can
  never fire immediately and look legitimate. What survives a restart is the
  remaining **duration** (`ClockSnapshot`), never the instant: an `Instant` has
  no epoch and means nothing on another host. The watchdog reads
  `std::time::Instant` and is therefore unreachable from a paused logical clock
  — a subprocess that stopped answering, a blocking callback that will never
  return, and a store whose `fsync` is stuck are not waiting for time, and only
  real elapsed time says so. Determinism claimed here is about which deadline is
  *eligible*; poll order across workers, observed external order and cross-host
  clock skew are not claimed. · why: #152 §1 · enforced by:
  `tests/sim_clock.rs` (`the_same_seed_replays_the_same_clock_trace`,
  `racing_logical_time_leaves_the_wall_watchdog_independent`,
  `a_restart_restores_the_remaining_budget_and_not_an_instant`,
  `a_wall_clock_refuses_a_caller_advance`,
  `the_elapsed_ceiling_saturates_instead_of_wrapping_into_the_past`,
  `an_over_advanced_clock_saturates_rather_than_wrapping`,
  `distinct_seeds_give_distinct_clock_traces`).
- **INV-BOT-31** Inspection reads the owner's own state; it is never a second
  ledger. `Supervisor::snapshot` is built from the same admission and reporting
  fields the permits and `Stats` are built from, so what it reports and what the
  supervisor admits are one fact read twice: a snapshot that says a free permit
  is followed by a spawn that starts. The live listing is capped at the
  in-flight ceiling and reports how many it excluded; terminal outcomes stay in
  the report stream, where the retention cap already governs them, so no outcome
  exists in two places. Every snapshot field is private behind an accessor: a
  read must not hand the caller the right to edit what they believe they
  observed. A caller that never drains loses detail, never memory, and the
  dropped counter says so. · why: #152 §2 · enforced by:
  `tests/inspect_contract.rs` (`the_snapshot_and_the_admission_decision_agree`,
  `the_live_listing_is_bounded_by_the_ceiling_and_says_it_truncated`,
  `cancellation_closes_admission_and_the_terminal_record_stays_readable`,
  `an_undrained_supervisor_reports_dropped_detail_without_growing`,
  `repeated_snapshots_do_not_accumulate`,
  `reading_a_snapshot_has_no_side_effect_on_admission`,
  `a_fresh_snapshot_admits_at_the_declared_ceiling`,
  `a_bounded_repeating_task_reports_exhaustion_not_a_hang`).
- **INV-BOT-60** A readiness is a typed, generation-bound fact, and a dependant
  never learns it by sleeping. `script::Readiness<T>` releases its dependants
  with a `Ready<T>` that carries the `Generation` the instance was admitted
  under, so four refusals are four different facts rather than one boolean: a
  signal from an older instance is `StaleGeneration`, one from a generation this
  readiness never issued is `UnknownGeneration`, a second release at a settled
  generation is `AlreadyReady` (the first release survives it, and it is not a
  second release), and a duplicate failure is `AlreadyFailed`. Every one of them
  released nobody, and `ReadinessError::released` says so for all of them. A
  failure *after* the release cancels the token of every dependant it released,
  so a dependant still running learns the service is gone, and `failed_at`
  distinguishes that from a failure before anyone was released. A shutdown is a
  stop rather than a failure: the waiting side sees `FlowError::Cancelled`, so a
  routine restart does not read as an outage. `Generation` is a monotone
  saturating counter, never a timestamp — cross-host clock skew is unmeasured
  (INV-BOT-30) — and a restart is a *new* readiness at a *new* generation, which
  is what makes a surviving handle from the old instance unable to release
  anybody. The wait is one admission, one `watch` subscription and one
  `select!`: no sleep and no poll loop, charged to the step's own budget through
  `within` so it is bounded and stopped by the scope's stop, and an
  already-released readiness resolves without spending its budget. Admission is
  charged **before** a slot is taken against `MAX_DEPENDANTS`, so a refused
  admission leaves capacity exactly as it was. · why: #87 T18 / LC-09 · enforced
  by: `tests/ready.rs` (`a_failure_before_ready_reaches_the_dependant_and_the_report`,
  `a_duplicate_ready_signal_is_refused_and_releases_nothing`,
  `a_stale_generation_is_refused_and_releases_nothing`,
  `a_restarted_service_arms_a_new_readiness`,
  `a_failure_after_ready_cancels_every_dependant_still_running`,
  `a_shutdown_is_a_stop_and_not_a_failure`,
  `a_wait_is_budgeted_cancellable_and_immediate_when_already_released`,
  `dependants_are_capped_at_the_declared_bound`,
  `no_readiness_path_sleeps_or_polls`) and `tests/sim_ready.rs`
  (`an_interleaving_never_releases_a_stale_generation`,
  `a_duplicate_releases_once_in_every_interleaving`,
  `a_failure_after_ready_reaches_every_running_dependant`,
  `two_tenants_never_cross_a_readiness`,
  `saturation_admits_up_to_the_declared_cap`,
  `the_same_seed_replays_the_same_trace`).
- **INV-BOT-61** A long-lived service's readiness is observed from its own
  output, never from a clock. `Supervisor::run_process_observed` hands the
  caller each newline-terminated line of the child's **stdout** from inside the
  same pipe read that retains it, so the observation and the capture are the same
  bytes seen once and cannot disagree about what the child wrote; the observer is
  called before the capture ceiling is consulted and whether or not the bytes are
  retained, so a chatty child cannot make its own readiness unobservable. A
  trailing fragment with no newline is not delivered — a partial line is not a
  line. The unterminated tail is capped at `MAX_OBSERVED_LINE_BYTES`, so a child
  that prints a megabyte without a newline cannot make the observer the
  unbounded buffer the capture ceiling exists to prevent. The observer arms no
  timer, and `Supervisor::run_process` is that same call with `None`: there is no
  second process driver and no second readiness path, on any target. · why: #87
  T18 / LC-09 · enforced by: `tests/ready.rs::real_process`
  (`a_child_printing_its_address_releases_the_dependants`,
  `a_child_that_dies_without_printing_never_releases_the_dependants`,
  `a_child_that_dies_after_announcing_stops_the_dependants`) and
  `tests/ready.rs::no_readiness_path_sleeps_or_polls`, which asserts against the
  module's own source that neither the wait nor the observer arms one.

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
  `fs::capability::Dir::entry_names` is Linux-only. The raw `getdents64` syscall is the only
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
- **INV-BOT-50** A durable record reaches the disk on a thread of the store's own,
  so no executor thread ever waits inside a flush. The whole ordered step — the
  in-memory checks, the length fence, the write, the `sync_all` and the fold into
  the store's index — runs on one `journal::owner` thread, which is what makes the
  fence and the write it guards un-overtakable. `RunRecords::append_async` is what
  a step awaits, so a parked device is a wait rather than a stalled runtime; a
  caller that walks away from an outstanding record latches the handle's poison,
  because the bytes may be on the disk under no acknowledgment and only a reopen
  settles that. · why: #87 step 5, the blocking-write defect #122 removed from
  `FileJournal` and the run store inherited · enforced by:
  `tests/resume_liveness.rs` (`a_parked_record_device_lets_the_runtime_turn`,
  `an_abandoned_record_leaves_the_store_consistent`,
  `a_parked_store_still_serves_its_own_records_only`,
  `the_parked_device_probe_measures_turns`), whose watchdog is an independent OS
  thread and whose assertion is on the unrelated task's poll count — measured at
  1,917 turns against a parked flush, where a blocking implementation reaches
  one
- **INV-BOT-51** The effect journal's frames and the run store's frames are one
  grammar, in `journal::frame`: the length prefix, the 32-byte stored head, the
  torn-tail scan, the refusal of a frame no writer produces, and the whole
  archive/bound/chain/lay-out step, parameterised by each store's record type,
  archiver and head-chaining function. "What a torn tail is" therefore has one
  answer in this crate rather than one per store, and a frame one store writes is
  framed exactly as a frame the other writes. `journal::file`'s behaviour is
  unchanged by the extraction. · why: #87 step 5 (duplicated estate capability)
  · enforced by: `journal::frame::tests` (six properties, including the three
  prefix endings and the frame round trip), `journal::file::tests` unchanged and
  green, and the `tests/sim_journal.rs` and `tests/sim_journal_liveness.rs`
  binaries
- **INV-BOT-52** A durable step's cost is a stated number, not an adjective. The
  run store's per-step cost is measured against the two things it could be: a
  plain un-recorded step, and the effect journal's append at the same payload
  size. Measured here: plain p50=1us, `remember` p50=17984us / p95=33189us /
  p99=44988us, `FileJournal` p50=15977us / p95=29949us / p99=39949us — so a
  durable step is not paying twice for one mechanism. A measurement that did not
  run says it did not run, never a bound nobody checked. · why: #87 step 5
  (frontier) · enforced by: `crates/lgwks-bot/examples/resume_cost.rs`, three
  mechanisms at one payload size in one harness
- **INV-BOT-53** The store holds every record at every concurrency tier it claims,
  with two tenants interleaved, and the counts are read back from a reopened
  store rather than from the handle that wrote them. Measured here: 100 runs →
  p50=3019us p95=3977us p99=4138us in 316.11ms; 1000 → p50=3005us p95=4001us
  p99=4136us in 3.13s; 10000 → p50=3017us p95=4144us p99=5312us in 32.74s,
  with no record lost, none duplicated and none attributed to the wrong tenant.
  Under contention the ordered step stays ordered: many appends through one owner
  thread lose nothing, a repeated submission commits one frame, and a replayed
  step is never re-recorded. · why: #87 step 5 (hyperscale) · enforced by:
  `tests/task_resume.rs::concurrent_runs_across_tiers` and
  `tests/sim_store_scale.rs` (`concurrent_appends_lose_nothing`,
  `an_interrupted_step_records_exactly_once`,
  `tenants_interleaved_stay_isolated`, `same_seed_replays`)
- **INV-BOT-54** A durable claim is backed by a store that outlives the process, and
  every part of it says which. A host with `run_store` installed mints a `RunId`,
  records each `remember` step's value with an `fsync` *before* returning it, and
  reports the run's identity and step count in `EffectKnowledge::StepRecords`; a
  host with no store reports `EffectKnowledge::None`, no `run_id` and no ticket,
  and its durable steps simply re-run. A resume under a run id another tenant owns
  is a typed `Refused`, never another tenant's records. Committed records are
  chain-verified and a broken chain is refused, not trimmed; an interrupted final
  append is the one thing dropped, because it was never acknowledged. Each of the
  three ceilings (per-record bytes, records per run, total bytes) is a typed
  refusal naming the bound, and a refusal leaves the store byte-identical. A step
  that ran but whose record did not land re-runs on resume, so the durable
  guarantee is exactly-once for a *recorded* step and at-least-once for an
  unrecorded one; an external effect a step performs still needs the effect
  journal, not this. · why: #87 step 5 (host-held continuation) · enforced by:
  `tests/task_resume.rs` (`a_killed_process_resumes_without_rerunning_finished_steps`,
  `a_run_without_a_store_claims_no_durability`,
  `two_tenants_resuming_one_run_id_stay_isolated`,
  `a_torn_final_record_is_dropped_and_earlier_ones_survive`,
  `a_store_corrupt_before_the_tail_is_refused_not_trimmed`,
  `an_oversized_record_is_refused_naming_the_ceiling`,
  `a_duplicate_append_of_the_same_record_is_a_no_op`) and
  `tests/sim_task_resume.rs` (`crash_points_resume_to_the_same_output`,
  `finished_steps_run_once`, `tenants_stay_isolated`, `same_seed_replays`),
  which sweeps every step boundary of every seeded run twice for an identical
  trace hash.
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

- **INV-BOT-80** A GitHub publication is reported only from an independent
  read-back, never from a client's exit code. `ReviewOutcome::Published` is
  produced only when a review at the reviewed commit, with the intended body and
  state, was observed on a separate call; the create's own success is transport
  evidence and is never one. A publish step that failed for any reason other
  than a `Refused` certainty — a non-zero exit, a dropped connection, a
  deadline — leaves the effect unobserved and is reconciled by exactly one read,
  because an exit code cannot distinguish "never arrived" from "applied and the
  answer was lost". A reconciliation that cannot establish the outcome stays
  `Unknown` and issues no second create. A head that changed between the pinned
  read and the publication is `TargetMoved`, naming both commits, and publishes
  nothing; the review subject is never silently re-pointed at the new head.
  Verification compares subject, body and state and ignores the application
  marker. A staged payload carries a name no two concurrent publications share,
  taken from `lgwks_std::random` under `ephemeral`; a build without that feature
  refuses to publish rather than reuse a name the OS recycles. A review read is
  bounded by the declared `domain::gh::MAX_REVIEWS_PER_PULL` rather than by
  `--paginate`'s patience: a list longer than the ceiling is refused whole with
  `GhError::ReviewCeiling`, because a prefix that decoded cleanly is
  indistinguishable from the whole history and would report "no matching
  review" for a review on a page nobody read. A build without the `process`
  feature refuses every call with `GhError::NoRunner` and returns no snapshot,
  review list or review id at all. · why: #151, #87 step 6 (PR-06, PR-07,
  PR-09) · enforced by:
  `tests/pr_review_journey.rs` (`a_lost_response_is_reconciled_by_reading_back_and_never_reposted`,
  `a_loss_that_cannot_be_reconciled_stays_unknown_and_still_does_not_repost`,
  `a_moved_head_is_a_typed_refusal_and_publishes_nothing`,
  `a_review_is_published_at_the_pinned_head_and_verified_by_a_separate_read`,
  `a_non_zero_exit_is_not_a_published_review`,
  `a_client_that_never_starts_is_a_definite_non_effect_and_is_not_reconciled`,
  `two_identities_on_one_repository_stay_isolated`) and
  `tests/sim_review_pr.rs` (`subject_r64`, `publication_r64`, `identity_r64`,
  `same_seed_same_trace_hash`, `every_outcome_is_reachable_in_the_family`),
  `tests/sim_review_path.rs` (`verified_and_not_observed_r32`,
  `gh_exit_failures_r32`, `deadline_stop_r32`,
  `head_moved_between_snapshot_and_publish_r32`,
  `malformed_and_oversized_answers_r32`,
  `a_publication_the_ceiling_cannot_verify_stays_unknown`,
  `same_seed_same_trace_hash_r32`, `saturation_r32`,
  `two_tenants_on_one_pull_request_r32`, `cancellation_under_faults_r16`,
  `duplicate_submission_r16`, `two_repositories_on_one_host_r16`) and
  `tests/gh_binding.rs` (`a_review_list_past_the_ceiling_is_refused_not_truncated`,
  `a_review_list_exactly_at_the_ceiling_is_read`,
  `a_malformed_review_list_is_refused_rather_than_decoded_into_a_partial_answer`)

- **INV-BOT-81** A review's subject is the repository the caller named and the
  diff that was read, and a publication is reported only from evidence of the
  exact state that landed. `Gh::read_diff` reads the changed-file inventory as
  **data — never executed**, including a `build.rs` whose patch text would
  perform an effect — bounded on two separate axes (file count against
  `MAX_DIFF_FILES_PER_PULL`, patch bytes against `MAX_DIFF_BYTES`) with typed
  refusals, and the inventory is refused whole rather than truncated. An
  unavailable diff (`406`, `GhError::DiffUnavailable`) or either diff ceiling is
  an `Incomplete` coverage decision, never a clean review; a renamed-repository
  answer is `GhError::MovedRepository` naming both the requested and canonical
  repositories, and the journey refuses rather than silently re-pointing the
  subject. The publish path distinguishes what landed from what was read back: a
  lost response reconciled onto an unsubmitted draft is `Pending`, onto a
  submitted review carrying fewer inline comments than intended is `Partial`
  (with both counts), and a create that returned an id whose read-back lost
  permission (`GhError::Unauthorized`, from `401`/`403`/`404`) is `Unverified`
  with the applied review id retained; a read that failed for any other reason
  stays `Unknown`. No path issues a second create, and a non-zero exit that named
  no HTTP status stays a transport failure rather than being guessed into a
  permission one. · why: #151, #87 step 6 (PR-02, PR-03, PR-04, PR-07, PR-08,
  PR-09, PR-10), T31/T33/T34 · enforced by:
  `tests/pr_review_journey.rs`
  (`an_unavailable_diff_is_an_incomplete_coverage_and_publishes_nothing`,
  `a_diff_past_the_file_ceiling_is_an_incomplete_coverage`,
  `a_diff_past_the_byte_ceiling_is_an_incomplete_coverage`,
  `a_renamed_repository_is_refused_and_never_silently_re_pointed`,
  `an_untrusted_build_script_in_the_diff_is_never_executed`,
  `a_pending_draft_is_reconciled_as_a_draft_and_never_reposted`,
  `a_partial_submission_is_reconciled_as_partial_and_never_reposted`,
  `a_lost_read_permission_reports_unverified_and_retains_the_review_id`),
  `tests/gh_binding.rs`
  (`a_renamed_repository_is_a_typed_move_naming_both_names`,
  `a_permission_refusal_is_a_typed_unauthorized_not_a_transport_failure`,
  `a_failure_naming_no_status_stays_a_transport_failure`,
  `a_diff_past_its_file_ceiling_is_a_typed_coverage_refusal`,
  `an_unavailable_diff_is_a_typed_coverage_refusal`,
  `a_changed_file_inventory_is_read_as_data`),
  `tests/sim_review_path.rs` (`subject_coverage_and_partial_faults` bands 00
  through 07 (64 seeds), `same_seed_same_trace_hash_subject` bands 08 through
  11 (32 seeds), `two_identities_subject` bands 12 through 13 (16 seeds)), and
  `tests/sim_review_pr.rs` (`review_comments_are_carried_and_omitted_r16`)
- **INV-BOT-90** Model output is an untrusted *task input*, and crossing into the
  host is a typed refusal rather than an instruction. A payload is decoded by a
  hand-written bounded decoder against a `Surface` of the operations the host
  registered, so `install` and `credential` have no operation to name and are
  refused by what they asked for rather than as malformed documents; an unknown
  field is refused rather than ignored, a refused payload returns no plan beside
  its refusal, and every refusal and every admitted plan carries the
  `Provenance` of the exact bytes that produced it — including which untrusted
  source, since a model's mistake and a hostile tool's output are the same bytes
  with a different provenance. A sandbox escape — an absolute, traversing or
  drive-qualified path, or a `host` field naming another tenant — is its own
  `SandboxEscape` arm precisely so it stays *observable*: a refusal reported as
  malformed is a refusal nobody can find. A capability the run does not hold is
  refused by name, so the repair is a deliberate grant. It is not a fifth verb:
  an admitted `Plan` is a list of names to perform through the existing verbs, and
  this crate calls no network model — `StubModel` is a deterministic double,
  because the guarantee is about admission and admission is identical whoever
  produced the bytes. The bytes reach the run through one step,
  `script::admit`, so a refusal a task body sees is located at that step like
  every other `FlowError`, carries the `Provenance` of the exact bytes, and
  keeps its typed arm rather than arriving as a string a caller must parse.
  · why: #87 T26 · enforced by: `tests/proposal.rs`
  (`a_malformed_payload_never_becomes_work`,
  `an_injected_instruction_is_refused_by_name`,
  `a_tool_install_is_refused_and_the_surface_is_unchanged`,
  `a_credential_read_is_refused`,
  `a_sandbox_escape_stays_an_observable_refusal`,
  `an_unknown_operation_is_refused_whatever_asked_for_it`,
  `a_capability_the_run_does_not_hold_is_refused_by_name`,
  `a_refusal_is_attributable_to_its_exact_bytes`,
  `the_model_is_a_deterministic_double`,
  `an_injected_instruction_is_refused_at_its_step_on_a_real_run`,
  `a_well_formed_proposal_is_admitted_on_a_real_run`,
  `a_capability_the_run_does_not_hold_is_refused_by_name_on_a_real_run`) and
  `tests/sim_proposal.rs`
  (`seeded_shapes_match_the_declared_outcome_band_00..07`,
  `seeded_runs_reach_the_declared_disposition_band_20..23`)

- **INV-BOT-91** A completion claim is admitted only with the evidence it names
  present, and truncated data never becomes a full-coverage claim. The three
  untrue successes are three outcomes, not one: a claim whose evidence is absent
  is `NotEvidenced` with the references named, a claim whose evidence is present
  but whose coverage is short is `Incomplete`, and only a claim passing both is
  `Admitted`. `Coverage::from_claim` maps *every* payload claim — including the
  exact spelling `complete` — onto `Partial`, so a `Plan` cannot be talked into
  full coverage however many lines it carries, and `Coverage::Complete` has no
  constructor reachable from a decoder. A claim naming more references than
  `MAX_EVIDENCE_REFS` is refused whole rather than trimmed, since a trimmed claim
  asserts completeness over the prefix it kept. An abandoned run is admitted as
  `Abandoned`, visibly not the same thing as a finish. A claim is settled inside a
  task body on the run path, so the three outcomes reach a caller through the run
  that produced them rather than through a call only a test made. · why: #87
  T27/T35 · enforced by: `tests/proposal.rs`
  (`the_three_untrue_successes_report_distinct_outcomes`,
  `an_abandoned_run_is_not_a_finished_one`,
  `an_over_long_evidence_claim_is_refused_whole`,
  `a_truncated_payload_never_becomes_a_full_coverage_claim`,
  `a_well_formed_proposal_is_admitted_on_a_real_run`) and
  `tests/sim_proposal.rs`
  (`same_seed_same_trace_hash_band_16..19`,
  `seeded_runs_reach_the_declared_disposition_band_20..23`)

- **INV-BOT-92** A context reset recovers what the run already learned, because
  the checkpoint is `Durable` and round-trips through the run store rather than
  living in the instance that wrote it. `Checkpoint` carries completed steps,
  user corrections *with their kind*, `Unknown`-classed effects and evidence
  references, so a new task instance resuming the same run id recovers them; a
  `Refusal` correction stays a refusal rather than being re-read as an override,
  and an `Unknown` effect is still `Unknown` rather than absent. Every list is
  bounded, every bound is a declared constant, and the charge comes **before**
  the append — a refusal leaves the checkpoint exactly as it was, which is the
  defect the suite found in its own first draft and fixed. Re-recording a
  completed step is a no-op, so a resumed step cannot inflate the count or trip
  the ceiling, and reconciling an existing effect reference replaces rather than
  joins. A truncated archive is refused, never decoded into a partial
  checkpoint. The *refusals* round-trip on the same store and under the same
  mechanism: `script::admit` records each one under `<step>/refusal` before it
  returns the `FlowError`, so a new instance resuming the same run id reads
  back which arm fired, at which step, for which bytes and from which
  untrusted producer — a fact it cannot re-derive, having never seen the
  payload. A resumed run reads it back from a *reopened* store through
  `RunStore::lookup`, not from the handle that wrote it. · why: #87 T27 ·
  enforced by: `tests/proposal.rs`
  (`a_context_reset_preserves_completed_work_corrections_unknowns_and_evidence`,
  `a_checkpoint_round_trips_through_the_run_store`,
  `a_checkpoint_refuses_to_grow_past_its_ceiling`,
  `a_resumed_run_reads_back_the_refusal_the_first_run_recorded`)

- **INV-BOT-93** An artifact is keyed by `(tenant, digest)`, so a digest is never
  an authorization. Two tenants holding identical bytes get distinct keys and
  separate shelves, and a tenant that wrote nothing reads nothing whatever digest
  it names; the isolation is of the index rather than a check the caller
  remembers. Writes to one key are serialized through one writer path, so exactly
  one writer commits and every later writer of identical content is told
  `AlreadyPresent` — with a `writers` receipt counting every writer that reached
  the key, which is how "serialized" is observed rather than asserted. Reads take
  no writer lock, so independent reads and writes to other keys progress. The
  store never hands out a *prefix* of an artifact, and every ceiling
  (per-artifact bytes, artifacts per tenant, tenant-name length) is a typed
  refusal that leaves the store byte-identical. · why: #87 T28 · enforced by:
  `tests/proposal.rs` (`two_tenants_on_one_digest_stay_isolated`,
  `conflicting_writes_to_one_key_are_serialized_and_idempotent`,
  `reads_progress_while_a_write_is_in_flight`,
  `an_oversized_artifact_is_refused_and_the_store_is_unchanged`) and
  `tests/sim_proposal.rs`
  (`two_tenants_on_one_digest_stay_isolated_band_08..11`,
  `concurrent_readers_and_conflicting_writers_band_12..15`,
  `saturation_reaches_100_1000_and_10000`,
  `two_tenant_saturation_keeps_its_shelves_apart`,
  `two_tenants_admitting_on_one_host_stay_isolated_band_24..27`)

- **INV-BOT-94** Repeated unchanged failure reaches a finite typed intervention,
  and new evidence does not erase what a failure already cost. `RepairLedger`
  counts one *unchanged* fingerprint and returns `Intervention::NoProgress`
  once the declared `repeat` ceiling is exceeded, and it never returns to
  repairing that fingerprint afterwards; a run filling the ledger with *different*
  failures reaches `Intervention::LedgerFull`, which is its own arm because those
  are different facts. `record_evidence` adds to the ledger and marks progress
  but moves no repetition count and does not clear the recorded-evidence mark, so
  the count the ceiling is measured against never falls — progress on one axis
  cannot buy unbounded attempts on another, and `spent` is monotone for the whole
  run. Bounded repair is a declared `PlanBudget` ceiling rather than a property of
  the loop, and a refused charge does not underflow it. The ceiling is **run-scoped**:
  `script::Gate` holds one budget and one ledger shared by every `Host::run` that
  admits through it, so the fourth identical refusal across four separate runs is
  the intervention — a per-call ledger would refuse every time and never intervene,
  and a per-body budget would be a fresh ceiling per fan-out item. The budget is
  charged **before** the decode, so a payload refused for its content still costs
  an admission and a refusal loop cannot dodge its own ceiling; a budget refusal is
  *not* recorded against the ledger, because no payload was tried and there is no
  unchanged failure to count. The lock is held across the charge, the decode and
  the ledger update and never across an `.await`. · why: #87 T29 · enforced
  by: `tests/proposal.rs`
  (`repeated_unchanged_failure_reaches_a_finite_intervention`,
  `a_ledger_of_distinct_failures_reaches_its_own_intervention`,
  `new_evidence_does_not_erase_root_spend`, `a_plan_budget_bounds_repair`,
  `repeated_unchanged_failure_reaches_a_finite_intervention_across_runs`,
  `a_plan_budget_bounds_repair_across_runs`) and `tests/sim_proposal.rs`
  (`saturation_over_admit_conserves_the_budget`,
  `seeded_runs_reach_the_declared_disposition_band_20..23`)

- **INV-BOT-95** Untrusted output reaches a run through one step, and that step is
  a `script` block rather than a helper a caller must remember to locate. The estate
  rule is "wired or it does not exist", and the defect this entry records is a
  capability shipped with no production caller: only tests reached the decoder and
  the ledger, so every property INV-BOT-90..94 state held for a boundary nothing
  invoked. `script::admit` is the invocation, and it is a block because a task body
  can only act on a `FlowError`: it enters its step (`scope.enter`), so a refusal
  reads `admit/plan` like every other located failure rather than at the run
  boundary; it returns a typed arm —
  `FlowError::Refused { at, refusal, provenance }` or
  `FlowError::Intervention { at, intervention }` — so the `Provenance` of the
  refused bytes survives to the `Report` instead of being flattened into a
  `Display` string; and both arms are non-retryable, since a payload refused for
  its content is refused however often it is re-read and another attempt is exactly
  the repair an intervention refused. It admits a `Plan` of operation *names* and
  performs nothing: not a fifth verb, and not an untyped plan interpreter. What is
  **not** claimed: `admit` records the refusal, not the admitted plan's execution —
  performing a plan's operations is the caller's job through the existing verbs,
  and a run whose plan is admitted has still performed no external effect, which is
  what `EffectKnowledge` continues to report. · why: #87 T26/T27/T29 (the
  no-production-caller defect) · enforced by: `tests/proposal.rs`
  (`an_injected_instruction_is_refused_at_its_step_on_a_real_run`,
  `a_well_formed_proposal_is_admitted_on_a_real_run`,
  `a_capability_the_run_does_not_hold_is_refused_by_name_on_a_real_run`,
  `repeated_unchanged_failure_reaches_a_finite_intervention_across_runs`,
  `a_plan_budget_bounds_repair_across_runs`,
  `a_resumed_run_reads_back_the_refusal_the_first_run_recorded`) and
  `tests/sim_proposal.rs` (`seeded_runs_reach_the_declared_disposition_band_20..23`,
  `two_tenants_admitting_on_one_host_stay_isolated_band_24..27`,
  `saturation_over_admit_conserves_the_budget`)

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
