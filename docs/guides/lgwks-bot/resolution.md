# Resolution and degraded verdicts

> **Unreleased.** This page describes `crates/lgwks-bot/src/lib.rs` on the
> development commit this branch was cut from. The `lgwks_bot-v0.4.2` tag does
> not export `language` or `semantic`, so an installed `lgwks_bot = "0.4.2"`
> does not have anything on this page.

A session asks a question with a known set of options and a person answers in
free text. Two implementations of the `Resolver` seam turn that text into a
choice: `language::LanguageResolver`, a tiered lexical match, and
`semantic::SemanticResolver`, which adds a comparison in a model's embedding
space. They answer different questions and they have different failure modes.

## The question goes in, not just its options

`Resolver::resolve(&self, utterance, &Question)` takes the question
(`crates/lgwks-bot/src/session.rs:1410`). A `Question` carries the id the answer
is being given for, the options currently on offer, and the kind of answer the
variable declares (`crates/lgwks-bot/src/session.rs:1358`). Both additions past
the option list exist because a resolver that sees only a list of strings cannot
tell two different questions apart, and cannot tell a number from a label.

The id is what scopes a learned alias: a phrase confirmed at one question is a
different fact from the same phrase at another question, and the resolver is told
which question it is answering.

The kind is an `AnswerDomain`, derived from the declared `VarType` rather than
chosen per call (`crates/lgwks-bot/src/session.rs:82`): an `Integer` variable
declares `Integer`, and `String`, `Boolean` and `Choice` declare `Label`.
`Question::new` defaults to `Label`, so a caller that says nothing gets the
lexical reading described below and no caller's meaning changes by surprise.

## The verdict is five-valued

`Resolver::resolve` returns a `Resolution`, never an `Option<usize>`
(`crates/lgwks-bot/src/session.rs:1469`). The trait documentation gives the
reason: a two-way verdict collapses "nothing matched" and "several matched
equally well" into one `None`, and the second is the dangerous one, because the
session re-asks the identical list and the identical answer resolves the
identical way forever.

| Variant | Means | What the session does |
|---|---|---|
| `Resolved { index, tier, score, lead }` | one option led by the required margin | routes to that option's node |
| `Ambiguous { tied, tier, score }` | options matched, none led | re-asks the narrowed `tied` list |
| `Absent { best_score }` | every option was considered, none fit | re-asks the full list |
| `StaleAlias { question, option }` | a confirmed phrase whose bound option is gone | re-asks, and records the broken binding under its own role |
| `Degraded { reason }` | the resolver never got to consider the options | re-asks, and records the cause under its own role |

`Degraded` is `Absent`'s neighbour, not a spelling of it
(`crates/lgwks-bot/src/session.rs:1533`). `Absent` means the person was not
understood, so asking again in different words can help. `Degraded` means a
dependency the resolver needs is unavailable, so the same utterance will degrade
identically every time. That is why `Degraded` carries no `best_score`: no
measurement was taken, and reporting `0.0` would be an observation that never
happened.

`StaleAlias` is separated from `Absent` for the same reason
(`crates/lgwks-bot/src/session.rs:1511`). `Absent` says the words did not fit the
options and a rephrase may help. `StaleAlias` says the words *did* have a
confirmed meaning at this question and the option it was bound to is no longer
offered — so the repair is not a rephrase, it is a conversation, and it names
both halves of the broken binding because either alone is unactionable.

`DegradedReason` has one variant in the inspected source,
`EmbedderUnavailable` (`crates/lgwks-bot/src/session.rs:1452`), reached when the
semantic tier's embedder returns an error or a vector of the wrong length.

## The two readings

Which reading an answer gets is decided by the question's `AnswerDomain` before
any matching happens, and the two readings share no tier.

### An `Integer` answer is decoded and compared as a value

The raw utterance goes through the shared decoder, the same one
`set_from_answer` uses to store the value (`crates/lgwks-bot/src/session.rs:102`).
It trims surrounding space and accepts a leading sign, so `" 5 "`, `"+5"` and
`"5"` all name the value `5`, while `"5.0"`, `"five"` and an out-of-range literal
such as `9223372036854775808` name nothing. The decoded value is then compared with each option's own
decoded value, and the ordinary decision rule runs over that set
(`crates/lgwks-bot/src/language.rs:440`).

