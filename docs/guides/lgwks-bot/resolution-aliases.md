# Resolution: aliases, the semantic tier and the receipt

Pages of this guide: [verdicts](resolution.md) · [the two readings and the lexical tier](resolution-readings.md) · [aliases, the semantic tier and the receipt](resolution-aliases.md) · [policy and logging](resolution-policy.md)

## Learned aliases are scoped to the question that learned them

An `Alias` is a confirmation: a phrase a person used, the question they used it
at, and the option text it was confirmed to mean
(`crates/lgwks-bot/src/language.rs:384`). The option is stored **verbatim** — the
option's own text, never its index and never a normalized form — so the binding
is resolved into the current candidate set at use time rather than frozen to a
position. A question that reorders or renames its options keeps every
confirmation that still names an option it offers.

| Operation | Effect |
|---|---|
| `learn(question, utterance, option)` (`crates/lgwks-bot/src/language.rs:611`) | binds the phrase, returning the binding it replaced, if any |
| `forget(question, utterance)` (`crates/lgwks-bot/src/language.rs:616`) | removes one binding, returning it |
| `with_aliases(aliases)` (`crates/lgwks-bot/src/language.rs:580`) | loads a shipped table, normalizing each phrase as it is stored |
| `aliases()` (`crates/lgwks-bot/src/language.rs:649`) | exports the table, so a session's confirmations survive a restart |

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

`Embedder` is a seam, not a dependency (`crates/lgwks-bot/src/semantic.rs:117`).
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
(`crates/lgwks-bot/src/session/receipt.rs:46`) through
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
