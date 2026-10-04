# Fixed-model AI authoring/execution

Measure fixed AI model versions authoring held-out orchestration tasks against
the **old** `lgwks_bot::rt` surface and the **new** task/Host + `script` facade,
under the same inputs, budgets and hidden oracle. Part of issue #87 step 7 and
issue #152's `Fixed-model AI authoring/execution` row.

The question is author burden under a fixed capability: *given the same task and
the same oracle, how much orchestration must an author write, how many tokens
does it cost, how many compiler repairs does it take, and does the artifact pass
the behaviour the old surface makes the author hold in their head?*

This directory is its own Cargo workspace root (like `bench/async`), so the
estate's dependency contract — which discovers packages through Cargo's own
`workspace_members` — never sees it. It is not published and builds no estate
member.

## What this measures, and what it does not

**Measured, per trial (never averaged across tasks or models):** whether the
solution compiled, the number of compiler repairs, tokens in and out, model wall
time, compile wall time, consumer lines, orchestration sites, the per-test
oracle verdict, the oracle's wall time and peak RSS, and whether the drop clause
passed.

**The evaluators are five fixed user profiles, not humans.** Each is a fixed
persona preamble prepended to the *same* prompt skeleton, and the profile is the
only thing that differs between two cells of one `(model, api, task)`:

| profile | who it is |
|---|---|
| `first-time` | has never used this crate's orchestration API; reads the sheet literally |
| `expert-hurry` | knows the domain, is in a hurry, writes the shortest thing that might work |
| `anxious` | distrusts the API; wants explicit cleanup and explicit error handling |
| `misuser` | makes the plausible first mistake, then repairs from compiler/oracle feedback |
| `agent` | an AI coding agent that must emit only code |

What a profile comparison therefore **does** measure: how the same fixed model
version behaves under five different instructions, on the same task, with the
same sheet, the same hidden oracle, the same repair budget and the same
sandbox. A profile that raises repairs or lines is a profile that costs the
author something on *this* model, and the per-`(api, profile)` table below is
that comparison.

What it **does not** measure, and the earlier claim on this page was wrong about
exactly this: it is **not** a measurement of human authorability, and no person
was asked or consulted. A fixed model reading a persona is not a novice, an
expert under time pressure, or an anxious user; it is that model reading text
describing one. What the profiles buy is coverage of *instruction* conditions
over a model that cannot itself change — which is the part of the axis
[`docs/orchestration-acceptance.spec.md`](../../docs/orchestration-acceptance.spec.md)
calls learnability that a fixed-model rig can reach at all. It does not
substitute for the human half of that row, and no score here should be read as
one.

**Also not measured / not claimed:**

- **The sample is small.** The default is three trials per cell. Uncertainty is
  reported as the *full per-trial list*, not as a confidence interval: a small
  sample does not earn one.
- **This is not a leaderboard.** It compares two API surfaces of one crate
  across fixed model versions under five instructions; it says nothing about any
  other library.
- The absolute numbers are one host, one toolchain, one run.
- `orchestration_sites` is a token-count proxy, not a proof of complexity.

## The two API sheets and the fairness rules

Both sheets (`api/old.md`, `api/new.md`) are the same shape, the same length
budget (≤ 250 lines each), and each ends with one small compiling example of a
bounded fan-out. Each says the model may use only the items on that sheet plus
`std` and `ai_task_support`.

Fairness rules held constant across every cell:

- the same prompt skeleton and framing;
- the same API-sheet length budget;
- the same hidden oracle (the model never sees `tasks/*/oracle.rs`);
- the same repair budget (`--max-repairs`, default 4);
- the same compile and test commands, and the same serialization (one cargo
  process at a time under an `fcntl` lock).

The only thing that differs between the two arms is the API sheet.

## The tasks and their hidden oracles

Each `tasks/<task>/prompt.md` states the signature and the contract in
API-neutral words. Each `tasks/<task>/oracle.rs` is an integration test copied in
as `tests/oracle.rs`; it asserts on the harness instrumentation, one
`#[test]` per contract clause.

- **`aggregate`** — `solve(ids, fetch, deadline)`. Clauses: empty input → `Ok(0)`
  (1); at most 4 fetches in flight (2); a failure names its id (3); no fetch
  starts after the failure is observed (4); the overall deadline → `Deadline`
  (5); dropping the future leaves no fetch live (6).
- **`pipeline`** — `solve(stage, deadline)`. Clauses: success publishes the
  combined artifact (1); a failed `fetch_a` stops combine and publish and names
  the stage (2); the same for `fetch_b` (3); a slow stage → `Deadline` and no
  publish (4); dropping the future leaves no stage live (5).
