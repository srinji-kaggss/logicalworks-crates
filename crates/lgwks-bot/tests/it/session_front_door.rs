//! The session module's documented doors, run as tests (#526).
//!
//! Each used to be a doctest that named the crate by path: the byte ceilings and the template
//! compiler. They are tests here, so each documented claim runs under nextest with the rest of the
//! suite.

use std::collections::BTreeMap;

use lgwks_bot::session::{
    FlowBounds, FlowSpec, MatchTier, NodeKind, Question, Resolution, Session, Terminal, VarType,
};
use lgwks_bot::{BotError, CompiledTemplate, Resolver, ResourceAxis, ResourceLimits, VarScope};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The two options of the ask, in the order they are offered.
fn size_options() -> Vec<String> {
    ["small", "large"].map(String::from).to_vec()
}

/// A two-option ask: `size` routes to `small_end` or `large_end`.
///
/// Shared with `sim_session_front_door`, which sweeps answer streams over this one flow.
pub(crate) fn size_flow() -> Result<FlowSpec, BotError> {
    let vars = BTreeMap::from([("size".to_owned(), VarType::Choice(size_options()))]);
    let routes = BTreeMap::from([
        ("small".to_owned(), "small_end".to_owned()),
        ("large".to_owned(), "large_end".to_owned()),
    ]);
    let ask = NodeKind::Ask {
        var: "size".to_owned(),
        options: size_options(),
        routes,
    };
    let nodes = BTreeMap::from([
        ("ask_size".to_owned(), ask),
        ("small_end".to_owned(), NodeKind::End),
        ("large_end".to_owned(), NodeKind::End),
    ]);
    FlowSpec::from_nodes(vars, "ask_size", nodes, FlowBounds::new(8))
}

/// The guide's program: an unrecognized answer leaves the ask where it was, a
/// recognized one routes and ends the flow, an answer after the end is a typed
/// error, and the resolver reports which tier matched.
#[test]
fn a_two_option_flow_routes_an_answer_and_refuses_after_the_end() -> TestResult {
    let mut session = Session::new("demo", size_flow()?)?;
    assert_eq!(
        session.current(),
        Some("ask_size"),
        "the entry ask is current"
    );

    // An unrecognized answer is `Absent`: the ask stays current and the prompt
    // is recorded again. The session does not advance and does not error.
    session.answer("something else entirely")?;
    assert_eq!(
        session.current(),
        Some("ask_size"),
        "an unrecognized answer leaves the ask current"
    );
    assert_eq!(
        session.transcript().len(),
        3,
        "the initial prompt, the utterance and the re-ask"
    );

    // A recognized answer routes and the flow terminates.
    session.answer("small")?;
    assert_eq!(session.current(), None, "the flow has ended");
    assert_eq!(
        session.terminal(),
        Some(&Terminal::Completed),
        "an End node completes"
    );
    assert!(session.steps() >= 2, "the ask and the end were charged");

    // Answering after termination is a typed error, not a silent no-op.
    assert!(
        matches!(session.answer("large"), Err(BotError::SessionTerminated)),
        "an answer after the end is refused"
    );
    // The runner's budget bounds charged steps, so a flow that loops cannot run
    // forever even if every node is reachable.
    assert!(
        session.steps() <= 8,
        "the charged steps stay inside the budget"
    );

    // A resolver reports *which* tier matched, which is what makes a wrong
    // resolution repairable. It is handed the question, not just the options.
    let resolver = lgwks_bot::language::LanguageResolver::new();
    let options = size_options();
    let question = Question::new("ask_size", &options);
    let verdict = resolver.resolve("small", &question);
    assert_eq!(
        verdict.resolution(),
        &Resolution::Resolved {
            index: 0,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        },
        "the exact option resolves at the exact tier"
    );
    let unknown = resolver.resolve("no idea", &question).into_resolution();
    assert!(
        matches!(unknown, Resolution::Absent { .. }),
        "an unrelated phrase is absent, got {unknown:?}"
    );
    Ok(())
}

#[test]
fn a_tightened_axis_reads_back_and_one_above_the_ceiling_is_refused() {
    let tight = ResourceLimits::shipped().tighten(ResourceAxis::Record, 4_096);
    assert_eq!(
        tight.get(ResourceAxis::Record),
        4_096,
        "the tightened axis reads back"
    );
    assert_eq!(
        tight.get(ResourceAxis::Session),
        ResourceLimits::shipped().get(ResourceAxis::Session),
        "the other axes keep the shipped ceiling"
    );
    assert!(
        tight.within_ceiling().is_ok(),
        "a request below the ceiling is inside it"
    );

    let greedy = ResourceLimits::shipped().tighten(ResourceAxis::Session, usize::MAX);
    assert!(
        matches!(
            greedy.within_ceiling(),
            Err(BotError::ResourceLimitAboveCeiling {
                axis: ResourceAxis::Session,
                ..
            })
        ),
        "a request above the ceiling is refused, naming its axis"
    );
}

#[test]
fn a_compiled_template_splits_into_parts_and_cannot_be_sized_before_its_name_has_a_value()
-> TestResult {
    let scope = VarScope::new(BTreeMap::new())?;
    let compiled = CompiledTemplate::compile("hello ${name}")?;
    assert_eq!(
        compiled.parts().len(),
        2,
        "a literal and one variable marker"
    );
    // The name has no value, so the size cannot be known yet.
    assert!(
        compiled.expanded_bytes(&scope).is_err(),
        "an unvalued name leaves the expansion unsized"
    );
    Ok(())
}
