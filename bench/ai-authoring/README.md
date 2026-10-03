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
time, compile wall time, consumer lines, orchestration sites, and the per-test
oracle verdict.

**Not measured / not claimed:**

- **No human evaluators were run.** Judging authorability by people needs the
  Director. This is a mechanical proxy: the model authors, the compiler and a
  hidden oracle judge.
- **The sample is small.** The default is three trials per cell. Uncertainty is
  reported as the *full per-trial list*, not as a confidence interval: a small
  sample does not earn one.
- **This is not a leaderboard.** It compares two API surfaces of one crate
  across fixed model versions; it says nothing about any other library.
- The absolute numbers are one host, one toolchain, one run.

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

The oracles are deterministic: no counter is asserted on a wall-clock sleep. The
only waits are short ones that let a future begin, plus the 200 ms the drop
clause allows a cancelled body to be counted out.

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

### The lockfile

`bench/ai-authoring/Cargo.toml` is a workspace root with `members = ["support",
"template"]`. `bench/ai-authoring/Cargo.lock` is generated once
(`cargo generate-lockfile --offline`) for that exact graph — including the
`script` feature's `lgwks_macros`/`syn`/`quote` edges — and the runner copies it
into each trial, rewriting the root package name to the trial's. `cargo
build --locked --offline` therefore works from the already-downloaded registry
cache with no resolution.

## Proof the plumbing works: references and mutants

`reference/<api>-<task>.rs` are hand-written correct solutions, one per API and
task. They are the dry-run's canned solutions and the proof that each task is
solvable in each API. All four compile and pass every oracle clause.

`reference/mutant-<task>.rs` are deliberately wrong inputs, one per task. The
oracle must fail each for its intended clause:

| mutant | intended clause it must fail | also fails |
|---|---|---|
| `mutant-aggregate` (unbounded fan-out) | `at_most_four_fetches_are_in_flight` | `the_overall_deadline_is_honoured` |
| `mutant-pipeline` (detached spawn, leaked task set) | `dropping_the_future_leaves_no_stage_live` | — |

The mutant sources are not estate code and are never built by the estate
workspace; the pipeline mutant deliberately leaks a `JoinSet` with `Box::leak`.
That is the defect, not an example.

## How to rerun

```sh
python3 bench/ai-authoring/run.py --dry-run --trials 1      # 4 reference cells
python3 bench/ai-authoring/run.py --mutants                 # 2 negative controls
AI_AUTHORING_CMD=<cli> python3 bench/ai-authoring/run.py \
    --models stealth/space-bunny-alpha,deepseek/deepseek-v4.1-flash \
    --apis old,new --tasks aggregate,pipeline --trials 3 --parallel 4
```

The default model ids are `stealth/space-bunny-alpha` and
`deepseek/deepseek-v4.1-flash`; the reviewer runs the real matrix, which needs a
model credential this rig does not hold.