- **`recovery`** — `recover(world, store_dir, deadline)`. Clauses: the work
  completes and the total is the sum (1); a second call finishes it without
  re-running a completed unit (2); the effect is applied exactly once across any
  number of calls (3); the overall deadline → `Deadline` with no unit left live
  (4); dropping the future leaves no unit live (5). The correct solution needs a
  host with a durable run store installed, a *derived* run identity that both
  attempts agree on, a task helper both attempts run, and a read-before-apply on
  the effect ledger.

  **`recovery` has no old-API arm, and the reason is architectural.** The old
  `lgwks_bot::rt` surface has no durable run store, no `remember`, no run
  identity and no resume, so it keeps no record of a completed unit to consult
  and cannot express clause 2 at all. A cell asking for one would measure a
  model guessing at a capability the sheet does not offer. The omission is
  recorded in every run's `summary.json` under `protocol.skipped_cells` rather
  than left as a missing row.

The oracles are deterministic: no counter is asserted on a wall-clock sleep. The
only waits are short ones that let a future begin, plus the 200 ms the drop
clause allows a cancelled body to be counted out. The `recovery` oracle's
interruption drops the first attempt's future once the requested number of unit
bodies has recorded — dropping rather than killing is deliberate, and is stated
at the call site: a killed process leaves the same durable evidence, and
arranging one would change what is measured from "does a resume work" to "does
this runner manage a subprocess".

## The harness-owned world: `support/`

`ai_task_support` is the instrumented world the solutions call. It is owned by
the harness, never by the solution.

- `Fetcher` (`Clone + Send + Sync + 'static`): `async fn fetch(&self, id) ->
  Result<u64, FetchError>`, returning `id * 3` after an async delay drawn from a
  seeded plan, failing for configured ids. Instrumentation via atomics: `live`
  (decremented by a drop guard, so a cancelled or aborted fetch counts out too),
  `max_live`, `started`, `started_after_first_failure`, `completed`, `failed`.
- `Stage` (`Clone + Send + Sync + 'static`): `async fn run(&self, name)` for
  `StageName::{FetchA, FetchB, Combine, Publish}`, with configurable failing and
  slow stages and the same live-count instrumentation. `Artifact::value()`;
  `Published: From<Artifact>`.
- `recovery::World`: the `recovery` task's durable units. `async fn unit(&Scope,
  u32) -> Result<u64, UnitError>` counts its own body *inside* the `remember`
  closure, so a replayed record does not increment the count — a replay and a
  re-run return the same value, so only the count separates them.
  `recovery::Ledger` is the effect: `applied(name)` is the read,
  `apply(name)` counts every call including duplicates, and it refuses nothing —
  a harness that refused a duplicate would make the no-duplicate clause
  unfalsifiable, which is what that clause exists to detect.
- Constructors for the oracle to configure fault plans: `failing(..)`,
  `delay(..)`, `uniform_delay(..)`, `slow(..)`.

The delay plan is a seeded SplitMix64, so a run replays exactly. Sleeping is
`lgwks_bot::rt::time::sleep` throughout; there is no `std::thread::sleep`.

## The runner: `run.py`

Python standard library only.

```
python3 bench/ai-authoring/run.py --dry-run --trials 1   # plumbing, no model
python3 bench/ai-authoring/run.py --mutants              # negative controls
python3 bench/ai-authoring/run.py                        # the real matrix
```

Per trial it makes an empty scratch directory holding only the prompt text
files, runs the model CLI with the prompt on stdin and a 900 s timeout, parses
the final NDJSON result line, extracts the last ` ```rust ` block, copies
`template/` into `<work>/<trial-id>/`, writes `src/lib.rs`, and builds with
`cargo build --locked --offline` against a shared `CARGO_TARGET_DIR`. On a failed
compile it re-prompts with the original prompt, the current code and the first 80
lines of compiler errors, counting one repair, up to `--max-repairs`. If it
compiles it copies the hidden oracle to `tests/oracle.rs` and runs
`cargo test --locked --offline -- --test-threads=1`. Every cargo process is
serialized by an `fcntl` lock over `<work>/.cargo-gate.lock`.

