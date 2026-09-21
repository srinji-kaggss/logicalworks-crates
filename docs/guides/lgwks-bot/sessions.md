# Guidance sessions

> **Unreleased.** This page describes `crates/lgwks-bot/src/lib.rs` on the
> development commit this branch was cut from. The `lgwks_bot-v0.4.2` tag does
> not export the `session` module, so an installed `lgwks_bot = "0.4.2"` does not
> have anything on this page. Check `CHANGELOG.md` for the release that carries
> `Session` before depending on it.

`Session` interprets a validated, declarative flow document and records the
conversation as it goes. It is a cursor over a graph, not a pipeline: the module
documentation is explicit that it "deliberately has no relation to
`crate::domain::flow::Pipeline`", because a pipeline composes capability-gated
effects while a session interprets a human-facing graph
(`crates/lgwks-bot/src/session.rs:1`).

## A two-option flow

The program below builds a flow with one ask, answers it twice, and checks that
an unrecognized answer leaves the session where it was. It compiles and runs
against this development commit.

```rust
use std::collections::BTreeMap;

use lgwks_bot::Resolver;
use lgwks_bot::session::{
    FlowBounds, FlowSpec, MatchTier, NodeKind, Question, Resolution, Session, VarType,
};

/// A two-option ask: `size` routes to `small_end` or `large_end`.
fn size_flow() -> Result<FlowSpec, lgwks_bot::BotError> {
    let vars = BTreeMap::from([(
        String::from("size"),
        VarType::Choice(vec![String::from("small"), String::from("large")]),
    )]);
    let nodes = BTreeMap::from([
        (
            String::from("ask_size"),
            NodeKind::Ask {
                var: String::from("size"),
                options: vec![String::from("small"), String::from("large")],
                routes: BTreeMap::from([
                    (String::from("small"), String::from("small_end")),
                    (String::from("large"), String::from("large_end")),
                ]),
            },
        ),
        (String::from("small_end"), NodeKind::End),
        (String::from("large_end"), NodeKind::End),
    ]);
    FlowSpec::from_nodes(vars, "ask_size", nodes, FlowBounds::new(8))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new("demo", size_flow()?)?;
    assert_eq!(session.current(), Some("ask_size"));

    // An unrecognized answer is `Absent`: the ask stays current and the prompt
    // is recorded again. The session does not advance and does not error.
    session.answer("something else entirely")?;
    assert_eq!(session.current(), Some("ask_size"));
    // One record from the initial prompt, then the utterance and the re-ask.
    assert_eq!(session.transcript().len(), 3);

    // A recognized answer routes and the flow terminates.
    session.answer("small")?;
    assert_eq!(session.current(), None);
    assert_eq!(
        session.terminal(),
        Some(&lgwks_bot::session::Terminal::Completed)
    );
    assert!(session.steps() >= 2);

    // Answering after termination is a typed error, not a silent no-op.
    assert!(matches!(
        session.answer("large"),
        Err(lgwks_bot::BotError::SessionTerminated)
    ));

    // The runner's budget bounds charged steps, so a flow that loops cannot run
    // forever even if every node is reachable.
    assert!(session.steps() <= 8);

    // A resolver reports *which* tier matched, which is what makes a wrong
    // resolution repairable. The default resolver is the lexicon, not a model.
    // It is handed the question, not just the options: the ask node's id scopes
    // any learned alias, and the domain says how the answers are read.
    let resolver = lgwks_bot::language::LanguageResolver::new();
    let options = vec![String::from("small"), String::from("large")];
    let question = Question::new("ask_size", &options);
    assert_eq!(
        resolver.resolve("small", &question),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        }
    );
    match resolver.resolve("no idea", &question) {
        Resolution::Absent { .. } => {}
        other => return Err(format!("expected Absent, got {other:?}").into()),
    }

    Ok(())
}
```

## The pieces

`FlowSpec` is the document. It owns variable declarations, an entry node, a node
map, explicit continuations, terminal overrides, and `FlowBounds`. `FlowSpec::new`
validates on construction and `FlowSpec::from_json` validates on parse
(`crates/lgwks-bot/src/session.rs:1079`). `MAX_FLOW_BYTES` is 2,097,152, and input
past it is refused with `BotError::FlowTooLarge` before parsing runs.

`NodeKind` is a closed set: `Say`, `Ask`, `Branch`, `Handoff`, `Refer`, `Route`,
`End`. `NodeKind::Ask` carries the variable it writes, the candidate option
strings handed to the resolver, and a route map from option string to node id.

`VarScope` holds the typed variables. `VarType` is `String`, `Integer`,
`Boolean`, or `Choice(Vec<String>)`. `VarType::decode_answer` converts one answer
string into the declared type, and `set_from_answer` is that decoder plus the
store: it returns `BotError::InvalidVariableValue` when the answer cannot be
converted.

The same decoder runs at load, over every candidate of every ask
(`crates/lgwks-bot/src/session.rs:1585`). An ask whose options include one the
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

