# The runner

What `run.py` does per trial, the metrics it records, and the lockfile every trial shares. Part of the [authoring benchmark](README.md).

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
- `oracle_wall_ms` — the wall time of the `cargo test` invocation that runs the
  oracle, taken under `/usr/bin/time -l`. The compile is already done by that
  point, so this is the oracle's own cost rather than a build's.
- `oracle_peak_rss_bytes` — that same process's maximum resident set size, in
  bytes, or `null` on a host with no `/usr/bin/time`. `null` means *not
  measured*, never *measured as zero*. The parser reads both field orders the
  flag takes: macOS prints `67911680  maximum resident set size` (bytes) and GNU
  prints `Maximum resident set size (kbytes): 66443` (kibibytes, scaled to
  bytes).
- `cleanup_ok` — the task's drop clause, read by name from
  `DROP_CLAUSE_BY_TASK`. A crate that never compiled reports `false` rather than
  a cleanup result, because "the drop test did not run" is not "cleanup was
  fine".

### The lockfile

`bench/ai-authoring/Cargo.toml` is a workspace root with `members = ["support",
"template"]`. `bench/ai-authoring/Cargo.lock` is generated once
(`cargo generate-lockfile --offline`) for that exact graph — including the
`script` feature's `lgwks_macros`/`syn`/`quote` edges — and the runner copies it
into each trial, rewriting the root package name to the trial's. `cargo
build --locked --offline` therefore works from the already-downloaded registry
cache with no resolution.