Every run is new evidence and never replaces an earlier run. A run creates a
fresh work root, `$TMPDIR/lgwks-ai-authoring-<UTC stamp>-<mode>/`, and a fresh
results directory, `bench/ai-authoring/runs/<UTC stamp>-<mode>/`; either one
already existing is a refusal, and nothing is deleted. Each trial's `lib.rs`,
raw NDJSON, build log and test log stay under `<work root>/<trial-id>/`. One JSON
line per trial goes to `<results dir>/results.jsonl`, and `<results dir>/summary.json`
holds the per-`(model, api, task)` aggregate with every per-trial value listed.

**Per-trial package names.** Every trial crate is built under a unique package
name (`ai_trial_<trial-id>`). A constant name makes cargo's `-C metadata`
identical across trials, so a shared target directory would let one trial's
`oracle` test binary stand in for another's — observed during development and
fixed here. The committed oracles name the crate `ai_trial`; the runner
substitutes the trial's real name when it copies them in.

### Metrics

- `consumer_lines` — non-blank, non-comment lines of `src/lib.rs`.
- `orchestration_sites` — occurrences in `src/lib.rs` of the author-burden proxy
  tokens, documented in `run.py` and recorded per trial in
  `orchestration_site_tokens`:
  `JoinSet`, `spawn(`, `Semaphore`, `CancellationToken`, `select!`, `loop {`,
  `Arc<`, `Mutex`, `channel(`, `abort(`.
  It is a proxy, not a proof of complexity: a token can appear in a comment or a
  type name. It is reported with `consumer_lines` so a reader can see both.
- `tokens_in` / `tokens_out` / `model_wall_ms` — summed over every model call in
  the trial (the initial attempt and every repair).
- `compile_wall_ms` — summed over every cargo build in the trial.
- `oracle_wall_ms` — the wall time of the `cargo test` invocation that runs the
  oracle, taken under `/usr/bin/time -l`. The compile is already done by that
  point, so this is the oracle's own cost rather than a build's.
- `oracle_peak_rss_bytes` — that same process's maximum resident set size, in
  bytes, or `null` on a host with no `/usr/bin/time`. `null` means *not
  measured*, never *measured as zero*. The parser reads both field orders the
  flag takes: macOS prints `67911680  maximum resident set size` (bytes) and GNU
  prints `Maximum resident set size (kbytes): 66443` (kibibytes, scaled to
  bytes).
- `cleanup_ok` — the task's drop clause, read by name from
  `DROP_CLAUSE_BY_TASK`. A crate that never compiled reports `false` rather than
  a cleanup result, because "the drop test did not run" is not "cleanup was
  fine".

### The lockfile

