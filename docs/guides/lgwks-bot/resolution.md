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

## The verdict is four-valued, and it carries its own provenance

`Resolver::resolve` returns a `Verdict`, never an `Option<usize>`
(`crates/lgwks-bot/src/session.rs:1211`). A `Verdict` is a `Resolution` and the
`Provenance` behind it, in one value, because *this score came from that model
under that rule* is one fact and a second call can only re-derive it. The trait
documentation gives the reason the verdict is four-valued: a two-way verdict
collapses "nothing matched" and "several matched equally well" into one `None`,
and the second is the dangerous one, because the session re-asks the identical
list and the identical answer resolves the identical way forever.

| Variant | Means | What the session does |
|---|---|---|
| `Resolved { index, tier, score, lead }` | one option led by the required margin | routes to that option's node |
| `Ambiguous { tied, score }` | options matched, none led | re-asks the narrowed `tied` list |
| `Absent { best_score }` | every option was considered, none fit | re-asks the full list |
| `Degraded { reason }` | the resolver never got to consider the options | re-asks, and records the cause under its own role |

`Degraded` is `Absent`'s neighbour, not a spelling of it
(`crates/lgwks-bot/src/session.rs:1482`). `Absent` means the person was not
understood, so asking again in different words can help. `Degraded` means a
dependency the resolver needs is unavailable, so the same utterance will degrade
identically every time. That is why `Degraded` carries no `best_score`: no
measurement was taken, and reporting `0.0` would be an observation that never
happened.

`Provenance` (`crates/lgwks-bot/src/session.rs:1301`) holds two fields, and the
second is optional on purpose:

- **`PolicyVersion`** — always present, including on the lexicon path, because
  "the lexicon decided this" is itself a claim about a rule. It is
  content-addressed over the tier's own parameters (`PolicyVersion::new`
  digests the numbers it is handed), so retuning a threshold in a later release
  changes the revision. A hand-kept version string is a claim that can drift.
- **`Option<EmbedderIdentity>`** — present only when a model was consulted. A
  lexicon verdict is reproducible from the input alone, and naming a model on it
  would attribute that verdict to a model that never ran. A degraded verdict
  does name its model, deliberately: the model is what was expected to answer
  and did not, so a record of the failure that omits it cannot be reproduced.

`DegradedReason` has one variant in the inspected source,
`EmbedderUnavailable` (`crates/lgwks-bot/src/session.rs:1421`), reached when the
semantic tier's embedder returns an error or a vector of the wrong length.

## The lexical tier

`LanguageResolver` is a tiered match over a lexicon, not a model
(`crates/lgwks-bot/src/language.rs:1`). Each tier is a pure function of the
input, so a verdict is reproducible and a wrong one is explicable by naming the
tier that produced it.

| Tier | Rule | Score |
|---|---|---|
| `Exact` | the normalized utterance equals the normalized option, or is a learned alias | 1.0 |
| `Phonetic` | the two collapse to the same sound-alike key | 0.9 |
| `Fuzzy` | weighted blend of token overlap and bounded edit distance | computed |

They are ordered tiers rather than one weighted sum because an exact match has to
short-circuit, and a weighted sum cannot express that. `MatchTier` is reported
rather than inferred (`crates/lgwks-bot/src/session.rs:1399`), because the tier
names the repair: an `Exact` miss is a learned alias pointing at the wrong
option, a `Phonetic` miss is the English bias of the sound-alike key, a `Fuzzy`
miss is a threshold.

The two constants are `MATCH_THRESHOLD = 0.55` and `MATCH_MARGIN = 0.08`
(`crates/lgwks-bot/src/language.rs:70`, `crates/lgwks-bot/src/language.rs:73`). A score at or above the threshold that
does not lead the runner-up by the margin is `Ambiguous`, not `Resolved`.

The phonetic tier deliberately does not absorb a differing initial consonant, so
`cat`/`kat` and `Catherine`/`Kathryn` do not match. The module documentation
calls that the conservative choice rather than an oversight.

## The semantic tier

`SemanticResolver` is consulted only when the lexicon returns `Absent`
(`crates/lgwks-bot/src/semantic.rs:11`). A lexicon verdict, including
`Ambiguous`, is returned verbatim and never rescored. The module gives two
reasons, and the second is the load-bearing one: a deterministic verdict is
reproducible from the input alone, and a model is a tie-breaker of last resort
rather than an override. The consequence is that this tier can only turn an
`Absent` into something else, so adding it is not a behaviour change for any
utterance that already resolved.

`Embedder` is a seam, not a dependency (`crates/lgwks-bot/src/semantic.rs:105`).
The crate carries no model, no tensor library, and no tokenizer. A consumer that
has a model supplies it.

What the seam requires is `Embedder::identity`, because a verdict a model
produced is only declarable if the model is named. "The semantic tier resolved
this" is not an audit record; a model name, digest, and dimension count is. The
identity travels out on the verdict itself, so a caller holding a
`Box<dyn Resolver>` still has it, and from the verdict it reaches the decision
receipt below.

## The decision receipt

Every answer a `Session` takes is recorded as a `DecisionReceipt`
(`crates/lgwks-bot/src/session.rs:1529`) through
`Journal::record_decision`. The receipt is the run's audit record; the
transcript is a rendering of the conversation and cannot be one, because it
says what was said and the receipt says what was decided. Its fields:

| Field | Answers |
|---|---|
| `version` | which schema wrote it (`RECEIPT_VERSION`) |
| `session`, `flow` | which session, running which digest of which flow document |
| `node`, `options` | where the decision was taken, and the *ordered* candidate list it was taken against |
| `resolution` | the verdict verbatim, tier included — a score without its tier is not readable |
| `provenance` | the policy in force, and the model when one was consulted |
| `selected`, `route` | what was chosen, by the option's own text, and where the session went |

Two of those choices are load-bearing. `options` is a digest over the ordered
list rather than a count, because reordering the options is a different
question with a different right answer — and `selected` carries the option's
text rather than its index for exactly that reason: an index is a position in a
list that can move. Fields that were not measured are absent rather than zero,
so `selected` and `route` are `None` on a re-ask and a `Degraded` resolution
carries no score at all.

`record_decision` is fallible and the `ReceiptAcceptance` it returns
(`InMemory` or `Durable`) is stored beside the receipt, not inside it: two
sinks handed identical bytes must produce identical receipts, and where a
receipt was accepted is a question only the sink can answer. A sink that
refuses the write aborts the answer — `BotError::ReceiptNotRecorded` — and the
session does not move, does not write the variable, and does not record the
utterance. A cursor that advanced while nothing recorded why would be a run
whose transcript describes a decision the run never took.

## Thresholds are declared, not fitted

`SemanticPolicy::DEFAULT` is `{ threshold: 0.72, margin: 0.05 }`
(`crates/lgwks-bot/src/semantic.rs:333`). Those numbers are constructor
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
  a decision, not a correctness proof.
- **Decision rule.** `SemanticPolicy` and the two `MATCH_*` constants are the
  rule. Their values are policy, and policy is not measurement.
- **Validation.** `FlowSpec::validate` proves the graph is well-formed. It says
  nothing about whether the questions are the right ones.

An option list that fits no phrasing will produce `Absent` forever, and no
resolver change fixes an option list that does not contain the answer.

## What to log

`Degraded` exists so an operator can tell a person who was unclear from a
dependency that is down. The session records the degraded re-ask under its own
transcript role (`crates/lgwks-bot/src/session.rs:2279`), so the two do not
render identically. If you are building a dashboard, that role is the signal to
alert on; `Absent` is not.
