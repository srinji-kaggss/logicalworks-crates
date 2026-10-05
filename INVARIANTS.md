# logicalworks-crates — invariants

Read before the first edit. Each entry: rule · why · enforced by. Add an entry in
the same PR as any Director correction or incident fix. Long-form: `AGENTS.md`,
`docs/dependency-doctrine.md`.

## Surfaces and dependencies

- **INV-DEP-1** Three surfaces (`lgwks_std`, `lgwks_bot`, `lgwks_deps`), the
  finished, standalone `lgwks_ast`, and the proc-macro crate `lgwks_macros`: five
  workspace members. Never grow `lgwks_ast`; never add a new owner or top-level
  crate for third-party code. A workspace member or approval owner outside those
  five is refused by name as `UnknownSurface`. · why: #207 · enforced by:
  `lgwks_deps::tests::a_rogue_approval_owner_is_refused_by_name`,
  `lgwks_deps::tests::a_sixth_workspace_member_is_refused_by_name`, and
  `lgwks-deps check .`
- **INV-DEP-2** Every authored external edge is an optional, default-off feature of
  `lgwks_deps` registered in `contract/APPROVED.toml` with owner, capability, source,
  requirement, consumers and kinds (INV-DEP-EDGE-OWNED). Exception: the `scan`
  gate-tool feature is default-on. · enforced by: `lgwks-deps check .`
- **INV-DEP-3** Never depend directly on tokio, futures, async-trait, pollster, syn,
  proc-macro2, regex, uuid, chrono, walkdir, glob, base64, hex, percent-encoding,
  serde_json, ureq, reqwest or ast-grep-*; use the workspace path. One `tokio` edge,
  owned by `lgwks_deps`. · enforced by: `lgwks-deps check .`, but only
  incidentally: the gate holds no forbidden-name list, it refuses an edge the
  register does not approve (`UnregisteredEdge`), so a listed name is kept out by
  the register's contents and by review, not refused by name
- **INV-DEP-4** `lgwks_std` never routes through `lgwks_deps` (cycle). · enforced
  by: Cargo's own dependency-cycle refusal at resolve time; a build-graph fact
  with no gate lane of its own
- **INV-DEP-5** Never widen consumers or edit a pinned version to clear a gate
  refusal. · enforced by: human review only; no machine observes it
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
  `tests/it/origin_binding.rs`, `tests/it/sim_origin.rs`, and
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
  significant. · why: #158 A2 · enforced by: `tests/it/feature_policy.rs`,
  `tests/it/sim_dependency_policy.rs`, and
  `lgwks_deps::tests::a_class_only_registry_approval_admits_crates_io_only`
