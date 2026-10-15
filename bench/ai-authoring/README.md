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
| [The runner](runner.md) | What `run.py` did per trial before the Python ban deleted it, the metrics it recorded, and the lockfile every trial shared. |
| [Results through 2026-10-04](history.md) | The profile matrix and the four-arm comparison against the ecosystem standard, on aggregate and pipeline. |
| [Recovery, re-measured](recovery.md) | The three rig defects behind recovery's first 2/20, found one per run, and the regrade. |
| [The guarantee tasks and their controls](guarantees.md) | The `script!` arm, the three tasks whose oracles fail a solution lacking the guarantee, and the reference and mutant controls. |

## How to rerun

There is no rerun command. `run.py` and `regrade.py` were Python, and the Python
ban deleted them in #537 (`22a19af25`; the last copy of each is at `22a19af25^`).
No Rust runner replaces them, so nothing here reproduces the recorded runs
under `runs/`; they are the evidence. The tasks, oracles, `support/`,
`template/` and the reference and mutant solutions are Rust and remain.

The deleted runner made each model call closed-book under macOS `sandbox-exec`,
refusing on a host without it rather than running the model open-book against
the hidden oracle. Its trial template built `lgwks_bot` with `script` and
`process`, alike for every arm, so the arms differed only in the API sheet the
author was given.
