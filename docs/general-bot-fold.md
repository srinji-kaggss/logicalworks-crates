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

### 3.5 Language understanding — **landed in this change**

The described platform forces a model's output into a strict JSON grammar; it
says nothing about understanding what a *person* typed, which is the harder
half. A recorded flow asks questions, and the answer arrives as free text with
whatever spelling, abbreviation and house vocabulary the person brought.

`language.rs` is that layer, built the way classic autocomplete and
command-and-control recognisers are built — a **tiered match over a lexicon**,
not a model. Three ordered tiers: `Exact` (normalized equality, or a learned
alias), `Phonetic` (Soundex equality), `Fuzzy` (a blend of `Jaccard` token
overlap and bounded `EditDistance`). They are ordered rather than summed because
an exact match must short-circuit, and a weighted sum cannot express that.

The resolution reports **which tier won**, and that is not decoration: an
`Exact` miss is a learned alias pointing at the wrong option, a `Phonetic` miss
is Soundex's English bias, and a `Fuzzy` miss is a threshold. Three different
repairs, and a bare `usize` names none of them.

#### The linguistic-change question, answered in two parts

"Linguistic change" is two unrelated problems and one mechanism cannot cover
both:

- **Spelling and sound** — typos, transliteration, regional spelling. A property
  of the *letters*, handled statically by the phonetic tier, no learning needed.
- **Vocabulary** — slang, house terms, abbreviations. `"the usual"` cannot be
  derived from `"Repeat last order"` by any amount of string distance. A property
  of the *group's usage*, handled by `LanguageResolver::learn`, which records an
  alias when a resolution is confirmed.

The second is deliberately a **readable table, not a fitted model**. Every alias
is a row a reviewer can read and revoking one is deleting a row. A learned
scorer would be as adaptive and would not be auditable, and the security case
rests on the run being declarable.

Two limits are stated rather than hidden in the code. The diacritic fold is a
bounded table (Latin-1 Supplement and Latin Extended-A), because this crate may
not take a Unicode dependency; a character outside it passes through and
degrades to the fuzzy tier. And Soundex preserves the first letter verbatim, so
`Smith`/`Smyth` collapse but `cat`/`kat` and `Catherine`/`Kathryn` do not — the
conservative side of the trade, because in a resolver a false positive silently
selects the wrong option while a false negative only re-asks.

#### The resolver seam changed shape

`Resolver::resolve` returned `Option<usize>`, which is the same two-way collapse
§3.1 exists to remove: an answer that fits two options equally well is reported
as *unrecognized*, so the session re-asks the full list and the identical answer
resolves identically forever. It now returns `Resolution` — `Resolved` /
`Ambiguous` / `Absent` — and `Session::answer` narrows an ambiguous re-ask to the
options still in play, which is a strictly smaller question and therefore
terminates.

`KeywordResolver` is deleted rather than kept beside the new one: it is a strict
subset (the same token tier, and nothing else), so keeping both would be two
implementations of one job. That is a breaking public-API change for the next
release, which the standing constraint reserves for a separate step.

### 3.6 The semantic tier — **landed in this change**

§3.5 states a limit rather than hiding it: `"the usual"` cannot be derived from
`"Repeat last order"` by any amount of string distance. That is true, and it is
what a lexicon is for. This section is the layer that answers it, and it is the
one place a model belongs in this design.

`semantic.rs` adds `Embedder` — a seam, not a dependency. The crate still
carries no model, no tensor library and no tokenizer; the weights are not
source, and compiling them in would make every consumer pay for them. A consumer
with a model supplies it. `lgwks_std::similarity` gains `Cosine` alongside the
other metrics, so the comparison itself is not reinvented here.

Three properties make this safe to add to a running system rather than a second
opinion that can disagree with the first.

**It cannot change a verdict the lexicon reached.** The tier is consulted only
when the lexicon returns `Absent`, and every other lexicon verdict — including
`Ambiguous` — is returned verbatim. A model is a tie-breaker of last resort, not
an override: if it could outvote an exact match it eventually would, and an
exact match is the one verdict that cannot be wrong. So for any utterance that
already resolved, enabling this tier is not a behaviour change.

**Its contribution is named.** `MatchTier` gains `Semantic`, so a resolution
says whether a model produced it, and `EmbedderIdentity` carries the model's
name, a content digest of its weights, and its vector width. The digest is what
makes two runs comparable — same name, different weights is a different model —
and it is supplied by the embedder rather than computed here, because only the
code that loaded the weights can know them. A verdict a model produced is only
declarable if the model is named.

**It reports rather than guesses.** A failed embedder, or one that contradicts
its own declared width, produces `Resolution::Degraded` (§6 item 4) and not
`Absent`. Re-asking is the right response to both, but the record distinguishes
them, so an operator can tell an unclear person from a dependency that is down.

