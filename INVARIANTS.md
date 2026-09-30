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
- **INV-DEP-8** Cargo metadata runs under a deadline with per-stream byte
  budgets; a hung or flooding child is killed and reaped, and a collection
  failure is a refusal, never an empty graph. · why: #143 R14 · enforced by:
  `lgwks_deps::metadata::tests`
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
- **INV-FS-2** A successful strict directory walk has no known omissions; a
  tolerant walk returns each known omission alongside its entries, and an
  unresolved root is always refused. Path-based identity rechecks are
  best-effort only and do not promise race-safe containment against hostile
  concurrent replacement. · why: #143 R15/R16 · enforced by:
  `lgwks_std::fs::tests`

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
