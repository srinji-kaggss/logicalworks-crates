# std × ast × deps closure matrix

Status: **an index of what exists and what each claim rests on, at one exact
revision.** It is not a certificate, and completing a row here does not close
its owning issue. Tracker: [#170](https://github.com/srinji-kaggss/logicalworks-crates/issues/170),
sibling trackers [#155](https://github.com/srinji-kaggss/logicalworks-crates/issues/155)
and [#156](https://github.com/srinji-kaggss/logicalworks-crates/issues/156).

**The revision this page describes is `977c79d4` (`origin/main`, 2026-10-05),
and every row's `test` column names a test that exists in that tree.** It was
first written against `b4a9f074`; on 2026-10-05
([#280](https://github.com/srinji-kaggss/logicalworks-crates/issues/280)) each of
the 33 unit tests Part 1 names was found again by `codegraph query <name>` at
`977c79d4`, and each `tests/it/sim_*.rs` family named below is listed by
`ls crates/lgwks-std/tests/it/`. The revision is named once, here, rather than
repeated per row, because a per-row revision is a per-row value that rots
exactly like the prose version pins this matrix exists to replace: the honest
form is *this page describes one tree, and the tree is named here*.

Every owner issue the first revision named — #150, #153 to #170 and #122 — is
now closed, so the last column records the issue that **closed** the row's open
item and its date, plus the seeded simulation family the closing work added
where there is one. A row whose item is still open names the open issue that
owns it.

## What a row means, and what it does not

Every row names a **real test in this tree**. That is the whole claim, and it is
a weaker claim than it looks, so the weaker claim is stated rather than left to
be inferred:

| State | Meaning | What it does **not** mean |
|---|---|---|
| `exercised` | A named test in this tree drives the public surface and observes the stated behaviour on this exact revision. | That the test is an *external* observation, or that the row's remaining open work is closed. |
| `present` | The module/symbol exists and compiles under its declared feature; a test exists but does not cover the stated property. | That the property holds. |
| `unexercised-gap` | A named defect or ergonomic gap is open against this row, with an owning issue. | That the module is broken. |
| `assurance-gap` | The code makes a claim (constant-time, supply-chain, advisory-lock scope) that source inspection cannot discharge. | That the claim is false. It is unproven, not disproven. |

**These distinctions are load-bearing and are not collapsed anywhere below.**

- A passing unit suite is **not** an external-effect observation.
- A compile-time refusal (a `t22_process_surface.rs`-style consumer probe) is
  **not** an executed sanctioned run.
- `cargo check` of a feature combination is a **build** receipt, not a
  behavioural one.
- An admitted dependency edge in `contract/APPROVED.toml` is a **policy
  decision**, not an implemented capability.
- `kernel-check` of `proofs/bot-spec.sml` is an abstract-model proof, **not** a
  refinement proof of this Rust.

## Part 1 — the twenty `lgwks_std` modules

The module inventory is read from `crates/lgwks-std/src/lib.rs`, which
declares exactly twenty `pub mod` items. Every one has a row. A module added
without a row is a coverage failure, and
`scripts/check-doc-citations.py` verifies the inventory against this table.

| # | Module | Feature | State | Test that exists on `977c79d4` | Closed by (date) · still open → owner |
|---|---|---|---|---|---|
| 1 | `encoding` | `core` | exercised | `base64_matches_the_rfc_4648_vectors`, `base64_roundtrips_every_byte_value` (unit); seeded family `tests/it/sim_encoding.rs` | Source-vs-decoded offsets, per-escape allocation → [#161](https://github.com/srinji-kaggss/logicalworks-crates/issues/161) (closed 2026-09-30) |
| 2 | `fs` | `core` (`fs-raw` for the capacity query) | exercised | `sim_fs_walk.rs` (26 tests); `fs/mod.rs` 17 unit tests, `fs/capability.rs` 28 | Uniform path basis, strict diagnostics, consuming report API, separate budgets → [#166](https://github.com/srinji-kaggss/logicalworks-crates/issues/166) (closed 2026-10-02) |
| 3 | `glob` | `core` | exercised | `glob_public.rs` (4 tests) + 10 unit tests incl. `question_and_star_preserve_separator_boundaries`; seeded family `tests/it/sim_glob_sweep.rs` | Quadratic transitions, scalar-vs-byte wildcards, invalid-pattern vs invalid-dialect → [#154](https://github.com/srinji-kaggss/logicalworks-crates/issues/154) (closed 2026-10-02) |
| 4 | `hash` | `hash` | exercised + **assurance-gap** | `empty_input_matches_blake3_spec`, `deterministic_across_calls` (13 unit tests); seeded family `tests/it/sim_hash.rs` | X1 (#275): equality delegates to `blake3::Hash`'s `constant_time_eq`, and `examples/digest_timing.rs` is a dudect Welch's t test at 10⁶ samples per class, gated by the `digest-timing` lane. 2026-10-05, both classes on one instruction stream: aarch64-apple-darwin `digest_eq` t = −0.921 (29.9 ns per comparison), early-exit control t = 10140.5; x86_64-unknown-linux-gnu (CI) `digest_eq` t = −0.355 (1.88 ns), control t = 34072.6 — so the harness sees a leak when there is one. The pre-#275 XOR fold measured t = −0.680 and 1.499 on the two targets, a property of these compilers that nothing guaranteed. A first harness that selected each class's operand by a branch reported t = −17.8 and then +32.3 on x86_64 at class means equal to 0.001 ns, a code-path artifact, not a leak. **`Ord`/`PartialOrd` stay variable-time**, documented for collections only. Determinism is not semantic identity. |
| 5 | `hex` | `core` | exercised | `encode_emits_two_lowercase_characters_per_byte`, `decode_accepts_uppercase_digits`; seeded family `tests/it/sim_hex.rs` | Shared decode-into primitive, destination modification semantics → [#161](https://github.com/srinji-kaggss/logicalworks-crates/issues/161) (closed 2026-09-30) |
| 6 | `http` | `http` | exercised | 43 unit tests incl. `gets_status_headers_and_body`, `default_option_wrappers_reach_the_same_path`; seeded family `tests/it/sim_http.rs` | Stage-stable timeout class, byte-preserving headers, redirect provenance, budget accounting → [#163](https://github.com/srinji-kaggss/logicalworks-crates/issues/163) (closed 2026-10-02) |
| 7 | `id` | `random` | exercised | `generated_value_carries_version_four_and_the_rfc_variant`, `successive_identifiers_differ`; seeded family `tests/it/sim_id.rs` | Fixed-size parse/format, error positions → [#161](https://github.com/srinji-kaggss/logicalworks-crates/issues/161) (closed 2026-09-30). **A generated v4 is not proof of provenance**; arbitrary-value constructors certify neither. |
| 8 | `json` | `json` | exercised | `roundtrips_through_string`, `compact_output_has_no_whitespace`; `serde_facade_consumers.rs`; seeded family `tests/it/sim_codec.rs` | Borrowed-input lifetime, complete facade-only derive support → [#162](https://github.com/srinji-kaggss/logicalworks-crates/issues/162) (closed 2026-09-30). Not canonical JSON, not schema migration, not transactional I/O. |
| 9 | `leb128` | `core` | exercised | `u64_roundtrip_vectors`, `i64_roundtrip_vectors`; seeded family `tests/it/sim_leb128.rs` | Canonical profile vs WebAssembly's permitted padded forms → [#161](https://github.com/srinji-kaggss/logicalworks-crates/issues/161) (closed 2026-09-30) |
| 10 | `online` | `online` | exercised | `open_port_probes_true`, `closed_port_probes_false` | One remaining connection budget, explicit resolver timing scope → [#163](https://github.com/srinji-kaggss/logicalworks-crates/issues/163) (closed 2026-10-02). **A boolean TCP probe is not a health check** and carries no TLS or universal-connectivity guarantee. |
| 11 | `pattern` | `pattern` | exercised | `producer_counter_detects_eager_collection_mutant`, `configured_limits_refuse_input_and_amplified_output`; `pattern_external.rs`; seeded family `tests/it/sim_pattern.rs` | Per-operation complexity, producer/resource controls, borrowed replacement → [#168](https://github.com/srinji-kaggss/logicalworks-crates/issues/168) (closed 2026-09-30). The discriminating laziness oracle is [#122](https://github.com/srinji-kaggss/logicalworks-crates/issues/122) (closed 2026-10-03) item 3. |
| 12 | `process` | `process` (**Unix-only**, `cfg`-gated) | exercised, platform-scoped | `invalid_pgid_is_rejected`, `invalid_pid_is_rejected_for_the_exit_observation`; seeded family `tests/it/sim_process_group.rs` | Real owned-child boundary tests and docs → X4; descendant containment beyond the group → [#263](https://github.com/srinji-kaggss/logicalworks-crates/issues/263) (open). **Non-Unix returns `std::io::ErrorKind::Unsupported` rather than a fabricated success**; the raw primitives are not managed lifecycle acceptance, and bot owns that. |
| 13 | `random` | `random` | exercised | `fills_the_whole_buffer`, `an_injected_os_code_round_trips_through_the_typed_error`, `a_refused_fill_returns_the_typed_error_and_leaves_the_buffer_alone`; seeded family `tests/it/sim_random_error.rs` (10 tests, four tiers to 100 000 callers) | Typed backend failure/cause, defined output validity after failure, and every `getrandom` target → [#276](https://github.com/srinji-kaggss/logicalworks-crates/issues/276) (closed 2026-10-05; was X3): the cause is a kind plus the raw OS code plus its `io::ErrorKind`, and the refused-fill buffer is documented as unspecified and driven by a test. **The typed cause is not a proof that any target's entropy source is sound, and no histogram here is a proof of cryptographic strength.** Builds for seven cross targets (`target-matrix` lane); only `full` and `lgwks_ast` need a C toolchain this runner lacks. |
| 14 | `retry` | `core` | exercised | `backoff_doubles_and_caps`, `jitter_only_shrinks`; seeded family `tests/it/sim_retry_arithmetic.rs` | Growth after attempt 31, `Duration`/`u64` jitter boundaries, effective-policy invariants → [#164](https://github.com/srinji-kaggss/logicalworks-crates/issues/164) (closed 2026-10-02). **Not an executor, and not evidence an effect is retry-safe.** |
| 15 | `ron` | `ron` | exercised | `struct_roundtrips_through_string`, `enum_roundtrips`; seeded family `tests/it/sim_codec.rs` | Borrowed types, RON-only derive path, typed partial-writer failure → [#162](https://github.com/srinji-kaggss/logicalworks-crates/issues/162) (closed 2026-09-30). **Buffered serialization is not an atomic external write.** |
| 16 | `similarity` | `core` | exercised | `edit_distance_handles_empty_and_identical_inputs`, `edit_distance_normalizes_known_distance`; `similarity_public_api.rs`; seeded family `tests/it/sim_similarity_sweep.rs` | Coherent domains, refusal-preserving composition, valid `Geometry` input, normalization units → [#160](https://github.com/srinji-kaggss/logicalworks-crates/issues/160) (closed 2026-10-02). **Heuristic normalization is not exact identity**, and missing evidence is not a zero. |
| 17 | `task` | `core` | exercised | `immediate_future_returns_value`, `yields_and_resumes`, `after_a_shutdown_admission_is_refused_and_the_job_never_runs`, `a_thread_that_cannot_start_refuses_the_job_and_it_never_runs`, `no_more_than_the_ceiling_run_at_once_and_every_job_completes`; seeded families `tests/it/sim_task_public.rs`, `src/sim_task_pool.rs`, `src/sim_pool_lifetime.rs`, plus `tests/sim_task_pool_public.rs` | **X2 closed by #264**: admission is typed on both entry points — `try_spawn_blocking` returns `SpawnError::{AtCapacity, Os, Shutdown}`, and a spawn failure reaches the caller as `Os` rather than the result/panic path — and the offload is one process-wide bounded pool whose ceiling `configure_blocking_pool` fixes once before first use (every later attempt refused typed, never silently ignored) and whose `shutdown_blocking_pool` joins every thread inside a deadline, letting queued and running jobs finish and reporting the threads still running when the deadline expires. A start joins the threads that have already returned, so a bursty process does not accumulate one handle per exited thread. **A dropped handle still does not stop a thread**, and cooperative polling is still not preemption — a closure that never returns is only detectable from outside the process. |
| 18 | `time` | `core` | exercised | `sim_time_profile.rs` (10 tests) + `time/mod.rs` 18, `time/parse.rs` 1 | Invalid offsets, false format round trip, epoch substitution, leap normalization, premature calendar saturation (INV-TIME-1) → [#153](https://github.com/srinji-kaggss/logicalworks-crates/issues/153) (closed 2026-10-02) incl. T5. **No timezone database is required or provided.** |
| 19 | `trace` | `trace` (**default-on**) | exercised | `format_parser_accepts_the_documented_names`, `config_refuses_an_empty_service_name` | External macro/span use, no forced global subscriber → X4. **Subscriber installation, storage, sampling/export and context propagation are owned outside this primitive.** |
| 20 | `wire` | `wire` | exercised | `wire_consumer.rs` (7 tests); seeded family `tests/it/sim_wire.rs` | Default width mismatch, canonical vs generic guarantees, error typing/zero-copy scope → [#167](https://github.com/srinji-kaggss/logicalworks-crates/issues/167) (closed 2026-10-02). The fixed derive-support re-export is retained. |

### Two facts about this table that are easy to misread

- **`core` is not `#![no_std]`.** `core` is a cargo feature, and selecting it
  withdraws the default set — including `trace`. It is not a promise about the
  `no_std` attribute.
- **Feature availability and default availability are different facts.** `trace`
  is default-on; every other feature is default-off; `full` turns on eleven of
  them. A disabled selector does not silently acquire an optional engine.

## Part 2 — `lgwks_ast`: finished, and explicitly not re-opened

`lgwks_ast` is the finished standalone parser. It is complete rather than
parked, so it is not a candidate for new capability, and this matrix does not
propose growing it into a fourth surface.

| Property | State | Evidence on `977c79d4` |
|---|---|---|
| 28 declared grammars, default 7 | exercised | `DECLARED_GRAMMARS: usize = 28` (`crates/lgwks-ast/src/lib.rs`) with a fixture table asserted to cover every declared grammar; `default =` in `Cargo.toml` lists 7 `lang-*` |
| Every declared grammar accepts its valid fixture and refuses its malformed one without a recovery node | exercised | The `GrammarFixture` table and its completeness assertion (`crates/lgwks-ast/src/lib.rs:3206-3209`) |
| Depth-bounded traversal, typed diagnostics with spans | exercised | Retained repair; see the crate's own tests |
| **Not claimed here** | — | In-process bot code inspection is [#150](https://github.com/srinji-kaggss/logicalworks-crates/issues/150)'s, and parser/detection/inspection semantics are [#165](https://github.com/srinji-kaggss/logicalworks-crates/issues/165)'s. **Neither is verified by this matrix.** |

## Part 3 — `lgwks_deps`: admission is a decision, not a capability

| Property | State | Evidence |
|---|---|---|
| Every authored external edge has an owner | enforced by a gate | `cargo run -p lgwks_deps -- check .` (gate lane `deps-register`); `contract/APPROVED.toml` |
| `lgwks_std` and `lgwks_ast` author edges directly under the grandfather clause | enforced | `contract/APPROVED.toml`; `scripts/check-std-first.py` reads the *source*, not the manifest |
| Feature-gated storefront capabilities are default-off | present | `crates/lgwks-deps/Cargo.toml` `[features]` |
| **`appcui` / `gpui` / `candle` / `bevy` are admitted dependencies** | **admitted-not-implemented** | An approved edge in `contract/APPROVED.toml` is a recorded policy decision. **It is not a CUA product, a model runtime, or a delivered capability, and no row here claims one is.** |
| Authored-edge assurance vs resolved transitive bytes, patches, source replacement, vendor integrity | **assurance-gap** | A no-deps graph is not a supply-chain proof → [#158](https://github.com/srinji-kaggss/logicalworks-crates/issues/158) |
| Unambiguous register parsing, complete member records, collector error-path cleanup | unexercised-gap | [#157](https://github.com/srinji-kaggss/logicalworks-crates/issues/157), [#159](https://github.com/srinji-kaggss/logicalworks-crates/issues/159) |
| Every selected capability has a usable public import path downstream | unexercised-gap | Declared-but-unexported `bevy_app`/`bevy_time`/`bevy_state` → [#169](https://github.com/srinji-kaggss/logicalworks-crates/issues/169) |

## Part 4 — closure

This matrix is **not** the closure condition. The tracker closes when its full
concept register is accounted for at the owning issues, which are:

**Gate correctness:** [#157](https://github.com/srinji-kaggss/logicalworks-crates/issues/157)
→ [#158](https://github.com/srinji-kaggss/logicalworks-crates/issues/158);
[#159](https://github.com/srinji-kaggss/logicalworks-crates/issues/159) proceeds
independently.

**Semantic correctness:** [#153](https://github.com/srinji-kaggss/logicalworks-crates/issues/153),
[#154](https://github.com/srinji-kaggss/logicalworks-crates/issues/154),
[#160](https://github.com/srinji-kaggss/logicalworks-crates/issues/160),
[#161](https://github.com/srinji-kaggss/logicalworks-crates/issues/161),
[#162](https://github.com/srinji-kaggss/logicalworks-crates/issues/162),
[#163](https://github.com/srinji-kaggss/logicalworks-crates/issues/163),
[#164](https://github.com/srinji-kaggss/logicalworks-crates/issues/164),
[#165](https://github.com/srinji-kaggss/logicalworks-crates/issues/165),
[#166](https://github.com/srinji-kaggss/logicalworks-crates/issues/166),
[#167](https://github.com/srinji-kaggss/logicalworks-crates/issues/167),
[#168](https://github.com/srinji-kaggss/logicalworks-crates/issues/168),
[#169](https://github.com/srinji-kaggss/logicalworks-crates/issues/169).

Every issue in both lists above is closed as of 2026-10-05.

**Assurance workstreams X1–X4:** X1 (constant-time hashing) was measured and closed
by #275; X2 (fallible thread admission) was closed by #264 — the `task` row now
carries a typed admission result on both entry points and an owned, joinable
blocking pool; X3 (entropy failure meaning) was closed by #276: the refusal
carries the OS's own code, its portable `io::ErrorKind` and a kind that
distinguishes "no source on this target" from "the OS refused", and the output
buffer's state after a refusal is documented and tested rather than assumed. X4
(feature-isolated ergonomics) has no issue of its own and stays open against the
`process` and `trace` rows. The rows marked **assurance-gap** stay marked until
their owners close.

**Nothing here converts an unresolved promise into a green checkbox.** A row
marked `unexercised-gap` or `assurance-gap` is a real, named, owned gap, and
this page exists so that it is visible rather than rounded up.