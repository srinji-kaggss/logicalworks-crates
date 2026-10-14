# Protocol

What a trial measures and does not, the five profiles, and the fairness rules every arm is held to. Part of the [authoring benchmark](README.md).

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
author something on *this* model, and the per-`(api, profile)` table in [history.md](history.md) is
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
