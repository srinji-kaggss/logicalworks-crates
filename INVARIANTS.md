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

## Docs

- **INV-DOC-1** Bare `//!` intra-doc links break when `lib.rs` also doc-comments the
  `mod`; use reference definitions. · enforced by: rustdoc `-D warnings` in CI

## Governance as code

- **INV-GOV-1** The gate is defined once in `scripts/gate-lanes.toml`; CI runs
  the same lane commands and `scripts/check-gate-parity.py` refuses drift. ·
  enforced by: `gate-parity` lane
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