Three things are deliberate rather than missing. The policy constants
(`0.72`, `0.05`) are **declared, not fitted**, and are constructor arguments
because the right values depend on the model; the margin is small on purpose,
since two phrasings of one option legitimately score within hundredths of each
other. There is **no cache**: an option list is authored and short, and a cache
is a second piece of state needing a bound, an eviction policy and a test for
both — the seam it would sit behind is `Embedder`. And there is **no logging**:
the cause travels in the verdict and is written into the transcript, which is
the run's record, rather than into a second unmanaged copy of the same fact.

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

## 5. Reaching a page that does not want to be reached

> **Corrected after review.** The first version of this section refused "stealth"
> as a single thing and refused it outright. That was wrong twice over: the
> cluster is three unrelated mechanisms, and two of them are legitimate
> configuration rather than evasion. The review position — *some stealth is
> needed, or a way for it to crawl eventually* — is correct, and this section is
> the corrected design. What survives is narrower and better founded: the
> security-boundary flags stay refused, declared identity is the default, and
> evasion becomes a **granted, recorded** capability rather than a default flag.

The framing error is worth naming because it is easy to repeat. "Getting blocked"
has several distinct causes, and each has a different remedy:

| Cause | Remedy | Is it evasion? |
|---|---|---|
| Request rate | adaptive per-host delay and backoff | no |
| Undeclared identity | a declared crawler identity and contact | no |
| No session | persistent profile and cookie reuse | no |
| `robots.txt` | obey it; negotiate access for the rest | no |
| Recognised as automation | a granted, recorded spoof | **yes** |

Four of the five remedies are not evasion at all, and they are the ones that
actually work: most blocking is triggered by rate and by an undeclared identity,
not by a fingerprint. A bot that declares itself, paces itself, and keeps a
session gets through where a spoofing hammer gets banned. The fifth is real and
is designed for below rather than denied.

### 5.1 The browser flag cluster — still refused

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

These three are also **not stealth and not needed for crawling**. They disable
the same-origin policy, process isolation, and container containment
respectively; none of them changes how a page classifies the client. Keeping
them out of the refusal is what makes the rest of this section possible: once
they are separated out, the remaining question — *how does the bot get through?*
— has honest answers.

### 5.2 Declared identity — permitted, and the default

A fixed user agent, declared Accept-Language and viewport, and a crawler
identity with a contact URL. This is configuration, not evasion, and the
distinction is not a technicality: a declared identity is one a site owner can
recognise, contact, allowlist, or block deliberately. That is the behaviour
`robots.txt` and the crawler ecosystem are built around, and it is what
`User-Agent` is for.

It is also deterministic, which the refusal above cared about: the identity is
declared once per flow and recorded, so every run of that flow presents the same
client. There is no conflict with `docs/bot-on-ecs.md` §5.

**Adopted as the default.** The estate's automation browser presents a declared
identity, and the flow document records which one.

### 5.3 Politeness, and why it is the remedy that actually works

Adaptive per-host delay, bounded per-host concurrency, and backoff that respects
`Retry-After`. This is not a concession to the target; it is the dominant term in
whether a crawl completes at all, because rate is the trigger for most blocking.
It is also already the estate's committed design: `docs/bot-on-ecs.md` §8
specifies Mercator's bounded per-host queue under a global host priority queue
and Scrapy's `AutoThrottle`-style adaptation, and records the correction that
Mercator's adaptive criteria live in US patent 6,263,364 rather than the paper.

Politeness is a **scheduling policy**, so it belongs on the frontier and not in
the browser. A delay implemented inside a page driver is one that a second
driver bypasses.

Two properties it must have to be worth having:

- **The clock is `Time<Virtual>`**, so a test can run a ten-thousand-request
  schedule without touching wall-clock (`docs/bot-on-ecs.md` §5).
- **The admission decision is a three-way verdict** — `Permitted`,
  `Throttled { retry_after }`, `Refused { reason }` — not a boolean and not a
  sleep. `Refused` is where an explicit disallow lands, and it is terminal for
  that host. This is the same shape as `Recognition` in §3.1 and for the same
  reason: *wait* and *no* are different answers, and a `bool` cannot hold both.

### 5.4 Per-execution randomization — the objection, and how it is answered

`puppeteer-extra-plugin-stealth`, `--disable-blink-features=AutomationControlled`,
per-request user-agent and hardware randomization.

