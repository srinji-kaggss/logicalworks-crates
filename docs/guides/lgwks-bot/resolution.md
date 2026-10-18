# Resolution and degraded verdicts

Pages of this guide: [verdicts](resolution.md) · [the two readings and the lexical tier](resolution-readings.md) · [aliases, the semantic tier and the receipt](resolution-aliases.md) · [policy and logging](resolution-policy.md)

> **Needs `lgwks_bot` 0.5.0 or later.** `language` and `semantic` first shipped in the
> `lgwks_bot-v0.5.0` tag; the `lgwks_bot-v0.4.2` tag does not export them.

A session asks a question with a known set of options and a person answers in
free text. Two implementations of the `Resolver` seam turn that text into a
choice: `language::LanguageResolver`, a tiered lexical match, and
`semantic::SemanticResolver`, which adds a comparison in a model's embedding
space. They answer different questions and they have different failure modes.

## The question goes in, not just its options

`Resolver::resolve(&self, utterance, &Question)` takes the question
(`crates/lgwks-bot/src/session/scope.rs:442`). A `Question` carries the id the answer
is being given for, the options currently on offer, and the kind of answer the
variable declares (`crates/lgwks-bot/src/session/scope.rs:294`). Both additions past
the option list exist because a resolver that sees only a list of strings cannot
tell two different questions apart, and cannot tell a number from a label.

The id is what scopes a learned alias: a phrase confirmed at one question is a
different fact from the same phrase at another question, and the resolver is told
which question it is answering.

The kind is an `AnswerDomain`, derived from the declared `VarType` rather than
chosen per call (`crates/lgwks-bot/src/session/value.rs:94`): an `Integer` variable
declares `Integer`, and `String`, `Boolean` and `Choice` declare `Label`.
`Question::new` defaults to `Label`, so a caller that says nothing gets the
lexical reading described below and no caller's meaning changes by surprise.

## The verdict is five-valued, and it carries its own provenance

`Resolver::resolve` returns a `Verdict` — a `Resolution` together with the
`Provenance` that produced it, in one value — and never an `Option<usize>`
(`crates/lgwks-bot/src/session/scope.rs:442`). The trait documentation gives the
reason the verdict is five-valued: a two-way verdict collapses "nothing matched"
and "several matched equally well" into one `None`, and the second is the
dangerous one, because the session re-asks the identical list and the identical
answer resolves the identical way forever.

The provenance travels in the return value rather than through a second call,
because *this score came from that model under that rule* is one fact. A caller
that has to ask a resolver afterwards what it just did is re-deriving a fact it
was already handed, and the derivation is only as good as the resolver's
willingness to keep answering the same way.

| Variant | Means | What the session does |
|---|---|---|
| `Resolved { index, tier, score, lead }` | one option led by the required margin | routes to that option's node |
| `Ambiguous { tied, tier, score }` | options matched, none led | re-asks the narrowed `tied` list |
| `Absent { best_score }` | every option was considered, none fit | re-asks the full list |
| `StaleAlias { question, option }` | a confirmed phrase whose bound option is gone | re-asks, and records the broken binding under its own role |
| `Degraded { reason }` | the resolver never got to consider the options | re-asks, and records the cause under its own role |

`Degraded` is `Absent`'s neighbour, not a spelling of it
(`crates/lgwks-bot/src/session/resolve.rs:282`). `Absent` means the person was not
understood, so asking again in different words can help. `Degraded` means a
dependency the resolver needs is unavailable, so the same utterance will degrade
identically every time. That is why `Degraded` carries no `best_score`: no
measurement was taken, and reporting `0.0` would be an observation that never
happened.

`StaleAlias` is separated from `Absent` for the same reason
(`crates/lgwks-bot/src/session/resolve.rs:256`). `Absent` says the words did not fit the
options and a rephrase may help. `StaleAlias` says the words *did* have a
confirmed meaning at this question and the option it was bound to is no longer
offered — so the repair is not a rephrase, it is a conversation, and it names
both halves of the broken binding because either alone is unactionable.

`Provenance` (`crates/lgwks-bot/src/session/resolve.rs:40`) holds two fields, and the
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

`DegradedReason` has two variants in the inspected source
(`crates/lgwks-bot/src/session/resolve.rs:245`). `EmbedderUnavailable` is reached when
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
