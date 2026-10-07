# Logical Works Workflow

> Provenance: copied verbatim from the estate governance suite
> (`logical-DB/CODEBOOK.md`, `logical-DB/WORKFLOW.md`) on 2026-09-21 and
> maintained here as this repository's copy. Where a rule names this
> repository's own gates, `scripts/ci-local.sh` is the definition.

How work moves through this repository. Code rules are in `CODEBOOK.md`.
Authority, decisions, and the completion ledger are in `GOVERNANCE.md`. The
agent entry point is `AGENTS.md`.

---

## 1. Local CI

Local CI is the gate. It runs every check a pull request needs, on this machine,
without GitHub, and writes a receipt.

```sh
./scripts/ci-local.sh              # full gate
./scripts/ci-local.sh --fast       # the lanes marked fast
./scripts/ci-local.sh --lane ID    # one lane, what CI runs per step
./scripts/ci-local.sh --list       # lane ids and surfaces
./scripts/ci-local.sh --receipt    # also write evidence/ci-local-<sha>.md
```

`scripts/gate-lanes.toml` is the one definition of "the gate". `ci_local.py`
executes the lanes from it; `.github/workflows/ci.yml` runs the same commands
against the same lane ids; `scripts/check-gate-parity.py` refuses any drift
between the two. They do **not** run in the same order — CI fans the lanes out
across jobs and needs — and a green local run is evidence only for the lanes
whose `surfaces` include `local`. The receipt names every lane this surface
did not cover.

When the two disagree, the lane table is the source of truth and the
disagreement is a defect in whichever side diverged. Fix that side and re-run
both.

Two evidence lanes exist because they are user-facing claims rather than
ordinary unit-test counts. `simulation-evidence` parses Nextest's executable
test listing and also counts source-visible `#[test]` attributes under
`tests/sim/` and `sim_*` files, then refuses the gate unless each view is at
least half deterministic simulation. `debug-e2e` drives the public
`lgwks-deps debug` command through its JSON success path and a fail-closed
manifest fixture, and prints the successful end-to-end journey result CI must
show. CI runs both lanes in the workspace job and exposes each verdict as its
own PR check row, `successful simulation result` and `successful end-to-end
journey result`, so the checks list shows the result without requiring a reader
to open the aggregate log. A check row reads the report its lane wrote and
passes only on that lane's own pass line; it does not build the workspace again.

CI runs on the self-hosted macOS arm64 runners on this machine (Director,
2026-10-04), with the Linux suites in a container on the same machine and the
Windows legs of the AppCUI and GPUI storefronts on hosted `windows-latest`.
§12 is the CI's specification. `check-gate-parity.py` refuses a required lane
with no matching CI step, a CI gate step no lane claims, a substituted command,
and a toolchain pin that drifted. It does not police runner labels; the OS
matrix is a coverage decision, not a routing accident.

---

## 2. The delivery loop

```
1. Read the task and query WWFD            (1 min, mandatory)
2. Find the best OSS implementations       (5 min, non-trivial work only)
3. Navigate with CodeGraph                 (2 min, find code and callers)
4. Implement to the codebook               (CODEBOOK.md)
5. Test and verify                         (affected tests + regression tests)
6. WWFD state sync, then PR                (mandatory before delivery)
```

**Rule 1, first tool call.** The first substantive tool call reads source you
will edit, runs a build or test, writes source, or queries CodeGraph to navigate
to the code you will edit. It is not reading this file, loading a skill, running
`wwfd boot`, or producing a plan document.

**Rule 2, five-minute code.** Within five minutes of session start you have
written or modified a source file, run a build/test/lint command, or named a
specific `file:line` and the exact change it needs. "Inventorying owners and
consumers" is not this.

**Rule 3, narration ratio.** Process words must never exceed 20% of any
response. Compliance citations, skill-loading narration, time accounting, and
scope inventorying are process words. Code, architecture decisions, error
analysis, test results, and diffs are implementation words.

**Rule 4, one repro then fix.** A prior CI failure is a defect verdict and the
receipt already exists. One repro is authorized only when the cause is genuinely
uncertain from the receipt alone. A second repro of the same state is forbidden.
Fix the code, run the new state once.