The objection is determinism, and it is narrower than the first version of this
document made it sound. It is not that a *declared* user agent is
non-deterministic — §5.2 settles that. It is that a fingerprint **re-randomized
per execution** is a non-deterministic input to a run that must reproduce:
`docs/bot-on-ecs.md` §5 puts determinism on this workspace, and a run whose
fingerprint varies cannot be replayed or diffed against its predecessor.

That is fixable rather than disqualifying, and the fix is the estate's existing
habit: **seed it from the run, record the seed, replay the same draw.** A
seeded draw is as reproducible as a constant, and it keeps the property a
constant loses — that two flows do not present an identical client.

It also settles a real contradiction the first version missed: `ElementFacts`
fingerprints an element, and a bot that randomizes what the page observes about
*it* while scoring what it observes about the page is measuring two different
worlds. A recorded seed makes both sides readable.

### 5.5 Evasion — a granted, recorded capability

Where a polite, declared, session-holding crawler is still refused access,
evasion is the remaining remedy and it is **built**, under three conditions that
make it compatible with the security case rather than fatal to it:

1. **It is a capability, not a default.** `bot.evade` is granted through
   `GrantSet` like every other side effect, so it is refused at build time for a
   bot that was not given it, and re-proved on every call.
2. **It is recorded in the run.** A grant *is* a declaration: the run states
   that it evaded, so an auditor reads it from the same document that lists
   every other authority the run held. That is what keeps
   `docs/security-posture.md` true — the claim was never "the bot cannot evade",
   it was "the run is declarable", and a recorded grant preserves exactly that.
3. **It is never ambient and never silent.** A run that used it says so; a run
   that did not, cannot have.

The residual risk is honest and belongs in the record: an evasion capability is
the one thing here that a reviewer will ask about, and it should be answered with
the grant log rather than with an argument. What is refused outright is the
version where evasion is unconditional, unrecorded, and indistinguishable in the
artefact from an ordinary run — which is what a launch flag gives you.

### 5.6 Code injection — and the locus distinction that makes this coherent

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

### 5.7 Free-text model output controlling the browser

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
2. **Language understanding.** ✅ Landed in this change: `language.rs`, the
   tiered lexicon; `Resolver` reshaped to the three-way `Resolution`, and
   `KeywordResolver` deleted rather than kept beside its superset (§3.5).
3. **The semantic tier.** ✅ Landed in this change: `semantic.rs`, the
   embedding-backed tier over an injected `Embedder`, with `Cosine` added to
   `lgwks_std::similarity`. Consulted only where the lexicon returns `Absent`,
   so it cannot change a verdict the deterministic tiers already reached, and
   the model's identity is recorded with every decision it informs (§3.6).
4. **The locator ladder.** `Anchor`, `Ladder`, and `recognize_with_ladder`, per
   §3.2. Requires `ElementFacts` to carry the anchors a candidate offers.
5. **The missing typed outcomes.** Three of the same invariant. ✅ Landed:
   `Resolution::Degraded` (§3.6), the verdict a resolver returns when a
   dependency it needs is unavailable — without it, *nothing matched* and *we
   could not look* render identically and asking the person again is the wrong
   repair for the second. Still open: `BotError` gains an indeterminate variant
   so a timed-out `Execute` is not retyped `Failed` and retried into a duplicate
   (the open defect named in §3.1), and `TerminalOutcome` gains `Partial` so a
   run that produced some but not all of its output is distinguishable from one
   that produced none. That last is also the described platform's
   `completed | partial | failed` triage, and it is the same invariant again.
6. **The politeness frontier.** Adaptive per-host delay, bounded per-host
   concurrency, and the three-way admission verdict, all under `Time<Virtual>`
   (§5.3). This is the piece that makes a crawl complete, and it is the highest
   value item on this list that has not landed. `docs/bot-on-ecs.md` §8 already
   specifies it; what is missing is the implementation.
7. **Declared identity**, wired as a flow property and recorded per run (§5.2).
8. **The `bot.evade` capability**, with the seeded, recorded draw and the grant
   log (§5.4, §5.5). Deliberately after 6 and 7, so the cheap remedies are the
   ones in place before the expensive one is reachable.
9. **The settlement policy.** `Time<Virtual>`-driven, declared per step, with
   non-settlement as a typed terminal outcome (§3.4).
10. **The recorder and its visible-delta compiler** (§3.3), on the ECS substrate.
11. **The agent domain**, closed-enum actions and `FlowBounds` budget (§5.7).
12. **The MCP adapter**, over the four verbs.

## 7. What this document does not decide

- Whether the durable queue is a storefront feature or stays out of the
  workspace entirely. It needs the dependency doctrine's ruling, not this
  document's.
- The scheduler decision in `docs/bot-on-ecs.md` §9, which still requires
  project-owner sign-off.
- Anything about the described platform's licence. Nothing was copied.
