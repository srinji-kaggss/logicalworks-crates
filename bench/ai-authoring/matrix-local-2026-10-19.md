# Two local-model matrices of 2026-10-18 and 2026-10-19

Two more runs of `ai-authoring --matrix`, each one model on the same host's
Ollama at zero charge, through the same runner as
[the muse matrix](matrix-2026-10-17.md): six arms, six tasks less the three
excluded cells, five profiles, one trial per cell, 165 records each, the same
closed-book `sandbox-exec` call, prompt skeleton, hidden oracle and repair
budget (four). The tables below are `ai-authoring --tables` on each run
directory, read for the full-pass rows.

## What ran

| | Qwen3.5-0.8B | Qwen2.5-coder 7B |
|---|---|---|
| Model tag | `qwen3.5:0.8b` | `qwen2.5-coder:7b` |
| Run directory | `~/.local/state/lwc-ai-authoring/matrix-ollama-qwen35-08b-20261018/` | `~/.local/state/lwc-ai-authoring/matrix-ollama-qwen25-coder-7b-20261019/` |
| Commit | `609fb2c72` | `609fb2c72` |
| Records | 165, none crashed | 165, none crashed |
| Compiled | 2 | 7 |
| Full passes | 0 | 5 |
| Repairs, all records | 236 | 262 |
| Tokens (in + out) | 3,446,753 | 1,846,071 |
| Charge | none | none |

The model command was a 23-line shell script that posted the prompt to
Ollama's `/api/chat` (`think: false`, `num_ctx` 32768, `temperature` 0.2, a
fixed seed, `keep_alive` 24 h) and printed the one result line the runner
reads. `llama-server` ran one slot, so the runner's four parallel calls queued.
The 0.8B run was interrupted once, when a probe of the 7B model beside it
wedged Ollama; the server was restarted and the run resumed on the same
results directory (`--resume`, one record kept, 164 run). The 7B run was
launched once and ran to its end. The run directories hold the generated
crates (`results.jsonl` carries each `lib.rs`), so they stay outside the
repository.

## Full passes per (task, arm), 95% Wilson over five

Qwen3.5-0.8B. A pass is every oracle clause.

| task | fan | futures | lexicon | new | old | script |
|---|---|---|---|---|---|---|
| aggregate | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |
| pipeline | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |
| recovery | 0/5 | — | 0/5 | 0/5 | — | 0/5 |
| capture | 0/5 | 0/5 | 0/5 | 0/5 | — | 0/5 |
| tenant-cache | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |
| durable-retry | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |

Qwen2.5-coder 7B.

| task | fan | futures | lexicon | new | old | script |
|---|---|---|---|---|---|---|
| aggregate | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |
| pipeline | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |
| recovery | 0/5 | — | 0/5 | 0/5 | — | 0/5 |
| capture | 0/5 | 0/5 | 0/5 | 0/5 | — | 0/5 |
| tenant-cache | 1/5 | 0/5 | 3/5 | 0/5 | 0/5 | 1/5 |
| durable-retry | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 | 0/5 |

Every 0 of 5 is the interval [0, 43]; 1 of 5 is [4, 62]; 3 of 5 is [23, 88].
All intervals overlap, so no row separates two arms at 95%.

## The compiled records

- 0.8B: `aggregate`/`new` and `aggregate`/`lexicon`, one each, both on
  `expert-hurry`; neither passed every clause.
- 7B: `tenant-cache` in `fan` (`expert-hurry`, 6 of 6 clauses), `lexicon` (three
  profiles, 6 of 6 each) and `script` (one profile, 6 of 6); `pipeline`/`futures`
  (`anxious`, 4 of 5, failing the second failed-fetch clause);
  `durable-retry`/`futures` (`expert-hurry`, 1 of 6).

The other 158 records of each run stopped at the compile step. They show a
floor for these two models on this sheet and say nothing of the arms.

## What this does not show

- No ranking of the arms. At five trials per cell and at most five passes in
  165, the intervals contain every difference the matrix could have found.
- No frontier closed model, no ten trials per cell and no human authors; the
  Director's amendment of 2026-10-18 on #270 removes them from the acceptance
  rather than claiming them.
- Why a `lexicon` author passed `tenant-cache` three times is not read here:
  the matrix does not read the solutions clause by clause.
- The records are not in `runs/`, so a reader cannot rerun `--tables` from the
  repository; the run directories are on the host that made them.

## Together with the muse matrix

Three model samples now stand behind the guarantee tasks and the `script` arm:
the muse 1.3 matrix (4,569,097 tokens billed) and these two (5,292,824 tokens,
no charge). Their sum, 9,861,921 tokens, is under the Director's 10M ceiling
for #270.