**Rule 5, skill budget.** At most two skills loaded before the first code edit.
Additional skills load only when a specific API question blocks you
mid-implementation.

**Rule 6, table proportionality.** Product runtime changes get the full nine-axis
SLO evidence matrix with measured evidence per axis. Config, docs, CI, README,
and script changes get one line of receipt per file. No table. No axes.

**The nine-axis matrix for the 2026-09-27 async-journal change.** This is a
product-runtime change, so Rule 6 asks for measured evidence per axis. The row
states what was measured, not what is hoped for.

| Axis | Measured evidence | Where |
|---|---|---|
| Frontier | Owner-serialized writes with a poisoned-ambiguity handle, rather than a per-write lock and a silent timeout. A dropped waiter used to complete invisibly; the handle is now poisoned with the reason | `journal::file::a_dropped_waiter_poisons_the_handle_and_a_reopen_does_not_duplicate` |
| Hyperscale | Concurrency tiers 100 / 1k / 10k / 100k swept; the reached level is 50,000 on one store, bounded by `MAX_JOURNAL_EVENTS` of 100,000 at two events per fact. Requested, reached and ceiling are all in the trace | `sim_scale::tier_r*` |
| Idiomatic | `clippy -D warnings` over the workspace and all targets: 0 errors. `cargo fmt --check`: clean. Every `allow` in new code carries a `reason` | workspace gate |
| Generalized | The async append shares the sync append's frame preparation and length-check, write, `sync_all` path, so the two cannot drift into writing different bytes | `src/journal/file.rs` |
| Decoupled | The harness owns time, network and disk; the code under test owns none of it. Tests drive `FileJournal`, `Bot`, `Broker`, `EffectScope` through their public interfaces only | `tests/sim/mod.rs` |
| Ephemeral | Every scratch store is under the system temp directory, named by seed and by `lgwks_std::random` bytes, and removed on drop. A process id would have been shorter and wrong: the OS reuses it | `sim::Sim::scratch` |
| Portable | Reopen across a fresh handle, path round-trip, and a real file-backed reopen; no wall-clock or pid enters a trace hash | `sim_journal::reopen_portability_r*`, `sim_scale::portable_r*` |
| Multi-tenant | 5,000 concurrent provisions, 5,000 distinct tails, 10,000 events, no cross-tenant history | `sim_scale::the_named_five_thousand_tenant_provision` |
| Fastest | 2,852 tests in 162.465s wall under nextest, 1,990 of them simulation, with source-visible simulation coverage at 1,111 of 2,055 tests | `cargo nextest run --workspace --locked`; `python3 scripts/ci_local.py --lane simulation-evidence` |

**Rule 7, refuse the theater.** No `wwfd boot` + `mem show` as a session-start
ritual. No prophylactic skill loading. No re-reading source already held in
context. No re-grepping something already found.

---

## 3. RAG yourself into expertise

Before implementing anything non-trivial, find two or three production OSS
implementations of the same thing and study their structural decisions: module
organization, trait design, error handling, concurrency primitives, tests. Five
to ten minutes. Record what you are adopting and why, and what you rejected and
why, in your first edit or message. Then implement at that level.

Skip this for trivial changes under twenty lines with no new abstraction, bug
fixes whose root cause is already identified, and config/docs/CI work.

This is not copying code. It is studying architecture and making an informed
decision. The mechanism is RAG for latent implementation expertise: reading real
code activates specifics that prompts alone do not.

---

## 4. One path, one opinion, one architecture

A capability is either **the** architecture or it does not exist.

- **No default-off parallel paths.** A feature flag that makes an implementation
  a *candidate* is how work fails to carry forward: nothing exercises it, the
  gate never compiles it, and it rots into a second opinion nobody chose. If it
  is how the estate does the thing, it is on by default and the gate runs it.
- **One implementation of a job.** Two builders, two executors, or two
  "equivalent" entry points for the same work is one too many. Pick the right
  one, make it the only one, delete the other including its tests, docs, and
  feature flag.
- **Unification beats a seam.** "Parallel seams, not demolition" governs *how* a
  change lands while the tree stays green. It is not a licence to leave both
  halves standing. The seam closes in the same task.
