# The guarantee tasks and their controls

The `script!` arm, the three tasks whose oracles fail a solution lacking the guarantee, and the reference and mutant controls. Part of the [authoring benchmark](README.md).

## Results: the `script!` arm and the guarantee tasks v2 (`runs/20261008T003455Z-dry/`, `runs/20261008T005932Z-dry/`)

The frontier follow-up to the [four-arm table](history.md) (#270): a fifth arm for the
`script!` language (`api/script.md`, added with `try_join!`/`join!` notes on
the old, new and fan sheets documenting why those macros do not exist there),
and three held-out tasks whose oracles fail any solution lacking the
guarantee. The `futures` arm is the same ecosystem comparator, scored as the
negative control it is: a plain `Command::output`, a resource-keyed map with
no token check, and an append-without-readback.

140 dry trials (reference solutions, ten per cell, first profile only — a canned
solution never reads the persona, so five copies would be one trial wearing
five labels). Every trial compiled; every `lgwks_bot` cell passed every clause
of its task on all ten trials; every `futures` cell failed exactly the
guarantee clauses on all ten trials, with zero variance anywhere:

| arm | capture (6 clauses) | tenant-cache (6) | durable-retry (6) | consumer lines (c/t/d) |
|---|---|---|---|---|
| `script` | 10/10 | 10/10 | 10/10 | 62 / 31 / 43 |
| `new` | 10/10 | 10/10 | 10/10 | 40 / 37 / 45 |
| `fan` | 10/10 | 10/10 | 10/10 | 53 / 17 / 47 |
| `old` | — (no process spawn on the sheet) | 10/10 | 10/10 | — / 26 / 30 |
| `futures` | 0/10 (3/6 each) | 0/10 (2/6 each) | 0/10 (4/6 each) | 28 / 19 / 22 |

The `futures` failures, identical on all ten trials per cell, are the negative
control for every new oracle:

- `capture`: fails the 70 MB flood (retains everything), the orphaned
  grandchild (outlives the call) and the promptness of the cancel (blocks
  through the sleep); passes the small output, the spawn refusal and the slow
  deadline verdict.
- `tenant-cache`: fails the two-tenant isolation, both token refusals and the
  thousand-call mix; passes consistency and post-drop usability.
- `durable-retry`: fails the drop-then-recover duplicate and the kill-then-
  recover duplicate; passes the fresh call, the known status and the deadline.

What this supports, stated per task including where `lgwks_bot` does not win:

- **`capture`: every `lgwks_bot` arm passes everything; `futures` keeps 3 of
  6.** The guarantee holds on all four surfaces that expose it. On lines the
  order inverts: `futures` 28, `new` 40, `fan` 53, `script` 62. The DSL costs
  the most lines here and buys nothing over the facade on this task — the
  supervisor does the owning in all four.
- **`tenant-cache`: every `lgwks_bot` arm passes everything; `futures` keeps 2
  of 6.** `fan` wins lines outright at 17 (one `FanOut` over one read), ahead
  of `futures` at 19 — the one task where a facade arm is also the shortest.
- **`durable-retry`: every `lgwks_bot` arm passes everything; `futures` keeps 4
  of 6.** The two failures are the two that matter: exactly-once across a
  drop and across a `SIGKILL`.
- **No variance, no intervals.** Ten identical verdicts per cell is a
  deterministic reference replayed, not ten model samples: confidence
  intervals over model behaviour need the model matrix, whose command is in the
  [README](README.md#how-to-rerun). Nothing here claims one.
- **The harness leaves no orphan process.** `ps` after each run shows no
  oracle, solution or sleep process; the capture oracle asserts it per trial
  under per-process markers.

## Proof the plumbing works: references and mutants

`reference/<api>-<task>.rs` are hand-written correct solutions, one per API and
task the sheet can express. They are the dry-run's canned solutions and the
proof that each task is solvable in each API. `recovery` has only
`new-recovery.rs` (and now `script-recovery.rs`), for the reason given in [tasks.md](tasks.md);
`capture` has no `old-capture.rs`, because the old sheet offers no supervised
process spawn; `recovery` has no `futures-recovery.rs`, because the combinators
have no durable surface. Every reference compiles and passes every oracle
clause of its task — 140 dry trials across the two v2 runs, every one green.

The tasks' fixed vocabularies (`CaptureError`/`CaptureResult`, `CacheError`,
`RecoverError`, and now `aggregate::SolveError`, `pipeline::PipelineError`
and `recovery::RecoveryError`) live in `support/` with one definition per
task, which the references re-export: the prompt fixes these items, so a copy
per reference file is how the copies drift. Model authors still write their
own from the prompt.

`reference/mutant-<task>.rs` are deliberately wrong inputs, one per task. Each is
its reference — placed beside the mutant as `mod reference`, never forked — plus
exactly one mutation, so the two cannot drift. The oracle must fail each for its
intended clause:

| mutant | mutation | clause it must fail | also fails |
|---|---|---|---|
| `mutant-aggregate` (unbounded fan-out) | `join_all_bounded(usize::MAX, …)` | `at_most_four_fetches_are_in_flight` | `the_overall_deadline_is_honoured` |
| `mutant-pipeline` (detached spawn, leaked task set) | work handed to a process-lifetime `JoinSet` | `dropping_the_future_leaves_no_stage_live` | — |
| `mutant-recovery` (a private store per attempt) | each attempt books a fresh store directory, so the resume finds nothing | `a_resume_does_not_rerun_a_completed_unit` | — |
| `mutant-capture` (unbounded retain) | the whole output is the head and `truncated` is always false | `a_flood_past_sixty_four_mebibytes_stays_within_its_ceiling` | `a_completed_run_leaves_no_orphan_behind`, `cancelling_leaves_no_descendant` |
| `mutant-tenant-cache` (resource-keyed map) | the token is checked but the key carries no tenant | `two_tenants_on_one_resource_stay_isolated` | `a_thousand_mixed_calls_all_resolve_their_own_value`, and `a_wrong_token_is_refused_and_caches_nothing` (beta's authorised read finds alpha's earlier entry under the same key — the same leak through shared test state) |
| `mutant-durable-retry` (checked yet duplicated) | the record is read and its answer discarded; every call appends | `dropping_mid_wait_applies_nothing_twice` | `kill_mid_effect_recovers_exactly_once` |

The mutant sources are not estate code and are never built by the estate
workspace; the pipeline mutant deliberately leaks a `JoinSet` with `Box::leak`
and the recovery mutant deliberately ignores its `store_dir` argument. Those are
the defects, not examples.
