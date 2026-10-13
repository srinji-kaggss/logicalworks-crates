# Results through 2026-10-04

The profile matrix and the four-arm comparison against the ecosystem standard, on aggregate and pipeline. Part of the [authoring benchmark](README.md).

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
  superseded by [recovery.md](recovery.md): the sheet omitted the durable surface and
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
harness fix in [recovery.md](recovery.md), against the same crate sources for everything the arms share.

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
