# Folding the recorder: recorder, settlement and language

Pages of this guide: [scope and finding](general-bot-fold.md) · [what is new](general-bot-fold-new.md) · [recorder, settlement, language](general-bot-fold-language.md) · [semantic tier and mapping](general-bot-fold-semantic.md) · [reaching a page](general-bot-fold-reach.md) · [evasion and injection](general-bot-fold-evasion.md) · [sequence 1 to 5](general-bot-fold-sequence.md) · [sequence 6 to 14](general-bot-fold-frontier.md)

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
