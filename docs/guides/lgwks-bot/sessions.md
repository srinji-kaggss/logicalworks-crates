# Guidance sessions

Pages of this guide: [the flow and its pieces](sessions.md) · [bounds and outcomes](sessions-bounds.md)

> **Needs `lgwks_bot` 0.5.0 or later.** The `session` module first shipped in the
> `lgwks_bot-v0.5.0` tag; the `lgwks_bot-v0.4.2` tag does not export it.

`Session` interprets a validated, declarative flow document and records the
conversation as it goes. It is a cursor over a graph, not a pipeline: the module
documentation is explicit that it "deliberately has no relation to
`crate::domain::flow::Pipeline`", because a pipeline composes capability-gated
effects while a session interprets a human-facing graph
(`crates/lgwks-bot/src/session.rs:1`).

## A two-option flow

`a_two_option_flow_routes_an_answer_and_refuses_after_the_end`
(`crates/lgwks-bot/tests/it/session_front_door.rs`) is the program for it, and it
runs under nextest with the rest of the suite. It builds a flow with one ask,
`size`, that routes `small` to `small_end` and `large` to `large_end`, then:

- starts a `Session` at `ask_size` and answers `something else entirely`, which
  is `Absent`: the ask stays current, the prompt is recorded again, and the
  transcript holds three records (the first prompt, the utterance, the re-ask);
- answers `small`, which routes to `small_end`: the flow ends `Terminal::Completed`
  with no current node;
- answers once more and gets `BotError::SessionTerminated`, a typed error and not
  a silent no-op, with the charged steps inside the budget of 8;
- asks `LanguageResolver` for `small`, which is `Resolution::Resolved` at
  `MatchTier::Exact` with score and lead `1.0`, and for `no idea`, which is `Absent`.

## The pieces

`FlowSpec` is the document. It owns variable declarations, an entry node, a node
map, explicit continuations, terminal overrides, and `FlowBounds`. `FlowSpec::new`
validates on construction, and both parsers validate on parse
(`crates/lgwks-bot/src/session/graph.rs:419`). `MAX_FLOW_BYTES` is 2,097,152, and input
past it is refused with `BotError::FlowTooLarge` before parsing runs.

A flow can be written in JSON or in RON. RON is the estate's notation for
human-facing documents, so it is the one to reach for by hand: it takes
comments, trailing commas and unquoted keys. The two are one document, and the
size limit, the node-kind check and the structural validation are the same on
both paths, so nothing is acceptable in one notation and refused in the other
(`crates/lgwks-bot/src/session/graph.rs:419`). One diagnostic does read differently,
and the cause is the notation rather than the flow: JSON checks a variant name
against nothing, so the guard names the node and the kind it carried, while RON
checks the name against the variant list it is handed and refuses before our own
visitor runs, so it names the variant and the enum but cannot name the node. Both
refuse (`crates/lgwks-bot/src/session/graph.rs:452`).

```ron
// review-pr.flow.ron
FlowSpec(
    vars: {},
    entry: "end",
    nodes: {
        "end": end,   // the only node
    },
)
```

A variant is written with its own name. `NodeKind` is a serde externally tagged
enum, so a unit variant is its own name (`end`) and a variant carrying fields
takes them in parentheses (`say(text: "hello")`). This is the document's contract
rather than a notation detail: the same variant is the bare string `"end"` in
JSON, and `{"say": {"text": "hello"}}` there. Every enum a flow document carries
is tagged this way — `NodeKind`, `FlowEdge`, `Terminal`, `Predicate`,
`ValueExpr`, `Value` and `VarType` — so no part of a document has a `kind` field.
`from_ron` says so in its own documentation, and
`crates/lgwks-bot/tests/it/flow_ron.rs` pins it in both directions, including that
the older `(kind: "end")` spelling no longer decodes.

`NodeKind` is a closed set: `Say`, `Ask`, `Branch`, `Handoff`, `Refer`, `Route`,
`End`. `NodeKind::Ask` carries the variable it writes, the candidate option
strings handed to the resolver, and a route map from option string to node id.

`VarScope` holds the typed variables. `VarType` is `String`, `Integer`,
`Boolean`, or `Choice(Vec<String>)`. `VarType::decode_answer` converts one answer
string into the declared type, and `set_from_answer` is that decoder plus the
store: it returns `BotError::InvalidVariableValue` when the answer cannot be
converted.

The same decoder runs at load, over every candidate of every ask
(`crates/lgwks-bot/src/session/validate.rs:121`). An ask whose options include one the
declared variable cannot hold is refused by `FlowSpec::validate` with
`BotError::AskOptionNotAssignable` — naming the node, the variable, the option,
the expected type and the cause — so a flow that loads is a flow every one of
whose options can be answered. A candidate is a *label* the resolver matches and
a *value* the variable stores, and they need not be the same string: `yes`
offered for a boolean variable stores `Value::Boolean(true)`, and a candidate
differing from a declared choice only in case stores the declared spelling.

`Session::new` installs the default language resolver and an in-memory journal.
`Session::with_components` replaces both, which is the seam: `Resolver` and
`Journal` are traits, and neither is required to be a model or a file.

`Journal` has two methods, and they carry different things.
`Journal::record` receives the rendered conversation — the transcript — and
cannot report a failure. `Journal::record_decision` receives a
`DecisionReceipt` and returns `Result<ReceiptAcceptance, JournalError>`; it is
required rather than defaulted, because a sink that has to do nothing to look
successful is a sink that silently drops every receipt. A sink that refuses one
aborts the answer (`BotError::ReceiptNotRecorded`) and the session does not move.
The receipt is what the run decided and on whose authority; `Session::decisions`
returns the receipts it recorded, and `Session::transcript` the conversation. See
`docs/guides/lgwks-bot/resolution.md` for the receipt's fields.

`Session::answer` is synchronous and returns `Result<(), BotError>`. The failure
cases are typed: `SessionTerminated` for an answer after the flow stopped,
`SessionNotAwaitingAnswer` for one at a node that is not an ask,
`ResolverReturnedInvalidOption` for a resolver that names an index outside the
candidate list, `MissingAskRoute` for a route table missing the selected option,
and `ReceiptNotRecorded` for a journal that would not take the decision receipt.