- A storefront may keep capability features default-off, because selecting is
  its stated purpose. A **consumer** states its opinion: it turns on what it
  uses, in its own defaults, and the workspace gate compiles and tests it.

---

## 5. Compounding, buildable, surgical

**Compounding.** Every unit, trait, or module added makes future tasks easier.
New capabilities plug in through existing traits without rewriting call sites. If
adding a feature requires rewriting half the repo, the previous architecture was
anti-compounding debt.

**Always buildable.** Every single commit compiles and passes tests. Never
commit a broken intermediate state intending to fix it in the next step. If a
refactor breaks the build for more than twenty minutes, the change radius is too
broad.

**Surgical change radius.**

- Touch only the lines the invariant requires. No gratuitous mass renames, no
  cosmetic formatting rewrites, no sweeping module migrations in the same turn
  as a semantic fix.
- Identify before touching: map callers and downstream consumers first. One
  CodeGraph query replaces five grep chains.
- Parallel seams over in-place demolition when replacing a subsystem: introduce
  the new implementation alongside the old, point callers at it one at a time,
  verify tests, then delete the old code.

Time-box is thirty minutes by default. Not terminal within the box: stop, report
the SHA and the receipt, await direction. Identical-state retry is forbidden.

---

## 6. Evidence proportionality

| Change | Evidence |
|---|---|
| Product runtime | full nine-axis SLO matrix, measured per axis |
| Config, docs, CI, README, scripts | one line of receipt per file |
| Instruction or config loading | config-loading evidence only |

**The nine axes.** Frontier (design beats credible alternatives), Hyperscale
(>1M concurrent, correct), Idiomatic (ownership, errors, lifetimes sound),
Generalized (works across declared inputs), Decoupled (components independently
replaceable), Ephemeral (survives process loss), Portable (same semantics on all
targets), Multi-tenant (isolated under concurrent use), Performance (fastest
correct implementation).

Every claim carries evidence: command, exit code, result, artifact ID. Before
completion, account for every requirement, every failed check, and every
unknown. No hidden exclusions.

AI and model output is untrusted first-party evidence until independently
verified. Every capability claim separately states which of these it is:
**planned**; **present in code at an exact commit**; **exercised on the exact
claimed path**; **independently evidenced**; **safe to rely on or merge**. Never
collapse these into "done".

---

## 7. Work ends with a PR

- **Always commit.** Never leave verified-green work uncommitted and never end a
  turn with it in the working tree. Uncommitted work has not carried forward.
- Commit in small, individually-green steps.
- **A local commit is not the finish line.** Work ends with a GitHub PR: branch,
  commits, pushed, PR opened with its evidence. A branch with no PR is unfinished
  work, and a green tree on `main` that was never proposed has not shipped.
- The PR carries the receipts: gate commands and their results. Not a narrative
  about them.

### Merge gate

Source review may proceed against local receipts. Merge eligibility requires
executed reproduction of the affected lanes on hosted or self-hosted CI. A
bounded exception requires explicit Director authorization recorded in
`GOVERNANCE.md` with its replacement evidence enumerated. A local PASS claim
alone is never a satisfied execution gate.

Production completion additionally requires a ratified product scope and every
applicable acceptance row satisfied. A green PR does not close unfinished
roadmap issues. Do not copy historical checkmarks into a current release claim.

---

## 8. Agent economics

Agents are the dominant cost in this workflow, orders of magnitude above a tool
call. Spend them like the expensive resource they are.

The escalation ladder, exhausted in order before spawning anything:

1. **CodeGraph** — is it already in this repo? Module DAG, callers, consumers.
2. **Graphify** — is it already in the estate?
3. **A quick 9router search** — one `/v1/search` call answers most "what is the
   standard for X" questions outright.
4. **wwfd** — has the estate already solved this?

Only if still uncertain after all four, spawn an agent. An agent is the last
rung, not the first.

- Fan-out is the cost driver. Batch related questions into one agent with one
  return format. Three agents covering three topics each beats nine covering one
  each.
- Do discovery yourself. A single `curl`, file read, endpoint probe, or `git log`
  is seconds of your own context.
- Default to doing it yourself. Delegate only work that is genuinely parallel,
  independent, and bounded.
