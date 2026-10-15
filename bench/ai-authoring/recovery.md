# Recovery, re-measured

The three rig defects behind recovery's first 2/20, found one per run, and the regrade. Part of the [authoring benchmark](README.md).

## Recovery, re-measured (#247)

The 2/20 in the [profile matrix](history.md) measured three defects in this rig, found one per run and each
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

`regrade.py` (deleted with the Python ban, #537) graded in place in the run's
trial directories under the system temp root, so it reproduced only on the host
that ran the trials; the verdicts it wrote are committed beside the run.
