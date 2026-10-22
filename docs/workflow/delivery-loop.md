# Workflow §2, §3: the delivery loop, and RAG

Moved verbatim from `WORKFLOW.md` on 2026-10-22, when every Markdown file in a
repository was held to 120 lines (Director, 2026-10-10). No rule changed in the
move; `WORKFLOW.md` is the index and keeps the § numbers.

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
