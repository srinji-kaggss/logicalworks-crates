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
[#109](https://github.com/srinji-kaggss/logicalworks-crates/issues/109) holds
eight invariants. The code for most of them has landed and is unit-tested
against an in-memory journal. As of `tests/durable_crash_observation.rs`,
**five of the eight have had their external observation run**: the journal-
ladder rows (#100, #101, #102, #104, #106) were driven against a real
file-backed journal, killed mid-ladder with a real `SIGKILL`, restarted, and
the recovered answer asserted against the one the design names. Three remain
unrun — #99 (the ECS poll path under a real store), #107 T21/T22 (a real
descendant tree), #108 (a real frame) — and a green observation of the
ladder is not a green observation of the machine: the verdict below stands
until every row has one and the mess rows of §4.4 close.

Stated plainly: you can ship this today for attended, inspectable automation
where a human is watching and can intervene. You cannot ship it yet for the
thing it is built for — a bot that runs for weeks and is allowed to act without
you.

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

*Not covered:* the survey sample is eight systems and one patent reading, not
the whole market. A rival that occupies the same axis would falsify the
position, and no one has looked for one since the survey ran.

### 4.2 Hyperscale — more than a million concurrent, correct

**❌ — not measured at any level.**

The measurement rig runs one process on one machine and reports `ns/tick` for a
schedule of up to 256 sources. There is no load test, no burst test, no
saturation curve, no queue-depth histogram, and no p99 under concurrency.

*To close it:* a stated concurrency target, then a saturation curve with the
declared bound where it starts dropping work. Until that exists, "runs for
weeks" is a design intent and not a measurement.

### 4.3 Idiomatic — ownership, errors, lifetimes sound

**✅ — compiler-enforced, with one named hole in the proof chain.**

- `unsafe_code = "forbid"` at workspace scope. Not `deny`. A `deny` is defeated
  by any single `#[allow]` anywhere, including one smuggled in by a macro this
  workspace does not own; `forbid` cannot be lowered from source and a
  conflicting allow is a hard `E0453`.
- The anti-pattern corpus's 75 entries: the 25 that a lint can catch are
  configured, the four names in the corpus that silently do nothing are
  corrected, and the other 50 are stated as needing Miri, perf counters or
  human review. The ceiling is published rather than claimed.
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

Covered and gated: a feature matrix across `lgwks_std`, `lgwks_bot`, `lgwks_ast`
and `lgwks_deps` (default, each feature alone, all features, none); 33
`lgwks_ast` grammars in CI; a `wasm32-wasip1` boundary lane; three host
operating systems; RON flow parsing; and the `--no-default-features` build of
every crate.

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
| A spawned process left orphans | ✅ tested | `process_ownership`, `#107` (T19/T20 landed; T21/T22 open) |
| A locator resolved to the wrong frame or wrong kind | ✅ tested | `locator_eligibility`, `#108` |
| Two equal payloads, distinct event ids | ✅ tested | `durable_dispatch` |
| A contradictory outcome overwriting a settled one | ✅ tested | `durable_dispatch`, refused |
| **The element is gone, not moved** | ⚠️ studied, not coded | [`frontier.md`](frontier.md): false-heal 40.5–57.1% on removed elements; decoy and drift score ranges **overlap**, so no threshold separates them |
| An OS dialog stole focus mid-flow | ❌ | no coverage |
| A file was half-written when read | ❌ | no coverage |
| Clock skew between two hosts | ❌ | no coverage |
| A credential expired mid-run | ❌ | no coverage |
| The locale changed a date or number format | ❌ | no coverage |
| Two operators edited one record | ❌ | no coverage |
| The app updated and the selector no longer resolves | ❌ | no coverage in the wild |
| Accessibility tree mutated during a read | ❌ | no coverage |
| A non-idempotent effect was sent twice by a retrying proxy | ⚠️ designed | the journal ladder refuses a second `OutcomeObserved` with different evidence; untested against a real retrying proxy |
| The user cancelled halfway through | ⚠️ | `CancellationToken` and `JoinSet` discipline are required; no end-to-end cancellation-under-load journey |

The rows marked ❌ are not oversights to be fixed in an afternoon each. They are
the reason the commercial RPA vendors have large QA organisations. Closing the
Generalized axis is the bulk of the remaining work on this crate.

### 4.5 Decoupled — components independently replaceable

**✅ — the seams are real and are load-bearing.**

- `lgwks_deps` is a dependency storefront with tiered admission
  (`boundary` / `vendor` only) and a checked `APPROVED.toml`. No crate authors a
  `tokio` edge; the async surface is a facade reached through the storefront.
  A build cannot acquire an unaudited dependency without the gate refusing.
- `EffectJournal` is a trait, and `DurableAck::new` is public specifically
  because an adapter outside the crate is where an acknowledgement is produced.
  The seam is empty by choice, not by accident.
- `state` and `authority` are separate: `GrantSet` does not own the schedule, the
  journal does not own the effects, the verbs do not own the runtime.

*Not covered:* no downstream compatibility evidence. A workspace caller search
proves nothing about external consumers, and the published tags trail `main`
(see the version-boundary note in the crate README).

### 4.6 Ephemeral — survives process loss

**⚠️ — the design is right and the proof of it has not been run.**

This is the axis the effect kernel exists for, and it is the axis #109 is
blocking on.

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
because it was never acknowledged. `tests/durable_crash_observation.rs` runs
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

`Until that run exists` no longer describes these rows. It still describes
#99, #107 T21/T22 and #108, and with rows of its own axis unobserved, this
axis stays ⚠️ and the §1 verdict stands.

The adapter's costs are measured, not estimated. One `sync_all` is the
platform's durable-append floor and costs about 3.0 ms on the development
machine's file system; a per-rung append pays it once per rung, and
`FileJournal::compare_and_append_all` — the group commit every durable log
converges on — pays it once per batch, all-or-nothing, with every
acknowledgment still minted after the shared flush. The per-key ladder check
is indexed, so an append's cost does not grow with the journal's length
(measured flat from an empty journal to 32,000 prior attempts, against the
linear walk it replaces, which measured 98× slower at 8,000). Replay holds
every committed entry in memory and reopens in time linear in the file;
rotation and compaction are not provided and the journal grows without bound
until a controller rotates it.

### 4.7 Portable — same semantics on all declared targets

**✅ — measured on three operating systems in CI.**

Thirteen hosted CI jobs green across `ubuntu-latest`, `macos-14` and
`windows-latest`, including the feature matrices and the no-default builds. A
`wasm32-wasip1` boundary lane checks that the default feature set compiles for
WASI. `os.uname` and other host-only calls are behind `platform` dispatch.

*Not covered:* ARM Linux, musl, 32-bit, and any target outside the three hosted
runners. The performance numbers in §4.9 are from one machine and are explicitly
not a cross-platform claim.

### 4.8 Multi-tenant — isolated under concurrent use

**❌ — not covered at all.**

`GrantSet` is a per-bot capability snapshot and `Auth` is a per-call proof, so
the *mechanism* for isolation exists. There are no cross-tenant isolation tests,
no negative tests that one tenant's grant cannot reach another's action, and no
concurrent-use test where two tenants' bots share a process.

*To close it:* negative cross-tenant tests at the `Auth` boundary and a
concurrent two-tenant journey. The type-level design makes this tractable; the
tests do not exist.

### 4.9 Performance — fastest correct implementation

**⚠️ — measured honestly, against the wrong comparator for the "fast" claim, and
not at all on the async path.**

Full method, raw numbers, fairness gate and exclusions are in
[`../bench/README.md`](../bench/README.md). The summary, with the loss stated at
the same volume as the wins:

| scenario | `lgwks_bot` ns/tick | hand-rolled loop ns/tick | ratio | 95% CI |
|---|---:|---:|---:|---|
| `poll-only-64x100` | 2 540.0 | 30.9 | 82.57x slower | [81.22, 83.38] |
| `steady-64x100` | 2 582.1 | 30.8 | 84.03x slower | [83.78, 84.42] |
| `churn-64x1` | 10 931.6 | 42.6 | 256.42x slower | [254.26, 257.39] |
| `fanout-1x64` | 2 329.5 | 32.2 | 72.38x slower | [71.99, 72.51] |
| `wide-256x10` | 12 726.5 | 132.4 | 96.01x slower | [95.45, 96.33] |

A hand-rolled loop doing provably identical work is **72x to 256x faster**. If
your workload is a hot path, use the loop. This document says so with a number
rather than burying it.

What the measurement also says: ~**390 000 ticks/s** on one M5 Pro core at 64
chains, ~98% of a tick in poll and change detection, 42.1 ns/tick (1.6%) for the
entire decision-and-effect layer, 36–131 ns per entry walked, admission of 64
chains in 0.066–0.43 ms, and cost linear in source count with no superlinear
term. `Auth::check` was quadratic in the capability count and is now
`n·log2 n`: 12.9 µs → 3.6 µs at 128 capabilities.

**Two exclusions that favour this crate and are named so they cannot be
unnoticed later:**

1. The rig withdraws `rt`. It measures the **synchronous** `Bot::tick` adapter.
   The async runtime — the thing `docs/async-parity.md` describes and the thing
   most consumers will actually run — has **no benchmark at all**.
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

**Speed, relative to a for-loop.** Slower, by 72x to 256x. Stated above.

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
- **No usability or authoring-time measurement.** [`async-sdk-shape.md`](async-sdk-shape.md)
  states the contract — a task author supplies the script, not the machinery —
  and notes that a tiny call site hiding hundreds of orchestration lines fails
  it as surely as a verbose one. That contract is **unimplemented** (issue
  #87). Excluding authoring time favours this crate heavily and is the single
  largest unmeasured risk to the replacement claim.
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
   (`tests/durable_crash_observation.rs`): the journal-ladder rows now have a
   real store, a real kill, a restart and the designed answer. #99, #107
   T21/T22 and #108 still need theirs, and the difference they measure — "the
   bot survives its own machine dying" — is still open. Nothing else in this
   list matters until all eight have run.
2. **Close the Generalized rows marked ❌ in §4.4.** Focus steal, torn reads,
   clock skew, credential expiry, locale, concurrent editors, selector drift.
   This is the long pole and the reason RPA is hard.
3. **Measure the async path.** §4.9's numbers are the sync adapter. The runtime
   consumers will actually run has no benchmark.
4. **Multi-tenant negative tests.** §4.8.
5. **Hyperscale.** §4.2. A stated concurrency target and a saturation curve.
6. **The authoring contract from #87.** §6's largest unmeasured risk: the
   call site that looks simple while hiding the orchestration.

Six items. The first is a measurement campaign rather than a design problem,
and the second is most of the product.

---

*This document is a diagnostic, not a brochure. It should be regenerated
whenever a gate moves, and the ❌ rows should be visibly harder to leave in
place than to fix.*
