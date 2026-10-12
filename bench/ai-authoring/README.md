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

## Where each part is

| Document | Covers |
|---|---|
| [Protocol](protocol.md) | What a trial measures and does not, the five profiles, and the fairness rules every arm is held to. |
| [Tasks and the harness-owned world](tasks.md) | Each task's contract and hidden oracle clauses, and the instrumented `support/` crate the solutions call. |
| [The runner](runner.md) | What `run.py` does per trial, the metrics it records, and the lockfile every trial shares. |
| [Results through 2026-10-04](history.md) | The profile matrix and the four-arm comparison against the ecosystem standard, on aggregate and pipeline. |
| [Recovery, re-measured](recovery.md) | The three rig defects behind recovery's first 2/20, found one per run, and the regrade. |
| [Guarantee tasks under a token ceiling, 2026-10-12](results-2026-10-12.md) | Meta Muse on `futures`, `script` and `lexicon` across the three guarantee tasks, at 455,548 tokens against a 1.2M ceiling. No arm wins. |
| [The guarantee tasks and their controls](guarantees.md) | The `script!` arm, the three tasks whose oracles fail a solution lacking the guarantee, and the reference and mutant controls. |

## How to rerun

```sh
python3 bench/ai-authoring/run.py --dry-run --trials 1      # every reference cell
python3 bench/ai-authoring/run.py --mutants                 # one negative control per task
AI_AUTHORING_CMD=<cli> python3 bench/ai-authoring/run.py \
    --models stealth/space-bunny-alpha,deepseek/deepseek-v4.1-flash \
    --apis old,new,script --tasks aggregate,pipeline,recovery,capture,tenant-cache,durable-retry \
    --profiles first-time,expert-hurry,anxious,misuser,agent \
    --trials 2 --parallel 6
```

`AI_AUTHORING_CMD=opencode` drives OpenCode instead (`opencode.py`), with
`--models opencode/<id>`: each call gets its own home, temp and XDG directories
inside the trial scratch, so it reads no user rules, session or sibling answer.

`--profiles` defaults to all five and `--tasks` to all six. A model
call must be closed-book and this runner enforces that with macOS
`sandbox-exec`, so a host without it refuses rather than running the model
open-book against the hidden oracle. The trial template builds
`lgwks_bot` with `script` and `process`, alike for every arm, so the arms
differ only in the API sheet the author is given.
