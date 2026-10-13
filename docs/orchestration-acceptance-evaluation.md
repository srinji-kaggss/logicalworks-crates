# Acceptance evaluation: evidence model and quality gates

Companion to [orchestration-acceptance.spec.md](orchestration-acceptance.spec.md),
which holds the T01–T36 falsifier rows and their per-revision evidence table.
This page holds how that evidence is graded, what the candidate tests do and
do not prove, and the evaluation that would accept the rows.

## Evidence model

Each requirement advances separately through planned -> present -> exercised
on exact revision -> independently evidenced -> accepted. Store command,
revision, feature set, platform, input identity, terminal status and artifacts.
A test name, comment, executable path or green compile alone proves none of
its asserted runtime outcomes. Keep failures and negative controls.

Documentation acceptance and implementation acceptance are different gates.
The spec PR may close only the documentation work; #87 stays open until the
required implementation/evidence exists. No reduced coverage floor, hidden
exclusions, ignored regression or invented success counters.

## Candidate tests

Candidate regressions now exist near T07, T09–T12, T14, T19 and T20, in
`crates/lgwks-bot/tests/it/durable_dispatch.rs` and
`crates/lgwks-bot/tests/it/process_ownership.rs`, and the design they enforce is
described in [effect-kernel.md](effect-kernel.md). **T22 additionally has
candidate tests** in `crates/lgwks-bot/tests/it/t22_process_surface.rs`
(`public_process_description_rejects_direct_execution_t22`,
`the_guaranteed_task_set_does_not_expose_detach_all` and
`supervisor_is_the_sanctioned_process_runner`). Those are unit and
integration journeys against the public surface. They are not this
specification's acceptance run: no row in the spec is marked accepted, and the
externally bounded subprocess and OS-containment evidence the rows require has
not been collected.

**A candidate test is presence, not acceptance, and T22 is where that is
easiest to get wrong.** `t22_process_surface.rs` type-checks single-file
consumers against the real workspace lockfile and asserts that `ProcessSpec`
exposes no `spawn`/`status`/`output` and that the task set has no `detach_all`.
What it does **not** do is the external observation the row asks for: it proves
a compile-time refusal, not that a sanctioned run was executed and observed. A
compile failure caused by missing dependencies is not a passing API-negative
test either — that failure mode is indistinguishable from the intended one
unless the probe also asserts the *positive* case compiles, which
`supervisor_is_the_sanctioned_process_runner` is for. The full
externally-evidenced campaign for T21 and T22 remains unaccepted.

## Developer-experience acceptance

Measure the complete task, not a screenshot of its shortest call site. Commit
one-time setup, registration, task body, adapters, helpers, configuration and
error-handling code. Distinguish reusable SDK/library code from task-specific
consumer code, and count newly written hidden helpers as consumer code.

For each canonical task, the common task body MUST require:

- zero manual retry/reap/readiness loops, permit/channel management or task
  ownership bookkeeping;
- zero explicit `Pin`, engine errors, `JoinSet`, task-plumbing `Arc` or lifetime
  coercions; ordinary domain types/async/await are allowed;
- no new Observe/Execute trait implementation merely to express a workflow
  already supported by installed adapters;
- a direct typed output or complete blocked/failure report, with no silent
  downgrade to grant-all, no verification omission and no detached work.

These are proposed acceptance constraints, not measured current scores.
A short API that forces every script to implement lifecycle logic fails.
An API with all the machinery hidden but no explainable failures also fails.

## Multi-axis experiment

Use held-out tasks for PR review, API aggregation, artifact conversion,
readiness-dependent service use and continuous watching. Compare the pinned
old API with the new facade under the same guarantees, adapter functionality,
inputs and hardware. Use both human authors and fixed AI model versions.
Report sample size, all attempts and uncertainty; do not select the best run.

| Axis | Units / measurement | Proposed gate |
|---|---|---|
| Task correctness | Required postconditions and coverage met per task | All deterministic acceptance cases pass; AI quality assessed separately |
| Safety/recovery | Unauthorized/duplicate effects; orphan resources | Zero in the stated fault suite, with uncertainty reported rather than guessed |
| Author burden | Distinct lifecycle decisions and manual orchestration sites | Zero in common task bodies; setup/adapter burden reported separately |
| Learnability | First working completion time; compiler/tool API repair count | Paired comparison; no numerical superiority claim without observed intervals |
| AI usability | Correct compiled scripts, tokens, invalid calls, interventions | Same-model controlled comparison with held-out tasks |
| Runtime cost | p50/p95/p99 latency, CPU, allocations, peak/steady RSS | Respect configured ceilings; justify any overhead against matching semantics |
| Scaling | Active/queued/result bytes and fairness under load | No growth outside declared budgets; no cross-run starvation |
| Observability | Attribution completeness, lost progress records, Unknown visibility | No lost terminal/uncertain state in the supported retention contract |
| Portability | Executed OS/feature/backend matrix | Only exercised combinations receive support claims |

Do not turn unlike axes into an average that lets performance compensate for
unsafe effects. Benchmark equal semantics: a bare loop without authority,
ledger and recovery is an overhead floor, not a competing durable engine.
HOL4 model theorems remain model evidence, not refinement proof of this Rust.

## Release handoff

Every acceptance row needs a test path and exact-head receipt before it becomes
accepted. Both now exist for all 36 rows: `docs/acceptance/t-rows.toml` is the
path, and `scripts/acceptance-receipts.py` writes the receipt for whichever
revision is checked out, so a stale receipt is visible rather than assumed. The
receipt can only lower a row's claim; promoting a row is a review decision, not
something a green run does.

The implementation PR must include compiled public examples and feature-matrix
checks under `AGENTS.md`, dependency-policy checks, docs version boundaries, and
a migration record. This specification change adds no production code and does
not claim the release gates have run.
