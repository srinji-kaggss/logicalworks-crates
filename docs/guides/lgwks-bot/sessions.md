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
    FlowBounds, FlowSpec, MatchTier, NodeKind, Resolution, Session, VarType,
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
    let resolver = lgwks_bot::language::LanguageResolver::new();
    let options = vec![String::from("small"), String::from("large")];
    assert_eq!(
        resolver.resolve("small", &options),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        }
    );
    match resolver.resolve("no idea", &options) {
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
(`crates/lgwks-bot/src/session.rs:577`). `MAX_FLOW_BYTES` is 2,097,152, and input
past it is refused with `BotError::FlowTooLarge` before parsing runs.

`NodeKind` is a closed set: `Say`, `Ask`, `Branch`, `Handoff`, `Refer`, `Route`,
`End`. `NodeKind::Ask` carries the variable it writes, the candidate option
strings handed to the resolver, and a route map from option string to node id.

`VarScope` holds the typed variables. `VarType` is `String`, `Integer`,
`Boolean`, or `Choice(Vec<String>)`. `set_from_answer` converts an accepted
option into the declared type and returns `BotError::InvalidVariableValue` when it
cannot.

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
budget (`crates/lgwks-bot/src/session.rs:1629`) and returns
`BotError::SessionBudgetExceeded`, so a flow whose graph lets the cursor loop
still terminates.

`Session::steps()` reports the charged count, which is what you compare against
your own bound.

## What a finished run reports

`Terminal` records how the run ended: `Completed`, `Referred { target }`,
`HandedOff { target }`, or `Refused { reason }`.

`Terminal::outcome(EffectLedger) -> Outcome` (`crates/lgwks-bot/src/session.rs:472`)
carries two independent facts through unchanged, and that is the whole of the
method:

- the `Disposition`, one of `Completed`, `Referred`, `HandedOff`, `Refused`
  (`crates/lgwks-bot/src/session.rs:293`);
- the `EffectLedger`, a `confirmed` count and an `unsettled` count
  (`crates/lgwks-bot/src/session.rs:320`).

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