No lexical, phonetic, fuzzy, alias or semantic tier runs for this domain, and
that is the point rather than an optimisation. `normalize` folds punctuation and
symbols, so `normalize("-5") == normalize("5")`; an answer read as a label would
match `"5"` at the Exact tier with score `1.0` and the sign would be gone. An
alias lookup is keyed on that same folded text and a phonetic key is computed
from it, so both are the same loss through another door — and a model asked which
of two numerals an utterance resembles would answer with the closest-looking one.
`-5` is not a near miss for `5`; it is a different value.

A number no option holds is `Absent`, so the question is re-asked and no route is
taken. Both signs can be offered (`"-5"` and `"5"` are two values), the ends of
the `i64` range round-trip exactly, and an answer that exceeds the range is
refused rather than wrapped or clamped. Nothing is ever parsed from a *selected
option* — the value stored is the value the person gave.

### A `Label` answer goes through the tiers

Everything below is the Label reading.

## The lexical tier

`LanguageResolver` is a tiered match over a lexicon, not a model
(`crates/lgwks-bot/src/language.rs:1`). Each tier is a pure function of the
input, so a verdict is reproducible and a wrong one is explicable by naming the
tier that produced it.

| Tier | Rule | Score |
|---|---|---|
| `Exact` | the normalized utterance equals the normalized option, or is a learned alias for it | 1.0 |
| `Phonetic` | the two collapse to the same sound-alike key | 0.9 |
| `Fuzzy` | weighted blend of token overlap and bounded edit distance | computed |

They are ordered tiers rather than one weighted sum because an exact match has to
short-circuit, and a weighted sum cannot express that. `MatchTier` is reported
rather than inferred (`crates/lgwks-bot/src/session.rs:1434`), because the tier
names the repair: an `Exact` miss is a learned alias pointing at the wrong
option, a `Phonetic` miss is the English bias of the sound-alike key, a `Fuzzy`
miss is a threshold.

**Precedence is over the whole candidate set, not per option**
(`crates/lgwks-bot/src/language.rs:30`). The best tier that any option reached is
the tier the question is answered in, and every option below it is discarded
before the margin is applied. An option does not win by scoring higher than an
option in a stronger tier, and it does not take an `Ambiguous` verdict from a
weaker tier as licence to be selected from a ranking of its own. Without that
rule a fuzzy competitor can outscore a phonetic match on the raw number — a
transposition of two words keeps the whole token set and can score above `0.9` —
and the tier order would be decoration.

The three constants are `MATCH_THRESHOLD = 0.55`
(`crates/lgwks-bot/src/language.rs:82`), `MATCH_MARGIN = 0.08`
(`crates/lgwks-bot/src/language.rs:85`) and `MAX_UTTERANCE_CHARS = 512`
(`crates/lgwks-bot/src/language.rs:73`). A score at or above the threshold that
does not lead the runner-up **in its own tier** by the margin is `Ambiguous`, not
`Resolved`. An utterance longer than the bound degrades rather than panicking.

The phonetic tier deliberately does not absorb a differing initial consonant, so
`cat`/`kat` and `Catherine`/`Kathryn` do not match. The module documentation
calls that the conservative choice rather than an oversight.

## Learned aliases are scoped to the question that learned them

An `Alias` is a confirmation: a phrase a person used, the question they used it
at, and the option text it was confirmed to mean
(`crates/lgwks-bot/src/language.rs:371`). The option is stored **verbatim** — the
option's own text, never its index and never a normalized form — so the binding
is resolved into the current candidate set at use time rather than frozen to a
position. A question that reorders or renames its options keeps every
confirmation that still names an option it offers.

| Operation | Effect |
|---|---|
| `learn(question, utterance, option)` (`language.rs:554`) | binds the phrase, returning the binding it replaced, if any |
| `forget(question, utterance)` (`language.rs:567`) | removes one binding, returning it |
| `with_aliases(aliases)` (`language.rs:531`) | loads a shipped table, normalizing each phrase as it is stored |
| `aliases()` (`language.rs:592`) | exports the table, so a session's confirmations survive a restart |

Two properties make this safe to spend. The phrase is normalized once, on the way
in, so a shipped table and a learned binding are compared the same way. And a
binding is only ever consulted at the question it was made at: a phrase confirmed
at one ask cannot answer a different ask, whatever the second question's options
happen to be.

