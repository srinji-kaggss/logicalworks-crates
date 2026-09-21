# Folding the general recorder-and-replayer architecture into `lgwks_bot`

Status: **design record.** The first slice is implemented — `crates/lgwks-bot/src/interface.rs`
— and the rest is sequenced in §6. Nothing here changes the four verbs, the
authority model, or the ECS schedule.

## 1. What this document is for

A full browser-automation platform was described to this workspace: a
no-code recorder that turns arbitrary web interactions into a declarative,
versionable workflow graph, replayed by a headless browser, with extraction,
scheduling, run-diffing, and an optional LLM agent. It is a mature design and it
covers ground this workspace has not.

This document folds that design into `lgwks_bot`. "Fold" means: for each
subsystem, either name the artefact in this workspace that already *is* it, or
name the exact seam it becomes, or refuse it and say why. It is not a
comparison and not a plan to build a second platform. The failure mode it exists
to prevent is a parallel architecture — a second IR, a second interpreter, a
second authority model — arriving one subsystem at a time.

The fold is at the design level. No code is copied from the described platform
or from any of the projects whose published architecture it shares. If code is
ever taken, it goes through the rung-7 vendor path in `docs/dependency-doctrine.md`
and its licence is checked first — which is not a formality: this crate is
mid-relicence to MPL-2.0 (see `LICENSING.md`).

## 2. The finding: the IR is already here

The described platform's intermediate representation is a `WhereWhatPair[]`:
a guard (`Where`: URL, cookies, selectors, with `$and` / `$or` / `$not`) paired
with an action pipeline (`What`: `goto`, `click`, `fill`, `scrape`, …).

That is this workspace's bot model, arrived at independently:

| Described platform | This workspace | Where |
|---|---|---|
| `WhereWhatPair` | a `ChainSpec`: a condition bound to an action | `crates/lgwks-bot/src/spec.rs` |
| `Where` boolean algebra | `Predicate::{And, Or, Not}` over a closed expression language | `session.rs:107` |
| `Where` URL / cookie / selector guards | the conditions an `Observe` domain reports | `domain/*.rs` |
| `What[]` action pipeline | `Execute::execute_action`, one effect per call | `verb.rs` |
| `WorkflowInterpreter` state matcher | the ECS schedule: a condition is `Changed<T>` | `docs/bot-on-ecs.md` §1 |
| `WorkflowFile.meta` + graph | `FlowSpec` (vars, entry, nodes, edges, terminals) | `session.rs:387` |
| a step that dispatches to a subsystem | `NodeKind::Route { dispatch, fallback }` | `session.rs:182` |

So the first and largest correction to make is that **the fold does not need a
new IR**. A recorder's output is a `FlowSpec`; a scheduler's unit of work is a
`ChainSpec`; the interpreter is `Session` deciding the next node and the ECS
schedule executing it. Adding a second document format here would be the exact
duplication `AGENTS.md` calls a defect, and it would split the audit story: two
IRs means two answers to "what did this run actually do".

## 3. What is genuinely new

Four things have no artefact here yet. Everything else in §4 is either already
present or a domain implementation.

### 3.1 The interface model — **landed in this change**

`docs/guidance-runner-spec.md:193` deferred *"the interface model (element
entities, recognition-vector components, acquisition systems) on the ECS
substrate"*, blocked on Workstream A. Workstream A has landed
(`lgwks_std::similarity`: `EditDistance`, `Jaccard`, `PathSimilarity`,
`Geometry`, `Weighted`), so the first two thirds are now implementable and are
implemented:

- `ElementFacts` — element entities: the facts a snapshot reports about one
  element, with identity (`tag`, `identity`, `frames`) separated from mutable
  content (`text`, `bounds`), which is the split `docs/bot-on-ecs.md` §4 measured
  as load-bearing.
- `IdentityComponent` / `PathComponent` / `TextComponent` / `GeometryComponent` —
  recognition-vector components: adapters projecting one field of `ElementFacts`
  onto a `lgwks_std::similarity` metric, so heterogeneous metrics compose in one
  `Weighted`.