- Justify each agent in one clause. If you cannot, it is not one.
- Never re-dispatch to redo. If an agent returns partial work, finish it yourself
  rather than spawning a successor.
- Do not kill running agents to save money. Their cost is already sunk. Change
  the *next* decision instead.
- Front-load the constraint. A wrong channel or a wrong premise costs a whole
  agent run.

Subagent return is not done. The parent owns closure.

---

## 9. Data safety and the guard

Deletion is `trash <path>` or move to `~/.Trash`. Never `rm`, `rmdir`, `shred`,
`truncate`, `git clean`, `git reset --hard`, `find -delete`, or `xargs rm`.
Emptying Trash is human-only. No secrets in output, commits, memory, or queries.
Every resource declares owner, lifetime, and cleanup.

The five enforced rules (`rust-guard`, wired into Claude Code, Codex, OpenCode,
and the global git pre-commit) cover the shell as well as the editor:

1. **OWNERSHIP** — never edit, commit, or sweep a repo the estate does not own.
2. **ARTIFACTS** — never commit derived build output (`graphify-out/`, `target/`,
   `.lgwks/`, `node_modules/`, `.codegraph/`).
3. **SUPPRESSION** — every `#[allow]` / `#[expect]` carries `reason = "..."`.
4. **PRINTS** — no `eprintln!` / `println!` / `writeln!(stderr, ..)` in library
   code. Use `tracing`. `main.rs`, `src/bin/`, `examples/`, `benches/` exempt.
5. **COMPILE** — an edit that leaves the crate not compiling is refused. Judge by
   rustc error codes, never by diagnostic count.

A refusal is a correction, not a discussion and not a stop. Fix it and carry on.
Two responses are forbidden because both are theatre: ending the turn to report
yourself blocked, and writing a paragraph about how you respect the rule.

---

## 10. Local CI and the runner map

| Repo | Gate script | Runner | Workflow |
|---|---|---|---|
| `logicalworks-crates` | `scripts/ci-local.sh` | six self-hosted instances on this Mac (`[self-hosted, macOS, ARM64, lwc]`), a Linux container on the same machine, hosted `windows-latest` for the Windows legs | `.github/workflows/ci.yml`, specified by §12 |

Every self-hosted job runs on one machine, so the OS matrix is kept by the
Linux container and the hosted Windows legs rather than by more machines. Keep
it: dropping a leg is a coverage loss, not a speed-up.

Linux is the correct rung wherever a test is `#![cfg(target_os = "linux")]` and
installs a seccomp filter. On a non-Linux host such a test compiles to nothing
and the suite reports green having verified nothing, which is silent coverage
loss. macOS is a reduced rung for quick signal, not an equivalent one.

A container (`act`, Docker) is not sufficient for a seccomp gate.

---

## 11. Mandatory WWFD cycle

**Before implementation**, query WWFD (`wwfd q "<error or concept>"` or MCP
`wwfd_query`). Check whether the problem or architecture has past lessons or
established estate standards. Never invent from scratch what WWFD already
solved.

**After verification and before delivery**, run
`wwfd state sync <repo> --task '<task>'` to record genuine state before closing
or opening a PR. No claim of green completion without a sync receipt.

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
every job's temporary files are on an APFS RAM disk, the
filesystem of the machine's own disk, so the move changes how fast a sync is
and nothing a test observes.

**Why the RAM disk changes no assertion.** No test simulates power loss. The
crash tests kill with `SIGKILL`, and a killed process's written pages survive
in the page cache, so what a reopen reads is the same whichever device holds
the file. Production code keeps `sync_all`; only where the test's scratch files
live changed. The Linux container gets the same with a tmpfs `/tmp`.

### The nine axes