- **INV-DEP-14** Cargo package identity is byte-exact: an approval admits an
  observed package name only when it is that name, or a name the entry lists in
  its explicit `aliases`. There is no implicit `-`/`_` fold or case fold, so two
  distinct packages whose spellings fold alike cannot share one authority. An
  alias names exactly one package (collision-checked at load, including against
  another package's real name), and a `package =` rename stays a local spelling
  of the upstream identity rather than a second one. · why: #158 A2 · enforced by:
  `tests/it/identity_binding.rs`,
  `contract::tests::lookup_is_exact_and_only_an_explicit_alias_is_tolerated`,
  `contract::tests::aliases_collide_rather_than_share_authority`
- **INV-DEP-15** A `check` receipt binds the subject root, the contract identity
  and schema version (a stable digest of the register text), the exact metadata
  subject (a stable digest over every direct edge's identity), the policy mode
  (`--contract` diagnosis versus committed enforcement) and the assurance scope;
  the `--json` form exposes the same under stable keys. The digest is an identity
  fingerprint, not an adversarial integrity claim. · why: #158 A6 · enforced by:
  `tests/it/check_cli.rs` (`the_human_receipt_binds_contract_subject_and_mode`,
  `the_json_receipt_has_stable_identity_fields`,
  `the_receipt_changes_when_its_subject_changes`)

## lgwks_bot — durable execution

Each of these was a shipped defect. Treat the list as the spec.

- **INV-BOT-141** A fixed-model authoring cell varies the profile and nothing
  else. A persona is a fixed preamble prepended to one prompt skeleton, and the
  task, the API sheet, the hidden oracle, the repair budget and the
  `sandbox-exec` closed-book profile are byte-identical across profiles, so any
  difference between two cells of one `(model, api, task)` is attributable to
  the instruction rather than to the inputs. The persona text is fixed rather
  than generated, because a per-trial preamble would be a second uncontrolled
  variable and the comparison would measure the draw. The profile is part of the
  trial id, so two profiles of one cell cannot share a cargo `-C metadata` and
  have one trial's oracle binary stand in for another's — the same defect the
  per-trial package name exists to prevent. **What this does not claim:** a fixed
  model reading a persona is not a novice, an expert under pressure, or an
  anxious user. The profile axis measures how one fixed model behaves under five
  instructions; it is not a measurement of human authorability and no human is
  evaluated. · why: #87 step 7, #152's learnability row · enforced by:
  `bench/ai-authoring/run.py` (`PROFILES`, `full_prompt`, and the trial-id
  construction), whose `--profiles` argument rejects an unknown name rather than
  silently running a cell with no preamble, and by the committed profile run
  under `bench/ai-authoring/runs/` whose `summary.json` carries
  `protocol.profiles` and `protocol.profile_text_is_fixed` beside the numbers
- **INV-BOT-142** An oracle's cost is measured, and a metric that was not
  measured says so. The oracle runs under `/usr/bin/time -l`, so
  `oracle_wall_ms` is the wall time of the invocation that runs the oracle's
  test binary — after the compile, so it is the oracle's own cost rather than a
  build's — and `oracle_peak_rss_bytes` is that process's maximum resident set
  size in bytes. `null` means *not measured* and never *measured as zero*:
  collapsing those two would let a host without the timer report every trial as
  using no memory at all. The RSS parser reads both field orders the flag takes,
  because macOS prints `<value>  maximum resident set size` and GNU prints
  `Maximum resident set size (kbytes): <value>`; matching on either one loses
  the metric on the other host, and GNU's kibibytes are scaled to bytes rather
  than reported 1024 times too small under a name that says bytes. · why: #87
  step 7 (runtime cost axis) · enforced by: `bench/ai-authoring/run.py`
  (`run_timed_cargo`, `parse_peak_rss`), verified against both real spellings and
  against a near-miss label (`peak memory footprint`) that must read as absent
- **INV-BOT-143** A trial that never compiled has not demonstrated cleanup. The
  per-trial `cleanup_ok` flag is read from the task's own drop clause — the
  oracle test that says a dropped future left nothing live — and a crate that
  did not compile reports `false` rather than a cleanup result, because "the
  drop test did not run" and "cleanup was fine" are different facts and the
  first is what a failed build produces. The clause is named per task rather than
  as a shape, so a task whose oracle renames it stops reporting a cleanup
  verdict instead of silently reporting the wrong one. · why: #87 step 7 (safety
  and recovery axis) · enforced by: `bench/ai-authoring/run.py`
  (`DROP_CLAUSE_BY_TASK`, `cleanup_ok`), and by the mutants run, where
  `mutant-pipeline` is the only cell with `cleanup_ok: false` because it is the
  only one that fails its drop clause
- **INV-BOT-144** A held-out task whose correct solution the harness cannot
  express on one API surface is not asked on it. `recovery` has no old-API cell,
  because the old `lgwks_bot::rt` surface has no durable run store, no
  `remember`, no run identity and no resume, so it keeps no record of a completed
  unit to consult and cannot express "finish the work without redoing a completed
  unit" at all. The omission is recorded in every run's
  `protocol.skipped_cells` with its reason, so it reads as a decision rather
  than as a missing row, and a fixture hand-rolled to imitate the missing store
  would be measuring that fixture rather than the crate. · why: #87 step 7 —
  an absent arm presented as an untested one is the failure this names · enforced
  by: `bench/ai-authoring/run.py` (`NEW_ONLY_TASKS`, `NEW_ONLY_TASK_WHY`, and the
  job filter), which prints the skipped cells and writes them into the run's
  protocol block

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
  `tests/it/sim_clock.rs` (`the_same_seed_replays_the_same_clock_trace`,
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
  `tests/it/inspect_contract.rs` (`the_snapshot_and_the_admission_decision_agree`,
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
  by: `tests/it/ready.rs` (`a_failure_before_ready_reaches_the_dependant_and_the_report`,
  `a_duplicate_ready_signal_is_refused_and_releases_nothing`,
  `a_stale_generation_is_refused_and_releases_nothing`,
  `a_restarted_service_arms_a_new_readiness`,
  `a_failure_after_ready_cancels_every_dependant_still_running`,
  `a_shutdown_is_a_stop_and_not_a_failure`,
  `a_wait_is_budgeted_cancellable_and_immediate_when_already_released`,
  `dependants_are_capped_at_the_declared_bound`,
  `no_readiness_path_sleeps_or_polls`) and `tests/it/sim_ready.rs`
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
  T18 / LC-09 · enforced by: `tests/it/ready.rs::real_process`
  (`a_child_printing_its_address_releases_the_dependants`,
  `a_child_that_dies_without_printing_never_releases_the_dependants`,
  `a_child_that_dies_after_announcing_stops_the_dependants`) and
  `tests/it/ready.rs::no_readiness_path_sleeps_or_polls`, which asserts against the
  module's own source that neither the wait nor the observer arms one.

- **INV-BOT-32** A reach for authority is checked at the step that reaches, and
  the refusal carries the whole shortfall. `Scope::require` names every capability
  the step needs and this run's authority — the host's grant plus any repair
  delta, checked together and never one replacing the other — does not cover, as
  one `FlowError::Blocked` carrying a complete `Deficit`. A run is therefore
  `Blocked` rather than `Failed`, which is the distinction a repair acts on: the
  host was willing and the authority was missing. The report's `needs` and its
  `repair` ticket are both derived from that one `Deficit`, so they cannot
  disagree about what the run was missing, and a task that reaches in its *first*
  step may still declare at its admission boundary with `Task::requiring` and be
  refused before anything runs. · why: #87 step 3 (T23) · enforced by:
  `tests/it/repair.rs` (`a_run_short_of_authority_is_blocked_naming_every_need`,
  `a_blocked_run_leaves_its_finished_analysis_recorded`,
  `a_host_without_a_ledger_refuses_every_repair`,
  `the_journey_declares_no_admission_boundary_needs`)
- **INV-BOT-33** A repair authorizes one run, once, for exactly the needs its
  ticket names. `Host::repair` resumes under the run's own id, so every step
  recorded before the block replays without its body being polled and only the
  blocked remainder runs; the analysis is not paid for twice. A grant that does
  not cover the ticket's needs is `NotAuthorized` and one that reaches outside
  them is `OverWide`, both refused before any authority is applied, so the caller's
  belief about what was granted cannot exceed what was asked. The host's own
  grant is never widened: the next run on that host is still blocked. A host with
  no repair ledger refuses every repair, because there is no epoch, root budget or
  applied-ticket set to decide one against. · why: #87 step 3 (T23) · enforced by:
  `tests/it/repair.rs` (`a_repair_runs_the_blocked_remainder_without_rerunning_the_analysis`,
  `a_repair_widens_one_run_and_not_the_host`,
  `a_denied_repair_leaves_the_run_blocked_with_its_authority_unchanged`,
  `an_over_wide_grant_is_refused_rather_than_narrowed`) and
  `tests/it/sim_repair.rs::seeded_orders_reach_the_same_state_band_*`
- **INV-BOT-34** A repair ticket is a report, never a grant, and its identity is
  its content. `RepairTicket::stamp` hashes the run, the tenant, the epoch and the
  *sorted* needs, so a caller that rebuilds a ticket from the same facts produces
  the same identity and two spellings of one request are one ticket. A ticket
  delivered twice is refused `AlreadyApplied` and a ticket from an epoch the run
  has moved past is refused `StaleEpoch`; the ledger's decide-and-write is one
  ordered step on its own thread, so "applied once" is a fact about bytes rather
  than about the order two threads happened to run in. A refused repair charges
  nothing, mints no epoch and leaves the ledger byte-identical. · why: #87 step 3
  (T24) · enforced by: `tests/it/repair.rs`
  (`the_same_ticket_delivered_twice_applies_once`,
  `a_ticket_from_an_older_epoch_is_refused_as_stale`,
  `a_denied_repair_costs_nothing`) and
  `tests/it/sim_repair.rs::seeded_orders_reach_the_same_state_band_*`
- **INV-BOT-35** A run's root budget is carried in its own ledger, is charged by
  every attempt including a repair, and is never refilled by one. The counters
  are cumulative read-modify-write state rather than a replayed step record, so
  they get their own chained file over the shared frame grammar and the shared
  storage-owner thread (INV-BOT-51). A budget that is spent refuses the next
  attempt with `BudgetSpent`, which is what makes a permanent refusal plus
  repeated `NotApplied` reach a finite typed answer rather than an unbounded retry
  loop, and an authorized repair is a distinct event that *consumes* budget rather
  than resetting it (T13). Two tenants over one directory keep separate ledgers,
  separate run ids and separate epochs, and one tenant's ticket is refused by the
  other tenant's host. · why: #87 step 3 (T13, T24) · enforced by:
  `tests/it/repair.rs` (`the_root_budget_stays_charged_across_repair_and_resume`) and
  `tests/it/sim_repair.rs` (`seeded_orders_reach_the_same_state_band_*`,
  `tenants_keep_their_own_tickets_and_budgets_band_*`,
  `saturation_applies_each_ticket_once_band_*`,
  `every_repair_charges_the_root_budget_once`,
  `a_spent_budget_refuses_every_later_attempt`,
  `a_host_spent_on_one_run_still_repairs_the_next`,
  `a_bounded_sweep_repairs_every_ticket_once`, and the opt-in
  `the_declared_repair_tiers_are_measured`)
- **INV-BOT-36** A refusal is one arm, not one shape, and the repair door is
  orderable: which arm a decision hits, what it charges and what it leaves behind
  are each observable rather than inferred from the run's final counters. A grant
  that is both short and wide reports the missing half first, so the half a caller
  must fix is the half they are told; the over-wide arm then names every capability
  the ticket never asked for — shipped or custom — at every need width, because the
  check walks the grant rather than a list of candidates. A ticket naming another
  tenant's run is refused by the ticket's own tenant check, before admission and
  before the ledger, so the asking tenant's ledger never gains an entry for a run it
  does not own. Each arm leaves the ledger **byte-identical**, which is a claim
  about the file and is measured on the file rather than on a handle agreeing with
  itself. A refused repair and a refused attempt are both exactly nothing: no
  budget, no epoch, no step, and the counters the ceiling was measured against never
  move afterwards. · why: #87 step 3 (T24) — the arms were stated by the type but
  exercised only through the order family, which reads an endpoint and could not
  say which arm produced it · enforced by: `tests/it/sim_repair.rs`
  (`a_mixed_decision_order_pins_each_arm`, `a_custom_capability_is_refused_at_every_width`,
  `a_ticket_never_names_another_tenants_run`) and `tests/it/repair.rs`
  (`an_over_wide_grant_is_refused_rather_than_narrowed`,
  `a_custom_capability_outside_the_ticket_is_refused`,
  `a_denied_repair_costs_nothing`)
- **INV-BOT-37** A run's durable state is read back from the file, not from a
  handle. The repair's replay rests on bytes a *second* host opened: the recorded
  analysis is not re-polled and the publication runs once, on the run that asked
  for it, with the first host dropped entirely before the second is built. The
  ledger's counters replay to exactly what the live write left — tenant, attempts,
  spend, epoch and applied-ticket count, for every run on the chain — so which
  handle a caller read cannot decide what a run holds. A budget refusal drops the
  step store's handle with the refused append outstanding (INV-BOT-50), so recovery
  from a spent budget is *through a reopen* and is claimed only in that form; the
  other runs sharing the host keep their own budgets and still close. · why: #87
  step 3 (T23, T13) — a replay served from a live handle would pass every assertion
  about poll counts while proving nothing about the store · enforced by:
  `tests/it/sim_repair.rs` (`a_repaired_run_survives_a_reopened_host`,
  `a_reopen_reads_back_the_charged_budget`,
  `a_host_spent_on_one_run_still_repairs_the_next`) and
  `tests/it/repair.rs::a_blocked_run_leaves_its_finished_analysis_recorded`
- **INV-BOT-38** A first-step reach is refused at the admission boundary with the
  whole shortfall in one pass, and costs nothing to name. A task that reaches in
  its first step declares its need with `Task::requiring` and is `Blocked` before
  its body: no step polled, no permit taken, no record written and no root attempt
  charged — so the report's `needs` and the ticket's needs are the *whole* of what a
  repair would grant, and there is no replay to buy. The complementary journey,
  which reaches after its analysis, still costs exactly one analysis at every need
  width, and the ticket names that run's shortfall in the order the reach named it.
  · why: #87 step 3 (T23) — the blunt form had no end-to-end evidence at all, and
  the one-need case is the only width where the ticket's grant is exactly the run's
  authority · enforced by: `tests/it/sim_repair.rs`
  (`the_step_that_reaches_is_the_step_that_blocks`, `a_wide_need_set_costs_one_analysis`)
  and `tests/it/repair.rs` (`a_run_short_of_authority_is_blocked_naming_every_need`,
  `the_journey_declares_no_admission_boundary_needs`)

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
  findings · enforced by: `tests/it/process_ownership.rs`, `tests/it/rt_process.rs`,
  and `rt::supervise::tests`
- **INV-BOT-13** Cleanup that remains pending after its process task ends transfers
  its group identity and admission permit to the supervisor's bounded cleanup
  owner; present/error observations retain both, and only observed absence may
  release capacity and emit an attributed terminal receipt. · why: #143 R10
  ownerless-cleanup finding · enforced by: `rt::supervise::tests` and
  `tests/it/rt_process.rs`
- **INV-BOT-110** A length-framed record a child's output carries is read through
  the crate's one frame grammar (`journal::frame`, INV-BOT-51), so "what a torn
  tail is" has one answer across the file stores and a subprocess's streams. A
  record is `FrameRead::Frame` only when its prefix named the bytes that
  followed; the two truncations, a malformed prefix and the caller's ceiling are
  refusals that carry no payload, and `payload()` returns `None` for every one of
  them, so there is no path from a truncated output to bytes a caller decodes.
  The payload ceiling is charged from the prefix *before* a payload is read, so a
  stream cannot ask for an allocation by claiming a large record. **Malformed is
  not "too big for what is left":** `MalformedPrefix` is exactly the two lengths
  no writer of this grammar produces — `declared == 0`, or `declared > ceiling`
  — and a *legal* declared length that merely exceeds the room remaining after
  earlier records is `CeilingReached`, because a well-formed record with nowhere
  to go is a bound, not rot. Each payload is read into its own exactly-sized
  `Vec`, allocated only after the charge and then moved into the record, so no
  byte is copied twice and a truncated partial is that same `Vec` truncated in
  place. A caller reads its child's output through
  `CapturedStream::frames(ceiling)`, the one door from a `ProcessRun` to the
  grammar, and the production door to that reading is the sys domain's own
  verbs: a `Process` built with `Process::frame_stdout` reports the framed
  reading on the `ProcessState` its `Observe`, `Execute` and `Query` calls
  return, so the grammar is reached from the run path rather than only from a
  test. · why: #87 acceptance row T05 (LC-02/11) · enforced by:
  `tests/it/sys_process_binding.rs`
  (`a_framed_record_cut_off_mid_frame_is_a_typed_refusal`,
  `a_framed_stream_that_ends_cleanly_is_complete`,
  `a_legal_record_without_room_is_the_ceiling_and_rot_is_still_refused`,
  `a_prefix_past_the_ceiling_is_refused_before_it_is_allocated`,
  `the_execute_verb_reports_stdout_frames_two_records_and_a_cut_third`,
  `the_execute_verb_reports_the_capture_ceiling_when_the_child_overruns_it`) and
  `tests/it/sim_process_output.rs` (`cuts_are_refused_never_decoded_band_00`,
  `cuts_are_refused_never_decoded_band_01`,
  `room_without_a_record_is_the_ceiling_band_00`,
  `room_without_a_record_is_the_ceiling_band_01`,
  `verb_framed_reads_agree_with_the_model_band_00`,
  `verb_framed_reads_agree_with_the_model_band_01`)
- **INV-BOT-114** A capture's own cut is reported as the capture's ceiling, never
  as the child's truncation and never as a clean end — but only for the endings
  the cut could have decided. When `CapturedStream::truncated()` is true the
  retained bytes are a prefix **the capture** cut, so they are not the child's
  whole output however they happen to end — including the case where they end
  exactly on a record boundary, which is the case a naive reader reports as a
  complete stream. A framed read through `CapturedStream::frames` therefore
  overrides exactly `EndOfStream`, `TruncatedPrefix` and `TruncatedPayload` with
  `FrameRead::CeilingReached { ceiling: <the capture's retained capacity> }`, and
  `is_complete()` is `false` for each. The two endings it does **not** override
  are the two the cut cannot have reached: a `MalformedPrefix` was decided from a
  whole prefix the capture did retain (declared `0`, or a length past the
  reader's ceiling), so it is rot in the child's output and stands; and a
  `CeilingReached` the *reader* reached stopped the pass before the cut mattered,
  so it names the reader's ceiling and stands. Replacing either would be
  fail-open in the same direction: a caller looking for corruption would be handed
  a bound it never hit, and a caller looking for its own bound would be told
  something larger stopped it. The two ceilings are separate facts and are never
  conflated: an untruncated capture reports the child's own truncation, and a
  reader ceiling reached over an untruncated capture reports the reader's. · why:
  #87 acceptance row T05 (LC-02/11), the capture-ceiling/confusable-child-truncation
  defect and the fail-open override the reviewer found in it · enforced by:
  `tests/it/sys_process_binding.rs`
  (`a_capture_ceiling_ends_the_framed_read_rather_than_the_child`,
  `an_untruncated_capture_reports_the_child_own_truncation`,
  `a_rot_prefix_before_the_capture_cut_stays_refused`,
  `a_reader_ceiling_over_a_truncated_capture_is_the_readers_own`) and
  `tests/it/sim_process_output.rs`
  (`capture_cuts_end_at_the_capture_ceiling_band_00`,
  `capture_cuts_end_at_the_capture_ceiling_band_01`,
  `capture_cuts_saturate_at_the_declared_tiers`,
  `rot_before_the_capture_cut_is_the_ending_band_00`,
  `rot_before_the_capture_cut_is_the_ending_band_01`)
- **INV-BOT-115** The frame grammar a child's output carries is reached from the
  sys domain's own verbs, not only from a reader a caller plumbs by hand. A
  `Process` built with `Process::frame_stdout` reports each run's stdout as
  `ProcessState::stdout_frames`, a reading of the same bytes the lossy
  `ProcessState::stdout` decodes, so a binary record round-trips byte-exact where
  the text view cannot, while a domain built without it reports `None` and an
  unchanged `stdout`. The production door is the `Observe`, `Execute` and `Query`
  verbs on `Process`: each calls the one private dispatch method, which runs the
  process once, and `ProcessState::from_run` fills the framed reading through
  `CapturedStream::frames` — so the capability `INV-BOT-110` and `INV-BOT-114`
  describe has a caller on the real run path rather than only in a test. · why:
  the wired-or-it-does-not-exist defect for T05 (INV-BOT-110 and INV-BOT-114
  shipped with zero production callers) · enforced by: `tests/it/sys_process_binding.rs`
  (`the_execute_verb_reports_stdout_frames_two_records_and_a_cut_third`,
  `the_execute_verb_reports_the_capture_ceiling_when_the_child_overruns_it`,
  `a_domain_without_frame_stdout_reports_no_frames_and_unchanged_stdout`,
  `a_binary_record_round_trips_through_frames_while_stdout_is_lossy`) and
  `tests/it/sim_process_output.rs`
  (`verb_framed_reads_agree_with_the_model_band_00`,
  `verb_framed_reads_agree_with_the_model_band_01`,
  `verb_two_tenants_never_cross_band_00`,
  `verb_two_tenants_never_cross_band_01`)
- **INV-BOT-111** A captured stream's four facts — the retained head at the
  ceiling, the retained capacity, the exact total and the truncation flag — are
  reported from one drain that keeps reading past its ceiling, so a flooding child
  against a slow reader is bounded by its declared ceiling rather than by a pipe
  buffer, and the total is still exact when the output was truncated. A reader
  that waits for the child to exit before reading would deadlock on such a child,
  so the deadline-bounded variant is the control that distinguishes a concurrent
  drain from a sequential one. · why: #87 acceptance row T05 (LC-02/11) ·
  enforced by: `tests/it/sys_process_binding.rs`
  (`a_flooding_child_against_a_slow_reader_stays_within_its_ceiling`,
  `a_flooding_child_is_drained_while_it_runs_not_after_it_exits`) and
  `tests/it/sim_process_output.rs` (`a_seeded_flood_stays_bounded_on_one_worker_band_00`,
  `a_seeded_flood_stays_bounded_on_one_worker_band_01`)
- **INV-BOT-112** `CleanupReceipt::CleanupConfirmed` claims that every process
  still in the supervised group when the group was last observed is gone — an
  observation of `killpg(group, 0)`. It does **not** claim that no process the
  supervisor started is still running: a descendant that called `setsid` has left
  the group by construction, so its survival is not a counterexample. A receipt
  claiming the stronger thing would need a kernel job object or a cgroup, neither
  of which this crate has, and the bound is stated on the receipt itself rather
  than left to be inferred from a green test. · why: #87 acceptance row T21
  (LC-10), observed against a real `setsid` escape · enforced by:
  `tests/process_escape.rs` (`a_session_escape_is_not_reported_as_complete_tree_cleanup`,
  `cleanup_never_signals_a_process_outside_the_supervisors_group`)
- **INV-BOT-113** A callback that never reaches an await point is observable only
  from outside the process that runs it, and the observation is **detection, not
  preemption**. A thread watchdog shares the fate of the executor it watches, so
  the row's oracle is a separately timed child process that the parent kills with
  a real `SIGKILL`; nothing in this crate can stop a poll in flight, and
  `Supervisor::shutdown` says so itself. · why: #87 acceptance row T03 (DX-05,
  LC-08) · enforced by: `tests/t03_non_yielding.rs`
  (`a_non_yielding_callback_is_detected_from_outside_and_not_preempted`,
  `the_same_front_door_admits_a_callback_that_does_yield`)
- **INV-BOT-14** Shipped journals refuse appends and opens beyond their explicit
  event/byte ceilings without deleting or partially replaying committed or
  unresolved evidence. · why: #143 R06 · enforced by:
  `journal::tests::memory_journal_refuses_history_beyond_its_declared_limit`,
  `file::tests::scanning_refuses_a_complete_event_beyond_the_limit`,
  `file::tests::batch_admission_refuses_history_over_the_event_limit_without_writing`,
  and `file::tests::open_refuses_an_over_limit_file_without_truncating_it`

- **INV-BOT-55** A recorded step value is only replayable under the definition
  that produced it. Every run-store record carries a `DefinitionIdentity` — the
  task name, a declared definition revision, the input digest, a declared
  durable-value schema id and the count of durable steps — and a resume whose
  declared identity disagrees on any axis is refused before admission with a
  typed `FlowError::Incompatible` naming the axis, so no step body is polled and
  no record is written. Only an identity the *caller declared* is compared:
  the host's own derivation from the run id is what the run was recorded under,
  so comparing it would fence nothing and would refuse every durable step that
  declared nothing. The identity the steps write and look up under is the
  recorded one, which is why a compatible resume finds its own records. The
  check is the *declared* identity against the *recorded* one; an earlier
  revision compared a run's records against themselves and every drift passed.
  A refusal leaves the store byte-identical, and it is typed down to the axis's
  own values: `FlowError::Incompatible` carries the exact `Drift`, naming for
  each axis the two revisions, digests, step counts or schema ids that
  disagreed, so a caller learns not merely which axis moved but against what.
  A store that cannot read its own records is refused as itself and never as a
  disagreement: its read answers `Err`, the step's compatibility check
  propagates the store's own typed error rather than a `false`, and the host's
  admission pre-flight refuses every non-drift store error as `FlowError::Store`
  carrying the store's `StoreError` — so a device fault reaches the caller as a
  device fault and never as a claim that the definition changed (INV-BOT-7).
  "Order" here is the count of declared durable steps, the only form of order a
  step key cannot see: two adjacent steps permuted inside the same shape keep
  every path and every recorded value, so nothing there is a drift. The store's
  format version is `\x02` and a `\x01` record is refused at open as
  `StoreError::FormatVersion { found, expected }` naming both versions rather
  than migrated, because inventing that migration would make every pre-version
  resume look compatible rather than unprovable. · why: T15 · enforced by:
  `tests/it/sim_replay_drift.rs` (`drift_kinds_are_refused_typed_band_00..03`,
  `compatible_resume_replays_without_a_new_request_band_04..07`,
  `tenants_drift_independently_band_08..10`, `every_axis_is_distinguishable`,
  `every_axis_is_refused_with_its_exact_drift_band_20..21`,
  `a_refusal_leaves_the_store_byte_identical_band_12..13`,
  `an_unreadable_store_is_refused_as_itself_band_18..19`,
  `same_seed_same_trace_hash_band_14..15`), `tests/it/store_read_failure.rs`
  (`an_unreadable_store_is_refused_as_itself_and_not_as_a_drift`,
  `the_fault_is_one_shot_and_the_step_after_it_replays`,
  `a_compatible_resume_still_replays_after_no_fault`), and `tests/task_resume.rs`
  (`a_pre_version_store_is_refused_naming_both_versions`,
  `a_foreign_file_is_still_refused_as_not_a_store`), and
  `tests/it/sim_store_faults.rs`
  (`only_the_current_format_is_admitted_band_04..07`)

- **INV-BOT-59** The durable run store's refusals reach the caller as
  themselves. A store that cannot read its own records answers `Err`, never a
  `false` that a caller would report as "these records were written under a
  different definition", and never a rendered `Failed` string. `Records::agrees`
  returns `Result<(), FlowError>` and propagates the store's error unchanged, so
  the step's compatibility check cannot turn a read failure into a drift; a
  resumed step that cannot be read is refused as `FlowError::Store` wrapping the
  store's own `StoreError`, and the host's admission pre-flight refuses every
  non-drift store error the same way. The store's own `From<StoreError>` keeps a
  genuine drift as the flow's typed `FlowError::Incompatible` carrying its
  `Drift` axis, so the one fault an operator must see is never indistinguishable
  from a device that failed to answer. · why: INV-BOT-7, T15 (R1) · enforced by:
  `tests/it/store_read_failure.rs`
  (`an_unreadable_store_is_refused_as_itself_and_not_as_a_drift`,
  `the_fault_is_one_shot_and_the_step_after_it_replays`,
  `a_compatible_resume_still_replays_after_no_fault`) and
  `tests/it/sim_replay_drift.rs`
  (`an_unreadable_store_is_refused_as_itself_band_18..19`) and
  `tests/it/sim_store_faults.rs`
  (`read_fault_reaches_the_report_as_the_store_band_00..03`,
  `same_seed_same_trace_hash_band_08..09`)

- **INV-BOT-56** An owner epoch is a fact on the disk, not a constant each
  process chooses for itself. `Broker::register` starts an environment at
  generation 1 whatever the journal holds, so a process that adopts a journal
  another worker wrote would mint warrants for a generation that worker had
  already been replaced past — two processes internally consistent and jointly
  wrong, which is the state in which every fence passes while fencing nothing.
  `Broker::adopt` reads the generation the journal's own committed history was
  written at and claims the one after it. Its three refusals are distinct and
  none of them is a generation of 1: a journal that cannot be read, a journal
  describing another environment, and a journal with no committed history to take
  over. The advisory writer fence is a *separate* mechanism and neither
  substitutes for the other: the fence stops a second writer, the epoch stops a
  second claimer, and a process can hold a live descriptor on a journal it no
  longer owns, because nothing revokes an open descriptor. · why: T16 ·
  enforced by:
  `tests/owner_epoch_takeover.rs`
  (`an_old_worker_returning_after_a_takeover_cannot_settle_or_authorize`,
  `a_warrant_from_the_previous_generation_is_superseded`,
  `a_generation_the_broker_never_issued_is_not_a_supersession`,
  `a_generation_and_a_tail_are_two_fences_and_both_answer`,
  `an_acknowledged_position_reads_back_identically_after_a_reopen`,
  `the_current_generation_may_still_settle_its_own_attempt`) and
  `tests/it/sim_epoch_identity.rs`
  (`seeded_takeover_orders_keep_the_generation_on_the_disk_band_04..07`,
  `same_seed_same_trace_hash_band_08..09`)

- **INV-BOT-116** A test's store directory is never a name two runs can share, and
  no run leaves one behind. A directory a test builds under the system temp root
  from a *fixed* name is a second ledger the test does not own: an earlier run on
  the same host leaves records there, and the next run reopens them rather than
  writing its own, so a format or budget refusal can be inherited from a world
  nobody in this run created. Every such directory comes from the one
  `shared::Scratch::new(tag)` guard — random hex in the name, `remove_dir_all` on
  drop — held for the whole test, so a run cannot see another run's files and a
  finished run leaves nothing to be inherited. The guard is the ephemerality rule
  (INV-DEP-6) applied to the durable stores rather than to an in-memory value:
  state that outlives a run belongs to a directory that dies with it. The store's
  own format refusal stays exactly as strict, because the fix is isolation and not
  a loosened `check_format_version` — a `\x01` store inside *this* run's own
  directory is still `FormatVersion { found, expected }` (INV-BOT-55). · why: the
  seven `tests/it/sim_repair.rs` arms that failed with
  `FormatVersion { found: 1, expected: 2 }` on a branch that had never written a
  `\x01` record · enforced by: `tests/it/sim_repair.rs`
  (`the_step_that_reaches_is_the_step_that_blocks`,
  `a_wide_need_set_costs_one_analysis`,
  `a_ticket_never_names_another_tenants_run`,
  `a_custom_capability_is_refused_at_every_width`,
  `a_repaired_run_survives_a_reopened_host`,
  `a_host_spent_on_one_run_still_repairs_the_next`,
  `a_bounded_sweep_repairs_every_ticket_once`) and
  `tests/task_resume.rs::a_pre_version_store_is_refused_naming_both_versions`

- **INV-BOT-57** Each boundary of the durable ladder recovers its own answer, and
  recovery is itself a window the crashing process can do damage in. A kill
  after the intent ack and before the preparation recovers `Prepared` and
  nothing uncertain — nothing was handed over, so refusing to retry it would
  strand an effect that provably never left the process. A kill after the
  outcome and before the verification recovers `Applied` and nothing uncertain:
  the outcome is the fact and the verification is an attestation made afterwards,
  so treating the missing attestation as an unknown would offer to dispatch the
  effect twice. A kill *during* recovery leaves the file byte-identical, because
  a replay that repairs a torn tail is writing and a reader that wrote would turn
  an interrupted append — which was never anyone's answer — into a committed one.
  The ladder makes the duplicate settlement unrepresentable rather than merely
  discouraged. · why: T14 · enforced by:
  `tests/durable_crash_observation.rs`
  (`a_kill_after_the_intent_ack_and_before_the_dispatch_recovers_as_prepared`,
  `a_kill_after_the_response_and_before_the_receipt_recovers_the_outcome`,
  `a_kill_during_recovery_leaves_the_journal_exactly_as_it_was`,
  and the pre-existing `a_real_kill_mid_append_leaves_no_duplicate_and_no_lost_receipt`)

- **INV-BOT-58** Each identity field is refused by the check that is *about* it,
  and the refusal says which. Seven deliveries — one correct and six differing
  in exactly one field — are refused as six distinct typed variants, because
  asserting "an error" would be satisfied by an earlier check that happens to
  catch the evidence first. An `ActionId` is derived from the bot's name, its
  position and its action's domain, **not** from the run, the flow revision or
  the environment, so a key that gets one of those three wrong still names a
  declared action and is refused one level later as `ActionNotDeclared`. None of
  those four is `NoSuchWork`: that variant means "no held effect at this
  address", and telling a caller that about work it *does* hold is the confusion
  the variant exists to prevent. The arms that do reach the finer
  classification are `EvidenceSuperseded` for a changed payload, which names
  both bindings, and `EffectUnrecorded` for a generation the journal holds no
  ladder for. A refused delivery moves no byte of a real `FileJournal`: the
  ordering ladder is per key, so a distinct identity is legitimately at the foot
  of its own, and what refuses is a rung that cannot follow what is committed for
  that key. A checked counter's exhaustion answers `None` rather than wrapping
  onto the first identity its sequence issued. · why: T12 · enforced by:
  `tests/it/wrong_identity_evidence.rs`
  (`every_wrong_identity_field_is_refused_and_the_correct_one_is_not`,
  `a_duplicate_is_idempotent_and_a_contradiction_is_refused`,
  `a_settlement_carrying_another_runs_identity_is_refused`,
  `a_refused_settlement_leaves_a_real_file_journal_byte_identical`,
  `a_checked_counter_exhaustion_never_aliases_an_issued_identity`,
  `a_journal_position_carries_a_head_so_a_wrapped_sequence_is_detectable`) and
  `tests/it/sim_epoch_identity.rs`
  (`every_identity_field_is_refused_by_its_own_check_band_00..03`)

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
  enforced by: `tests/it/similarity_evidence_contract.rs`
  (`cosine_trait_impl_stays_inside_the_documented_unit_interval`,
  `a_refused_component_is_not_accepted_at_threshold_zero`,
  `all_zero_weight_refuses_regardless_of_threshold`,
  `typed_refusals_carry_component_identity_through_composition`,
  `bounded_jaccard_refuses_before_the_quadratic_scan`,
  `budget_refusal_precedes_amplification_and_is_measurable`,
  `the_edit_budget_charges_the_normalized_unit_not_the_raw_scalar_count`,
  `the_heuristic_path_score_is_never_an_exact_match_proof`), `similarity.rs`
  (`every_evidence_error_variant_is_exercised_by_a_test`), and
  `tests/it/sim_similarity_sweep.rs` (`the_same_seed_replays_to_the_same_trace`)
- **INV-STD-SIM-1** The documented `Geometry::score` accepts both `[f64; 4]`
  and `BoundingBox`. · why: #160 S3 · enforced by:
  `tests/it/similarity_public_api.rs`
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
  and `tests/it/sim_hex.rs`
  (`a_seeded_payload_round_trips_through_encode_and_decode`,
  `decode_into_requires_the_exact_destination_length`,
  `a_refused_decode_into_never_writes_a_prefix_of_the_destination`,
  `a_non_digit_is_reported_at_its_first_exact_offset`,
  `an_odd_length_is_refused_before_any_destination_width_check`,
  `the_empty_and_single_byte_payloads_are_exact_endpoints`,
  `uppercase_and_lowercase_spellings_decode_to_the_same_bytes`,
  `refusals_report_their_arm_and_both_of_their_offsets`,
  `constant_payloads_are_exact_at_every_boundary_length`,
  `every_truncated_prefix_is_refused_or_is_a_shorter_value`,
  `the_wide_payload_boundary_is_exercised_at_its_declared_length`,
  `the_same_seed_replays_to_the_same_hex_trace`,
  `distinct_hex_seeds_diverge_in_their_trace`)
- **INV-ENCODING-1** Percent escape errors report original-input byte offsets;
  UTF-8 errors name offsets in decoded bytes and never present them as source
  coordinates. · enforced by: `encoding::tests::percent_refuses_a_non_hex_escape`,
  `encoding::tests::percent_refuses_escapes_that_decode_to_invalid_utf8`,
  and `tests/it/sim_encoding.rs`
  (`a_seeded_component_round_trips_through_percent_encoding`,
  `multi_byte_scalars_are_escaped_one_byte_at_a_time`,
  `a_malformed_escape_is_reported_at_its_original_input_offset`,
  `a_utf8_failure_is_reported_in_decoded_bytes_not_source_bytes`,
  `a_truncated_escape_names_the_percent_in_source_bytes`,
  `lower_and_upper_case_escape_digits_decode_to_the_same_byte`,
  `the_empty_and_single_byte_payloads_are_exact_endpoints`,
  `every_truncated_prefix_is_refused_or_is_shorter`,
  `every_reported_coordinate_is_a_real_position_in_its_own_space`,
  `a_seeded_payload_round_trips_through_base64`,
  `every_truncated_base64_prefix_is_refused_on_its_length`,
  `a_corrupted_base64_character_is_refused_at_its_offset`,
  `the_wide_boundary_renders_and_decodes_at_its_declared_length`,
  `refusals_fold_their_arm_and_both_of_their_coordinates`,
  `the_same_seed_replays_to_the_same_encoding_trace`,
  `distinct_encoding_seeds_diverge_in_their_trace`)
- **INV-ID-1** UUID v4 masks apply to generated IDs only; parsing and raw-byte
  construction preserve arbitrary UUID values, and malformed hex reports both
  group start and invalid character offsets. · enforced by: `id::tests` and
  `tests/it/sim_id.rs`
  (`arbitrary_bytes_round_trip_through_parse_and_display`,
  `the_rendered_form_has_the_documented_hyphen_layout`,
  `generated_identifiers_carry_the_v4_masks`,
  `parsing_preserves_version_and_variant_bits_a_generator_would_have_stamped`,
  `the_reported_version_follows_the_variant_rule_of_the_drawn_bits`,
  `malformed_hex_reports_both_the_group_start_and_the_character_offset`,
  `a_wrong_length_is_refused_before_anything_is_parsed`,
  `every_separator_position_is_required`,
  `an_upper_case_spelling_parses_to_the_same_identifier`,
  `every_truncation_of_a_canonical_form_is_refused`,
  `the_all_zero_and_all_one_values_are_exact_endpoints`,
  `a_multi_byte_character_in_a_group_is_refused_at_its_own_offset`,
  `refusals_fold_their_arm_and_both_of_their_coordinates`,
  `the_same_seed_replays_to_the_same_id_trace`,
  `distinct_id_seeds_diverge_in_their_trace`,
  `two_seeds_draw_two_different_identifiers`)
- **INV-LEB128-1** Integer decoders accept only minimal encodings and return the
  consumed prefix length; trailing input remains with the caller. · enforced by:
  `leb128::tests::distinguishes_prefix_trailing_bytes_from_nonminimal_and_truncated_input`
  and `tests/it/sim_leb128.rs`
  (`a_seeded_unsigned_value_round_trips_at_both_widths`,
  `a_seeded_signed_value_round_trips_at_both_widths`,
  `every_boundary_value_is_encoded_minimally`,
  `trailing_bytes_are_left_to_the_caller`,
  `a_redundant_padding_group_is_refused_as_non_minimal`,
  `an_unterminated_run_is_refused_as_truncated_at_its_own_length`,
  `an_over_wide_run_is_refused_as_overflow_at_its_own_group`,
  `the_target_width_decides_where_a_run_stops_being_valid`,
  `every_truncation_of_an_encoding_is_refused`,
  `every_single_byte_corruption_is_refused_or_changes_the_value`,
  `the_endpoints_are_exact_at_both_widths`,
  `refusals_fold_their_arm_and_their_offset`,
  `the_encoder_appends_to_the_buffer_it_is_given`,
  `the_same_seed_replays_to_the_same_leb128_trace`,
  `distinct_leb128_seeds_diverge_in_their_trace`)
- **INV-STD-HASH-1** A digest is a function of the bytes alone: the same message
  always hashes to the same digest, an incremental `Hasher` equals the one-shot
  `blake3` at every chunking of that message, and a framed feed is the digest of
  its documented `u64` little-endian length prefix followed by the part — so a
  stream of variable-length parts is unambiguous where a plain concatenation is
  not. · why: the `hash` module had no deterministic simulation family at all,
  its whole coverage being nine unit tests on one hand-written message each ·
  enforced by: `hash::tests` and `tests/it/sim_hash.rs`
  (`the_same_bytes_always_produce_the_same_digest`,
  `incremental_hashing_equals_one_shot_at_every_seeded_chunking`,
  `a_trailing_byte_changes_the_digest`,
  `a_single_bit_change_changes_the_digest`,
  `unframed_splits_conflate_where_framed_splits_do_not`,
  `a_framed_feed_equals_the_digest_of_its_documented_prefix`,
  `an_empty_framed_part_is_distinct_from_no_part`,
  `a_keyed_digest_depends_on_the_key_and_the_message`,
  `the_hex_form_round_trips_at_both_cases`,
  `a_digest_hex_of_the_wrong_length_is_refused_on_its_length`,
  `a_digest_hex_with_a_bad_character_is_refused_as_a_hex_refusal`,
  `the_empty_message_is_hashed_as_a_message`,
  `the_wide_boundary_is_hashed_at_its_declared_length`,
  `constant_messages_at_distinct_lengths_are_distinct_digests`,
  `the_same_seed_replays_to_the_same_hash_trace`,
  `distinct_hash_seeds_diverge_in_their_trace`,
  `two_seeds_draw_two_different_messages`)
- **INV-TIME-1** RFC 3339 parsing validates offset component bounds and refuses
  leap-second labels the `SystemTime` profile cannot preserve; checked Unix
  conversion and canonical formatting report range failures instead of
  manufacturing the epoch or extended-year RFC text. Civil-to-day conversion
  narrows only after the complete mathematical count is computed. · why: #153
  T1–T5 · enforced by: `tests/it/sim_time_profile.rs`
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
  `tests/it/sim_http.rs` (`seeded_ceiling_families_match_the_declared_outcome`,
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
  `tests/it/glob_public.rs` and `tests/it/sim_shared_policy_tiers.rs`
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
  `tests/it/sim_shared_policy_tiers.rs`
  (`one_shared_matcher_evidence_policy_and_retry_policy_serve_every_tier`,
  `the_shared_values_are_reachable_through_an_arc_clone`),
  `glob::tests::a_compiled_pattern_is_shareable_across_threads_by_construction`,
  and `tests/it/sim_tenant_isolation.rs`
- **INV-CODEC-1** JSON and RON text/slice decoders preserve input borrowing
  where their decoders support it; escaped text that needs allocation is not
  reported as borrowed. RON writer failures distinguish serialization from I/O
  and preserve the underlying cause without claiming unobserved byte progress.
  · why: #162 · enforced by:
  `json::tests::unescaped_string_fields_borrow_from_text_and_slice`,
  `json::tests::escaped_string_cannot_be_returned_as_a_borrowed_str`,
  `ron::tests::unescaped_string_fields_borrow_from_text_and_slice`,
  `ron::tests::writer_preserves_serialization_and_io_failures`,
  `tests/it/serde_facade_consumers.rs`, and `tests/it/sim_codec.rs`
  (`a_seeded_value_round_trips_through_json_text`,
  `a_seeded_value_round_trips_through_ron_text`,
  `an_unescaped_json_field_borrows_from_the_supplied_text`,
  `an_unescaped_json_field_borrows_from_the_supplied_slice`,
  `an_unescaped_ron_field_borrows_from_the_supplied_input`,
  `an_escaped_field_is_refused_as_a_borrow_in_both_codecs`,
  `a_malformed_document_is_refused_by_both_the_borrowing_and_owned_paths`,
  `a_non_utf8_slice_is_refused_as_a_transport_fault`,
  `a_non_utf8_json_slice_is_refused_with_a_document_location`,
  `trailing_content_after_a_document_is_refused_in_both_codecs`,
  `a_syntax_error_keeps_its_source_location`,
  `each_codec_renders_its_own_documented_shape`,
  `a_value_tree_round_trips_the_drawn_value`,
  `the_empty_containers_are_values_in_both_codecs`,
  `multi_byte_text_round_trips_through_both_codecs`,
  `a_shape_with_empty_collections_still_round_trips`,
  `the_same_seed_replays_to_the_same_codec_trace`,
  `distinct_codec_seeds_diverge_in_their_trace`,
  `two_seeds_draw_two_different_values`)
- **INV-WIRE-1** `lgwks_std::wire` is a feature-unified rkyv archive facade, not a
  canonical semantic encoding or versioned envelope. The effective byte order,
  alignment, and archived pointer width are observable via
  `wire::format_descriptor`; callers bind those properties and their own schema
  version before persisting or exchanging bytes. Structural validation does not
  establish application validity or schema identity. A retained fixture pins the
  schema and format and is read on every target whose format matches; a
  feature-unification probe selects an alternate pointer width and proves the
  descriptor and the emitted bytes move with it. · why: #167 · enforced by:
  `tests/it/wire_consumer.rs`, `tests/it/wire_fixture.rs`, `tests/it/sim_wire.rs` and
  `tests/it/wire_feature_unification.rs`
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
  `pattern::tests::bounded_replacement_expands_exactly_like_the_engine`,
  `tests/it/pattern_external.rs`, and `tests/it/sim_pattern.rs`
  (`a_bounded_match_agrees_with_the_unbounded_engine`,
  `an_input_past_the_ceiling_is_refused_by_every_operation`,
  `the_input_ceiling_admits_its_own_boundary_and_refuses_one_byte_past`,
  `an_amplifying_replacement_is_refused_before_the_append`,
  `a_refused_replacement_returns_no_prefix`,
  `a_bounded_replacement_expands_exactly_like_the_engine`,
  `a_bounded_find_all_yields_the_engines_spans_in_order`,
  `a_bounded_split_yields_the_engines_pieces`,
  `a_bounded_capture_agrees_with_the_engines_group_by_group`,
  `a_no_match_borrows_and_still_honours_the_output_ceiling`,
  `a_compile_ceiling_is_refused_with_the_limit_and_the_size`,
  `the_compile_ceilings_and_a_syntax_error_are_three_distinct_refusals`,
  `a_refusals_escape_the_pattern_text`,
  `a_hostile_haystack_is_answered_under_a_backtracking_pattern`,
  `the_empty_pattern_and_empty_haystack_are_exact_degenerate_cases`,
  `the_output_ceiling_is_exact_at_its_boundary`,
  `the_same_seed_replays_to_the_same_pattern_trace`,
  `distinct_pattern_seeds_diverge_in_their_trace`,
  `two_seeds_draw_two_different_haystacks`)
- **INV-FS-2** A successful strict directory walk has no known omissions; a
  tolerant walk returns each known omission alongside its entries, and an
  unresolved root is always refused. Path-based identity rechecks are
  best-effort only and do not promise race-safe containment against hostile
  concurrent replacement. · why: #143 R15/R16 · enforced by:
  `lgwks_std::fs::tests` and `tests/it/sim_fs_walk.rs`
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
  `lgwks_std::fs::tests`, the public API doctest, and `tests/it/sim_fs_walk.rs`,
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
  `tests/it/content_detection.rs`, and `tests/it/sim_diagnostics.rs`, which checks 64
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
- **INV-BOT-150** A complete length prefix over a frame the file cannot hold is
  decided on the bytes, never on the prefix. The two files that look alike there, an
  append a writer never finished and an acknowledged frame whose prefix was changed
  afterwards (`L` to `L + k`, a final frame or one with frames behind it), are told
  apart by the stored head, which only bytes a writer really framed reproduce. An
  early end after a complete prefix, in the payload **or in the head**, is resolved
  by `frame::holds_acknowledged_frame`: a payload length under the bytes present that
  reproduces the stored head, or a later whole frame that authenticates (against the
  head the cut frame's payload implies, or against the 32 bytes before it), means
  the prefix lied, and the open is `JournalError::Corrupt` with the file
  byte-identical. Only a tail that is a prefix of one cut-short append is trimmed.
  The streaming `Replay` gives the same answer and a refusal ends the stream. The
  search is bounded by construction, because a short read was short of at most
  `MAX_FRAME_BYTES` plus a head. **Not claimed:** a final frame whose length and
  head are both damaged authenticates as nothing and is trimmed, and a file
  truncated mid-frame by a hand reads as a crash. · why: #262 (orphaned from #143
  R02) · enforced by: `journal::file::tests`
  (`a_lengthened_acknowledged_final_frame_is_refused_not_trimmed`,
  `an_inflated_non_final_length_is_refused_and_every_byte_survives`,
  `an_append_cut_at_every_byte_of_the_final_frame_is_repaired`,
  `a_damaged_cut_frame_with_an_acknowledged_frame_behind_it_is_refused`,
  `a_final_frame_with_a_lying_length_and_a_damaged_head_is_the_stated_limit`,
  `a_streaming_replay_refuses_a_lengthened_final_frame_and_then_ends`),
  `journal::frame::tests`, and `tests/it/sim_journal_tail.rs` (`lying_lengths_band_00..03`,
  `cut_appends_band_04..07`, `damaged_cut_frames_band_08..11`,
  `tenants_beside_a_refusal_band_12..13`)
- **INV-BOT-15** One owner serializes journal writes, and an ambiguous write is
  never reported as a clean failure. A capacity-one request slot preserves
  ordering; a `FileView` gives lock-free fence checks; and when a waiter is
  dropped mid-write the owner poisons the handle with the reason instead of
  letting a later append proceed on an unknown outcome. A poisoned handle is
  recovered by reopening, which replays to the same facts and never a second
  effect. · enforced by: `journal::file::a_stalled_device_does_not_stop_the_
  task_waiting_on_it`, `journal::file::a_dropped_waiter_poisons_the_handle_and_a_
  reopen_does_not_duplicate`, and the `tests/it/sim_journal` torn-tail, replay and
  chain families
- **INV-BOT-16** The journal's event cap is a reported bound, not a hidden one.
  A scale measurement that had to clamp to the cap records the requested level,
  the level reached and the ceiling together, so a reader is never told a
  concurrency number nobody ran. · enforced by: `tests/it/sim_scale::tier_r*`
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
  `tests/it/spec_materialize.rs` and `tests/it/sim_spec_materialize.rs`
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
  `tests/it/sys_process_binding.rs` (including
  `concurrent_calls_on_one_process_share_its_ceiling`), `tests/it/sim_process.rs`,
  `tests/it/sys_process_portable.rs` (non-Unix), and `rt::supervise::tests`
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
  `tests/it/inspect_non_execution.rs` (independent filesystem, process-liveness and
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
  `tests/it/inspect.rs::every_budget_has_its_own_refusal`,
  `tests/it/inspect.rs::invalid_syntax_is_incomplete_and_never_a_clean_report`,
  `tests/it/inspect.rs::a_declared_language_version_is_undecidable_not_clean`,
  `tests/it/inspect.rs::the_report_round_trips_and_preserves_identity_spans_and_coverage`,
  `sim_seeded_subjects_agree_across_every_entry_point`,
  `sim_same_seed_same_trace_hash`,
  `sim_seeded_multitenant_reports_stay_isolated`,
  and `tests/it/sim_inspect.rs` (`sim_seeded_fragments_match_the_rule_model`,
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
  `tests/it/task_front_door.rs` and `tests/it/sim_task.rs`
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
  `tests/it/resume_liveness.rs` (`a_parked_record_device_lets_the_runtime_turn`,
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
  green, and the `tests/it/sim_journal.rs` and `tests/it/sim_journal_liveness.rs`
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
  `tests/it/sim_store_scale.rs` (`concurrent_appends_lose_nothing`,
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
  `tests/it/sim_task_resume.rs` (`crash_points_resume_to_the_same_output`,
  `finished_steps_run_once`, `tenants_stay_isolated`, `same_seed_replays`),
  which sweeps every step boundary of every seeded run twice for an identical
  trace hash.
- **INV-BOT-130** Concurrent durable appends share one `fsync`, and nothing about
  the acknowledgement moved to make that safe. An ordered step on
  `journal::owner` is two phases: the *stage* phase runs every queued request's
  in-memory checks, the length fence, the framing and the `write_all`, in
  submission order, and the *settle* phase performs **one** `sync_all` for the whole
  batch and only then publishes each answer and folds each record into the store's
  index. So no append is acknowledged before the `sync_all` covering its bytes has
  returned `Ok`; a batch whose write or sync fails acknowledges **none** of its
  members, hands each the typed failure, and latches the poison once for all of
  them (INV-BOT-15/50) rather than per record; a waiter that drops mid-batch still
  poisons the handle; and the layout order, the length fence and the hash chain
  hold across batch boundaries because the stage phase is per record and strictly
  ordered (INV-BOT-51). The readable half of a record's fold waits for the flush —
  the chain head and committed length do not, because the next member of the same
  batch must chain over them and a head that lagged a write would fork the chain.
  A reopen therefore replays exactly the file's complete frames — every
  acknowledged record, plus any unacknowledged batch whose bytes the device did
  take (at-least-once, INV-BOT-54) — and never a torn or invented one. That is why
  the failed-batch test reads its answer back from `RunStore::open` rather than
  from the handle, whose index folded nothing.
  Bounded on both axes and declared as constants: `MAX_BATCH_RECORDS` (64 records)
  and `MAX_BATCH_BYTES` (256 KiB) cap one flush, and two bounded rings cap the
  requests — `DEFAULT_QUEUE_DEPTH` (64) for what the owner drains and
  `MAX_WAITING_SUBMITTERS` (64) for submitters parked waiting for room. A full
  first ring makes a caller **wait** (backpressure, never growth); only a caller
  that has outrun both rings is refused, with `SubmitError::QueueFull`. There is
  **no linger**: the owner drains whatever is queued at the moment it wakes and
  syncs once, so a lone append stages one record and pays exactly one `sync_all`
  with no timer to wait out — measured at 16 fsyncs for 16 sequential records
  (1.00 per record), which is the property that distinguishes grouping from
  batching-with-delay. Wired: `RunRecords::append_async`, the door `remember` takes
  from `Host::run`, is the only write path for a step record, and
  `RunStore::flush_counts` reports the mechanism's own flush and staged-record
  counters so the ratio below is measured rather than inferred.
  Measured here, release build, one payload size, same harness (`resume_cost`),
  BEFORE = merge base `3d3ec7e8` built in a separate target directory:
  at concurrency 1 the two are identical at 1.00 fsync/record; at concurrency 16,
  256 appends took **2.913s (88/s)** before at 256 fsyncs, and **0.184s (1,394/s)**
  after at **25 fsyncs** — a 15.9x throughput gain at 0.09 fsync/record, with
  per-append p50/p95/p99 falling from 149,839/396,700/590,351us to
  7,987/47,991/48,101us. The ceiling the before path could not reach at all: at
  concurrency 256 it **refused outright** with `QueueFull`, and after the change it
  serves 128 concurrent submitters (the two ring ceilings) and refuses the 129th —
  reproduced exactly at c=128 succeed / c=130 refuse. Peak RSS 14,139,392 bytes
  before and 13,287,424 bytes after at c=16, i.e. unchanged within noise.
  · why: #152 §group commit · enforced by:
  `crates/lgwks-bot/src/journal/owner.rs::tests::a_failed_batch_flush_acknowledges_nobody_and_folds_nothing`,
  which refuses the covering flush of a three-member batch on the shipped store
  and observes every member refused, no member folded into the handle's index, one
  poison latched, every later submit refused, and a reopen that replays exactly the
  file's complete frames; `tests/it/sim_group_commit.rs`
  (`acknowledged_equals_replayed`, `submission_order_is_layout_order`,
  `every_flushed_batch_acknowledges_every_member` — the control: no seeded
  simulation can reach the `#[cfg(test)]` flush switch, so a sim proves only that
  a store whose batches all flushed acknowledges every member —
  `no_record_is_acknowledged_before_its_covering_sync`,
  `a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record`,
  `two_tenants_on_one_store_stay_isolated`,
  `saturation_reaches_every_tier_and_records_the_ceiling`,
  `the_batch_bounds_are_declared_and_never_silent`, `same_seed_replays`),
  `tests/it/resume_liveness.rs::a_grouped_batch_still_lets_the_runtime_turn`,
  `tests/durable_crash_group_commit.rs`
  (`a_real_kill_mid_batch_holds_exactly_the_acknowledged_prefix`,
  `a_killed_run_with_no_acknowledgment_leaves_no_record`), and
  `crates/lgwks-bot/examples/resume_cost.rs`
- **INV-BOT-131** A batch's evidence is read from a **reopened** store, never from
  the handle that acknowledged the records. Group commit's whole risk is an answer
  that outruns its bytes, and that risk is invisible from the writer's side: the
  handle's index is the thing that would be wrong. So every count in the group
  commit family is answered by `RunStore::open` over the same file, and the
  saturation tiers record the requested level, the level reached and the store's
  own ceiling together, so no reader is told a concurrency number nobody ran
  (INV-BOT-16's rule). Measured here: 100 → 100 reached in 100 fsyncs, 1,000 →
  1,000 in 1,000 fsyncs, 10,000 → 10,000 in 10,000 fsyncs, against
  `MAX_RECORDS_PER_RUN` of 65,536. Those fsync counts are *equal to* the record
  counts, and that is the point rather than a disappointment: a sequential store has
  no company to batch with, so paying one flush per record is correct and is the
  control against which the concurrent tier's 0.09 is a measurement.
  · why: #152 §group commit · enforced by: `tests/it/sim_group_commit.rs`
  (`acknowledged_equals_replayed`, `saturation_reaches_every_tier_and_records_the_ceiling`),
  `crates/lgwks-bot/src/journal/owner.rs::tests::a_failed_batch_flush_acknowledges_nobody_and_folds_nothing`
  (the failed batch's durable reality is read from a reopen of the file, not the
  handle that refused it), and
  `tests/durable_crash_group_commit.rs::a_real_kill_mid_batch_holds_exactly_the_acknowledged_prefix`
- **INV-BOT-132** One storage owner serves every durable store with one ordered
  step whose answer has three shapes: `Settled` (nothing was written, the answer is
  ready), `Unsynced` (bytes are on the file and the answer and fold are owed the
  batch's one flush), and `Committed` (the step wrote and flushed its own bytes
  inside the ordered step, so it owes the batch nothing). A step whose *later*
  member must decide against its fold — the run ledger's charge, which applies a
  repair ticket once and moves an epoch — answers `Committed`, so its fold cannot
  be deferred to a batch settle the way a step record's can. Both stores therefore
  run on the same owner thread, the same bounded rings and the same poison latch,
  and `StorageOwner::enqueue_awaiting` returns the concrete `Send` future the
  host's own path needs, unboxed, without weakening the erased `BoxFuture` a task
  body awaits. · why: #152 §group commit merged with the repair ledger (#87
  T13/T23/T24) · enforced by: the ledger's own families (`tests/it/repair.rs`,
  `tests/it/sim_repair.rs`), which charge through the owner's unboxed awaiting future
  and answer `Committed`, and
  `crates/lgwks-bot/src/journal/owner.rs::tests::a_failed_batch_flush_acknowledges_nobody_and_folds_nothing`,
  which drives the `Settled`/`Unsynced` failure path on the same owner
- **INV-BOT-100** A request key is an identity, not a lock, and `Host::submit`
  makes the three outcomes distinct. The run identity is derived, not minted:
  it is a domain-separated hash of the host's tenant and the key, so the same
  request on another process or day derives the same run and its receipt is
  findable without a second table. The input's canonical `InputDigest` (the
  `InputIdentity` schema id framed with its identity bytes) is recorded under
  `@request` before any step runs; the terminal outcome is recorded under
  `@terminal` after, and **only for the dispositions that are the request's own
  verdict** — `Succeeded`, `Failed` and `DeadlineExceeded` (INV-BOT-102).
  A repeat with the same key and digest reattaches to the recorded terminal and
  never re-enters the body; a repeat with the same key and a different digest is
  a typed `RequestError::Conflict` naming both digests and writes nothing;
  distinct keys derive distinct runs and are never collapsed; one key under two
  tenants derives two runs over a shared store file. A host with no store
  refuses with `RequestError::NoStore` rather than downgrading to a plain run.
  · why: #87 step 7 (T30) · enforced by: `tests/it/request_key.rs`
  (`a_duplicate_identical_request_reattaches_without_rerunning`,
  `a_reattach_survives_a_reopened_store`,
  `same_key_with_a_different_payload_is_a_typed_conflict`,
  `distinct_request_keys_are_distinct_runs`,
  `two_tenants_never_share_a_request_run`,
  `a_submission_without_a_store_is_refused`,
  `a_recorded_request_answers_over_a_changed_body`,
  `an_expired_deadline_is_the_requests_recorded_outcome`) and
  `tests/it/sim_request_key.rs` (`collisions_across_two_tenants`, `same_seed_replays`,
  `expired_deadlines_are_recorded_band_16..23`,
  `concurrent_submissions_across_tiers`).
- **INV-BOT-101** A durable submission's receipt outlives the client that
  submitted it, and the in-flight state is reported as two separate facts. If a
  waiter is dropped while the body is in flight, the `@request` receipt and any
  completed step records are already on the disk and no `@terminal` record is;
  a later client that submits the same key and input receives
  `Submission::InFlight` — not a fabricated success and not a re-run of the
  parked body — which names the run to settle (`InFlight::run`) and how many
  records survived (`InFlight::records`). A host stop leaves the same state, for
  the same reason and with the same answer (INV-BOT-102). The host remains the
  cleanup owner: the run store is the host's, so settling the un-recorded step is
  a `resume` of that run, never the client's to hold. The crate deliberately does
  not drive a dropped non-`Send` body in the background; what survives is the
  durable record, which is what a resume needs. · why: #87 step 7 (T17) ·
  enforced by: `tests/it/request_key.rs`
  (`a_dropped_client_leaves_the_request_in_flight_for_a_later_client`,
  `a_host_stop_mid_run_leaves_the_request_resumable`) and
  `tests/it/sim_request_key.rs` (`drop_and_reattach`,
  `host_stops_never_poison_a_key_band_00..03`).
- **INV-BOT-102** A host-side stop is not the request's outcome, so it is never
  recorded as one. `Host::submit` records `@terminal` for exactly the three
  dispositions that are **the request's own verdict** under its declared task —
  `Succeeded`, `Failed` and `DeadlineExceeded`, the deadline included because
  the same declaration that fixed the key also fixed the run's budget.
  `Disposition::Cancelled` (the host's stop arrived after admission) and
  `Disposition::Refused` (the host declined before it) are this host declining
  to finish the run, and record **no** terminal path: a key whose terminal
  record names a host stop is a key nothing can ever complete, because the key
  *is* the request's identity, its body runs at most once, and the one fact that
  made the request resumable — that no terminal record exists — is the very fact
  a stop recorded as the verdict destroys. The stop is still reported to the
  caller that saw it (`Submission::Executed` carrying the disposition), so the
  call is never silent; what it does not do is turn one restart into a request
  that can never succeed. The classification is one **exhaustive** match over
  `Disposition` (`task::terminal_for`), so a variant added later breaks the build
  until its relationship to a request key is decided by hand. `Host::resume` is
  the door that **settles** a request a `submit` left incomplete — it records
  the same three verdicts, under the same rule — because `submit` reports an
  incomplete request rather than re-running its body; without it a stopped
  request would report `InFlight` forever, which is the permanent outcome this
  entry removes. Settling is deliberately narrow: `Host::run` writes no reserved
  record, a resume of a run with no `@request` receipt is an ordinary resume and
  records nothing, a run already holding a terminal record is left alone, and a
  `Refused`/`Cancelled` report is returned untouched so a store that cannot be
  *read* while checking an outcome nobody will write cannot turn a cross-tenant
  `Refused` into a `Failed`. A store that refuses to record a verdict a run
  *reached* is reported as `Failed`, because recording an outcome and reporting
  success are one fact. The same declaration that fixed the key also fixed the
  run's budget, so `DeadlineExceeded` is the request's own verdict and is
  recorded: a request that overran its own budget reattaches to that deadline
  rather than re-running a body that has already overrun once, and the recorded
  step before the overrun survives it. A refused settlement records **nothing**,
  so the request stays unsettled and a later client reads `InFlight` rather than
  a verdict that was never written. · why: #87 step 7 (T30), the review defect
  where one shutdown poisoned a key permanently · enforced by: `tests/it/request_key.rs`
  (`a_host_stop_mid_run_leaves_the_request_resumable`,
  `a_refusal_before_admission_is_not_recorded_as_the_outcome`,
  `a_failed_run_is_the_requests_recorded_outcome`,
  `an_expired_deadline_is_the_requests_recorded_outcome`,
  `a_store_that_refuses_the_terminal_write_reports_the_refusal`,
  `a_dropped_client_leaves_the_request_in_flight_for_a_later_client`) and
  `tests/it/sim_request_key.rs` (`host_stops_never_poison_a_key_band_00..03`,
  `same_seed_replays_host_stops_band_00..03`,
  `expired_deadlines_are_recorded_band_16..23`,
  `settle_refusals_are_reported_and_leave_the_request_unsettled_seed_a..p`).
- **INV-BOT-40** The committed record can be replayed without materializing it:
  `FileJournal::replay` streams frames from its own read-only descriptor,
  retaining at most one event, applies the same frame validation and event
  ceiling `open` does, and yields exactly the acknowledged history. The
  materialized `events()` view and the stream agree on every seed. · why: #122
  item 2 · enforced by: `tests/it/sim_journal_liveness.rs`
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
  handle. · why: #122 item 4 / #156 · enforced by: `tests/it/journal_liveness.rs`
  (`a_slow_store_lets_the_runtime_and_the_release_progress`,
  `a_cancelled_append_leaves_the_runtime_and_the_handle_live`)
- **INV-BOT-140** An awaited answer registers its waker before it reads the
  slot. The owner writes the slot and only then takes the waker to fire it, and
  a wake is fired once, so read-then-register is a lost wakeup: the poll
  decides the answer is absent, the publish finds no waker, and the task parks
  for ever; registering first and reading second means whichever side moves
  second observes the other. · why: GitHub CI parked
  `tests/it/sim_repair.rs::saturation_applies_each_ticket_once_band_09` (PR #239)
  and `_band_03` (PR #241) past 600 s and cancelled the job at its 15-minute
  timeout; the run ledger's charge goes through this owner (INV-BOT-35/51).
  · enforced by:
  `crates/lgwks-bot/src/journal/owner.rs::tests::an_answer_published_during_registration_still_wakes_the_poll`
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
  / #156 · enforced by: `tests/it/journal_scale.rs`
  (`concurrent_tenant_appends_scale_with_isolation`,
  `append_latency_tails_are_bounded`) and `tests/it/sim_journal_liveness.rs`
  (`tenant_tiers_replay_at_the_level_the_sim_can_drive`)
- **INV-BOT-46** A registry identifier declared twice in one role has one
  meaning: refused. `DomainRegistry::validate` names the identifier, role and
  both positions before any build, and the raw `source`/`action` lookups never
  resolve an ambiguous identifier to its first declaration, so dispatch never
  depends on declaration order. One identifier used once per role stays valid.
  · why: #122 item 1 · enforced by: `tests/it/registry.rs`
  (`an_ambiguous_identifier_is_not_resolved_by_declaration_order`,
  `a_duplicate_source_identifier_is_refused_with_both_positions`,
  `refusal_is_independent_of_declaration_order`)
- **INV-BOT-47** An append whose reply was lost but whose record committed is
  reconciled by readback and settled, never resent and never stalled on; an
  append that may have committed and did not reports the occurrence as certain
  and lands its record on the retry without re-entering the action.
  · why: #118 item 1 · enforced by: `tests/it/ambiguous_commit.rs`
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
  `tests/it/sim_task_axes.rs` (`saturation_conserves_permits`,
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
  `tests/it/pr_review_journey.rs` (`a_lost_response_is_reconciled_by_reading_back_and_never_reposted`,
  `a_loss_that_cannot_be_reconciled_stays_unknown_and_still_does_not_repost`,
  `a_moved_head_is_a_typed_refusal_and_publishes_nothing`,
  `a_review_is_published_at_the_pinned_head_and_verified_by_a_separate_read`,
  `a_non_zero_exit_is_not_a_published_review`,
  `a_client_that_never_starts_is_a_definite_non_effect_and_is_not_reconciled`,
  `two_identities_on_one_repository_stay_isolated`) and
  `tests/it/sim_review_pr.rs` (`subject_r64`, `publication_r64`, `identity_r64`,
  `same_seed_same_trace_hash`, `every_outcome_is_reachable_in_the_family`),
  `tests/sim_review_path.rs` (`verified_and_not_observed_r32`,
  `gh_exit_failures_r32`, `deadline_stop_r32`,
  `head_moved_between_snapshot_and_publish_r32`,
  `malformed_and_oversized_answers_r32`,
  `a_publication_the_ceiling_cannot_verify_stays_unknown`,
  `same_seed_same_trace_hash_r32`, `saturation_r32_tier_100`,
  `saturation_r32_tier_1000`, `saturation_r32_tier_10000`,
  `two_tenants_on_one_pull_request_r32`, `cancellation_under_faults_r16`,
  `duplicate_submission_r16`, `two_repositories_on_one_host_r16`) and
  `tests/it/gh_binding.rs` (`a_review_list_past_the_ceiling_is_refused_not_truncated`,
  `a_review_list_exactly_at_the_ceiling_is_read`,
  `a_malformed_review_list_is_refused_rather_than_decoded_into_a_partial_answer`)
- **INV-BOT-120** A source declares when its own cached baseline is unsound, and
  the substrate acts on the declaration rather than on a heuristic it could not
  have derived. `Observe::cache_state` returns a typed
  `RefreshReason::{Disconnected, WatchOverflow, StaleRemoteKey, InvalidationFailed}`,
  read from the source itself after its poll resolves and never guessed: the
  substrate cannot know whether somebody else's transport is up, so a heuristic
  would be a guess about a domain it does not own. A chain that declares one is
  polled with `None` as its baseline, which makes the poll a read, which commits
  the value the source reported and marks the chain moved — a forced refresh that
  compared its fresh read against the baseline it had just been told was unsound
  would keep the stale value forever and re-observe on every tick without ever
  converging. The mark is spent only by a read that *committed*; a poll that
  failed leaves it standing, because the baseline it was supposed to replace is
  still there. The mark is per chain, so one source's failure never re-reads a
  healthy sibling. `TickReport::forced` names the chain, the source's own
  `domain_id` and the cause, published before the schedule runs so a failed tick
  still reports what its sources had already declared. Every report field is
  private behind an accessor: a caller that could edit the record of what a tick
  observed could make a bot that went quiet for the wrong reason look like one
  that went quiet for the right one. · why: #87 step 3 (T08, LC-04) · enforced by:
  `tests/it/observe_refresh.rs`
  (`a_declared_failure_forces_a_refresh_rather_than_a_permanent_quiet_state`,
  `a_forced_refresh_commits_the_newer_value_and_then_returns_to_quiet`,
  `a_failed_refresh_keeps_the_baseline_marked`,
  `two_tenants_sources_forced_refreshes_stay_attributed_to_their_own_chain`),
  `tests/it/sim_observe_refresh.rs` (`forced_refresh_matches_the_schedule` and
  `a_refresh_that_never_lands_stays_marked`, each swept over bands 00–05 and
  06–09 by the shared `band_family!` declaration, and
  `a_seeded_reason_per_chain_is_reported_against_its_own_chain`,
  `a_failed_forced_refresh_keeps_the_mark_until_a_committed_read_spends_it`,
  `two_tenants_interleaved_ticks_never_cross_attribution`) and
  `verb::tests::only_supersession_leaves_the_baseline_sound`
- **INV-BOT-121** An observation the substrate passes over is reported as its own
  outcome, not as a fired effect and not as a retire. `Committed` records per
  chain whether the value sitting in the observation slot has been admitted into
  a generation yet, and a commit that replaces an *unacted* value reports it in
  `TickReport::superseded` with the revision the **replaced** value carried —
  what a caller correlates is "the generation for revision 4 never ran", and
  revision 4 is the one this names. Three states rather than one boolean, because
  "replaced before it was acted on" and "never observed at all" both read as
  `false` in the two-state form, and conflating them makes every chain's first
  commit a reported skip. Admission is marked where a generation *takes* the
  value out of the slot, not where the transition is handed back: for an entry
  awaiting evidence the handover is never reached, so the handover would leave
  every value a held generation is holding reported as unacted — a pass-over
  claim about a value that was already owed work. A forced refresh is not a
  supersession and a supersession is not a forced refresh; neither is counted
  among the other's. · why: #87 step 3 (T09, DX-07) · enforced by:
  `tests/it/observe_refresh.rs`
  (`an_intermediate_value_is_reported_as_superseded_rather_than_fired_or_retired`,
  `identical_payloads_with_distinct_event_ids_both_execute_and_a_redelivery_does_not`)
  and `tests/it/sim_observe_refresh.rs` (`event_identities_are_per_event` and
  `tenants_never_cross`, swept over bands 14–17 and 10–13 by the shared
  `band_family!` declaration, and
  `a_seeded_value_sequence_under_a_held_action_reports_each_replaced_revision_once`,
  `two_tenants_interleaved_ticks_never_cross_attribution`)
- **INV-BOT-122** A chain held open does not starve an independent chain. A
  generation whose action reports an indeterminate outcome stays held, so its
  transition is walked on every tick and never released; the walk stops *at that
  chain* and the chains behind it are still reached, which is what the existing
  "a failure stops its own chain" rule already gives and this names from the
  capacity side. The saturation is bounded: a transition is one state per entry of
  the spec, so a held chain costs its declared slots and nothing more. At 100,
  1,000 and 10,000 held chains on one bot an independent chain declared beside
  them still completes, every held chain still reaches its own action once, and
  every one is still **reported** as held — a dropped hold is a lost effect
  nobody would ever see. The tier reached is recorded rather than clamped.
  · why: #87 step 3 (T06, LC-03), first half · enforced by: `tests/it/observe_refresh.rs`
  (`a_chain_held_at_capacity_does_not_starve_an_independent_chain`,
  `a_saturated_mass_does_not_starve_an_independent_chain_at_every_tier`) and
  `tests/it/sim_observe_refresh.rs`
  (`a_seeded_mass_of_held_chains_never_starves_an_independent_chain`)
- **INV-BOT-123** One slow source cannot hold the tick. Every source poll in the
  observation wave runs under a **declared per-poll deadline** — the wall watchdog
  half of the crate's one declared clock (`Clock`, INV-BOT-30), never its
  caller-advanceable counter, because a source that stopped answering is not
  waiting for time to pass — and `MAX_IN_FLIGHT_POLLS` bounds the fan-out while
  this bounds the wait, which is the half it never did. A poll that
  misses it is **dropped mid-flight**: it commits nothing, keeps its chain's
  baseline and its forced-refresh mark standing (the same rule a failed poll
  already follows, since the value it was to replace is still there), and is
  reported in `TickReport::stalled` naming the chain, the source's own
  `domain_id` and the budget applied. A cancellation is **not** a park: it does
  not stop the tick, so the chains beside a wedged source commit and act in the
  *same* tick — the rule a domain failure follows is deliberately not extended to
  it, because a source that said nothing cannot be a reason to withhold the
  observations other sources did read. The bound is `DEFAULT_POLL_DEADLINE` with
  `EcsBuilder::with_poll_deadline` as the override, refused at build for zero and
  for anything above `MAX_POLL_DEADLINE`, because a silently clamped deadline is
  indistinguishable from the one the caller asked for. The watchdog thread is
  joined on every path, so a resolved poll leaves no thread parked and a cancelled
  one leaves none outliving the tick. **Not claimed:** that cancelling a poll stops
  its side effects. Dropping a future is cooperative, so a poll that handed work to
  `spawn_blocking` has its handle released and its thread runs to completion — the
  stall is about this bot's observation, not about the source's work. · why:
  #87 step 3 (T06, LC-03), slow-source half, closing the gap INV-BOT-122 named ·
  enforced by: `tests/it/observe_refresh.rs`
  (`a_slow_source_does_not_block_an_independent_chain`,
  `a_stalled_chain_is_re_polled_and_commits_when_it_answers`,
  `a_poll_deadline_that_bounds_nothing_is_refused_at_build`) and
  `tests/it/sim_observe_refresh.rs`
  (`a_wedged_source_is_reported_and_costs_its_neighbours_nothing`,
  `a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit`,
  `the_same_seed_replays_a_stalled_wave`,
  `a_stalled_chain_keeps_its_mark_and_is_re_polled_next_tick`,
  `a_poll_deadline_around_both_edges_is_accepted_or_refused_at_build`,
  `only_the_seeded_wedged_chain_is_reported_stalled`,
  `siblings_of_a_wedged_source_act_in_the_same_tick`,
  `two_tenants_interleaved_ticks_never_cross_attribution`)
- **INV-BOT-124** The per-poll deadline's watchdog is one per observation wave
  and is started lazily. `MAX_IN_FLIGHT_POLLS` chains form one wave under one
  `poll_deadline`, so a wave has one deadline to watch and exactly one
  `lgwks-poll-deadline` thread to watch it with; that thread starts only when the
  first poll in the wave returns `Pending`, so a wave whose every source answers
  on its first poll — the ordinary tick — starts no thread at all and
  `TickReport::watchdogs()` is zero. The spawn is serialized on the reaper's own
  lock, so a poll can never park against a thread that was never started; a
  refused spawn stalls only the poll that asked and leaves every already-resolved
  sibling with its answer; and the reaper is joined on every path, so a resolved
  wave leaves no thread parked and an expired wave is reaped before its stalls
  are acted on. The per-chain `PollStalled` report is unchanged. Measured here
  (`examples/poll_deadline_cost.rs`, release, 200 ticks per configuration) —
  AFTER (this change): ordinary tick p50/p95/p99 in µs — 1 chain 1/3/5, 32
  chains 9/9/12, 1,000 chains 158/191/218, 10,000 chains 1082/1120/1156, every
  tier 0 watchdogs; 1,000 chains with one wedged source under a 100 ms budget
  104724/110578/110632, about one deadline rather than one per chain, with one
  watchdog per tick. BEFORE (`bad47c6d`, same harness with the `watchdogs()`
  report removed): 30/37/48, 611/959/1047, 19171/19421/19512,
  160646/191745/193909; the wedged run 127945/137216/140410. A 1,000-chain tick
  therefore paid ~19 ms of thread churn per ordinary tick before and ~0.16 ms
  after. · why: #87 step 3 (T06, LC-03) · enforced by:
  `tests/it/observe_refresh.rs`
  (`a_wave_spends_one_watchdog_and_a_mass_of_waves_spends_one_each`) and
  `tests/it/sim_observe_refresh.rs`, whose `band_family!` declaration runs
  (`a_wave_spends_one_watchdog_and_a_fast_wave_spends_none`), and its
  source-visible deadline-watchdog families
  (`a_fast_wave_spends_no_watchdog_across_seeded_widths`,
  `a_pending_source_spends_one_watchdog_for_its_wave`,
  `a_seeded_run_spends_one_watchdog_per_pending_tick`,
  `a_cancelled_tick_leaves_the_bot_usable`,
  `the_same_seed_replays_a_deadline_wave`).

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
  `tests/it/pr_review_journey.rs`
  (`an_unavailable_diff_is_an_incomplete_coverage_and_publishes_nothing`,
  `a_diff_past_the_file_ceiling_is_an_incomplete_coverage`,
  `a_diff_past_the_byte_ceiling_is_an_incomplete_coverage`,
  `a_renamed_repository_is_refused_and_never_silently_re_pointed`,
  `an_untrusted_build_script_in_the_diff_is_never_executed`,
  `a_pending_draft_is_reconciled_as_a_draft_and_never_reposted`,
  `a_partial_submission_is_reconciled_as_partial_and_never_reposted`,
  `a_lost_read_permission_reports_unverified_and_retains_the_review_id`),
  `tests/it/gh_binding.rs`
  (`a_renamed_repository_is_a_typed_move_naming_both_names`,
  `a_permission_refusal_is_a_typed_unauthorized_not_a_transport_failure`,
  `a_failure_naming_no_status_stays_a_transport_failure`,
  `a_diff_past_its_file_ceiling_is_a_typed_coverage_refusal`,
  `an_unavailable_diff_is_a_typed_coverage_refusal`,
  `a_changed_file_inventory_is_read_as_data`),
  `tests/sim_review_path.rs` (`subject_coverage_and_partial_faults` bands 00
  through 07 (64 seeds), `same_seed_same_trace_hash_subject` bands 08 through
  11 (32 seeds), `two_identities_subject` bands 12 through 13 (16 seeds)), and
  `tests/it/sim_review_pr.rs` (`review_comments_are_carried_and_omitted_r16`)
- **INV-BOT-96** Each refusal arm of a review is a *seeded property of its own*,
  not one point in a sweep that checks outcome shapes. `subject_coverage_and_partial_faults`
  asserts that every fault reaches *some* correct variant; that assertion is
  satisfied by a world in which the right arm is reached for the wrong reason, so
  every arm below asks a different question of the same seeded worlds. A coverage
  refusal reaches the receiver with **zero** creates, so a review cannot be
  published against a scope nobody read. The two diff ceilings are refused on
  **separate axes** — a file-count refusal names files, a byte refusal names
  patch bytes, and a run charged against one bound fails the other family — and
  the byte family's draws are additionally asserted to stay under the *file*
  ceiling, so a refusal for the wrong bound cannot pass as evidence for it. A
  renamed repository is refused naming **both** the requested and the canonical
  name and publishes nothing. A `build.rs` in the changed-file inventory is
  **never executed**: the oracle is a marker file named in the child's own
  environment and referenced by the patch text, so any execution — compiling,
  shelling, or handing the patch anywhere — removes a file the test asserts is
  still there, while the review itself still publishes because a hostile file in
  an inventory is data. A lost response onto an unsubmitted draft is `Pending`
  and a partial submission carries **both** the applied and intended counts, each
  measured at the receiver, and neither issues a second create. A create whose
  read-back lost permission is `Unverified` with the **applied review id
  retained**, and the id must lie in the range the receiver actually handed out —
  a `Unverified` without it would force a caller to re-post to find out whether
  anything landed. The permission and transport arms are **disjoint**: the
  permission arm names an HTTP status and a credential, the transport arm names a
  child's exit and no status, and through the whole journey they are `Unverified`
  and `Unknown` respectively — merging them either discards applied-effect
  evidence or over-reports a blocking state for a failure nobody can attribute to
  permissions. A publication is pinned to the commit that was read, so a run that
  re-pointed the subject after a rename would fail rather than publish at code it
  never read. · why: #231 review (the arm families were untested under fault
  density) · enforced by: `tests/sim_review_path.rs`
  (`an_unavailable_diff_publishes_nothing`,
  `a_diff_past_the_file_ceiling_publishes_nothing`,
  `a_diff_past_the_byte_ceiling_publishes_nothing`,
  `the_two_diff_ceilings_are_refused_separately`,
  `a_renamed_repository_is_refused_naming_both`,
  `an_untrusted_build_script_is_never_executed`,
  `a_pending_draft_is_never_reposted`,
  `a_partial_submission_reports_both_counts`,
  `an_unverified_effect_retains_its_review_id`,
  `unauthorized_and_transport_stay_distinct`,
  `permission_loss_and_transport_outcome_differ`,
  `subject_saturation_conserves_creates`,
  `two_tenants_coverage_stays_isolated`,
  `a_publication_is_pinned_to_the_read_commit`)
- **INV-BOT-97** A scale measurement is not evidence when the world it ran
  against went degenerate. The saturation tiers drive 100, 1,000 and 10,000
  runs of the real journey, and every one of them must reach a *verified*
  `Published` outcome: with no fault configured and a history inside both the
  capture ceiling and `domain::gh::MAX_REVIEWS_PER_PULL`, an `Unknown` is a
  failure of the measurement, not a tolerated outcome. Creates are therefore
  exactly one per run per receiver — `==`, never the `<=` a duplicate-post
  defect satisfies as readily as a correct run — and read-backs are at least
  creates per receiver as well as in total. The fixture is what does not
  survive scale, so the tier is sharded across receivers of at most 100 runs:
  every read-back reads its receiver's whole `reviews.jsonl`, so one receiver
  holding the whole tier pipes a quadratic answer, and past ~590 reviews that
  answer exceeds `CAPTURE` and past 1,000 it is refused with `ReviewCeiling` —
  which ends each run `Unknown` while every inequality the family asserted
  still holds. The shard keeps the concurrency under test unchanged — one
  `Host`, one `join_all_bounded` pipeline at `in_flight = min(N, 64)`, and the
  inputs ordered receiver-major so the bound is reached on a *single* pull
  request. A tier that could not reach its own run count, or a receiver whose
  share is cancelled out by a healthy one in the totals, is a measurement that
  proved nothing; the per-receiver assertions exist to make that visible
  instead of arithmetic. · why: #151 review finding on
  `tests/sim_review_path.rs`'s saturation tiers (a >300 s family that also timed
  a degenerate world) · enforced by: `tests/sim_review_path.rs`
  (`saturation_r32_tier_100`, `saturation_r32_tier_1000`,
  `saturation_r32_tier_10000`, which assert per receiver and in total, and
  `same_seed_same_trace_hash_r32` for the replay the tier still owes)

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
  · why: #87 T26 · enforced by: `tests/it/proposal.rs`
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
  `tests/it/sim_proposal.rs`
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
  T27/T35 · enforced by: `tests/it/proposal.rs`
  (`the_three_untrue_successes_report_distinct_outcomes`,
  `an_abandoned_run_is_not_a_finished_one`,
  `an_over_long_evidence_claim_is_refused_whole`,
  `a_truncated_payload_never_becomes_a_full_coverage_claim`,
  `a_well_formed_proposal_is_admitted_on_a_real_run`) and
  `tests/it/sim_proposal.rs`
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
  enforced by: `tests/it/proposal.rs`
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
  `tests/it/proposal.rs` (`two_tenants_on_one_digest_stay_isolated`,
  `conflicting_writes_to_one_key_are_serialized_and_idempotent`,
  `reads_progress_while_a_write_is_in_flight`,
  `an_oversized_artifact_is_refused_and_the_store_is_unchanged`) and
  `tests/it/sim_proposal.rs`
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
  by: `tests/it/proposal.rs`
  (`repeated_unchanged_failure_reaches_a_finite_intervention`,
  `a_ledger_of_distinct_failures_reaches_its_own_intervention`,
  `new_evidence_does_not_erase_root_spend`, `a_plan_budget_bounds_repair`,
  `repeated_unchanged_failure_reaches_a_finite_intervention_across_runs`,
  `a_plan_budget_bounds_repair_across_runs`) and `tests/it/sim_proposal.rs`
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
  no-production-caller defect) · enforced by: `tests/it/proposal.rs`
  (`an_injected_instruction_is_refused_at_its_step_on_a_real_run`,
  `a_well_formed_proposal_is_admitted_on_a_real_run`,
  `a_capability_the_run_does_not_hold_is_refused_by_name_on_a_real_run`,
  `repeated_unchanged_failure_reaches_a_finite_intervention_across_runs`,
  `a_plan_budget_bounds_repair_across_runs`,
  `a_resumed_run_reads_back_the_refusal_the_first_run_recorded`) and
  `tests/it/sim_proposal.rs` (`seeded_runs_reach_the_declared_disposition_band_20..23`,
  `two_tenants_admitting_on_one_host_stay_isolated_band_24..27`,
  `saturation_over_admit_conserves_the_budget`)
- **INV-SCAN-ZERO** No file this workspace ships carries a source finding: no
  error is silently discarded, no fallible return reaches a caller with no
  recorded signal, no statement chains more than three fallible steps without an
  intermediate binding, and no public item's documentation is a paraphrase of its
  own name. A file the scanner cannot parse is a refusal, not a pass. · why: the
  name appeared in `scan.rs`'s module doc while 433 findings sat in shipped source
  and no lane ran it — the invariant was advertised and unenforced · enforced by:
  the `scan` lane, which exits 2 on any finding

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
- 2026-10-05 (#280): #109 closed on 2026-09-23 with those three rows repaired
  and covered by in-process regression tests, not by an external observation.
  INV-BOT-9's T21 is now observed by `tests/process_escape.rs`, which shows a
  `setsid` descendant escaping the group and the receipt not claiming it
  (INV-BOT-112); stopping that descendant is #263. INV-BOT-5's real-store poll
  path and INV-BOT-10's real frame still have no named external test.
