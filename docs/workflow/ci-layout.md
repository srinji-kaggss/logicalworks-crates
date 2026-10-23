# Workflow §12: the CI specification

Moved verbatim from `WORKFLOW.md` on 2026-10-22, when every Markdown file in a
repository was held to 120 lines (Director, 2026-10-10). No rule changed in the
move; `WORKFLOW.md` is the index and keeps the § number.

---

## 12. CI: the specification

`.github/workflows/ci.yml` and `.github/actions/local-rust/action.yml` implement
this section. A change to either is held to it, and a number below is replaced
only by a newer measurement of the same thing, with its run id.

### The budget

| Measure | Bound | Source |
|---|---|---|
| Wall clock of a warm run | ≤ 5 min | the contract's gate budget (2026-09-30), the tightest of the Director's three statements (5, 6 and 7 min) |
| Wall clock of a cold run | ≤ 10 min | Director: "10 MINUTES COLD" |
| Outcome | every lane passes | Director: "U CANNOT CLAIM SPEED OF CODE IF UR FUCKING CI DOESNT PASS" |

*Wall clock* is the run's first job start to its last job end, read from the
Actions API (`jobs[].started_at`, `jobs[].completed_at`), queueing included.
*Warm* means each runner instance's target directory holds an earlier commit's
build; *cold* means it holds nothing (the cargo registry stays). A bound is met
by a run that passes; a red run's wall clock is not evidence of anything.

### What sets the wall clock

Every self-hosted job runs on one machine: 15 cores (5 performance, 10
efficiency), 24 GiB, thirteen runner instances. A run's wall clock is therefore
its job-seconds divided by thirteen plus what waits in the queue, and adding
jobs does not add machines. Three measured facts set the layout:

1. **Duplicated work, not parallelism, was the cost.** Run 37579691890 (main,
   3c008cbf) ran 35 jobs, 7,500 job-seconds, in 21.4 min. Each job compiled
   the workspace into a target directory of its own (25 directories per
   instance, 206 GiB), and the test suites ran in 20 configurations whose
   once-each minimum is 1,131 test-seconds against the 17,010 they took
   contended. The job that existed only to list the simulation tests compiled
   every test binary for 364 s; the same listing took 6 s in the job that had
   just built them.
2. **`sync_all` is device-wide on macOS.** Rust's `File::sync_all` is
   `fcntl(F_FULLFSYNC)`, which flushes the whole SSD's cache and serialises
   against every other flush on the device. The durable-store tests sync every
   record, so six instances' suites queued on one another's flushes. On an
   idle machine the full workspace suite (5,430 tests) took **385 s** with its
   temporary files on the SSD and **116.5 s** with them on a RAM disk, every
   test passing both times, peak RSS 484 MB.
3. **Memory, not cores, is the binding constraint.** Thirteen jobs each
   compiling with four slots is 52 compiler processes plus ~50 test threads on
   24 GiB. Run 37654108625 (warm, green) left **35 GiB of swap used** with
   1.3 billion page reactivations outstanding afterwards; a hashing unit test
   that takes one second idle took 138 s, the dudect example 47 s idle and
   458 s in the run, the workspace suite 116.5 s idle and 390 s in the run.
   Warm compiles take seconds, so compiler slots are capped at a share that
   sums near the machine (`build-share`), test threads likewise
   (`test-share`: nodefault 6, bot-full 8, workspace 4, the rest 1-2), and the
   suites' CPU is bought down rather than scheduled: `[profile.test]`
   opt-level 2 halves the heaviest sims (the 5,000-tenant provision 67.9 s to
   34.3 s at the same seeds), and the frame search hashes once per candidate
   length instead of twice.

So: lanes that build one feature set share one job (the lane table's
`group`); a job that only reads a result reads it from the job that produced
it; every job that builds is pinned to one instance, which keeps one target
directory for it, so a run after the first finds its dependencies built; and
every job's temporary files are on an APFS RAM disk, the filesystem of the
machine's own disk, so the move changes how fast a sync is and nothing a test
observes.