| Axis | Requirement on the CI | Evidence |
|---|---|---|
| Frontier | The layout beats each alternative it was measured against. 35 single-lane jobs with per-job targets: 21.4 min. Hosted runners: refused by the Director (2026-10-04, CI runs on the local runner). One orchestrator job running the whole lane table: loses the per-job rows and isolates nothing. Sharding one suite across instances of one machine: recompiles it per shard and adds no cores. | the run table below |
| Hyperscale | Every resource the run takes is bounded. Self-hosted jobs in flight ≤ the thirteen instances; nextest threads per job = share × cores ÷ instances (nodefault 6, bot-full 8, workspace 4, rest 1-2, sum near the machine); compiler slots likewise (`build-share`: the heavy compilers 2, the rest 1); a RAM disk ≤ `tmp-gib` (4 GiB) per instance and only the pages written; a container tmpfs ≤ 3 GiB; every job has `timeout-minutes`; every test has nextest's `slow-timeout` with `terminate-after`. Nothing grows with the number of runs except the target directories, which are cleaned below `min-free-gib`. | `action.yml`, `.config/nextest.toml` |
| Idiomatic | One definition of the gate: `scripts/gate-lanes.toml`. CI steps carry the lane commands verbatim and `check-gate-parity.py` refuses drift. Standard actions only; no wrapper that hides a command. | `python3 scripts/check-gate-parity.py` |
| Generalized | Every lane runs on every pull request: every feature set, the 8-target matrix, WASI, all 28 grammars, macOS, Linux and Windows. A layout change moves steps between jobs and never drops one; the parity check proves each CI lane still has its step. | parity: 58 lanes, 49 shared |
| Decoupled | No job waits on another except three readers (the two check rows and the acceptance receipt), each of which reads a file another job wrote and builds nothing. A lane's command does not depend on which job runs it. | `needs:` appears three times |
| Ephemeral | A job leaves nothing that changes the next one's result. The RAM disk is detached and recreated at every job start; the container's tmpfs dies with the container; artifacts expire (7 and 30 days). The per-instance target directory is the only state that crosses runs, and it is a cache: deleting it costs a cold build and never changes an outcome. | `action.yml` |
| Portable | macOS arm64 natively, Linux aarch64 in a container with `--init` as PID 1, Windows on hosted runners for the two storefronts that have a Windows backend, and `cargo check` across the declared target matrix. A host without a RAM disk keeps its temporary files on disk and says so in a warning. | `scripts/linux-container-tests.sh`, `scripts/check-target-matrix.sh` |
| Multi-tenant | Runs of different pull requests share the machine and never a writer: each instance has its own target directory, RAM disk and container target volume, and an instance runs one job at a time. Concurrent runs of one pull request supersede rather than race. | `concurrency:` in `ci.yml` |
| Fastest | Warm ≤ 5 min and cold ≤ 10 min, measured on passing runs and reported as below. | the run table below |

### Runs

