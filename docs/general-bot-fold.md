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
and its licence is checked first.

The licence governing any given revision is the one in that crate's `Cargo.toml`
and its sibling `LICENSE` file: `Apache-2.0` for all four crates as of this
writing, and the root `README.md` indexes them. A move to a dual MPL-2.0 plus
commercial licence is **drafted and not landed** — there is no `LICENSING.md` in
this tree, and an earlier revision of this document cited one as though it
existed. Nothing here relicensed a published version, and a consumer should take
the manifest at their pinned revision as authoritative rather than this design
record.

## 2. The finding: the IR is already here

The described platform's intermediate representation is a `WhereWhatPair[]`:
a guard (`Where`: URL, cookies, selectors, with `$and` / `$or` / `$not`) paired
with an action pipeline (`What`: `goto`, `click`, `fill`, `scrape`, …).

That is this workspace's bot model, arrived at independently:

| Described platform | This workspace | Where |
|---|---|---|
| `WhereWhatPair` | a `ChainSpec`: a condition bound to an action | `crates/lgwks-bot/src/spec.rs` |
| `Where` boolean algebra | `Predicate::{And, Or, Not}` over a closed expression language | `crates/lgwks-bot/src/session.rs:583` |
| `Where` URL / cookie / selector guards | the conditions an `Observe` domain reports | `domain/*.rs` |
| `What[]` action pipeline | `Execute::execute_action`, one effect per call | `verb.rs` |
| `WorkflowInterpreter` state matcher | the ECS schedule: a condition is `Changed<T>` | `docs/bot-on-ecs.md` §1 |
| `WorkflowFile.meta` + graph | `FlowSpec` (vars, entry, nodes, edges, terminals) | `crates/lgwks-bot/src/session.rs:1021` |
| a step that dispatches to a subsystem | `NodeKind::Route { dispatch, fallback }` | `crates/lgwks-bot/src/session.rs:698` |

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

Two facts are decided before scoring rather than weighted: an element in another
document, and an element of an incompatible tag. A fact that is absent on either
side — no identifying attribute, no text, no structural path — contributes
nothing, rather than the `1.0` its metric returns for the empty-vs-empty case.
The weights are not renormalized around the gap, so identity and path together
being `3/4` is what makes them required for the `3/4` threshold to be reachable.
GitHub issue #39 filed this: two snapshot fields that are missing on *both*
sides were scoring as agreement, which let an unrelated element with a different
tag resolve against a target that had nothing to identify it by.

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
| `BotError` | no variant distinguishes "the effect did not happen" from "it may have happened" |
| this module | no two-way verdict distinguishes "nothing matched" from "several matched" |

One invariant, three arrivals. The first and third landed here; the second
landed in §6 step 5, and the two halves it turned out to have are one law rather
than two patches.

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
its own declared width, produces `Resolution::Degraded` (§6 item 5) and not
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

Each step lands green and independently. Steps 1–3, 5 and 6 have landed; step 4
is unblocked now.

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
   repair for the second. And the two halves of the §3.1 defect:
   `BotError::EffectIndeterminate` so a timed-out `Execute` is not retyped
   `Failed` and retried into a duplicate.

   **The second half landed wrong, and the correction is the interesting part.**
   It shipped first as a three-arm `TerminalOutcome` (`Success | Partial |
   Failure`) returned by `Terminal::outcome(unsettled)`, on the reasoning that a
   run which left an effect unsettled is neither a success nor a failure. That
   classifier then turned out to destroy exactly the information it was built to
   carry, in two ways. It mapped every `Refused` terminal to `Failure` whatever
   the unsettled count, so a refused run that had also left an earlier effect in
   doubt reported the refusal and dropped the doubt. And `Partial`'s own
   documentation claimed "part of its output is not known to exist, and part of
   it is", which does not follow from the count it was handed: one attempted
   effect whose response was lost may have produced none at all. `unsettled > 0`
   is uncertainty about *occurrence*, not evidence of partial completion.

   The repair is that the two facts are independent and are stored that way.
   `Outcome` carries a `Disposition` (`Completed | Referred | HandedOff |
   Refused` — the routing class, payload-free) beside an `EffectLedger`, which
   holds `confirmed` and `unsettled` counts. A refusal no longer overwrites
   uncertainty, and neither count is read as evidence of the other.

   That is the same invariant arriving a fourth time, from the opposite
   direction. The first three arrivals were types that could not say a thing;
   this one was a type that said something it had not been told — which is the
   harder failure to notice, because a loss of information leaves a smaller
   surface than a missing one.

   **They are one law, not two patches.** The error says an effect *may* have
   happened; the ledger is where that fact is retained rather than resolved.
   `EffectIndeterminate` changes no consumer's behaviour until something records
   the run it occurred in. Two findings came out of writing it:

   - **The variant, not the cause, has to carry the retry decision.** A consumer
     that parses a cause string to learn whether it may retry will eventually
     get it wrong, and the price is a duplicated merge, message, or process
     launch. So `EffectIndeterminate` is a sibling of `DomainError` rather than
     one variant with a flag: two errors with identical domain *and* identical
     cause text are still distinguishable, which is the property that makes the
     state representable rather than inferred. The regression test asserts
     exactly that pair.
   - **A refusal outranks an unsettled effect.** `Terminal::Refused` stays
     `Failure` however many effects were left unsettled. Folding a decided
     refusal into `Partial` would let it hide in the same class as a possible
     duplicate, and a consumer that must act on a refusal has to see it. This is
     the frontier's rule again — a provisional or decided outcome may not be
     spelled as the other — arriving in the outcome vocabulary.

   **Named honestly, because the step is smaller than it sounds:** there is no
   live producer yet. All three `Execute` domains in this workspace
   (`notify::Slack`, `gh::Merge`, `sys::Process`) are unbound stubs that always
   return `DomainError { cause: "binding required" }`, so nothing today times
   out mid-effect. What landed is the vocabulary and the law; the first domain
   to bind a real request is the one that has to report `EffectIndeterminate`
   rather than `DomainError` on a timeout, and until it does, this closes the
   *type* defect without changing a runtime path.
