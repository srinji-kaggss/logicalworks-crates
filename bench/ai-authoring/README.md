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
| [The runner](runner.md) | `ai-authoring` per trial, the metrics it records, what it refuses and the lockfile every trial shares. |
| [Results through 2026-10-04](history.md) | The profile matrix and the four-arm comparison against the ecosystem standard, on aggregate and pipeline. |
| [Recovery, re-measured](recovery.md) | The three rig defects behind recovery's first 2/20, found one per run, and the regrade. |
| [The guarantee tasks and their controls](guarantees.md) | The `script!` arm, the three tasks whose oracles fail a solution lacking the guarantee, and the reference and mutant controls. |
| [The model matrix of 2026-10-17](matrix-2026-10-17.md) | The first `--matrix` run: one model, 165 records, passes per task and arm, and the 14 crashed records. |
| [The local-model matrices of 2026-10-18 and 2026-10-19](matrix-local-2026-10-19.md) | Qwen3.5-0.8B and Qwen2.5-coder 7B on Ollama at zero charge: 330 records, passes per task and arm, and what they do not show. |

## Which arm wins, per task

Measured so far, and no further. Full passes of 20 per arm and task are model
samples (two models, ten trials per cell, `runs/20261004T214357Z-models/` and
`runs/20261004T224716Z-models/`); the last three rows are reference solutions,
one per cell at HEAD (`runs/20261016T225212Z-dry/`), not model samples
([guarantees.md](guarantees.md)).

| task | old | new | fan | futures | winner and why |
|---|---|---|---|---|---|
| `aggregate` | 19/20 | 17/20 | 19/20 | 20/20 | `futures`: no pass-rate separation, but the fewest lines and tokens and 40/40 on-sheet. |
| `pipeline` | 19/20 | 20/20 | 18/20 | 19/20 | None on passes. `futures` is again the shortest. |
| `recovery` | no sheet | 16/20 | — | no sheet | No comparison: `old` and `futures` cannot express a durable resume. |
| `capture` | no sheet | all clauses | all clauses | 3/6 clauses | `lgwks_bot` arms: `futures` fails the flood, the orphan and the cancel, in fewer lines (36 against 44 for `new`). |
| `tenant-cache` | all clauses | all clauses | all clauses | 2/6 clauses | `lgwks_bot` arms: `futures` fails isolation, both refusals and the mixed thousand, in 19 lines (`fan` 24). |
| `durable-retry` | all clauses | all clauses | all clauses | 4/6 clauses | `lgwks_bot` arms: `futures` duplicates an effect across a drop and a `SIGKILL`, in 30 lines (`old` 38). |

The `script` arm passes every clause of the three guarantee tasks as a
reference too. Three models have authored in it, and in `lexicon` (a `script!`
author whose whole context is the generated `docs/script-lexicon.md`), five
trials per task and arm: muse 1.3 in the matrix of 2026-10-17
([matrix-2026-10-17.md](matrix-2026-10-17.md), `runs/20261017-matrix-models/`)
and two local models at zero charge, Qwen3.5-0.8B and Qwen2.5-coder 7B, 5 full
passes of 330 between them ([matrix-local-2026-10-19.md](matrix-local-2026-10-19.md)).
Not measured: a frontier closed model, ten trials per cell and human authors
(removed from #270's acceptance by the Director on 2026-10-18). The oracles of
the last three tasks are proven by negative controls (`ai-authoring --mutants`
and the `futures` references, [guarantees.md](guarantees.md)); no interval is
claimed over deterministic references.

## How to rerun

`ai-authoring` is a binary of `lgwks_deps`; [runner.md](runner.md) has the
flags, the exit codes and what each run writes. From the repository root:

```
cargo run --locked -p lgwks_deps --bin ai-authoring -- --dry-run --trials 1   # reference solutions, free
cargo run --locked -p lgwks_deps --bin ai-authoring -- --mutants               # negative controls, free
cargo run --locked -p lgwks_deps --bin ai-authoring -- --tables RUN_DIR        # tables with 95% intervals
cargo run --locked -p lgwks_deps --bin ai-authoring -- --matrix                # the model matrix
```

`--matrix` calls the model and costs money: the manifest's defaults are one model
(OpenCode's Go muse 1.3), six arms, six tasks less three excluded cells and five
profiles at one trial each, 165 trials. One trial per cell, because a second
puts the same prompt twice, and the Director's order of 2026-10-17 allows no
repetition in meaning. The run bills at most `token_budget` tokens, 9M, under
the Director's 10M: `budget.rs` refuses a call before it is made when its worst
case would pass the budget or when it repeats an earlier one. Its one run is
`runs/20261017-matrix-models/` (4,569,097 tokens billed). Every other model run
there was made by `run.py` (one regrade by `regrade.py`), and the Python ban deleted
both in #537 (`22a19af25`; the last copy of each is at `22a19af25^`); `regrade`
is not ported.

The model call is closed-book under macOS `sandbox-exec`, refusing on a host
without it rather than running the model open-book against the hidden oracle.
The trial template builds `lgwks_bot` with `script` and `process`, alike for
every arm, so the arms differ only in the API sheet the author is given.