- `RecognitionVector::fingerprint()` — the shipped opinion: weights `1/2`
  identity, `1/4` path, `1/8` text, `1/8` geometry. Powers of two, so they sum to
  exactly `1.0` in binary floating point rather than by rounding luck.
- `Recognition` — the verdict.

`RecognitionVector::recognize` returns `Resolved` / `Ambiguous` / `Absent`, and
**this is the load-bearing decision in the whole fold.** The described platform
resolves an element to a single best candidate by similarity distance. A
best-candidate resolver has no way to report *"two elements matched equally
well"*: it returns the first, and the run either clicks the wrong control or
clicks nothing and reports a timeout. `Ambiguous` makes that state
representable, and the margin requirement makes it reachable — the best
candidate must lead the runner-up by a declared amount or the caller must
re-ask rather than commit.

This is the same invariant the workspace has now reached three times:

| Arrival | Statement |
|---|---|
| `docs/bot-on-ecs.md` §8.1 | a single-bit seen-set cannot distinguish "done" from "never started" |
| `BotError` (open defect) | no variant distinguishes "the effect did not happen" from "it may have happened" |
| this module | no two-way verdict distinguishes "nothing matched" from "several matched" |

One invariant, three arrivals. The third is now closed; the second is not, and
§6 sequences it.

### 3.2 The locator ladder and selector synthesis

The described platform generates, for each selected element, an ordered ladder —
stable `id`, then `data-testid`, then ARIA role and accessible name, then visible
text, then a class path — and falls back down it.

This is the next slice and it is deliberately **not** in this change. A ladder
must be walked against candidates, which means each candidate must offer the
anchors the ladder names; `ElementFacts` does not carry them yet, and shipping a
ladder that cannot be joined to its candidates would be a half-built path of
exactly the kind the one-path rule forbids. The shape is settled, though, and it
is small:

- `Anchor` — a closed enum (`Id`, `TestId`, `Role`, `Text`, `ClassPath`) with an
  explicit `rank()`. Explicit rather than derived `Ord`, because a derived order
  puts the strongest variant first *or* last depending on declaration order, and
  that is a coin-flip a reader should not have to resolve.
- `Ladder` — anchors sorted strongest-first, deduplicated, constructible only
  non-empty (an empty ladder is an error, matching `Weighted`'s treatment of an
  empty component set).
- `RecognitionVector::recognize_with_ladder` — walk strongest-first, and return
  the *strongest* non-resolved verdict observed. An `Ambiguous` at a strong
  anchor must not be silently retried at a weak one: a weak anchor that succeeds
  where a strong one was ambiguous is a likely mis-match, not a recovery.

### 3.3 The recorder, and what a compiler over it may not do

Recording is `Observe` plus an acquisition system on the ECS substrate: the
snapshot the browser already produces is the observation, and
`ElementFacts` is its Rust-side type. No new verb and no new transport.

The described platform's generator is an *optimizing compiler* over the recorded
actions: it merges an action into an existing guard block when the page state
already matches (overshadowing elimination) and injects synchronization barriers
between state-mutating actions. Both are good, and both are silent rewrites —
the executed flow differs from the recorded flow.

That is the thing to refuse. `docs/guidance-runner-spec.md` already states that
the flow document is an external contract and must be additive-only, and the
security case in `docs/security-posture.md` rests on the run being *declared*:
the point of the whole design is that an auditor can read what a bot will do.
An optimizer that rewrites the authored document breaks that, and does it
invisibly — the document a reviewer reads is not the document that ran.

So: **the generator optimizes, and records what it did.** Every merge and every
inserted barrier is emitted as a declared node or an annotation on the flow, not
applied behind the author's back. The compiled artefact and the authored
artefact differ by a *visible* delta, and that delta is the review surface. If a
compilation cannot be represented as a declared delta, it does not happen.

### 3.4 The settlement policy, and why it is declared rather than heuristic

The described platform chooses its wait strategy by inspecting downstream
operations: a DOM-only scrape navigates with `domcontentloaded` to skip
subresources, while a screenshot upgrades to `networkidle` and waits for images
to settle. To absorb single-page-application updates it polls matching element
counts until they hold steady for a window (default 400 ms).

