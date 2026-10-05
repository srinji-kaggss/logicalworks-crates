# Acceptance: simple authoring, complete orchestration

Status: **the acceptance run has executed; no row is accepted yet**. Every
T01–T36 falsifier now names the tests that address it, and
`scripts/acceptance-receipts.py` ran exactly those tests against this head. The
per-row result is in [Per-row evidence at this revision](#per-row-evidence-at-this-revision)
and the machine-readable record is in `evidence/`. What remains UNRUN is the
externally bounded subprocess and per-backend OS campaign, which is why no row
claims `accepted`. Tracker: [#87](https://github.com/srinji-kaggss/logicalworks-crates/issues/87),
release gate [#109](https://github.com/srinji-kaggss/logicalworks-crates/issues/109).
A passing existing workflow does not execute tests that have not been added.

Candidate regressions now exist near T07, T09–T12, T14, T19 and T20, in
`crates/lgwks-bot/tests/it/durable_dispatch.rs` and
`crates/lgwks-bot/tests/it/process_ownership.rs`, and the design they enforce is
described in [effect-kernel.md](effect-kernel.md). **T22 additionally has
candidate tests** in `crates/lgwks-bot/tests/it/t22_process_surface.rs`
(`public_process_description_rejects_direct_execution`,
`the_guaranteed_task_set_does_not_expose_detach_all` and
`supervisor_is_the_sanctioned_process_runner`). Those are unit and
integration journeys against the public surface. They are not this
specification's acceptance run: no row below is marked accepted, and the
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

## Falsifiers through public interfaces

| ID | Contract | Scenario and required observation |
|---|---|---|
| T01 | DX-01/03/04 | A one-shot task returns a typed value with one task declaration and call; no fake observer/tick loop or manual reaping. |
| T02 | DX-04/05 | Compile-fail source/condition/action mismatch and cross-schema result binding; equivalent correct local borrowed/non-Send program compiles and runs. |
| T03 | DX-05/LC-08 | Drop local composition while suspended; no borrowed child escapes. An independently timed child process detects a non-yielding callback without claiming in-process preemption. |
| T04 | LC-02/03 | Nested `all`/`map` at concurrency one finishes; parents waiting for children do not hoard execution permits. |
| T05 | LC-02/11 | Large outputs, full stderr and slow stdout consumer stay within declared retained-byte ceilings; malformed/truncated framed output is not parsed as success. |
| T06 | LC-03 | Saturated task A cannot starve independent B; one slow source does not block unrelated ready work. Use controlled completions, not sleeps as the oracle. |
| T07 | LC-04 | A succeeds with a new fingerprint, B fails the atomic observation group, then B recovers; A's uncommitted update still executes exactly once. Test initial and subsequent changes. |
| T08 | LC-04 | Disconnect, watch overflow, stale remote key and invalidation failure force refresh rather than permanent quiet state. |
| T09 | DX-07 | Identical event payloads with different event IDs both execute; duplicate delivery of the same event does not. Latest-state mode reports intentional supersession separately. |
| T10 | DX-06/LC-01 | A required predecessor fails, is abandoned, is skipped or becomes Unknown; dependent writes never run merely because it is no longer active. |
| T11 | LC-05/06 | A Unknown -> NotApplied -> B Unknown; delayed A NotApplied and A Applied cannot alter B. Duplicate A evidence stays attributable to A. |
| T12 | LC-05/06 | Wrong tenant/run/action/payload/revision/environment/epoch evidence is refused without mutation; checked counter exhaustion never aliases an issued identity. |
| T13 | LC-06/07 | Permanent policy refusal plus repeated NotApplied does not yield unlimited retries; authorized repair is distinct and root budgets remain charged. |
| T14 | LC-05/12/13 | Crash before intent ack, after ack before dispatch, after dispatch before response, after response before receipt and during recovery. No unknown effect is blindly replayed. |
| T15 | LC-12 | Definition/input/order/schema drift during replay returns a typed incompatibility before new effects; replayed model results incur no new model request. |
| T16 | LC-13 | Old worker returns after owner-epoch takeover; its receipt cannot authorize a stale dispatch or overwrite the current owner. |
| T17 | LC-08 | Drop a scoped waiter during an effect, then drain the still-owning host; report uncertainty and cleanup separately. Explicit durable submission outlives its client by contract. |
| T18 | LC-09 | Readiness failure, duplicate readiness, stale generation and failure after ready reach the right caller/dependants; no readiness sleeps in task code. |
| T19 | LC-10 | `sh -c 'sleep 60 & exit 0'`: parent exit is not reported as complete tree cleanup while descendants remain. Also test nonzero parent exit. |
| T20 | LC-10 | Abort/drop before manager future's first poll; spawned descendants remain owned and are stopped or cleanup is explicitly unresolved. |
| T21 | LC-10 | Signalling failure, PID/group reuse and child escape attempts cannot kill unrelated processes or produce false cleanup success. Execute per supported platform/backend. |
| T22 | DX-10 | External consumer cannot execute a public process description via `spawn`, `status`, `output`, mutable engine handle or Deref escape. Exercise the sanctioned path positively. |
| T23 | DX-08 | Multiple missing permissions/adapters/inputs yield one complete presently knowable NeedSet. Repair only publication authority; prior analysis is not rerun. |
| T24 | DX-08/LC-01 | Same repair ticket delivered twice, stale repair epoch and denied repair cannot widen authority or repeat an already completed effect. |
| T25 | DX-09 | Native task and supported wire form produce the same normalized operation trace for the same inputs. Unknown operations fail before execution. |
| T26 | AI-02/03 | Malformed model output and tool-output instruction injection cannot change trusted intent, install tools or obtain credentials. Sandbox escape refusals remain observable. |
| T27 | AI-04 | Context reset preserves completed work, user corrections, Unknown effects and evidence references; truncated data never becomes a full-coverage claim. |
| T28 | AI-04/05 | Parallel workers cannot read another tenant's same-digest artifact; conflicting writes are serialized while independent reads progress. |
| T29 | AI-06 | Repeated unchanged failure reaches finite typed intervention, not infinite tool/model repairs; new evidence is recorded and does not erase root spend. |
| T30 | AI-07 | Same request key plus different payload is Conflict; a duplicate identical request reattaches; distinct request keys are not silently collapsed. |
| T31 | PR-02/03/04 | Untrusted PR, pagination, rename, unavailable diff and build-script execution produce correct subject/coverage and sandbox decisions. |
| T32 | PR-05/06 | Head advances between analysis and publication; report reviewed A/current B or refuse per declared policy, never relabel evidence as B. |
| T33 | PR-07/08 | Provider accepts review but response is lost; reconciliation finds and verifies the exact review, or remains Unknown. No duplicate post. Test partially submitted reviews/comments. |
| T34 | PR-09/10 | Real readback proves exact IDs/state/subject/payload; lost read permission reports unverified/blocking state while retaining applied-effect evidence. |
| T35 | LC-01/11 | Process exit zero with invalid result, model says done without evidence, and successful draft with failed publish all report their distinct true outcomes. |
| T36 | DX-11 | Progress buffer overflow loses only declared detail and increments counters; authoritative terminal/Unknown state is still retrievable. A quiet client never requires a task-owned reporting loop. |

Test implementations must use externally bounded subprocesses for hangs and
crash paths. A timeout on the executor that may be blocked is not an independent
oracle. Run process containment checks on the actual supported OS backends;
compiling a cfg branch is not runtime evidence for it.

## Per-row evidence at this revision

Generated from [`docs/acceptance/t-rows.toml`](acceptance/t-rows.toml) and the run recorded in [`evidence/2f2bb0ee42bcf7fd4e12372212558e792957464c.json`](../evidence/2f2bb0ee42bcf7fd4e12372212558e792957464c.json): every test the map names, run once through `scripts/acceptance-receipts.py` at this exact head. A row is `exercised` only when a named test drives its falsifier; `present` means named tests cover part of the text and the gap column says what is not asserted. No row is `accepted`, and nothing here claims the externally bounded subprocess and per-backend campaign has run.

| ID | State | Tests | Gap |
|---|---|---|---|
| T01 | exercised | 3/3 | — |
| T02 | exercised | 4/4 | — |
| T03 | exercised | 3/3 | — |
| T04 | exercised | 5/5 | — |
| T05 | exercised | 5/5 | — |
| T06 | exercised | 6/6 | — |
| T07 | present | 4/4 | No named test drives A's new fingerprint and B's refused atomic observation group in one tick and then reads B's recovery with A's uncommitted update executed exactly once; the named tests cover per-chain baseline marks and cross-chain attribution separately. |
| T08 | exercised | 8/8 | — |
| T09 | exercised | 6/6 | — |
| T10 | present | 5/5 | The Unknown half is named; no single test drives all four predecessor states (Failed, Abandoned, Skipped, Unknown) for one dependent and asserts the dependent never runs for any of them. |
| T11 | present | 10/10 | No named test delivers A's evidence late, after A has already moved Unknown -> NotApplied, and asserts that B stays Unknown; the named tests settle A Unknown and prove isolation and duplicate idempotence separately. |
| T12 | exercised | 7/7 | — |
| T13 | exercised | 6/6 | — |
| T14 | present | 7/7 | Four of the five named boundaries are covered; the crash before intent ack (nothing committed) has no named test of its own. |
| T15 | exercised | 6/6 | — |
| T16 | exercised | 6/6 | — |
| T17 | present | 5/5 | Dropping the client and holding the uncertain attempt are named; no named test asserts that uncertainty and cleanup are reported as two independent facts from the same drained host. |
| T18 | exercised | 8/8 | — |
| T19 | exercised | 4/4 | — |
| T20 | exercised | 3/3 | — |
| T21 | present | 3/3 | Signalling only into the supervised group and a session escape are named; no named test exercises PID or process-group reuse, and the row is not executed per backend. |
| T22 | exercised | 8/8 | — |
| T23 | exercised | 7/7 | — |
| T24 | exercised | 5/5 | — |
| T25 | present | 5/5 | The shared trace for native and generated forms is named; no named test drives an unknown operation name and asserts refusal before any effect. |
| T26 | exercised | 7/7 | — |
| T27 | exercised | 2/2 | — |
| T28 | present | 5/5 | Conflicting writes and concurrent readers are named; no named test asserts that a worker in one tenant cannot read another tenant's artifact when both bytes are identical. |
| T29 | exercised | 4/4 | — |
| T30 | exercised | 6/6 | — |
| T31 | present | 11/11 | Untrusted PR, rename, unavailable diff, both diff ceilings and build-script execution are named; pagination of a list endpoint has no named test. |
| T32 | exercised | 3/3 | — |
| T33 | exercised | 7/7 | — |
| T34 | exercised | 4/4 | — |
| T35 | exercised | 4/4 | — |
| T36 | present | 3/3 | Overflow counting and terminal retrieval are named; no named test drives a client that reads no progress at all and asserts no task-owned reporting loop runs. |

At `2f2bb0ee`: 36 rows, 195/195 named tests passing, 26 rows exercised, 10 still partial, 43.578s wall. The ten partial rows are the work this run says is missing.

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