6. **The politeness frontier.** ✅ Landed in this change: `frontier.rs` —
   `ConstraintKey`, `Resolved`, `RulesState`, the three-armed `Admission`, a
   validated `PolitenessPolicy`, and the `Frontier` that admits against every
   constraint a request shares. Three of this document's own commitments were
   revised while implementing it, and each revision is a finding rather than a
   preference:

   - **The verdict is three-armed, not four, and §5.3's three are right.** The
     first draft promoted each cause to its own arm. That is wrong for a reason
     the cause-list makes obvious once written out: upstream failure mechanics
     are unbounded — a DNS `SERVFAIL`, a TLS timeout, and a redirect loop on
     `/robots.txt` are all *we could not look*, and none changes what the
     scheduler does. The epistemics belongs in `RulesState` and `Resolved`,
     which are explicit state machines, and the verdict reports only the
     operational result. The property that makes that safe, and the bug it
     avoids: **nothing provisional may be spelled `Reject`**. A terminal drop and
     a re-queue are different actions, and a consumer that must inspect a payload
     to learn which one it holds has no verdict.
   - **RFC 9309 supplies a third arrival of the invariant, and RFC 8020 a
     fourth.** *No rules were retrieved* has two opposite answers decided by
     why: §2.3.1.3's 4xx means unavailable and "the crawler MAY access any
     resources", while §2.3.1.4's 5xx means unreachable and the crawler "MUST
     assume complete disallow" — and only *until a fresh, valid file is
     obtained*, which is why `Unreachable` carries an instant and expires.
     Resolution splits the same way: `NXDOMAIN` is authoritative and rejects
     (RFC 8020), while `SERVFAIL` and timeouts are not and defer.
   - **Politeness is keyed on infrastructure, not hostnames, and the delay was
     the wrong control variable.** A per-host limit is blind to what a host is:
     two hundred Cloudflare-fronted domains are two hundred "independent" polite
     queues that are one reverse proxy seeing two hundred requests a second from
     one egress — a Layer 7 flood with a polite-looking config. So a request is
     admitted only when every constraint it shares admits. And the controlled
     quantity is an **integer concurrency window**, not a delay derived from
     latency: Scrapy's `AutoThrottle` makes `target_concurrency` a divisor rather
     than a bound, holds instead of backing off on a non-200, never reads
     `Retry-After`, and reads latency as load when a CDN edge cache answering
     fast makes it *speed up* under origin strain.

   Not yet here, and named rather than implied: the *drivers* that fetch rules
   and resolve names — this module models both states and records the
   observations, but nothing performs them — and §8.1's durable `Pending`/`Done`
   frontier, whose two-phase mark matters as soon as a run can be killed
   mid-flight.
7. **Declared identity**, wired as a flow property and recorded per run (§5.2).
8. **The `bot.evade` capability**, with the seeded, recorded draw and the grant
   log (§5.4, §5.5). Deliberately after 6 and 7, so the cheap remedies are the
   ones in place before the expensive one is reachable.
9. **The settlement policy.** `Time<Virtual>`-driven, declared per step, with
   non-settlement as a typed terminal outcome (§3.4).
10. **The recorder and its visible-delta compiler** (§3.3), on the ECS substrate.
11. **The agent domain**, closed-enum actions and `FlowBounds` budget (§5.7).
12. **The MCP adapter**, over the four verbs.
13. **Semantic-tier calibration.** `SemanticPolicy::DEFAULT` is `{ threshold:
    0.72, margin: 0.05 }`: declared, not fitted (§3.6), and constructor
    arguments because the right values depend on the model. What is missing is
    any way to *find* them or to *measure* whether they are right. Semantic
    Router — the closest production implementation — ships `fit(X, y)`,
    `evaluate(X, y)` and `get_thresholds()` for exactly this. The useful part of
    that precedent is its own published result: a five-route healthcare split
    scores **54.2%** validation accuracy *after* fitting, so the honest number
    for embedding-similarity routing can be far worse than a threshold's
    confidence suggests. Until this exists, "the semantic tier helps" is an
    assertion with no measurement behind it, and step 3's constants are
    unexamined.
14. **Precomputed embeddings at the resolver boundary.** `SemanticResolver`
    embeds the utterance and every candidate option on each `resolve`, so a flow
    with *n* options costs *n + 1* provider calls per turn, every turn. The same
    implementation's `BaseRouter.__call__` takes an optional `vector` that skips
    encoding entirely and pushes the lifetime decision to the caller. That shape
    is what fits here: it needs no cache, which the estate forbids without a
    bound and an eviction policy, and it keeps one implementation of the
    decision rather than adding a second entry point to the same work.

One note against a future simplification. Semantic Router's `BaseRouter`
*raises* when its index is not built — the same *could not look* situation
`Resolution::Degraded` carries as a verdict. Both express it; only one forces
the caller to handle it. It is recorded here because `Degraded` looks like a
value that could be folded into `Absent`, and it cannot: that is the exact
collapse step 5 exists to prevent.

## 7. What this document does not decide

- Whether the durable queue is a storefront feature or stays out of the
  workspace entirely. It needs the dependency doctrine's ruling, not this
  document's.
- The scheduler decision in `docs/bot-on-ecs.md` §9, which still requires
  project-owner sign-off.
- Anything about the described platform's licence. Nothing was copied.