**The compile cache.** One sccache serves all thirteen instances, so the first
instance to compile a registry crate pays and the rest read it. That held only on
paper until 2026-10-23: sccache keys a compile by the `CARGO_` variables rustc is
given, cargo gives rustc its whole environment, and the action's
`CARGO_TARGET_DIR` names the instance, so no instance read another's compile and
a toolchain pin cost its pull request seven runs (#738, runs 39960738059 to
39974411070). `RUSTC_WRAPPER` is now the program built from
`.github/actions/local-rust/rustc-sccache/main.rs`, which runs sccache without
that variable: a second instance's compile of tokio, regex, serde, serde_json and
syn went from 25 s with 0 of 27 Rust compiles served to 11 s with 22 of 27, at a
load average of 81. Enforced by `sccache_target_key` in
`crates/lgwks-deps/tests/it`: one crate into two target directories, plain sccache
must miss and the wrapper must hit. Proc macros and build scripts are never
cached, and `SCCACHE_BASEDIRS` does not reach Rust before a release after 0.18.0.

**Why the RAM disk changes no assertion.** No test simulates power loss. The
crash tests kill with `SIGKILL`, and a killed process's written pages survive
in the page cache, so what a reopen reads is the same whichever device holds
the file. Production code keeps `sync_all`; only where the test's scratch files
live changed. The Linux container gets the same with a tmpfs `/tmp`.

### The nine axes

| Axis | Requirement on the CI | Evidence |
|---|---|---|
| Frontier | The layout beats each alternative it was measured against. 35 single-lane jobs with per-job targets: 21.4 min. Hosted runners: refused by the Director (2026-10-04, CI runs on the local runner). One orchestrator job running the whole lane table: loses the per-job rows and isolates nothing. Sharding one suite across instances of one machine: recompiles it per shard and adds no cores. | the run table in `docs/workflow/ci-runs.md` |
| Hyperscale | Every resource the run takes is bounded. Self-hosted jobs in flight ≤ the thirteen instances; nextest threads per job = share × cores ÷ instances (workspace 6, bot-full 8, no-default 6, matrix 3, rest 1-2); compiler slots one per job; a RAM disk ≤ `tmp-gib` (4 GiB) per instance and only the pages written; a container tmpfs ≤ 3 GiB; every job has `timeout-minutes`; every test has nextest's `slow-timeout` with `terminate-after`. Nothing grows with the number of runs except the target directories, which are cleaned below `min-free-gib`. | `action.yml`, `.config/nextest.toml` |
| Idiomatic | One definition of the gate: `scripts/gate-lanes.toml`. CI steps carry the lane commands verbatim and `check-gate-parity.py` refuses drift. Standard actions only; no wrapper that hides a command. | `python3 scripts/check-gate-parity.py` |
| Generalized | Every lane runs on every pull request: every feature set, the 8-target matrix, WASI, all 28 grammars, macOS, Linux and Windows. A layout change moves steps between jobs and never drops one; the parity check proves each CI lane still has its step. | parity: 58 lanes, 49 shared |
| Decoupled | No job waits on another except three readers (the two check rows and the acceptance receipt), each of which reads a file another job wrote and builds nothing. A lane's command does not depend on which job runs it. | `needs:` appears three times |
| Ephemeral | A job leaves nothing that changes the next one's result. The RAM disk is detached and recreated at every job start; the container's tmpfs dies with the container; artifacts expire (7 and 30 days). The per-instance target directory is the only state that crosses runs, and it is a cache: deleting it costs a cold build and never changes an outcome. | `action.yml` |
| Portable | macOS arm64 natively, Linux aarch64 in a container with `--init` as PID 1, Windows on hosted runners for the two storefronts that have a Windows backend, and `cargo check` across the declared target matrix. A host without a RAM disk keeps its temporary files on disk and says so in a warning. | `scripts/linux-container-tests.sh`, `scripts/check-target-matrix.sh` |
| Multi-tenant | Runs of different pull requests share the machine and never a writer: each instance has its own target directory, RAM disk and container target volume, and an instance runs one job at a time. Concurrent runs of one pull request supersede rather than race. | `concurrency:` in `ci.yml` |
| Fastest | Warm ≤ 5 min and cold ≤ 10 min, measured on passing runs and reported as below. | the run table in `docs/workflow/ci-runs.md` |

### Runs

The run table — 25 runs, their layouts, wall clocks and verdicts — is
[`ci-runs.md`](ci-runs.md), kept whole so this file stays inside the size law.
