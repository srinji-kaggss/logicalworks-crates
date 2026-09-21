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

## The verdict is four-valued

`Resolver::resolve` returns a `Resolution`, never an `Option<usize>`
(`crates/lgwks-bot/src/session.rs:1209`). The trait documentation gives the
reason: a two-way verdict collapses "nothing matched" and "several matched
equally well" into one `None`, and the second is the dangerous one, because the
session re-asks the identical list and the identical answer resolves the
identical way forever.

| Variant | Means | What the session does |
|---|---|---|
| `Resolved { index, tier, score, lead }` | one option led by the required margin | routes to that option's node |
| `Ambiguous { tied, score }` | options matched, none led | re-asks the narrowed `tied` list |
| `Absent { best_score }` | every option was considered, none fit | re-asks the full list |
| `Degraded { reason }` | the resolver never got to consider the options | re-asks, and records the cause under its own role |

`Degraded` is `Absent`'s neighbour, not a spelling of it
(`crates/lgwks-bot/src/session.rs:1295`). `Absent` means the person was not
understood, so asking again in different words can help. `Degraded` means a
dependency the resolver needs is unavailable, so the same utterance will degrade
identically every time. That is why `Degraded` carries no `best_score`: no
measurement was taken, and reporting `0.0` would be an observation that never
happened.

`DegradedReason` has two variants in the inspected source
(`crates/lgwks-bot/src/session.rs:1248`). `EmbedderUnavailable` is reached when
the semantic tier's embedder returns an error or a vector of the wrong length.
`UnmeasurableEmbedding` is reached when an embedding comes back as a value no
angle can be computed from — an all-zero vector, or one carrying `NaN` or an
infinity. The first is a dependency that is down; the second is a dependency
that answered with something unusable. Different causes, different repairs, so
they are not one variant.

`lgwks_std::similarity::CosineError` splits that second case further, naming a
zero magnitude apart from a non-finite value, because the two send you to
different places: a zero vector is usually an empty or padded input, a
non-finite one a numeric fault inside the provider. The resolver does not repeat
the split in its own verdict, because it routes both the same way.

An incomplete comparison set is never decided. If any embedding in the set — the
utterance's or a competitor's — carries no direction, the whole decision
degrades rather than scoring the surviving options. Dropping the unusable
candidate leaves a field one member short, and a winner measured against fewer
competitors than the list holds reports a lead it never held; scoring the
degenerate vector as `0.0` would assert a similarity that was never observed.
GitHub issue #50 filed this: a zero competitor turned
`Resolved { score: 1.0, lead: 1.0 }` — the whole of the winner's score, reported
as a margin over a competitor that was never measured — into an ordinary
verdict, and a zero *utterance* vector turned every comparison invalid and
reported `Absent { best_score: 0.0 }`, which is what a healthy model returns
when it looks at a field and finds nothing. Unknown future `CosineError`
variants fail closed into the same degraded verdict rather than being skipped.

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
rather than inferred (`crates/lgwks-bot/src/session.rs:1230`), because the tier
names the repair: an `Exact` miss is a learned alias pointing at the wrong
option, a `Phonetic` miss is the English bias of the sound-alike key, a `Fuzzy`
miss is a threshold.

The two constants are `MATCH_THRESHOLD = 0.55` and `MATCH_MARGIN = 0.08`
(`crates/lgwks-bot/src/language.rs:70`, `crates/lgwks-bot/src/language.rs:73`). A score at or above the threshold that
does not lead the runner-up by the margin is `Ambiguous`, not `Resolved`.

The threshold and the margin are asked in that order, and the order is the
contract. Every option is measured first; the threshold then says which options
*may win*, and the margin asks whether the winner separated itself from the best
of everything else that was measured — including an option whose score fell just
short of the threshold. A below-threshold option does not stop being a measured
proximity because it may not be selected, so it still counts against the lead
and still appears in `Ambiguous::tied`. Filtering first and measuring second was
the defect GitHub issue #31 filed: it let a competitor one hundredth below the
threshold disappear, so a winner holding a `0.02` lead against a required `0.05`
reported the whole of its score as a margin and was accepted.

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
transcript role (`crates/lgwks-bot/src/session.rs:1739`), so the two do not
render identically. If you are building a dashboard, that role is the signal to
alert on; `Absent` is not.