When the option a binding names is no longer offered, the binding has been
*superseded* rather than invalidated, and the resolver reports `StaleAlias`
instead of a reading. It does **not** displace a reading the current option list
produces: a phrase that literally names an option still on screen resolves to it,
because a verdict that vetoed the visible option would trap the person — the
taught phrase and the option text normalize the same, so no other answer could
select it. `StaleAlias` appears where the withdrawn binding is the only thing
that would have matched, which is exactly where the old behaviour would have
selected whichever option inherited the position.

## The semantic tier

`SemanticResolver` is consulted only when the lexicon returns `Absent`
(`crates/lgwks-bot/src/semantic.rs:13`). A lexicon verdict, including
`Ambiguous`, is returned verbatim and never rescored. The module gives two
reasons, and the second is the load-bearing one: a deterministic verdict is
reproducible from the input alone, and a model is a tie-breaker of last resort
rather than an override. The consequence is that this tier can only turn an
`Absent` into something else, so adding it is not a behaviour change for any
utterance that already resolved.

It is not consulted at all for an `Integer` question, nor for a phrase whose
binding has been superseded. Both are verdicts the deterministic path already
reached, and a model that could rehabilitate either would be a second,
unreviewable way for the folded-text defect to select an option.

`Embedder` is a seam, not a dependency (`crates/lgwks-bot/src/semantic.rs:88`).
The crate carries no model, no tensor library, and no tokenizer. A consumer that
has a model supplies it.

What the seam requires is `Embedder::identity`, because a verdict a model
produced is only declarable if the model is named. "The semantic tier resolved
this" is not an audit record; a model name, digest, and dimension count is.

## Thresholds are declared, not fitted

`SemanticPolicy::DEFAULT` is `{ threshold: 0.72, margin: 0.05 }`
(`crates/lgwks-bot/src/semantic.rs:301`). Those numbers are constructor
arguments, not tuned constants, because the right values depend on the model.

`docs/general-bot-fold.md` §3 item 13 records what is missing, and it is worth
quoting rather than paraphrasing: there is no way to *find* them or to *measure*
whether they are right, and "until this exists, 'the semantic tier helps' is an
assertion with no measurement behind it, and step 3's constants are unexamined."

So: do not treat these defaults as calibrated for your domain. There is no
held-out result, no abstention study, and no per-domain evaluation in the
inspected source. Choosing a threshold for your data is your evaluation to run.
The provider seam and its limits are documented; the quality claim is not
available.

## Representation, decision rule, validation

Three things are easy to conflate, and the code keeps them apart.

- **Representation.** `Resolution::Resolved { tier, score, lead }` records that a
  resolver selected an outcome according to its own rules. It does not certify
  that the selected element is the one the person meant. A `Resolved` verdict is
  a decision, not a correctness proof. In particular, `Exact` means the two
  strings are equal under a *declared* normalization policy — it is not a claim
  that the transformation preserved everything the input meant, which is why a
  reading whose policy would erase something meaning-bearing does not use the
  policy at all.
- **Decision rule.** `SemanticPolicy` and the two `MATCH_*` constants are the
  rule. Their values are policy, and policy is not measurement.
- **Validation.** `FlowSpec::validate` proves the graph is well-formed, and it
  now has something to say about the options themselves: two options that the
  declared policy cannot tell apart — the same normalized label, or the same
  decoded value — are refused at construction, naming both options. A question
  that offers them could never report which one the person chose, and the
  failure would surface as a tie at runtime rather than as an error in the flow.
  A signed integer question offering `-5` and `5` is accepted, because under its
  policy those are two values; the same two strings offered as labels are not.

An option list that fits no phrasing will produce `Absent` forever, and no
resolver change fixes an option list that does not contain the answer.

## What to log

`Degraded` exists so an operator can tell a person who was unclear from a
dependency that is down. The session records the degraded re-ask under its own
transcript role (`crates/lgwks-bot/src/session.rs:1998`), and a superseded
binding under a different one (`crates/lgwks-bot/src/session.rs:2010`), so the
three do not render identically. If you are building a dashboard, the degraded
role is the signal to alert on; `Absent` is not. The superseded-binding role is
neither: it is a prompt for someone to ask the person what they meant, and it
will repeat exactly as often as the phrase is used until the flow's options or
the table are reconciled.
