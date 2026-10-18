# Folding the recorder: evasion, injection and model output

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

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
