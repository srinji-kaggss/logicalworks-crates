# The runner

`ai-authoring`, a binary of `lgwks_deps`, runs the benchmark: what it does per trial, the metrics it records, what it refuses, and the lockfile every trial shares. Part of the [authoring benchmark](README.md).

## Commands

Run from the repository root (`--bench` defaults to `bench/ai-authoring`). The bare `cargo run -p lgwks_deps` runs `lgwks-deps`, so name the binary:

```
cargo run --locked -p lgwks_deps --bin ai-authoring -- --dry-run --trials 1   # plumbing, no model: free
cargo run --locked -p lgwks_deps --bin ai-authoring -- --mutants               # negative controls: free
cargo run --locked -p lgwks_deps --bin ai-authoring -- --tables RUN_DIR        # README tables, read-only: free
cargo run --locked -p lgwks_deps --bin ai-authoring -- --matrix                # calls the models: costs money
```

`--models`, `--apis`, `--tasks`, `--profiles`, `--trials`, `--parallel`, `--max-repairs`, `--cmd`, `--out`, `--work` and `--resume RUN_DIR` narrow or redirect a run; `--help` lists them. A name the manifest does not declare is refused with the names it does. Exit codes: `0` the run finished clean; `1` it finished with a crashed trial or a mutant the oracle did not catch; `2` a usage, manifest or host refusal.

## `bench.toml` holds the benchmark

The binary holds none of it. `bench.toml` declares the arms and their sheets, the tasks and the clause that says a dropped future left nothing live, the five profiles, the file layout, the limits, the defaults, the model command line, the excluded cells and the protocol statements a summary records. A change to what the benchmark is changes that file. The defaults are #270's planned run: three models, six arms, six tasks less three excluded cells, five profiles, two trials per profile, which is 990 trials and ten per `(model, arm, task)` cell.

## Per trial

A trial makes an empty scratch directory holding only the prompt text files, runs the model command with the prompt on stdin under a 900 s bound, parses the final NDJSON result line, extracts the last ` ```rust ` block, copies `template/` into `<work>/<trial-id>/`, writes `src/lib.rs`, and builds with `cargo build --locked --offline` against one `CARGO_TARGET_DIR` per run. On a failed compile it re-prompts with the original prompt, the current code and the first 80 lines of compiler errors, counting one repair, up to `--max-repairs`. If it compiles it copies the hidden oracle to `tests/oracle.rs` and runs `cargo test --locked --offline -- --test-threads=1`. Every cargo process is serialized by an exclusive lock on `<work>/.cargo-gate.lock`.

The model call is closed-book under macOS `sandbox-exec`; `--matrix` refuses on a host without it rather than run the model open-book against the hidden oracle. A dry run compiles each arm's reference solution in the model's place (the lexicon arm uses the `script` arm's, because it authors `script!` from the generated lexicon), and a mutant run compiles each task's deliberate mistake.

**Per-trial package names.** Every trial crate is built under a unique package name (`ai_trial_<trial-id>`). A constant name makes cargo's `-C metadata` identical across trials, so a shared target directory would let one trial's `oracle` binary stand in for another's. The committed oracles name the crate `ai_trial`; the runner substitutes the trial's real name.

## Evidence is never reused

A run creates a fresh work root, `$TMPDIR/lgwks-ai-authoring-<UTC stamp>-<mode>/`, and a fresh results directory, `runs/<UTC stamp>-<mode>/`; either one already existing is a refusal, and nothing is deleted. Every finished trial is appended and synced to `results.partial.jsonl`, so a `kill -9` keeps every finished trial. At the end the sorted `results.jsonl` is written beside its name and renamed over it, the log is retired and `summary.json` is written. `run.json` (the work root, mode and commit a resume reads) names a temporary directory of the host, so a committed run is `results.jsonl` and `summary.json` only.

`--resume RUN_DIR` finishes an interrupted run at the same commit and mode, refusing a finished run, another mode and another commit. It runs only the trials with no whole record, moves an interrupted trial's directory aside as `<id>.interrupted-N` rather than deleting it, and drops a torn last log line.

## Bounds and orphans

Every child leads its own process group under a bound. A child still running at its bound is stopped as a group (`TERM`, ten seconds, `KILL`) and what it had printed comes back: an oracle past `oracle_s` (1,800 s) keeps the verdicts of the clauses that finished, the rest are `timeout`, and the trial is a recorded failure, not a crash. A watchdog process in a group of its own stops every group the runner started if the runner itself is killed, so no oracle, solution or `cargo` outlives a run.

## Metrics

- `consumer_lines` — non-blank, non-comment lines of `src/lib.rs`.
- `orchestration_sites` — occurrences in `src/lib.rs` of the author-burden proxy tokens listed in `bench.toml` (`JoinSet`, `spawn(`, `Semaphore`, `CancellationToken`, `select!`, `loop {`, `Arc<`, `Mutex`, `channel(`, `abort(`), recorded per trial in `orchestration_site_tokens`. A proxy, not a proof of complexity: a token can appear in a comment or a type name.
- `tokens_in` / `tokens_out` / `model_wall_ms` — summed over every model call in the trial.
- `compile_wall_ms` — summed over every cargo build in the trial.
- `oracle_wall_ms` — the wall time of the `cargo test` invocation that runs the oracle, under `/usr/bin/time -l` (macOS; `-v` where `time` is GNU).
- `oracle_peak_rss_bytes` — that process's maximum resident set size in bytes, or `null` on a host with no `/usr/bin/time`. `null` is *not measured*, never *measured as zero*.
- `cleanup_ok` — the task's `drop_clause` from `bench.toml`; a crate that never compiled reports `false`.

## What it refuses and what it did not port

`--tables` reads `results.jsonl` strictly. The four earliest runs under `runs/` (`20261003-closed-book-models`, `20261003T033649Z-dry`, `20261003T033827Z-mutants`, `20261003T033929Z-mutants`) predate the `profile`, `cleanup_ok`, `oracle_wall_ms` and `oracle_peak_rss_bytes` fields, so `--tables` refuses them with `results line 1 is not a whole trial record` instead of inventing values; every run from `20261003T153558Z` on reads, as `--tables` run over each of them exited 0.

`regrade.py`, which re-graded a finished run's solutions against a corrected oracle (`regraded.jsonl` in `20261003T190050Z-models`), is not ported. The Python ban deleted both scripts in #537 (`22a19af25`; the last copy of each is at `22a19af25^`), and the runs under `runs/` before this binary were made by them.

## The lockfile

`bench/ai-authoring/Cargo.toml` is a workspace root with `members = ["support", "template"]`. `bench/ai-authoring/Cargo.lock` is generated once (`cargo generate-lockfile --offline`) for that exact graph, including the `script` feature's `lgwks_macros`/`syn`/`quote` edges, and the runner copies it into each trial, rewriting the root package name to the trial's. `cargo build --locked --offline` therefore works from the registry cache with no resolution.

## Proof

`cargo nextest run --locked -p lgwks_deps --bin ai-authoring` runs the unit and simulation tests (seeded worlds for the plan, record, store, scaffold, statistics, fence parsing, CLI and cargo gate). `--test it` with the `ai_authoring` module drives the shipped binary against a fixture benchmark through real cargo: usage refusals, a dry run and its tables, a mutant the oracle cannot catch (exit 1), an oracle past its bound (recorded `timeout`), and a `kill -9` mid-trial followed by `--resume` with no process left behind.
