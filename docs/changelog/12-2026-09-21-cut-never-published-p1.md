# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (1/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 1 of 13. [Changelog index](../../CHANGELOG.md)

### Proofs

- **`proofs/bot-spec.sml` states and proves seven theorems about the
  spec/condition layer**, machine-checked by the HOL4 kernel: totality of the
  loop-free evaluator (`T1`), budget monotonicity (`T2_MONO`), budgeted-verdict
  agreement (`T3`), **capability-gate soundness** — every entry that fires is one
  the grant permits (`T4`) — gate monotonicity in the grant (`T5`), and
  **record/replay agreement** (`T6`). `T7` proves that the loop constructor
  admits no value at any finite budget, which is the formal reason `T1`'s
  totality is a property of the loop-free language and does not survive the
  extension. `proofs/proof-run.log` is the unedited run; no `mk_thm`,
  `new_axiom` or `cheat` appears anywhere in the script.
- **It is not a refinement proof, and the README says so first.** The theorems
  are about the script's own hand-written datatypes, not the Rust; nothing here
  proves that `spec.rs` implements the model. The claim is therefore split: the
  *design* has these properties (proved), and the *implementation* cannot express
  the states that would break them (argued from type construction — a private
  `Auth` constructor, a `Cap` newtype, a grant written only in `assemble`). The
  model's abstractions are enumerated too: `Contains` is an opaque literal,
  `Env` is total, effects are not modelled, capability shortfall is not modelled,
  and the runtime is absent entirely. A bug in the model is a bug in what is
  proved.

### Benchmarks

- **`bench/` measures `lgwks_bot` against a hand-rolled baseline doing identical
  work**, gated on the bot and the baseline agreeing on both effect counts and
  condition-evaluation counts. That second axis is load-bearing: gating on
  effects alone let a baseline that returned `entries` as an integer instead of
  looping over them report a 2937x ratio where the counted-work gate measures
  72.38x at the same scenario. The rig is its own Cargo workspace root so the
  dependency contract never sees it — `lgwks-deps check .` still reports the four
  crates.
- **The measured result is that the bot is 72x–256x slower than the loop, and
  ~98% of a tick is the poll and change-detection phase before any decision is
  made** (poll-only 2540.0 ns/tick against steady 2582.1 ns/tick at 64 chains).
  Source cost is linear; change detection now pays 4.23x, because both changes
  below made a quiet tick cheaper without making a churning one cheaper, so a
  workload that never goes quiet pays about four times what a quiet one does.
- **The rig found a defect, and the fix it was measured against has landed.**
  `Auth::check` grew 3794x for a 128x increase in required capabilities, reaching
  12.9 µs per call at 128 capabilities, because coverage was a linear scan of
  granted names per required name. The sorted-and-deduplicated proof with a
  binary search replaced it, and the same rig measures growth of 750.5x and
  3.64 µs at 128. The bench directory measures and does not touch crate code.
- **These are the figures for the tip of this release, and every one is paired
  within a run.** `bench/README.md` carries the method, the drift control and the
  attribution between the two changes; `bench/results.json` is the committed run
  they come from.

### Decision