`Session::answer` is synchronous and returns `Result<(), BotError>`. The failure
cases are typed: `SessionTerminated` for an answer after the flow stopped,
`SessionNotAwaitingAnswer` for one at a node that is not an ask,
`ResolverReturnedInvalidOption` for a resolver that names an index outside the
candidate list, and `MissingAskRoute` for a route table missing the selected
option.

## The bounds

`FlowBounds::new(budget)` caps runtime steps including answer attempts, and
`FlowSpec::validate` refuses a node count that exceeds the declared budget
(`BotError::FlowBudgetExceeded`). The runner charges each step against the same
budget (`crates/lgwks-bot/src/session.rs:2998`) and returns
`BotError::SessionBudgetExceeded`, so a flow whose graph lets the cursor loop
still terminates.

`Session::steps()` reports the charged count, which is what you compare against
your own bound.

### Bytes, which the step budget does not bound

A step budget bounds how many times something happens and not how large it is,
and the two are independent: one `say` node is one step whether it says four
bytes or four gigabytes, and a document that names `${answer}` twenty thousand
times is a two-hundred-kilobyte file whose expansion is a hundred and sixty
megabytes. `Session` therefore enforces four byte ceilings as well
(`crates/lgwks-bot/src/session.rs:62`):

- `MAX_UTTERANCE_BYTES` — the bytes accepted in one answer, checked in
  `Session::answer` before the step is charged and before the utterance is
  copied anywhere, so a refused line consumes no budget and leaves no record.
- `MAX_VALUE_BYTES` — the bytes one stored variable value occupies when
  rendered. Checked at load, against every candidate of every ask, so a
  candidate the session could never store is refused before anyone is asked it
  (`BotError::AskOptionTooLarge`).
- `MAX_RECORD_BYTES` — the bytes of one rendered record payload.
- `MAX_SESSION_BYTES` — the total bytes one session retains across its whole
  transcript and visited path, which is what bounds a conversation the graph
  lets repeat.

Expansion is bounded at the *computed* size rather than the built one. A
template is compiled once into literal and placeholder parts, and the sum of the
parts is accumulated with `checked_add` before any output buffer exists; a
template whose computed expansion exceeds the record ceiling is refused with
`BotError::TemplateExpansionTooLarge`, and arithmetic that would overflow is the
same refusal at a larger size. A template whose *literal* bytes alone exceed the
ceiling is refused when the document loads, because no value can shrink it.

`FlowBounds::resources` lets a document declare ceilings of its own, and
`Session::with_limits` lets an operator declare theirs. Neither can widen the
other: an axis takes the smaller of the two, a request above the shipped ceiling
is refused (`BotError::ResourceLimitAboveCeiling`) rather than clamped, and the
ceiling in force is what `Session::limits()` reports. `Session::retained_bytes()`
reports the aggregate charged so far.

## What a finished run reports

`Terminal` records how the run ended: `Completed`, `Referred { target }`,
`HandedOff { target }`, or `Refused { reason }`.

Which one a node produces is computed in exactly one place,
`FlowSpec::effective_terminal`, and validation and execution both read it. An
`End` node completes unless the terminal map declares something else, and every
declared outcome is a genuine override there — that is how a flow refuses
(`Terminal::Refused`) rather than ending quietly. A `Handoff` or `Refer` node
already names its target, so the only declaration it accepts is the one that
repeats that outcome; one that contradicts it, whether a different target or a
refusal, is refused at load with `BotError::ConflictingTerminalDeclaration`
(`crates/lgwks-bot/src/session.rs:1515`). A document that says a handoff is not
authorized therefore never runs as a handoff, which is what a consumer
dispatching on the returned disposition depends on.

`Terminal::outcome(EffectLedger) -> Outcome` (`crates/lgwks-bot/src/session.rs:948`)
carries two independent facts through unchanged, and that is the whole of the
method:

- the `Disposition`, one of `Completed`, `Referred`, `HandedOff`, `Refused`
  (`crates/lgwks-bot/src/session.rs:769`);
- the `EffectLedger`, a `confirmed` count and an `unsettled` count
  (`crates/lgwks-bot/src/session.rs:796`).

It does not classify, and the reason is in the source: an earlier version
returned a single enum and had to choose, for a refused run that also left an
effect unsettled, whether to report the refusal or the uncertainty. Both are
true, and a report that drops either one is wrong for whoever needed it.

`EffectLedger::needs_reconciliation()` reads the count and never the
disposition, so it stays true for a refused run that left an effect in doubt.
A non-zero `unsettled` does not imply that anything succeeded either. One
attempted effect whose response was lost may have produced zero effects or one,
which is why the type says "nothing is confirmed, and one is in doubt" rather
than "part of the output exists".

`Outcome::disposition()` and `Outcome::effects()` are how you read the two back
out. Route on the disposition; reconcile on the ledger.

Validation is not execution. `FlowSpec::validate` proves the graph is
well-formed: reachable nodes, declared variables, resolvable transitions, a
budget that fits. It does not prove the flow asks the right questions. See
[resolution](resolution.md) for the boundary between a resolver selecting an
outcome and the outcome being correct.
