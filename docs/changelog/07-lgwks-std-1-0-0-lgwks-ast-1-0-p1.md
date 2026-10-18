# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (1/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 1 of 13. [Changelog index](../../CHANGELOG.md)

### 1.0.0 — lgwks_std, lgwks_bot, lgwks_ast, lgwks_deps, lgwks_macros

First stable cut: from here the public API follows semantic versioning (a
breaking change takes a major version). No public item changes relative to
`lgwks_std 0.10.0`, `lgwks_bot 0.8.0`, `lgwks_ast 0.4.0`, `lgwks_deps 0.4.0` and
`lgwks_macros 0.1.2`, with these `lgwks_deps` changes since 0.4.0: new
`metadata::read_with_members` and `Refusal::UnknownSurface` (a workspace member
or approval owner outside the five INV-DEP-1 surfaces is refused by name, #207);
`Refusal::VendorTierConflict` (#210) and `Refusal::LicenseNotAccepted` (#208);
a register entry now requires a `license` key holding an SPDX expression, so a
register written for 0.4.0 must add it; a `Cargo.lock` with a nameless package
block or no packages is refused (#209). `vendor/` is present but not configured
as a Cargo source.

### Benchmarks

- `bench/ai-authoring`: five fixed user profiles and a third held-out task.
  `--profiles` takes `first-time`, `expert-hurry`, `anxious`, `misuser` and
  `agent` and defaults to all five; each is a *fixed* persona prepended to the
  same prompt skeleton, so the API sheet, the hidden oracle, the repair budget
  and the `sandbox-exec` closed-book profile are identical across profiles and
  the profile is the only thing that differs between two cells of one
  `(model, api, task)`. `profile` is recorded in every `results.jsonl` line and
  `summary.json` groups by `(model, api, task, profile)`.
  The new task, `recovery`, needs host setup, a derived run identity, a task
  helper and recovery together; its oracle has one test per clause and its
  mutant fails `a_resume_does_not_rerun_a_completed_unit` and only that clause.
  Three per-trial metrics are added and every existing one is kept:
  `oracle_wall_ms` and `oracle_peak_rss_bytes`, both taken under
  `/usr/bin/time -l` (the RSS parser reads macOS's value-before-label form and
  GNU's kibibyte form), and `cleanup_ok`, which reads the task's drop clause and
  reports `false` for a crate that never compiled.
  **This is not a measurement of human authorability and no person was
  consulted.** The README's earlier claim that authorability "needs the
  Director" is replaced by a statement of what the profile axis does and does
  not reach. `recovery` has no old-API cell, and the reason is recorded in each
  run's `protocol.skipped_cells`: the old `rt` surface has no durable run store,
  no `remember`, no run identity and no resume, so it cannot express the task.

### lgwks_bot Breaking

- `task::StoreError`: a new `FormatVersion { found, expected }` variant, and the
  run store's file format version moves from `\x01` to `\x02` (T15). **Migration:**
  opening a `\x01` run store is refused with `StoreError::FormatVersion { found:
  1, expected: 2 }`, which names both versions, instead of the generic
  `StoreError::NotAStore` it previously produced. The refusal is typed rather
  than generic on purpose: `NotAStore` asserts the bytes were never this store's,
  and a `\x01` store was written by an earlier version of this very crate, so
  that message would tell an operator their own data was never theirs — the one
  conclusion a refusal must never produce, because it is what makes someone
  delete a file a system still relies on. There is no migration, deliberately:
  the two available readings are to invent a `DefinitionIdentity` those records
  never carried, which would make every pre-version resume look exactly
  compatible, or to discard acknowledged evidence. The format has never shipped a
  version that could lose a record, so there is nothing to convert. A deployment
  that needs its records keeps its own copy and re-runs. A file whose magic does
  not match is still `NotAStore`, and the two refusals are asserted apart by
  `tests/task_resume.rs::a_pre_version_store_is_refused_naming_both_versions` and
  `::a_foreign_file_is_still_refused_as_not_a_store`.

### lgwks_bot Fixed

- `tests/sim_repair.rs`: seven repair arms no longer reopen a store an earlier run
  on the same host left behind. The tests built their store directory under the
  system temp root from a *fixed* name (`lgwks-first-step`,
  `lgwks-reopened-repair`, `lgwks-repair-recovery`,
  `lgwks-cross-tenant-ticket`, and the `-{reach}`/`-{tier}` families), so the
  directory was a ledger the test did not own: a run that predated the `\x02`
  format left records there, and every later run refused to open them with
  `StoreError::FormatVersion { found: 1, expected: 2 }` — a refusal that named a
  version this branch has never written. Each now takes its directory from the
  `shared::Scratch` guard already shared with `tests/repair.rs` (random hex in the
  name, removed when the guard drops), held for the whole test, so no run can see
  another run's files and none leaves one behind (INV-BOT-116). This is a test
  isolation and ephemerality fix, not a format change: `check_format_version` is
  untouched, and a `\x01` store written inside a run's *own* scratch directory is
  still refused by `tests/task_resume.rs::a_pre_version_store_is_refused_naming_both_versions`.
- Merge resolution against `#240` (group commit): the run store's ordered append
  keeps **both** sides' guarantees rather than either one wholesale. The two-phase
  stage is main's — every queued request's checks, fence, framing and `write_all`
  run in submission order, one `sync_all` covers the batch, and only then are the
  answers published and the records folded — and the record/index fold additionally
  carries this branch's `DefinitionIdentity`, so a run's records still name the
  definition they were written under and `check_format_version`/`FlowError::Store`
  are unchanged. `Awaiting::poll` keeps main's ordering exactly: register the waker
  *before* reading the slot, or a publish landing between the two is a lost wakeup
  (INV-BOT-140). Both `journal::owner` tests from `#240` are kept.

### lgwks_bot Added
