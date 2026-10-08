# Production readiness of `lgwks_bot`, and why it replaces non-AI automation

Status: **diagnostic, provisional, and currently a NO-GO for unattended
consequential automation.** The purpose of this document is to show what is
measured, what is proved, what is only argued, and what would have to be true
before a person should point this at money, messages, or infrastructure without
watching it.

It is written to be argued with. Every score below is against a named gate, and
several of them are ❌ against this crate rather than against a rival. If a
number here is wrong or a gate is the wrong gate, that is a defect in this
document and it should be filed.

Companions: [`framework-comparison.md`](framework-comparison.md) for the field
survey, [`frontier.md`](frontier.md) for the nine research areas,
[`effect-kernel.md`](effect-kernel.md) for the durability design,
[`async-parity.md`](async-parity.md) for the runtime surface, and
[`../bench/README.md`](../bench/README.md) for the measurement rig and its raw
numbers.

---

## 1. The headline, before the method

**This does not currently pass its own release gate.** Issue
[#109](https://github.com/srinji-kaggss/logicalworks-crates/issues/109) held
eight invariants. Every repair landed by 2026-09-22 (#99 as #110, #107 as #117,
#108 as #116, and the five ladder rows), and #109 was closed on 2026-09-23 by
commit `7d9ad29f` (#142). That commit ran **five of the eight external
observations** in `tests/it/durable_crash_observation.rs`: the journal-ladder rows
(#100, #101, #102, #104, #106) were driven against a real file-backed journal,
killed mid-ladder with a real `SIGKILL`, restarted, and the recovered answer
asserted against the one the design names. The other three closed on their
repair and its in-process regression tests, not on an external observation:
#99 (the ECS poll path) has no real-store run and #108 (`locator_eligibility`)
has no real frame. Of #107's rows, T21 is now observed by
`tests/it/process_escape.rs`, which drives a real descendant that calls `setsid`
out of the group and asserts that the cleanup captured it while its leader lived,
stopped it by pid (`kill -0` reports it gone) and says so in a complete
containment report (INV-BOT-112,
[#263](https://github.com/srinji-kaggss/logicalworks-crates/issues/263)) — on
Unix only — and T22's `ProcessSpec` surface landed in #128. A closed issue is not a green
observation of the machine: the verdict below stands until those rows have one
and the mess rows of §4.4 close.

Stated plainly: you can ship this today for attended, inspectable automation
where a human is watching and can intervene. You cannot ship it yet for the
thing it is built for — a bot that runs for weeks and is allowed to act without
you.

**Update, 2026-09-28.** One of the three unrun rows moves. INV-BOT-15 — one
owner serializes journal writes and an ambiguous write is never reported as a
clean failure — now has a deterministic proof in
`journal::file::a_dropped_waiter_poisons_the_handle_and_a_reopen_does_not_duplicate`:
the waiter is dropped mid-write, the handle is poisoned, and a reopen replays
without a second effect. The simulation families now contribute 1,990
executable deterministic cases under Nextest, with 1,111 source-visible
`#[test]` cases under `tests/sim/` or `sim_*` files, including tenant isolation
at 5,000 stores and a reported ceiling at the concurrency tiers. The release
gate still does not pass: #99, #107 and #108 remain unrun, and no p50/p95/p99
SLO has been measured on the named VPS profile, so the verdict above is
unchanged. The next regression is still where `INVARIANTS.md` says it is.

**Update, 2026-10-04.** What moved, each with the test or run that moved it:

- **Journal status stopped destroying information** (#257, #259, #260). A failed
  verification is now `AttemptStatus::VerificationFailed` instead of folding into
  `Applied`; `Verified` can be superseded by a later verification, so a verdict can
  go stale and say so; `Recovered` exposes the latest predicate, version and
  observation digest, and a per-attempt history with journal positions. Real
  file-backed reopen tests: `a_failed_verification_survives_a_reopen_as_its_own_state`
  and `a_verdict_is_revised_by_a_later_verification_after_a_reopen`.
- **`Status::Attested` is bound to the repository** (#258). A recorded revision must
  be one commit in the history of `HEAD` and the enforcer must be unchanged since,
  or the entry is refused; where Git cannot answer, it is refused. Five scratch-repo
  tests in `lgwks_deps::invariants`.
- **All 36 acceptance rows now name the tests that address them**, in
  `docs/acceptance/t-rows.toml`, and `scripts/acceptance-receipts.py` ran exactly
  those tests against the checked-out head into a SQLite database under the
  state directory (`$XDG_STATE_HOME/lgwks-acceptance/receipts.sqlite`, or
  `~/.local/state/...`), one row per revision, row, test, platform and feature
  set, so a re-run is idempotent and two revisions coexist.
  The run at `740d3c68` passed 215 of 215 named tests in 44.4s: 30 rows observed
  `exercised`, and six still `present` with the gap written down rather than
  rounded up — T10, T11, T14, T17, T21 and T31. Fifteen of the named tests are
  row-addressed, so `cargo nextest list --workspace -E 'test(/_t07$/)'` answers
  which tests address a row without reading the map. The receipt can lower a
  row's claim and never raise one, and `scripts/acceptance-receipts.py --check`
  fails when the spec's generated table disagrees with the map and the receipt
  for one exact revision. In CI that receipt is built from the JUnit reports the
  four lgwks-bot shards already wrote, by a job that needs no Rust toolchain and
  no build and takes about 0.2 s, so the named tests execute once per run rather
  than twice. A row reads `accepted` only when that revision is the
  head being rendered. No row is `accepted`; the externally bounded subprocess
  and per-backend OS campaign has not run. `--check` reads the local database, so
  it is a local gate rather than a CI step: a CI checkout has no receipt for its
  own head until the run records one.
- **The authoring contract (#87, §7 item 6) was measured four ways**, not argued:
  160 fixed-model trials across the old surface, the facade, the facade plus
  `FanOut`, and the `futures` crate. See `bench/ai-authoring/README.md`. The finding
  is unflattering: on the two tasks the oracle asks, the ecosystem standard is the
  better authoring surface (39/40, no off-sheet code, 0.15 repairs, 37 lines), and
  only 12 of 40 facade solutions used the facade. The oracle does not ask for what
  `lgwks_bot` adds over `futures`, so this does not say the crate has no case; it
  says the case has to be measured on tasks that ask for it.
- **Two gaps a real downstream migration found are closed**: `ProcessSpec` can send
  a stream to a file, and the runtime `Builder` can set the worker stack size. The
  migration itself (Keel, 64 compile errors to 0, 4,035 unit tests passing) is in
  `docs/guides/lgwks-bot/migrating-to-1-0.md`.
- **Multi-tenant (§4.8) has negative tests at the `Auth` boundary under
  concurrency**: sixteen threads, two grant sets, 256 rounds, and the proof-cover
  checks in both directions.

- **Hyperscale moved off ❌** (#269). An open-loop saturation curve at six
  in-flight bounds from 64 to 131,072 with the knee declared on both the facade
  and a raw Tokio baseline; 1,048,576 tasks concurrently admitted with peak RSS
  per in-flight task reported; a 30-second overload at twice the knee that
  recovered with nothing lost; and the facade's per-task allocations cut from
  15.33 to 3.61 against the issue's ≤ 6 target. §4.2 is ⚠️, not ✅: the
  1–2 vCPU / 1–2 GB VPS profile the estate asks for is **not measured** and
  cannot be on this host, and the async path's per-task overhead is still
  1.4x–4.5x raw Tokio on a short body.

What did not move: CI is still over five minutes, process containment is still
Unix-only, the seeded sweeps still have no shrinking, and `process_escape`'s
intermittent failure is still unexplained. The verdict above is unchanged.

**Update, 2026-10-05.** The hold the journal module documentation named, an
acknowledged final frame whose length field was changed being truncated on open,
is closed by a refusal on the stored head
([#262](https://github.com/srinji-kaggss/logicalworks-crates/issues/262), §4.6).
The verdict above is unchanged, and no SLO has been measured on the named VPS
profile.

**Regenerated, 2026-10-05
([#280](https://github.com/srinji-kaggss/logicalworks-crates/issues/280)).** This
pass corrected what had gone stale rather than what had moved, each from the tree
at `977c79d4` or the GitHub API, with the command in the PR: the #109 rows (§1,
§4.6, §7 item 1) are described by how each one closed rather than as "unrun"; the
async runtime has a matched benchmark (§4.9, §7 item 3); every crate is tagged at
`1.0.0` or later (§4.5); the CI job count is 21 (§4.7); the authoring contract
was measured (§4.1, §6, §7 item 6). No ❌ row was upgraded. What still holds the
verdict at NO-GO: the hyperscale saturation curve (§4.2, #269), the real-world
mess rows (§4.4), Unix-only and `setsid`-escapable containment (§4.7, #263),
capacity isolation (§4.8, #268), and the three #109 rows with no external
observation.

Everything below is the reasoning behind that sentence.

## 2. The claim under test

> `lgwks_bot` replaces every non-full-AI automation and RPA solution: UiPath,
> Automation Anywhere, WalkMe, Zapier, n8n, Node-RED, Ansible, Robocorp,
> Playwright scripts, and `cron` plus shell.

The field survey in [`framework-comparison.md`](framework-comparison.md) tested
a weaker version of this claim and found the structural answer. Every framework
examined implements the same five parts — Observe, Decide, Act, State, Bounds —
and there is no sixth part anywhere. The differences are four parameters:
observation surface, concurrency model, decision source, and authority.

Axes 1 and 3 are where the commercial field competes. Axis 4 is **empty
everywhere in the sample**. Nothing scopes what an action may do at the
invocation, carries proof that a specific call was authorised, or records a
decision with provenance a third party can verify.

So the replacement claim has two halves and they should be scored separately.

**Half A — the mechanism.** Yes. Same five parts, and this is one of them done
in a language that makes the bounds checkable. That is a claim about category,
not about maturity.

**Half B — the product.** Not yet. Coverage, connectors, and the observed
behaviour under real-world mess are what people actually buy from RPA, and those
are the three places this crate is thinnest. See §6.

## 3. Why each rival should win

A comparison that only lists a rival's weaknesses is not evidence. Each of these
has a reason to be chosen over this crate, and that reason is real.

| Rival | Why it should win | Where it actually loses |
|---|---|---|
| **UiPath / Automation Anywhere** | Two decades of enterprise selectors, SAP and Oracle connectors, an orchestration server, attended and unattended robot fleets. Nothing here is close on packaged line-of-business coverage. | Runs its rules engine on the application's own thread and carries ambient, unbounded authority. |
| **WalkMe / Pendo / Appcues** | In-page on the live form state, which no external observer can match for fidelity. Ships as a script tag. | Same thread problem, worse: every check competes with the application for the frame budget. No declared limits, no tiered escalation. |
| **Zapier / Make / n8n / Activepieces** | Several thousand SaaS connectors, hosted, zero ops. Connect two APIs in four minutes. | Hosted means the automation's authority is whatever the vendor's OAuth token holds. No local effects, no process control, no proof of what ran. |
| **Node-RED / Huginn** | Visual flow editor, enormous contrib library, runs on a Raspberry Pi. | JavaScript interpreter on every decision, and an unbounded authority model. |
| **Ansible / Salt / Rundeck** | Agentless over SSH, idempotent modules, a whole server fleet in one playbook. | Not a state watcher at all. Reaches outside the host through an unscoped shell, and its unit of work is a playbook run rather than a reaction. |
| **Playwright / Selenium scripts** | Any page, any language binding, raw DOM and network access. | A library, not a runtime. Every caller reinvents state, retry, bounds and authority, and most of them get authority wrong. |
| **cron + shell** | Zero dependencies, zero learning curve, forty years of operational knowledge. | No observation, no change detection, no authority, no record. |

Each rival's strength is on axes 1–3 of the survey. The table is here so the
gaps in §6 are read as gaps rather than as surprises.

## 4. Nine axes

The gates are the estate's SLO evidence matrix. Each axis is scored ✅ (measured
or proved at the stated gate), ⚠️ (partially covered, with the missing half
named), or ❌ (not covered at a level that supports a production claim).

### 4.1 Frontier — does the design beat credible alternatives?

**✅ — argued from a survey, with the winning axis named.**

[`framework-comparison.md`](framework-comparison.md) covers eight systems across
crawler, Discord-bot, browser-agent, scraping and commercial-guidance families.
[`frontier.md`](frontier.md) covers nine research areas with fetched leaderboard
and paper numbers (Similo 88.0% vs 75.6% on real drifted pages; ScreenSpot-Pro
18.9% → 82.7% in eighteen months; the false-heal study's overlapping score
ranges that no threshold separates).

The design position is one axis nobody in the sample occupies: **authority
bounded at the invocation, with decision provenance a third party can verify.**
`GrantSet` mints the only `Auth`, `Auth` travels with the call, and `T4` is the
gate-soundness theorem.

**Scored against the ecosystem standard on authoring, the facade loses.** The
2026-10-04 fixed-model runs (`bench/ai-authoring/README.md`) put the facade, the
facade plus `FanOut`, the old `rt` surface and the `futures` crate on the same
two oracle tasks, 40 trials per arm:

| arm | full pass | sheet API used | hand-rolled `std::thread` | mean repairs | mean lines |
|---|---:|---:|---:|---:|---:|
| `futures` combinators | 39/40 | 40/40 | 0/40 | 0.15 | 37 |
| old `lgwks_bot::rt` | 38/40 | 38/40 | 1/40 | 0.85 | 68 |
| `script`/`task` facade | 37/40 | 12/40 | 24/40 | 0.75 | 140 |
| facade + `FanOut` | 37/40 | 31/40 | 8/40 | 1.07 | 70 |

Pass rate does not separate the arms at ten trials per cell; what does is that
only 12 of 40 facade solutions used the facade, and `futures` did the same work in
about a quarter of the facade's lines.

The oracle does not ask for what `lgwks_bot` adds over `futures` (durable steps,
authority, repair), so this does not say the crate has no case; it says the case
has not been measured on tasks that ask for it
([#270](https://github.com/srinji-kaggss/logicalworks-crates/issues/270)). Until
it is, the frontier claim rests on the authority axis above and not on authoring.

*Not covered:* the survey sample is eight systems and one patent reading, not
the whole market. A rival that occupies the same axis would falsify the
position, and no one has looked for one since the survey ran.

### 4.2 Hyperscale — more than a million concurrent, correct

**⚠️ — measured at a million concurrent and through a saturation curve on this
host; the estate's VPS profile is still not measured, and the async path's
per-task overhead against raw Tokio is still 1.4x–4.5x on short bodies.**

Every number is from `bench/async`, is this host, this toolchain and this run,
and is read with its condition beside it. Host: **Apple M5 Pro, 15 cores, 24 GB,
macOS 27.0, rustc 1.99.0.** That host is **shared** and its one-minute load
average ran from 2 to 97 across the work, so the rig now reads and prints the
load with every run: a scenario table taken with three other builds running on
it reported the *baseline* at 1.08 ms where an idle host reported 0.35 ms, a 3x
shift in the control leg of a paired comparison. Each table below carries its
load, and the paired facade-versus-baseline columns are not contaminated by it
because both sides of a point see the same load.

**The knee, at six in-flight bounds** (backpressure door, load 79.13, 6:48.56
wall, peak RSS 250,953,728 bytes). Offered rate against a derived per-bound body
cost so every ceiling's declared capacity is 20,000 arrivals/s; knee read against
`max(50 ms, one further service time)`:

| bound | facade knee offered/s | achieved/s | p99 at knee | baseline knee | baseline achieved/s |
|---:|---:|---:|---:|---:|---:|
| 64 | 5,000 | 4,987 | 6.5 ms | 5,000 | 4,979 |
| 1,024 | 5,000 | 4,763 | 59.7 ms | 5,000 | 4,754 |
| 10,000 | 20,000 | 13,340 | 504.9 ms | 20,000 | 13,329 |
| 16,384 | 20,000 | 10,968 | 831.5 ms | 20,000 | 11,009 |
| 100,000 | 80,000 | 13,331 | 5,008.0 ms | 80,000 | 13,339 |
| 131,072 | 80,000 | 10,589 | 6,551.5 ms | 80,000 | 10,593 |

**The facade and the raw baseline declare the same knee at every bound**, to
within 0.5% on achieved rate at five of the six. At a service time of 3.2 ms or
more the ceiling is the binding constraint, not the accounting.

**Past the knee it refuses, and it refuses at exactly the declared bound.**
`try_spawn` at 320,000 arrivals/s: bound 16,384 admitted **exactly 16,384**,
bound 100,000 admitted **exactly 100,000**, bound 131,072 admitted **exactly
131,072**, both sides, to the same number; 59% of arrivals refused and counted in
`Stats::refused`; peak RSS unchanged at the bound's worth of state; the p99 of
what *was* admitted still one service time.

**A million concurrent, reached.** 1,048,576 tasks **concurrently admitted** —
the tier is the bound and every body parks until the whole tier is admitted, so
the peak is observed rather than inferred — all completed, on both sides:

| tier | facade peak RSS | facade RSS per in-flight task | baseline RSS/task | facade p50 | baseline p50 |
|---:|---:|---:|---:|---:|---:|
| 10,000 | 27,115,520 B | 2,712 B | 3,267 B | 16.3 ms | 12.2 ms |
| 100,000 | 210,501,632 B | 2,105 B | 2,672 B | 100.8 ms | 72.0 ms |
| 1,048,576 | 2,137,751,552 B | **2,039 B** | 2,595 B | 1,007.7 ms | 726.7 ms |

**Peak RSS per in-flight task at the million tier: 2,039 bytes**, against the raw
baseline's 2,595 — memory favours the facade, by 21%. Admission latency does not:
1.39x the baseline's at both wide tiers.

**Overload and recovery** (load 19.88): a 30-second overload at twice the knee
built a 229,554-arrival queue and took the served p99 from 6.7 ms to 11.4 s, with
**nothing refused and nothing lost** — offered = admitted = completed on both
sides, checked by the rig's conservation gate. The facade drained in 0.023 ms
against the baseline's 4.801 ms; both sides' served p99 was back inside its own
baseline at the first recovery window, 500.7 ms and 505.1 ms after the overload
stopped. The recovery figure is a **bound at one 500 ms window's resolution**, not
a point estimate.

**Allocations, per supervised task**, 1,024 tasks at bound 8, one harness:
**15.33 before, 3.61 after**, against a raw baseline's 2.02 — the issue's ≤ 6
target met, by cutting an eagerly-built `watch` channel per cancellation token
and a 100 ms timer armed on every uncontended spawn. The remaining 3.61 is
itemised in `bench/async/README.md` with the guarantee each allocation pays for.

**The latency target was not met, and that is the finding.** The issue asks for
≤ 2x the baseline at p99 on every scenario; `quiet-async-bot` is **4.49x**. The
same 76% cut in allocations moved the paired ratios not at all — the remaining
gap is work (the per-spawn reap, the identity map, the `TaskOutcome` the wrapper
builds), not allocation, and closing it is a different change.

*Not covered, and named:*

- **The 1–2 vCPU / 1–2 GB VPS profile is NOT measured.** macOS exposes no
  cgroup, no `taskset`, no `taskpolicy` CPU set and no `cpulimit` — all four
  checked on the reference host, all four absent — so no run here can be
  presented as that profile's. The closest runnable thing, `--workers=2`,
  produced **identical knees at every bound**, because every body in the sweep is
  a timer: a thread count is not a vCPU count, and this workload would not
  separate them. A CPU-bound profile needs a CPU-bound body and a machine whose
  cores are 1–2, and neither exists here.
- The synchronous rig still reports only `ns/tick` for a schedule of up to 256
  sources, and `Bot::tick` beyond 256 sources is still unmeasured.
- The per-task overhead against raw Tokio is still 1.4x–4.5x on a ~1 µs body,
  with the 4.49x worst case named above rather than averaged away.
- Every figure is one host. Nothing here is a cross-platform claim.

*To close the axis:* the VPS profile on a real 1–2 vCPU box with a CPU-bound
body, and the per-task overhead reduced on the short-body path. "Runs for weeks"
is now a measured knee and a measured recovery rather than a design intent, and
it is still not a measured week.

### 4.3 Idiomatic — ownership, errors, lifetimes sound

**✅ — compiler-enforced, with one named hole in the proof chain.**

- `unsafe_code = "forbid"` at workspace scope. Not `deny`. A `deny` is defeated
  by any single `#[allow]` anywhere, including one smuggled in by a macro this
  workspace does not own; `forbid` cannot be lowered from source and a
  conflicting allow is a hard `E0453`.
- The anti-pattern corpus's 75 entries against 75 configured lints (69 `clippy`,
  5 `rustc`, 1 `rustdoc`); the four names in the corpus that silently do nothing
  are corrected. The rest need Miri, perf counters, allocation profiling or
  human review, and the corpus and the lint table are not a partition — see
  `CODEBOOK.md` §2.3. The ceiling is published rather than claimed.
- Every `#[allow]` and `#[expect]` must carry `reason = "..."`.
- `clippy --workspace --all-targets --locked -- -D warnings` is a required gate
  lane and is green, across a feature matrix and a `--no-default-features` pass.
- 15 integration test files covering authority, tick semantics, the effect
  journal, durable dispatch, identity, flow parsing, locator eligibility,
  process ownership, the async tier, and the no-default build.

**The named hole.** [`proofs/`](../proofs/README.md) kernel-checks seven theorems
about the condition language and the capability gate. It is **not a refinement
proof**. The theorems are about a hand-written abstract model; nothing proves
`spec.rs` implements that model. The honest form of the claim is "the design has
these properties, and the implementation is argued not to express the states
that would break them." That second half is an argument.

### 4.4 Generalized — works across declared inputs, including messy ones

**⚠️ — the declared input space is covered. The messy one is not.**

Covered and gated: per-feature compile lanes for `lgwks_std` (7 of its 12
non-`default`/`full` features — `hash`, `pattern`, `json`, `wire`, `http`,
`online`, `fs-raw`) and three `lgwks_deps` single-feature checks (`tokio`,
`gpui`, `ml-candle`+`ml-tokenizers`); 4 `lgwks_bot` legs (no-default, `full`,
`net`+`process`+`fs`+`signal`, and `wasm32-wasip1` with `rt`) and one combined
28-grammar `lgwks_ast` invocation; plus RON flow parsing. It does **not** cover
every crate or every feature: `lgwks_deps` and `lgwks_macros` have no bare
`--no-default-features` lane, and `lgwks_std`'s remaining 5 features (`core`,
`trace`, `random`, `ron`, `process`) have no per-feature lane of their own. The
28-grammar figure is `DECLARED_GRAMMARS` in `crates/lgwks-ast/src/lib.rs`, not
a wider matrix.

**This is the axis the whole product turns on, and it is where the gap is
largest.** The reason automation is hard is not the happy path. It is that
reality delivers a stream of half-formed, mistimed, interrupted, conflicting
inputs, and a bot that handles only well-formed ones is a demo.

The state of that, honestly:

| Real-world mess | Covered? | Where |
|---|---|---|
| The request may or may not have landed before the process died | ✅ tested | `durable_dispatch`, `OutcomeUnknown` barrier |
| The journal itself cannot be read | ✅ tested | `durable_dispatch`, `#123` |
| An event returns, or a payload is equal but the event is new | ✅ tested | `durable_dispatch`, `#129` |
| A spawned process left orphans | ⚠️ partly | `process_ownership` (T19/T20); `process_escape` stops a real `setsid` descendant by pid and observes it gone (T21, INV-BOT-112); a descendant orphaned to init before the cleanup, and every non-Unix target, are still uncontained (#263) |
| A locator resolved to the wrong frame or wrong kind | ✅ tested | `locator_eligibility`, `#108` |
| Two equal payloads, distinct event ids | ✅ tested | `durable_dispatch` |
| A contradictory outcome overwriting a settled one | ✅ tested | `durable_dispatch`, refused |
| **The element is gone, not moved** | ⚠️ studied, not coded | [`frontier.md`](frontier.md): false-heal 40.5–57.1% on removed elements; decoy and drift score ranges **overlap**, so no threshold separates them |
| An OS dialog stole focus mid-flow | ❌ | no coverage |
| A file was half-written when read | ✅ tested | `stability::read_stable_file`: two reads must agree on length, mtime and digest, an unsettled subject is `BotError::UnstableObservation` (pending, `NotDelivered`, never a change); `tests/it/stability.rs` drives a real file under a real child-process writer, `tests/it/sim_stability.rs` sweeps 1,024 seeds |
| Clock skew between two hosts | ❌ | no coverage |
| A credential expired mid-run | ✅ tested | `GrantSet::grant_expiring` + `Auth::check`: a lapsed proof is `BotError::CredentialExpired` (`Refused`, `RetryClass::Never`), an upstream 401/403/404 is `BotError::CredentialRejected` carrying a `NeedSet` repair (`cap::is_credential_status`, `GhError::repair`), wired through `PrSnapshotSource::poll` and every `gh` flow; `tests/it/credential.rs` spends a real credential's life on a real wall clock over a real file, `gh_binding.rs` and `sim_review_path.rs` drive a real `gh` child refusing the token, `tests/it/sim_credential.rs` sweeps 1,024 seeds |
| The locale changed a date or number format | ✅ tested (macOS) | every `lgwks_std` reader of human text (RFC 3339, hex, percent, JSON/RON numbers) answers the same under `C`, `de_DE`, `ar_SA`, `fa_IR` and `ja_JP` in their own time zones and refuses Arabic-Indic and Persian digits, a decimal comma and a grouping space; the one reader that leaves the process, `ps -o lstart`, is pinned to `TZ=UTC0 LC_ALL=C`. `tests/it/sim_locale.rs` sweeps 1,000 seeds in-process and re-runs the sweep and a process-start read in a child under each locale (INV-STD-LOCALE-1); removing the `ps` pin fails it. The in-process readers consult no locale, so for them the child re-run guards against a future libc call rather than discriminating one; a host without these locales (the Linux container) falls back to `C` and passes without exercising them |
| Two operators edited one record | ❌ | no coverage |
| The app updated and the selector no longer resolves | ❌ | no coverage in the wild |
| Accessibility tree mutated during a read | ❌ | no coverage |
| A non-idempotent effect was sent twice by a retrying proxy | ⚠️ designed | the journal ladder refuses a second `OutcomeObserved` with different evidence; untested against a real retrying proxy |
| The user cancelled halfway through | ⚠️ partly | `tests/it/sim_cancel_under_load.rs` stops a real `Host` with 1,000 flows in flight at a seeded point, over 1,000 seeds. Every flow reports `Succeeded`, `Cancelled` or `Refused`; the host's admitted and refused counts agree with the reports; no body outlives the call or starts after the stop; every permit returns. A run still queued at the stop is `Refused` whatever order the stop's wakes arrive in (INV-BOT-20). Before that rule the same seed did not replay. Supervised processes under a stop are `process_escape` (INV-BOT-156), at tens of processes rather than 1,000 |

The rows marked ❌ are not oversights to be fixed in an afternoon each. They are
the reason the commercial RPA vendors have large QA organisations. Closing the
Generalized axis is the bulk of the remaining work on this crate.

### 4.5 Decoupled — components independently replaceable

**✅ — the seams are real and are load-bearing.**

- `lgwks_deps` is a dependency storefront with a closed tier vocabulary
  (`boundary` / `vendor`) and a checked `APPROVED.toml`. The tier is enforced
  in one respect only: a `vendor` approval over a registry or git edge is
  refused (`VendorTierConflict`). Admission is otherwise decided by owner,
  source, origin, requirement, features, scope and licence, not by tier. No crate authors a
  `tokio` edge; the async surface is a facade reached through the storefront.
  A build cannot acquire an unaudited dependency without the gate refusing.
- `EffectJournal` is a trait, and `DurableAck::new` is public specifically
  because an adapter outside the crate is where an acknowledgement is produced.
  The seam is empty by choice, not by accident.
- `state` and `authority` are separate: `GrantSet` does not own the schedule, the
  journal does not own the effects, the verbs do not own the runtime.

*Not covered:* downstream compatibility rests on one consumer. Keel's migration
to `lgwks_bot` 1.x (`docs/guides/lgwks-bot/migrating-to-1-0.md`) is the only
external caller with evidence; a workspace caller search proves nothing about
others. The newest tags (`git tag --sort=-creatordate`) are `lgwks_std-v1.1.0`,
`lgwks_bot-v1.1.0`, `lgwks_deps-v1.1.0`, `lgwks_macros-v1.1.0` and
`lgwks_ast-v1.0.0`, and the manifests at `977c79d4` read the same versions. A
source tag is not a registry upload.

### 4.6 Ephemeral — survives process loss

**⚠️ — the design is right; five of #109's eight observations ran against a real
store, and three rows closed without one.**

This is the axis the effect kernel exists for, and the axis #109's register
covered until it closed on 2026-09-23.

Landed and unit-tested against `MemoryJournal`:

- the four-rung journal ladder, with `OutOfOrder` refusing a skipped rung
- `recover`'s single fold into `AttemptStatus`, so no second authority derives a
  second answer
- `OutcomeUnknown` as a barrier that never auto-replays
- durable input identity separating an event from a content value (`#129`)
- an unreadable journal reported as `EffectRefused`, never as `NoSuchWork` (`#123`)
- external handoff refused before the effect leaves when the journal cannot
  outlive the process
- an acknowledged outcome that cannot be upgraded by a weaker promise (`#119`)

**Every one of those was proved on a journal that lives in process memory.**
Now five of them have been proved on one that does not. `FileJournal`
(`journal/file.rs`) is a real store: a length-framed file whose appends are
written and `sync_all`-ed before the acknowledgment is minted, whose stored
chain heads make a tampered frame a refusal rather than a trim, and whose torn
tail — an append a killed writer never finished — is truncated on open
because it was never acknowledged. `tests/it/durable_crash_observation.rs` runs
the register's observations against it:

- **#106**: a child process walked a key to `OutcomeObserved(Applied)`, was
  `SIGKILL`-ed, and the restarted journal recovered `Applied`; a duplicate
  settlement is refused out-of-order; a new key climbs fresh.
- **#102 / #104**: a child killed between `DispatchPrepared` and any outcome
  recovered `OutcomeUnknown`, named in `uncertain()`, never `NotApplied`;
  evidence appended after the restart settled it without a resend. A
  settlement followed by a torn (failed) recording append still recovers the
  settlement.
- **#101**: after the kill, a re-admission under a changed digest is a second
  identity with a fresh ladder — `recover()` reports both, never one.
- **#100**: the external marker exists only after the durable ack, and the
  real store admits the handoff its promise earns (`ProcessCrash`, honestly
  below `PowerLoss`).

`Until that run exists` no longer describes these rows. #99 and #108 closed on
their repair and in-process tests without an external observation, and #107's
T21 is observed only as an honest non-containment (§1); with rows of its own
axis unobserved, this axis stays ⚠️ and the §1 verdict stands.

**A length prefix that lies about an acknowledged frame is refused, not
trimmed ([#262](https://github.com/srinji-kaggss/logicalworks-crates/issues/262)).**
Until this change the one hold the module documentation itself named was real: an
acknowledged final frame whose stored length was changed from `L` to `L + k`
was truncated on open, because a complete prefix over a frame the file cannot
hold looks like an append cut short, and the head-short shape (`k` in `1..=32`)
was classified as torn without being asked at all. An open now decides on the
bytes. Any payload length under the bytes present that reproduces the stored head,
or a later whole frame that authenticates, was acknowledged, and the file is
refused with `JournalError::Corrupt` and left byte-identical; only a tail that is a
prefix of one cut-short append is trimmed. Observed by
`journal::file::tests::a_lengthened_acknowledged_final_frame_is_refused_not_trimmed`
(`k` in `1..=1024`, file hash compared),
`an_inflated_non_final_length_is_refused_and_every_byte_survives`,
`an_append_cut_at_every_byte_of_the_final_frame_is_repaired` (the control: no
genuine cut is refused), and by `tests/it/sim_journal_tail.rs`, which sweeps seeded
journals of one to a thousand frames through inflated, deflated, bit-flipped and
cut prefixes, two tenants concurrently. **What it does not cover:** a final frame
whose length and head are both damaged has nothing to authenticate it and is
trimmed (`a_final_frame_with_a_lying_length_and_a_damaged_head_is_the_stated_limit`),
and a hand that truncates a file mid-frame is indistinguishable from a crash. The
journal stays below the unattended-automation bar for the reasons in the ceiling
paragraph below, not for this one. The run store and the repair ledger shared the
same defect through `frame::read_raw`, which treated every cut frame as a clean end;
they now ask the same question of the same bytes, with each store's own head over
its decoded record (`task::store::tests` and `task::ledger::tests`, including a
ceiling-sized noise tail decided in bounded time). Their tests were not run against
the unfixed code, so for those two stores the control is the cut-append test only.
The cost of the decision is measured, not estimated
(`cargo run -p lgwks_bot --features script,ephemeral --release --example tail_cost`,
64-frame files, Apple silicon, one run, peak RSS 6.3 MB for the whole process):

| Open of | journal p50 / p95 / p99 | run store p50 / p95 / p99 |
|---|---|---|
| an undamaged file (n=400) | 65 / 100 / 121 µs | 268 / 319 / 370 µs |
| a final frame lengthened by 1–2048, refused (n=400) | 109 / 130 / 154 µs | 210 / 245 / 360 µs |
| an append cut at a seeded byte, repaired (n=60, an `fsync` each) | 3,127 / 4,010 / 5,922 µs | 3,806 / 3,930 / 4,687 µs |
| a ceiling-sized prefix over noise, trimmed (n=8) | 2,266,477 / 2,371,675 / 2,371,675 µs | 5,685 / 5,864 / 5,864 µs |

The last row is the stated worst case and it is slow for the journal: the search
tries every payload length under the bytes present, each is a blake3 over a
length-framed payload that cannot be extended from the one before, so a 64 KiB tail
costs about 2.3 s once, at open. It is bounded and it needs a tail that is a
maximal-size frame cut mid-write (or a hand-made one); a journal's ordinary frames
are a few hundred bytes and cost the third row. The run store decodes each candidate
and a wrong length fails at once, which is why its worst case is milliseconds.

The adapter's costs are measured, not estimated: a throwaway release-mode
probe (since deleted) timed each path on the development machine's file
system, and the numbers with their method are recorded in the pull-request
evidence rather than gated in the tree. One `sync_all` is the platform's
durable-append floor and costs about 3.0 ms there; a per-rung append pays it once per rung, and
`FileJournal::compare_and_append_all` — the group commit every durable log
converges on — pays it once per batch, all-or-nothing, with every
acknowledgment still minted after the shared flush. The per-key ladder check
is indexed, so an append's cost does not grow with the journal's length
(measured flat from an empty journal to 32,000 prior attempts, against the
linear walk it replaces, which measured 98× slower at 8,000). Replay holds
every committed entry *of one generation* in memory and reopens in time linear
in that generation's file, which the continuation below bounds.

**The ceiling is a backstop now, and continuation is what a long run meets
(#267).** Both `open` and `append` still refuse with
`JournalError::CapacityExceeded` at `MAX_JOURNAL_BYTES` or `MAX_JOURNAL_EVENTS`,
naming the limit, and never truncate. A journal opened with
`FileJournal::open_continuing` continues itself before it gets there: at 80% of
either ceiling (`ContinuationPolicy::declared`, which leaves the settlement
headroom #143 asked for) it writes one sealed checkpoint frame, byte for byte, into
itself and into a successor (`<name>.cont/000001`, then the next six-digit name).
The checkpoint carries the folded state, one record per action for what is settled
and every unresolved attempt in full, so an attempt that was `OutcomeUnknown` stays
`OutcomeUnknown` across the boundary until evidence settles it, and a settled
attempt presented again is refused rather than re-admitted. The controller asks the
trigger on its own append path (`ecs::Effects::append`), so a bot built over a
continuing journal continues without the host doing anything
(`durable_dispatch::the_controller_continues_its_own_journal_on_the_shipped_append_path`).

Measured, not estimated
(`cargo run --locked --release -p lgwks_bot --example journal_continuation`,
Apple silicon, one run each, the shipped trigger):

| | default allocator | `MallocSpaceEfficient=1` |
|---|---|---|
| 10,000,000 attempts (1% unknown, 2% failing verification) | 488 continuations, 81 s | 488 continuations, 106 s |
| oracle agreement | at every one of the 488 boundaries, and after a reopen | same |
| every attempt presented a second time | 10,000,000 refused | same |
| resident set at 1 M / 5 M / 10 M attempts | 1,152 / 1,155 / 870 MB | 24.6 / 16.3 / 18.8 MB |
| peak resident set, whole process (`/usr/bin/time -l`) | 1,205 MB | 53 MB |
| reopen of the live successor behind 1 K / 100 K / 1 M attempts, p50 | 79 / 59 / 58 µs | 89 / 65 / 64 µs |
| reopen p99 | 104 / 78 / 79 µs | 135 / 97 / 85 µs |
| live successor on disk behind 1 K / 100 K / 1 M attempts | 19,780 / 20,100 / 20,100 bytes | same |

The two columns are one program. The journal's live memory is the right column,
flat at about 20 MB; the left column's resident set is macOS's allocator keeping
freed large blocks for reuse (`vmmap` attributes 276 MB at 300,000 attempts to
"Malloc Large (empty)" against 28 MB of live heap), which a host under memory
pressure gets back and which glibc returns for blocks over its mmap threshold.
Neither column grows with the history: a successor weighs its carry, not what is
behind it.

Crash safety is proved with real processes:
`journal_continuation::a_kill_at_every_seal_boundary_leaves_exactly_one_authoritative_journal`
`SIGKILL`s a child at each of the four write and sync boundaries inside a
continuation. Each reopen through the original path finds exactly one journal that
takes appends, holds every settled attempt as settled and refuses each a second
time, still holds the unknown attempt as unknown, and continues again on demand.
`tests/it/sim_continuation_seal.rs` sweeps the mechanism underneath as 25 seeded
properties, each replayed for an identical trace: where each trigger falls, the
generation names, the seal frame's bytes in both files, every refusal around a
sealed or half-sealed chain, both checkpoint bounds at their size and one past it,
and what a reopen and a streaming replay read back on every file of a chain. The
sweep found three defects the example tests had not: a successor could not be
streamed at all, the action bound was a count no checkpoint could reach, and a
lengthened event behind a carried seal was trimmed as a torn append.

**Not claimed.** Sealed predecessors are kept until the host removes them; the
journal has no retention policy of its own, and the measurement above deletes each
one once its successor exists. A run holding more than 128 unresolved attempts at a
boundary cannot continue (`MAX_CHECKPOINT_UNRESOLVED`), and it is refused rather
than truncated, so a controller that never reconciles still meets the ceiling. A
checkpoint folds at most 160 actions (`MAX_CHECKPOINT_SETTLED`), the most whose
widest records fit the one 64 KiB frame beside a full unresolved carry, so a bot
with more actions than that cannot continue and is refused by that count. And
the byte watermark bounds the file, not the records needed to settle work already
handed to the outside world.

### 4.7 Portable — same semantics on all declared targets

**⚠️ — one operating system is executed, and eight targets are built. A build
receipt is not an execution receipt, and this section says which is which.**

Twenty-two hosted job definitions in `.github/workflows/ci.yml`, the
twenty-one at `977c79d4` plus #276's `target-matrix`. Their `runs-on`
distribution is **18 `ubuntu-latest`, 2 `macos-14` (`gpui-macos`,
`candle-macos`), 1 `windows-latest` (`gpui-windows`), 1 `matrix.os`**, and that
single `matrix.os` job is `appcui-native` — so "three operating systems" is
true of those storefront features and not of the estate. The `lgwks_std` /
`lgwks_bot` / `lgwks_ast` test and clippy lanes are `ubuntu-latest` only. A
`wasm32-wasip1` boundary lane checks that the default feature set compiles for
WASI — a build check, not an executed containment test. `os.uname` and other
host-only calls are behind `platform` dispatch.

**The executed target matrix (#276).** `scripts/check-target-matrix.sh` is a
required lane that runs `cargo check --locked --target <t>` per declared target
and per crate. On `aarch64-apple-darwin` (rustc 1.99.0), 42 checks in 69 s from
an empty target directory:

| Target | `lgwks_std` | `lgwks_std` `--no-default-features` | `lgwks_std` `--features random` | `lgwks_deps` | `lgwks_std` `--features full` | `lgwks_ast` |
|---|---|---|---|---|---|---|
| `aarch64-apple-darwin` (host) | builds | builds | builds | builds | builds | builds |
| `aarch64-unknown-linux-gnu` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `x86_64-unknown-linux-musl` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `wasm32-wasip1` | builds | builds | builds | builds | builds | **not exercised** |
| `x86_64-unknown-freebsd` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `aarch64-linux-android` | builds | builds | builds | builds | **not exercised** | **not exercised** |
| `aarch64-apple-ios` | builds | builds | builds | builds | builds | builds |

**"Builds" is a build receipt and "not exercised" is a named gap, not a pass.**
The nine not-exercised cells each fail with the exact error
`failed to find tool "aarch64-linux-gnu-gcc"` (or the musl, Android, FreeBSD or
`clang` equivalent): `ring`, reached through `http`'s rustls, and the
tree-sitter grammars under `lgwks_ast` compile C per target, and this runner has
no cross C toolchain for them. The lane accepts that one failure reason by name
and fails on any other, so the exemption cannot become a lid. **Support is
therefore claimed for `lgwks_std` on seven targets with the `full` feature set
only where the row says `builds`, and for `lgwks_ast` and `lgwks_deps` on the
targets where their row says `builds` — never for a target that was not
exercised.**

`random` was the module that made the gap load-bearing: it carried a
three-target `compile_error!` while the backend it wraps supported dozens more,
so `lgwks_std` with `random` or `http` could not be built for iOS, Android, a
BSD or WASI at all. It now holds no target list of its own, and its typed
`EntropyError` is what a caller on any of these targets gets when the source
fails.

**Containment per OS, as executed evidence (#263, #337).** On Unix the supervisor
captures the leader's descendants and stops each by pid as well as signalling
the group, so a `setsid` escapee is stopped: `tests/it/process_escape.rs`
observes it gone with `kill -0`, 4 tests × 1,000 iterations with zero failures
on `aarch64-apple-darwin` (4,000 executions, `--retries 0`, `leaky` only where
the test intends it). On Linux the tree additionally runs in its own cgroup v2
scope, killed first with `cgroup.kill`, and the supervisor adopts orphans
through the child-subreaper flag: `sim_confinement ::
a_cgroup_scope_kills_a_setsid_escapee_where_the_mount_allows` passes with the
receipt naming `CgroupKill` where the mount admits a scope (privileged
`rust:1.99.0-bookworm` container, `aarch64`), and falls back to the capture
rounds where it does not; `sim_kernel_owners ::
an_adopted_orphan_is_named_by_its_adoption` proves the attribution (a pid that
is both captured and adopted is signalled, then reaped), and the sweep in
`start` / `confirm_absence` collects the adoptees no guard captured, so a
killed descendant is never left as a `kill -0`-present zombie. The fire test
drives 1,000 supervised depth-3 trees under cancellation at in-flight 100 (ten
rounds of one hundred 7-process trees, 7,000 processes) and at in-flight 1,000
(one round of one thousand 3-process chains, 3,000 processes — the host's
`maxprocperuid` of 4,000 forbids ten thousand processes at once, so the second
level uses chains rather than fan-out): zero surviving descendants in every
round, verified from the OS process table. Cleanup latency p50/p99 559/574 ms
at in-flight 100 and 6,582 ms at in-flight 1,000 (one thousand macOS `ps`
snapshots dominate the second number), peak RSS 13.8 MB and 58.8 MB
respectively. A descendant orphaned to init before the cleanup ran, where no
subreaper adopted it, is still not reachable from the leader: a cleanup that
begins after the leader exited on its own reads no table and reports
`ResidualRisk::LeaderExited`, so its containment is never `is_complete()`
(INV-BOT-158, #347). macOS lanes additionally confine through
`ProcessSpec::confinement` (`sim_confinement ::
a_sandboxed_child_is_denied_its_denied_subtree`): a denied write fails with the
kernel's refusal and leaves no file, while a confined `true` still exits zero.
The sandbox costs one extra fork and exec per spawn, measured on this host at
p50 10.80 ms against 3.17 ms for a bare `true` (n=200 each); the in-process
`sandbox_init` that would avoid it needs an FFI leaf the workspace gate
refuses, and Linux subtree-deny needs Landlock syscall bindings the estate has
not admitted — both recorded, neither shipped. Windows has no containment at
all: a job-object backend needs either an admitted Windows API crate or the
same refused FFI leaf, so the `windows-latest` job stays a build receipt.

**The feature × OS × backend × assurance matrix, and what it does not say.** The
bot's supervised process backend returns `Unsupported` on non-Unix, so a
green `windows-latest` job is a *build* receipt for the Windows target and
never a *containment* receipt: no Windows run in CI can have killed a process
group. A build check and an executed containment test are different facts, and
only the second one is a portability claim about the runtime. The eight-target
matrix above is itself a build receipt: every row was executed by `cargo check`
and none of them ran a test.

*Not covered:* 32-bit, any target outside the eight rows, and any executed test
on a non-hosted-runner target. The performance numbers in §4.9 are from one
machine and are explicitly not a cross-platform claim.

### 4.8 Multi-tenant — isolated under concurrent use

**⚠️ — covered at the stated boundaries, with the missing half named.**

`GrantSet` is a per-bot capability snapshot and `Auth` is a per-call proof. Two-
identity tests now exist at: the `Auth` boundary under concurrency
(`authority::a_tenant_without_the_grant_is_refused_while_the_other_is_served`,
sixteen threads and 256 rounds, refusals named, and
`a_proof_minted_for_one_tenant_does_not_cover_the_others_grant` in both
directions); the artifact store (`proposal::two_tenants_on_one_digest_stay_isolated`
and, racing, `concurrent_writers_of_one_key_commit_exactly_once_per_tenant`);
request keys (`request_key::two_tenants_never_share_a_request_run`); forced
refreshes (`observe_refresh`), the journal at scale
(`journal_scale::concurrent_tenant_appends_scale_with_isolation`), clocks
(`sim_clock_wiring`) and inspection (`inspect_wiring`). Two tenants through one
journal directory, each in its own journal file, under a mid-run kill are `sim_tenant_journal_kill`
(`kill_band_00..07`, 1,000 seeds). The kill lands between appends, tears the
in-flight frame at a seeded byte, or loses only its acknowledgment. After the
restart each tenant holds exactly its acknowledged prefix plus a landed frame,
never the other tenant's keys. The retry at the tail the controller saw lands a
torn append and is refused for a landed one, so every interrupted effect is
present exactly once. Capacity is admitted per tenant by
`Supervisor::with_tenancy` (INV-BOT-151; `tenancy::a_noisy_tenant_cannot_starve_a_quiet_one`,
`a_tenant_that_submits_ten_thousand_tasks_cannot_starve_another`,
`an_adversarial_tenants_spawns_do_not_cost_its_neighbour_throughput`).
A `Scope`'s tenant reaches every body under `each`, `retry` and
`FanOut::run_in` (`sim_tenancy_scope`: two tenants key the same path
differently, every retry attempt shares one tenant and key, `run_in` enters
the caller's step, clock and stop; the standalone `FanOut::run` keeps its
reserved tenant). `each` and `FanOut` still take no permit — tenant admission
bounds who is admitted, not the CPU an admitted body burns — so a scope's
tenant reaches item scopes, not admission.

Measured on one macOS arm64 host (15 cores, shared with other work) and one
Linux arm64 container (4 vCPU, `rust:1.99.0-bookworm`), one run each:

- **Noisy neighbour** (A 10,000 slow tasks, B 100 × ~2 ms, limit 64):
  quiet p99 under the flood is 11.375 µs (macOS) / 5.5 µs (Linux) against
  3.828 ms / 3.084 ms alone — ratios 2‰ / 1‰ against the 2000‰ bound. The
  alone-side p99 in both runs is one slow baseline admission (host noise on a
  shared machine), not the flood: the medians are 583 ns alone vs 1.5 µs under
  the flood (macOS), sub-microsecond apart in absolute terms.
- **5,000 tenants provisioned** (`tenancy_scale`, every admission inside a 5 s
  progress bound, 0 refusals, every task terminal):

  | tier | macOS p50 / p99 / max | macOS peak RSS | Linux p50 / p99 / max | Linux peak RSS |
  |---|---:|---:|---:|---:|
  | 100 | 9.9 µs / 37.4 µs / 339 µs | 6,592 KiB | 7.0 µs / 120.2 µs / 1.91 ms | 9,536 KiB |
  | 1,000 | 7.9 µs / 34.1 µs / 83 µs | 8,128 KiB | 6.9 µs / 80.5 µs / 785 µs | 11,792 KiB |
  | 10,000 | 0.54 µs / 16.0 µs / 34 µs | 16,544 KiB | 0.63 µs / 10.1 µs / 707 µs | 19,060 KiB |
  | 100,000 | 0.46 µs / 14.6 µs / 148 µs | 97,744 KiB | 0.71 µs / 12.2 µs / 693 µs | 100,368 KiB |

  The 100,000 tier holds 100,000 parked tasks (5,000 tenants × 20): ~1 KiB of
  peak RSS per in-flight task on both hosts.
- **Adversarial split** (#351, `tenancy_scale::the_floods_cost_is_scheduling_not_admission`,
  41 interleaved rounds × 2,000 neighbour tasks, bound unchanged at 10%): the
  mean arrival-decision cost (lock + round, `Supervisor::admission_cost`) is
  569 ns alone vs 583 ns attacked (macOS) and 189 ns vs 195 ns (Linux) —
  decision-mean ratios 1024‰ / 1031‰, inside the envelope on both hosts, so
  there is no admission-side cost to fix. The remainder is stated separately:
  arrival decisions are ~29% of a neighbour submission on macOS (8–12% on
  Linux, where the clock read is cheaper); the rest is the task spawn, the
  bounded reap and the executor's polls beside the flood's completions. Wall
  ratios 989‰ / 692‰ — the attacked arm measured faster on both hosts, which
  is scheduling noise, not isolation.

*Not covered:* two tenants appending to one journal *file*; each tenant owns its
own file, so the run proves the kill and the restart keep each file's prefix,
not that a shared file separates tenants. The kill is a cut of the file a real writer produced, not a
`SIGKILL` of two tenants' process (`durable_crash_observation` kills a single
journal for real). The 11.2% neighbour gap was seen only on GitHub's x86_64
runners and is still unreproduced anywhere else: arm64 Linux (container and
dedicated cores) and arm64 macOS show no gap, and the split above attributes
the measured difference to scheduling. An x86_64 4-vCPU host with the split
instrumentation is the run that would confirm or refute the SMT hypothesis.

### 4.9 Performance — fastest correct implementation

**⚠️ — measured honestly on both paths, and the facade loses on both.**

Full method, raw numbers, fairness gate and exclusions are in
[`../bench/README.md`](../bench/README.md). The summary, with the loss stated at
the same volume as the wins:

| scenario | `lgwks_bot` ns/tick | hand-rolled loop ns/tick | ratio | 95% CI |
|---|---:|---:|---:|---|
| `poll-only-64x100` | 3 720.9 | 31.4 | 117.99x slower | [117.35, 119.05] |
| `steady-64x100` | 4 909.2 | 31.9 | 154.37x slower | [151.59, 156.10] |
| `churn-64x1` | 138 037.7 | 52.1 | 2 664.56x slower | [2 630.82, 2 698.80] |
| `fanout-1x64` | 116 680.7 | 45.2 | 2 578.12x slower | [2 555.76, 2 601.45] |
| `wide-256x10` | 66 270.8 | 138.4 | 477.51x slower | [473.00, 481.37] |

A hand-rolled loop doing provably identical work is **118x to 2 665x faster**.
If your workload is a hot path, use the loop. This document says so with a
number rather than burying it. #279's targets (≤ 10x quiet, ≤ 30x churn) are
**missed** on every scenario.

These ratios are larger than the 72x–256x this section used to carry because
that run's bots fired effects with no ledger, no warrant and no record, and
skipped 99 of every 100 polls through a digest seam that has since been
removed; neither configuration is one the crate builds today
([`../bench/README.md`](../bench/README.md) states both). The per-stage profile
(`--profile`, measured on the tick path itself) itemises the gap: the effect
path (`act` — ledger, warrant and journal record) is the largest stage in every
scenario, 41% of a quiet tick and 84–98% of the effect-heavy ones; poll is
445 ns on a quiet tick, down from 7 064 ns before per-source change ticks. A
quiet tick still makes 20 heap allocations (`--alloc-report`), flat in the
number of chains. Admission and cost stay linear in source count with no
superlinear term. `Auth::check` was quadratic in the capability count and is now
`n·log2 n`: 12.9 µs → 3.6 µs at 128 capabilities.

**Two exclusions that favour this crate and are named so they cannot be
unnoticed later:**

1. The rig withdraws `rt`. It measures the **synchronous** `Bot::tick` adapter.
   The async runtime is measured separately, by `bench/async/`
   ([`../bench/async/README.md`](../bench/async/README.md)), against pinned raw
   Tokio doing identical work behind a fairness gate that aborts on any
   work-count mismatch, in the required `bench-async-fairness` lane:

   | scenario | tasks | bound | facade / raw p50 | 95% CI |
   |---|---:|---:|---:|---|
   | `quiet-async-bot` | 256 | 8 | 5.25x | [3.33, 5.62] |
   | `at-capacity` | 512 | 4 | 2.47x | [1.97, 2.94] |
   | `high-fanout` | 2,048 | 32 | 2.21x | [1.98, 2.26] |
   | `single-permit` | 512 | 1 | 1.37x | [1.28, 1.63] |

   and 15.33 allocations per task against raw Tokio's 2.02 (7.6x). This is a
   closed-loop measurement at one bound per scenario; the open-loop saturation
   curve is #269.
2. The comparator is a hand-rolled loop in the same process. There is no
   measurement against UiPath, n8n, Node-RED or Temporal. Such a measurement
   would be a category error dressed as a result, and this document does not
   substitute a claim for one.

**What the multiplier buys** (each is proved at the design level, see §4.3):
no effect without a grant (T4), replay exactness (T6), a total condition language
that decides at a finite bound (T1, with T7 stating why a loop constructor needs
a budget), and determinism by construction.

## 5. Why Rust, without the adjectives

"Blazing fast and safe" is a marketing sentence. Here is what the language
actually contributes, and what it does not.

**Speed, relative to the RPA category.** The commercial guidance platforms run
their rules engine on the application's own JavaScript thread. That is the
direct answer to "WalkMe is too slow", and it is an architectural defect rather
than a tuning problem: every check competes with the host application for the
frame budget. `lgwks_bot` runs beside the process and observes it. Flow
engines (Node-RED, n8n) pay interpreter dispatch on every decision; `Evaluate`
here is a monomorphised Rust closure. JVM and .NET automation platforms pay GC
pauses at exactly the wrong moments — during a UI wait — and Rust has no collector.
Those are category-level differences and they compound.

**Speed, relative to a for-loop.** Slower, by 118x to 2 665x. Stated above.

**Safety, mechanically rather than aspirationally.**

- `forbid(unsafe_code)` at workspace scope. The language will not compile a
  data race into existence in safe Rust, and this workspace does not permit the
  escape hatch.
- Capability is a value. `Auth` is minted only by `GrantSet::issue`, travels
  with the call, and is checked at build and on every invocation. In every
  rival in §3, authority is a token in a config file or an ambient global.
- `forbid`, not `deny`, on the anti-pattern lint set, because `deny` loses to a
  single `#[allow]` and `forbid` is `E0453`.
- Exhaustive enums, `#[non_exhaustive]` on public surfaces, typed error
  hierarchies with `source` chains rather than `String`.
- Bounded channels only; `JoinSet` and `CancellationToken` tracked for every
  spawn; every loop has a bound.
- A condition language with a proved termination bound, which a language with
  iteration does not have (T7 is the theorem that says so).

**What Rust does not give you.** Correctness of the model. Memory safety is not
crash safety, is not protocol safety, and is not "the effect did not fire
twice." Those are the effect kernel's job and §4.6 says how far that job has
actually been verified.

## 6. What is deliberately excluded from this evaluation

Following the rig's own convention, the exclusions are named with their
direction of bias.

- **No comparison against commercial RPA on coverage.** UiPath's connector
  catalogue beats anything here by orders of magnitude. Excluding it favours
  this crate. It is excluded because counting connectors is not an engineering
  comparison, and because the claim being tested is about the mechanism, not the
  catalogue.
- **No human usability measurement.** [`async-sdk-shape.md`](async-sdk-shape.md)
  states the contract — a task author supplies the script, not the machinery.
  #87 implemented it (closed 2026-10-03), and `bench/ai-authoring/runs/` holds
  324 fixed-model trials across six model runs plus 38 dry and mutant control
  trials (`results.jsonl` line counts), including the four-arm comparison against
  `futures` that §4.1 scores. A fixed model is not a person (INV-BOT-141), so
  human authoring time is still unmeasured, and excluding it favours this crate.
- **No cost-per-outcome numbers.** RPA is bought on cost per successful
  transaction. Nothing here measures that.
- **No AI-in-the-loop comparison.** The claim is scoped to non-full-AI
  automation. A model in the decision source is axis 3 of the survey and is
  deliberately out of scope here.
- **No third-party audit.** `proofs/` is a design proof by the author. The
  security posture document states what a reviewer can check without trusting
  the vendor; that is weaker than an external audit.

## 7. The verdict, and the gate that would change it

**Today: not production-ready for unattended consequential automation.**
Production-ready for attended use where a human inspects the record and can
intervene, on the three operating systems CI covers, for workloads whose real-
world mess is one of the seven covered rows in §4.4.

To change the verdict, in the order that matters:

1. **Finish #109's eight external observations.** Five are run
   (`tests/it/durable_crash_observation.rs`): the journal-ladder rows have a real
   store, a real kill, a restart and the designed answer. #109 closed with the
   other three repaired but not externally observed: #99 needs the poll path
   under a real store and #108 a real frame. #107's T21 now has its descendant
   stopped and observed gone on Unix (#263, INV-BOT-112); it still has no
   Windows or orphan-adoption half. The difference they measure — "the bot
   survives its own machine dying" — is still open.
2. **Close the Generalized rows marked ❌ in §4.4.** Focus steal, clock skew,
   concurrent editors, selector drift, a mutating accessibility tree.
   This is the long pole and the reason RPA is hard.
3. **Cut the async facade's cost.** §4.9 measures it at 1.37x–5.25x raw Tokio
   and 7.6x the allocations per task (#269).
4. **Multi-tenant negative tests.** §4.8.
5. **Hyperscale, on the profile the estate names.** §4.2 has the saturation
   curve, the declared knees, a million concurrent tasks and the recovery
   measurement. What it does not have is the **1–2 vCPU / 1–2 GB VPS** figure
   the axis asks for, which needs a machine whose cores are one or two and a
   CPU-bound body — macOS offers no cgroup, no `taskset` and no `taskpolicy` CPU
   set, and the two-thread run this host *can* do produces identical knees to a
   fifteen-thread one because the workload is timer-bound.
6. **Measure the authoring case on tasks that ask for it.** #87's contract is
   built and the fixed-model run says `futures` is the better surface on the
   tasks it asked (§4.1); the tasks that exercise what this crate adds are
   #270.

Six items. The first is a measurement campaign rather than a design problem,
and the second is most of the product.

---

*This document is a diagnostic, not a brochure. It should be regenerated
whenever a gate moves, and the ❌ rows should be visibly harder to leave in
place than to fix.*