| Run | Commit | Layout | Wall clock | Job-seconds | Result |
|---|---|---|---|---|---|
| 37579691890 | 3c008cbf | 35 jobs, per-job targets, SSD temp | 21.4 min | 7,500 | pass |
| 37607930551 | 3f331527 | 14 jobs, per-instance targets (cold), HFS+ RAM temp | cancelled | — | fail: six instances compiled every dependency at once; four jobs timed out compiling |
| 37613720499 | 3a2a855f | + one shared sccache, server on a job's RAM disk | cancelled | — | fail: the next job ejected the server's temp dir; HFS+ decomposed names broke two walk simulations |
| 37616318637 | 4993b33d | + APFS RAM temp, sccache with every target as a base directory | cancelled | — | fail: five jobs timed out compiling at load 70-85; jobs landed on instances cold for their feature sets, and sccache hit 0 of 195 Rust compiles across target directories, so it was removed and jobs pinned |
| 37619560219 #1 | 7b48f3f1 | 14 jobs pinned to 6 instances (cold per instance), APFS RAM temp | 20.5 min | 5,879 | fail: the feature matrix reached its timeout compiling |
| 37619560219 #2 | 7b48f3f1 | same commit, warm | 10.0 min | 2,470 | fail: both Linux jobs found no engine through the Docker context; the no-default job queued 259 s behind another on its instance |
| 37624988459 #1 | b9eadc91 | 11 instances, one per building job; Linux leg on OrbStack | 17.2 min | — | fail: both bot-full shards reached their 15 min timeout at load 100+, beside the 10,000-run saturation tier's 50,000 process starts |
| 37624988459 #2 | b9eadc91 | same commit, warm | 8.5 min | 2,729 | fail: a 100 ms parse deadline measured 392 ms on the host clock (fixed: proved on a driven clock); every other job passed by 291 s, the two bot-full shards at 491-497 s |
| 37630747929 | 99670c70 | bot-full in three shards; deadlines and stalls on the seeded clock | 13.0 min | 5,473 | fail: four doc citations into ecs.rs moved with the source; five jobs compiled the same feature sets at once, 4-9 min each, while their tests took 66-150 s |
| 37633462471 | 436b9c9e | one job per feature set; gate checks apart from the workspace build | 9.0 min | 3,508 | fail: the acceptance job's artifact pattern missed the unsharded name; the workspace build took 5m40s because the runners' `CI=true` turns incremental compilation off |
| 37635420955 | 89ba090b | + walk-complexity bound judged at the min window | fail | — | fail: two timing flakes under load (walk median 6.5x, hostile byte-ceiling tiling on a 10 s host deadline); fixed, not weakened |
| 37650313690 | 627e4629 | + doc-citation re-anchor | cancelled | — | superseded by 37651284179 |
| 37651284179 | 627e4629 | same commit | 10.3 min | — | fail: hostile byte-ceiling tiling timed out at 88 s under load (fixed: tile past any deadline); every other job green |
| 37654108625 | 6d792999 | + hostile tiling fix; thirteen instances | 10.2 min | 4,807 | **pass.** Six jobs over the 5 min budget: nodefault 605 s, docs 517 s, storefront 494 s, timing 492 s (dudect 458 s), build 488 s, bot-full 378 s. Step logs show warm compiles finishing in seconds and suites inflated 3-10x: the machine is swapping, not compiling (35 GiB swap used). The fix is §12 fact 3 above, not another layout |
| 37661085639 | f34c581d | + build/test shares, test opt-level 2, frame one-hash, merged doc lanes (transition run: every instance rebuilt at the new profile) | cancelled (15 min timeouts on build, nodefault) | — | **not a verdict on the layout.** Transition walls paid the rebuild storm (build's full-workspace rebuild 638 s on two slots; nodefault's serial feature rebuilds). What it proved warm and fast: gate 279 s → 112 s, the workspace test step 390 s → 165 s, merged doc lanes passing. Timeouts move 15 → 20 min (hang bound, not budget) so a transition run survives its rebuilds |
| 37663658069 | f380fb4a | + 20 min hang bounds (warm: opt-2 everywhere) | 8.7 min | — | **pass.** Down from 10.2: saturation 221→121 s, clippy 211→122 s, gate 279→188 s, timing 492→343 s, storefront 494→342 s, docs 517→365 s, bot-full 378→348 s. Six jobs still over budget: nodefault 518 s (bot suite 355→161 s; the 12 per-feature suites are compile-bound at opt-2), build 505 s, docs 365 s, bot-full 348 s, timing 343 s (dudect still 1M samples), storefront 342 s (GPUI check 174 s). Next: per-feature suites move to the feature-matrix job at opt-0, dudect to 250k samples |
| 37666041415 | 40a3175d | + shard fusion, dudect 250k, thread bumps | fail | — | fail: bot-full at ten threads broke INV-BOT-156's drain test (two cleanups Aborted past the grace under the process-table storm). Reverted to eight. Lesson recorded in the step comment |
| 37668304200 | a2b50ff6 | + bot-full back to eight (warm; shards cold at opt-0 on lwc-2) | 9.4 min | — | **pass.** Timing 343→237 s (dudect 250k), nodefault 518→220 s (bot-only), storefront 342→300 s, gate 225 s; but stdmatrix 129→561 s (12 cold opt-0 compiles serialized under the storm, starving bot-full 348→483 s and docs 365→501 s), build 505→488 s. Shard-c proves the mechanism: 42 s uncontended against 214 s for shard-b. Next: test opt-level 3 (provision sim 34.3→21.5 s same seeds) |
| 37669895633 | e0ace416 | + test opt-level 3 (transition: every instance rebuilt) | fail + timeout | — | fail: the S4 scaling test read 6.5x growth on an unchanged path — 200k-call trials at opt-0 span ~14 ms and every one of five caught the storm (fixed: 50k calls, fastest of nine, same bound and verdict); build timed out on the 808 s opt-3 full rebuild. What it proved: workspace tests 265→195 s, bot-full suite through in one piece at opt-3. Timeouts hold at 20 min |