The observation is right and the mechanism is wrong for this workspace. A
settlement window read off the wall clock is a non-deterministic input:
`docs/bot-on-ecs.md` §5 puts determinism on this workspace rather than on the
substrate, and §5 already supplies the remedy — `Time<Virtual>` as the clock
root, so a test runs ten thousand ticks without touching wall-clock.

And the failure path is the one that matters. "Poll until stable, else retry"
is how a flaky test is born: the retry hides the fact that the page never
settled. This is correction 4 in `docs/guidance-runner-spec.md` §"Corrections
from the brittleness sweep" — *flakiness is a typed outcome with a quarantine,
never a silent retry*. Non-settlement is therefore **a typed terminal outcome**,
not a retry and not a timeout error: the run reports that the page did not reach
the declared stable state, with the observed counts, and the flow's author
decides what that means.

The wait strategy is likewise a **declared property of the step** — "this step
needs layout", "this step needs content" — rather than a heuristic the engine
applies. A heuristic here means two runs of the same flow can take different
navigation paths, which is precisely what the determinism section forbids.

## 4. What maps onto something that already exists

| Described subsystem | This workspace | Note |
|---|---|---|
| REST gateway with JWT and scopes | `Cap` / `GrantSet`, proof-carrying | Strictly stronger: a scope checked once at a gateway is ambient for the rest of the request; `Auth` is re-proved on every call, so a grant revoked after build cannot fire (`lib.rs`) |
| Socket.io stateful multiplexer | `rt::sync` bounded channels | Bounded by construction; `unbounded_channel` is forbidden workspace-wide |
| Browser pool manager, lease/release | `rt::supervise`, `rt::cancel` | A supervised bounded pool is exactly what these already are |
| Playwright Chromium behind a WebSocket | the CDP allow-list in `docs/security-posture.md` | The allow-list is the contract; the driver is an implementation of it |
| Workflow queue (`SKIP LOCKED`, `graphile-worker`) | `rt::task` (`JoinSet` + `CancellationToken`) for in-process; a storefront edge for durable | The durable half is a database client, which is a `lgwks_deps` optional feature, not a new surface |
| Cron / schedule worker | `Bot::tick()` under a `Time<Virtual>` clock | Scheduling is already the tick; §7 of `bot-on-ecs.md` states the bors rule — derive queue membership, never store it |
| Artifact offload to object storage | `domain::fs` plus a storefront edge | The bot hands the payload to a sink; it does not own the sink |
| Webhooks, Sheets, Airtable dispatch | `domain::net`, `domain::notify` | Each is an HTTP+JSON domain impl, not a new architecture |
| The replayer UI (React, rrweb) | **not the bot's job** | The bot is a library. `docs/bot-on-ecs.md` §6 already concluded a console belongs on `gpui` and that the bot must not grow a UI |
| MCP stdio JSON-RPC server | an adapter over the four verbs | An adapter, not a fifth verb. The four-verb contract is `INV-BOT-FOUR-VERBS` and is not negotiable |
| Autonomous agent: page analyzer, indexed handles, screenshot, strict JSON | `domain::chat` + `domain::eval` | See §5 for the two constraints that survive the fold |

## 5. What is refused, and on what ground

Each of these is refused as a **default**. None is refused as an idea; each is
refused as something the bot does without a caller declaring it.

### 5.1 The browser flag cluster

`--disable-web-security`, `--disable-site-isolation-trials`, `--no-sandbox`.

- `--disable-web-security` disables the **same-origin policy**. A browser driving
  an authenticated session with the same-origin policy off can read cross-origin
  responses from the pages it visits. That is not a hardening trade-off; it is
  the removal of the boundary the rest of the web's security model is built on.
- `--disable-site-isolation-trials` removes **process isolation between
  origins**, which is the mitigation for the Spectre class of attacks.
- `--no-sandbox` removes the container's second line of defence: a renderer
  escape becomes a host escape.