`bench/ai-authoring/Cargo.toml` is a workspace root with `members = ["support",
"template"]`. `bench/ai-authoring/Cargo.lock` is generated once
(`cargo generate-lockfile --offline`) for that exact graph — including the
`script` feature's `lgwks_macros`/`syn`/`quote` edges — and the runner copies it
into each trial, rewriting the root package name to the trial's. `cargo
build --locked --offline` therefore works from the already-downloaded registry
cache with no resolution.

## Results: the profile matrix (`runs/20261003T154213Z-models/`)

Two models x two APIs x three tasks (recovery is new-API only) x five profiles x
two trials: 100 trials, run once, `--parallel 6`, 8,085 s wall, harness peak RSS
1,031,733,248 bytes (`/usr/bin/time -l`). "Full pass" is compiled *and* every
oracle clause passed; repairs are the per-trial compiler-repair counts, sorted.
The per-`(model, api, task, profile)` rows are in `results.jsonl`, and
`summary.json` carries the same grouped by profile.

| model | api | task | full pass | compiled | repairs (per trial) |
|---|---|---|---|---|---|
| `deepseek-v4.1-flash` | new | aggregate | 10/10 | 10/10 | 0 0 0 0 0 0 0 0 1 1 |
| `deepseek-v4.1-flash` | new | pipeline | 10/10 | 10/10 | 0 0 0 0 0 0 0 0 1 1 |
| `deepseek-v4.1-flash` | new | recovery | 2/10 | 3/10 | 1 2 4 4 4 4 4 4 4 4 |
| `deepseek-v4.1-flash` | old | aggregate | 10/10 | 10/10 | 0 0 0 0 0 0 0 0 0 1 |
| `deepseek-v4.1-flash` | old | pipeline | 10/10 | 10/10 | 0 0 0 0 0 0 0 1 1 2 |
| `space-bunny-alpha` | new | aggregate | 6/10 | 9/10 | 0 0 1 1 1 1 2 3 3 4 |
| `space-bunny-alpha` | new | pipeline | 6/10 | 7/10 | 0 0 0 1 1 2 2 3 4 4 |
| `space-bunny-alpha` | new | recovery | 0/10 | 3/10 | 2 3 4 4 4 4 4 4 4 4 |
| `space-bunny-alpha` | old | aggregate | 8/10 | 10/10 | 0 0 0 0 0 0 0 1 2 2 |
| `space-bunny-alpha` | old | pipeline | 7/10 | 10/10 | 0 0 0 0 0 0 0 1 3 3 |

What the run shows, stated at the level the data supports:

- **Recovery's 2/20 in this run was the harness, not the API.** It is
  superseded by the section below: the sheet omitted the durable surface and
  the prompt omitted the world's width, and once both were on the page and the
  oracle stopped requiring one unit at a time, recovery reached 16/20.
- **On aggregate and pipeline the new API matches the old for deepseek** (40/40
  vs 40/40 full passes) **and trails it for space-bunny** (12/20 vs 15/20).
- **The profile axis did not separate the cells** at two trials each: no profile
  is consistently better or worse across models and tasks. Two trials per cell
  cannot show a difference of that size, and this does not claim one.
- **Two trials hung the oracle for its 1,800 s timeout** (space-bunny, new,
  aggregate/expert-hurry t1 and pipeline/agent t1). They are recorded as crashed
  trials (`trial_id: "crashed"`, with the timeout in `error`) and count as
  failures above.

## Results: four arms, including the ecosystem standard (`runs/20261004T214357Z-models/`, `runs/20261004T224716Z-models/`)

Two models x four API sheets x two tasks (aggregate, pipeline) x five profiles x
two trials: 160 trials, every one recorded, none selected. The arms are the old
`lgwks_bot::rt` surface (`old`), the `script`/`task` facade (`new`), the facade
plus the one-call `FanOut` (`fan`, `api/fan.md`), and the credible alternative a
Rust author would otherwise reach for: `futures` stream and future combinators
over a `lgwks_bot` runtime and timer (`futures`, `api/futures.md`). The oracle,
prompt skeleton, repair budget, sandbox and profiles are identical across arms;
only the API sheet differs. The `futures` arm was run after the others, with the
harness fix below, against the same crate sources for everything the arms share.

| arm | trials | full pass | sheet API used | hand-rolled `std::thread` | mean repairs | mean lines | mean tokens out |
|---|---|---|---|---|---|---|---|
| `old` | 40 | 38/40 | 38/40 | 1/40 | 0.85 | 68 | 17,396 |
| `new` | 40 | 37/40 | 12/40 | 24/40 | 0.75 | 140 | 25,859 |
| `fan` | 40 | 37/40 | 31/40 | 8/40 | 1.07 | 70 | 15,762 |
| `futures` | 40 | 39/40 | 40/40 | 0/40 | 0.15 | 37 | 6,399 |

| model | task | old | new | fan | futures |
|---|---|---|---|---|---|
| `deepseek-v4.1-flash` | aggregate | 9/10 | 10/10 | 10/10 | 10/10 |
| `deepseek-v4.1-flash` | pipeline | 10/10 | 10/10 | 10/10 | 10/10 |
| `space-bunny-alpha` | aggregate | 10/10 | 7/10 | 9/10 | 10/10 |
| `space-bunny-alpha` | pipeline | 9/10 | 10/10 | 8/10 | 9/10 |

What this supports, and what it does not:

- **Pass rate does not separate the arms.** 37 to 39 of 40 each; at ten trials per
  cell a difference of that size is noise, and this does not claim one.
- **The `new` facade was abandoned more often than it was used.** Only 12 of 40
  `new` solutions used `script` at all; 24 of 40 wrote their own `std::thread`,
  `Condvar` and `Waker` executor instead. The facade's `Scope`, `Tenant` and
  shared-cell ceremony costs about twice the lines and tokens of any other arm.
  That is the measured reason `FanOut` exists.
- **`FanOut` moved adoption from 12/40 to 31/40** and cut lines from 140 to 70
  and tokens by 39%. It did not remove the escape: 8 of 40 still left the sheet.
- **The ecosystem standard is the better authoring surface, on this measure.**
  `futures` combinators: 39/40, 40/40 used the sheet, no threads, 0.15 repairs,
  37 lines, 6.4k tokens. Nothing here beats that for the two tasks measured, and
  the score for AI usability does not claim to.
- **What this does not measure** is what the oracle never asks: tenant-scoped
  step identities, located errors, a durable record that survives a kill, and
  process-group containment. Those are where `lgwks_bot` differs from `futures`,
  and they need tasks whose oracle asks for them. `recovery` is the one that does,
  and it has no `futures` arm because the combinators have no durable surface.
- **Off-sheet code is the dominant failure.** Five of the eight failed solutions
  left the sheet for a hand-rolled executor. No crate surface can prevent
  `std::thread`; only the oracle catches it.
- **The harness no longer hangs.** A timeout kills the whole process group, and a
  hung oracle is recorded as that trial's failure. No oracle timed out in these
  160 trials and no process was left running.

## Recovery, re-measured (#247)

The 2/20 above measured three defects in this rig, found one per run and each
fixed before the next. No model call was repeated to grade a fix that needed none.

| run | what was wrong | full pass | compiled |
|---|---|---|---|
| `20261003T154213Z-models` | `api/new.md` had no `run_store`, `RunId::from_hex`, `Host::resume` or `remember`; the models guessed `Store`, `RunStore` | 2/20 | 6/20 |
| `20261003T181420Z-models` | sheet fixed; the prompt never named `World::width()`, so solutions probed indices past the end and hit the deadline, and six deepseek sessions spent their turns searching for it | 0/20 | 10/20 |
| `20261003T190050Z-models` | prompt fixed; the oracle asserted *exactly two* unit bodies at the interruption, which only a one-at-a-time solution meets — all 12 compiled failures were that one assertion | 4/20 | 16/20 |
| `20261003T190050Z-models/regraded.jsonl` | the same 16 compiled solutions (each `lib.rs` byte-identical to its receipt) re-graded by `regrade.py` against the oracle that asserts *at least two* | **16/20** | 16/20 |

The regrade is equivalent to a re-run because the oracle is hidden: a model's
repairs only ever see compiler output, so the code a corrected oracle grades is
the code the model would have written anyway. The four that never compiled stay
failures. The controls were re-run on the corrected oracle
(`20261003T193523Z-dry`, and the mutants): every reference passes every clause,
and `mutant-recovery` — a fresh store per attempt, so the resume re-runs every
unit — still fails `a_resume_does_not_rerun_a_completed_unit`, so the relaxed
count did not stop the clause catching a solution that re-runs.

`regrade.py` grades in place in the run's trial directories under the system
temp root, so it reproduces only on the host that ran the trials; the verdicts
it wrote are committed beside the run.

## Proof the plumbing works: references and mutants

`reference/<api>-<task>.rs` are hand-written correct solutions, one per API and
task. They are the dry-run's canned solutions and the proof that each task is
solvable in each API. `recovery` has only `new-recovery.rs`, for the reason given
above. Every reference compiles and passes every oracle clause of its task.

`reference/mutant-<task>.rs` are deliberately wrong inputs, one per task. Each is
its reference — placed beside the mutant as `mod reference`, never forked — plus
exactly one mutation, so the two cannot drift. The oracle must fail each for its
intended clause:

| mutant | mutation | clause it must fail | also fails |
|---|---|---|---|
| `mutant-aggregate` (unbounded fan-out) | `join_all_bounded(usize::MAX, …)` | `at_most_four_fetches_are_in_flight` | `the_overall_deadline_is_honoured` |
| `mutant-pipeline` (detached spawn, leaked task set) | work handed to a process-lifetime `JoinSet` | `dropping_the_future_leaves_no_stage_live` | — |
| `mutant-recovery` (a private store per attempt) | each attempt books a fresh store directory, so the resume finds nothing | `a_resume_does_not_rerun_a_completed_unit` | — |

The mutant sources are not estate code and are never built by the estate
workspace; the pipeline mutant deliberately leaks a `JoinSet` with `Box::leak`
and the recovery mutant deliberately ignores its `store_dir` argument. Those are
the defects, not examples.

## How to rerun

```sh
python3 bench/ai-authoring/run.py --dry-run --trials 1      # every reference cell
python3 bench/ai-authoring/run.py --mutants                 # one negative control per task
AI_AUTHORING_CMD=<cli> python3 bench/ai-authoring/run.py \
    --models stealth/space-bunny-alpha,deepseek/deepseek-v4.1-flash \
    --apis old,new --tasks aggregate,pipeline,recovery \
    --profiles first-time,expert-hurry,anxious,misuser,agent \
    --trials 2 --parallel 6
```

`--profiles` defaults to all five and `--tasks` to all three. The default model
ids are `stealth/space-bunny-alpha` and `deepseek/deepseek-v4.1-flash`. A model
call must be closed-book and this runner enforces that with macOS
`sandbox-exec`, so a host without it refuses rather than running the model
open-book against the hidden oracle.