`docs/security-posture.md` states the ceiling of this workspace's security case
and rests it on the bot being readable and declared. Adopting these three flags
would make that document false, and it would do so in the one place a reviewer
looks first. They are refused.

### 5.2 Stealth, and the reason is determinism before it is ethics

`puppeteer-extra-plugin-stealth`, `--disable-blink-features=AutomationControlled`,
dynamic user-agent and hardware randomization.

The first-order objection is engineering, not moral: a randomized user agent and
randomized hardware parameters are **non-deterministic inputs to a run that must
reproduce**. `docs/bot-on-ecs.md` §5 records determinism as this workspace's
responsibility, and a run whose fingerprint varies per execution cannot be
replayed, cannot be diffed against the previous run, and cannot be audited. It
also contradicts the interface model itself: `ElementFacts` fingerprints an
element, and a bot that randomizes what the page observes about *it* while
scoring what it observes about the page is measuring two different worlds.

The second-order objection is that detection evasion is the single fact a
security reviewer leads with, and the workspace has just spent a document
arguing the opposite posture. It is not built. If a legitimate authorized need
ever appears, it arrives as a named capability gated by a grant like every other
side effect — and under the one-path rule, a capability nobody exercises is one
that does not exist yet.

### 5.3 Code injection — and the locus distinction that makes this coherent

`page.exposeFunction`, `addInitScript`, and in-page analyzer injection are
**permitted**, and the reason is the distinction the security case already
draws: the allow-list in `docs/security-posture.md` constrains what runs in **the
consumer's browser**. The automation browser is a different locus, owned by the
estate, and injecting a recorder into it is the normal mechanism.

The rule that survives the fold is that the two loci are never conflated: an
`ExecutionLocus` is fixed when a session opens, and a session that drives the
estate's automation browser may not be the session that observes a consumer's
page. Injection into the second is what the whole design exists to avoid — so
the fold adds capability, not ambiguity, provided the locus stays explicit.

### 5.4 Free-text model output controlling the browser

The described platform already gets most of this right: it forces a strict JSON
grammar and enumerates the actions (`click`, `type`, `press`, `select`, `scroll`,
`wait`, `done`). The fold keeps that and closes the last gap: the JSON is parsed
into a **closed Rust enum**, not a `String` action name, so an unrecognized
action is a parse error rather than a no-op. A model that emits `"submit"` must
fail loudly, not silently do nothing.

The step budget is likewise explicit — `FlowBounds` already carries one, and the
described platform's "max 20 steps" is the same parameter. What is added is that
exhausting it is a typed terminal outcome, not a silent stop.

## 6. Sequence

Each step lands green and independently. Steps 1–2 are unblocked now.

1. **The interface model.** ✅ Landed in this change: `interface.rs` —
   `ElementFacts`, the four recognition-vector components, `RecognitionVector`,
   `Recognition`.
2. **The locator ladder.** `Anchor`, `Ladder`, and `recognize_with_ladder`, per
   §3.2. Requires `ElementFacts` to carry the anchors a candidate offers.
3. **The two missing typed outcomes.** `BotError` gains an indeterminate
   variant so a timed-out `Execute` is not retyped `Failed` and retried into a
   duplicate (the open defect named in §3.1); `TerminalOutcome` gains `Partial`
   so a run that produced some but not all of its output is distinguishable from
   one that produced none. This is also the described platform's
   `completed | partial | failed` triage, and it is the same invariant again.
4. **The settlement policy.** `Time<Virtual>`-driven, declared per step, with
   non-settlement as a typed terminal outcome (§3.4).
5. **The recorder and its visible-delta compiler** (§3.3), on the ECS substrate.
6. **The agent domain**, closed-enum actions and `FlowBounds` budget (§5.4).
7. **The MCP adapter**, over the four verbs.

## 7. What this document does not decide

- Whether the durable queue is a storefront feature or stays out of the
  workspace entirely. It needs the dependency doctrine's ruling, not this
  document's.
- The scheduler decision in `docs/bot-on-ecs.md` §9, which still requires
  project-owner sign-off.
- Anything about the described platform's licence. Nothing was copied.
