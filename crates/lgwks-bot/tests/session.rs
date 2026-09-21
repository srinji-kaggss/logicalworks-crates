//! Public acceptance tests for the synchronous flow/session surface.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use lgwks_bot::{
    AnswerRejection, BotError, DegradedReason, Disposition, EffectLedger, Embedder,
    EmbedderIdentity, FlowBounds, FlowEdge, FlowSpec, Journal, LanguageResolver, MAX_RECORD_BYTES,
    MAX_SESSION_BYTES, MAX_UTTERANCE_BYTES, MAX_VALUE_BYTES, NodeKind, Predicate, Resolution,
    Resolver, ResourceAxis, ResourceLimits, SemanticResolver, Session, Terminal, TranscriptEntry,
    Value, ValueExpr, VarType,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Build a 20-node graph containing every shipped node kind.
fn twenty_node_flow() -> Result<FlowSpec, BotError> {
    let mut vars = BTreeMap::new();
    vars.insert(
        String::from("choice"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    );
    vars.insert(
        String::from("decision"),
        VarType::Choice(vec![String::from("go"), String::from("stop")]),
    );
    vars.insert(
        String::from("next"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    );

    let mut nodes = BTreeMap::new();
    nodes.insert(
        String::from("n00"),
        NodeKind::Say {
            text: String::from("Welcome"),
        },
    );
    nodes.insert(
        String::from("n01"),
        NodeKind::Ask {
            var: String::from("choice"),
            options: vec![String::from("yes"), String::from("no")],
            routes: BTreeMap::from([
                (String::from("yes"), String::from("n02")),
                (String::from("no"), String::from("n05")),
            ]),
        },
    );
    nodes.insert(
        String::from("n02"),
        NodeKind::Branch {
            var: String::from("choice"),
            when: Predicate::Eq(
                ValueExpr::Var(String::from("choice")),
                ValueExpr::Literal(Value::Choice(String::from("yes"))),
            ),
            then: String::from("n03"),
            otherwise: String::from("n04"),
        },
    );
    nodes.insert(
        String::from("n03"),
        NodeKind::Say {
            text: String::from("You chose ${choice}"),
        },
    );
    nodes.insert(
        String::from("n04"),
        NodeKind::Refer {
            target: String::from("docs"),
            text: String::from("Please read the ${choice} guide"),
        },
    );
    nodes.insert(
        String::from("n05"),
        NodeKind::Route {
            dispatch: String::from("n07"),
            fallback: String::from("n08"),
        },
    );
    nodes.insert(
        String::from("n06"),
        NodeKind::Handoff {
            target: String::from("support"),
        },
    );
    nodes.insert(
        String::from("n07"),
        NodeKind::Say {
            text: String::from("Continue"),
        },
    );
    nodes.insert(String::from("n08"), NodeKind::End);
    nodes.insert(
        String::from("n09"),
        NodeKind::Say {
            text: String::from("Choose a direction"),
        },
    );
    nodes.insert(
        String::from("n10"),
        NodeKind::Ask {
            var: String::from("decision"),
            options: vec![String::from("go"), String::from("stop")],
            routes: BTreeMap::from([
                (String::from("go"), String::from("n11")),
                (String::from("stop"), String::from("n12")),
            ]),
        },
    );
    nodes.insert(
        String::from("n11"),
        NodeKind::Branch {
            var: String::from("decision"),
            when: Predicate::Eq(
                ValueExpr::Var(String::from("decision")),
                ValueExpr::Literal(Value::Choice(String::from("go"))),
            ),
            then: String::from("n13"),
            otherwise: String::from("n14"),
        },
    );
    nodes.insert(
        String::from("n12"),
        NodeKind::Refer {
            target: String::from("help"),
            text: String::from("Stopping here"),
        },
    );
    nodes.insert(
        String::from("n13"),
        NodeKind::Route {
            dispatch: String::from("n15"),
            fallback: String::from("n16"),
        },
    );
    nodes.insert(
        String::from("n14"),
        NodeKind::Handoff {
            target: String::from("specialist"),
        },
    );
    nodes.insert(
        String::from("n15"),
        NodeKind::Say {
            text: String::from("One last answer"),
        },
    );
    nodes.insert(String::from("n16"), NodeKind::End);
    nodes.insert(
        String::from("n17"),
        NodeKind::Ask {
            var: String::from("next"),
            options: vec![String::from("yes"), String::from("no")],
            routes: BTreeMap::from([
                (String::from("yes"), String::from("n18")),
                (String::from("no"), String::from("n19")),
            ]),
        },
    );
    nodes.insert(
        String::from("n18"),
        NodeKind::Refer {
            target: String::from("next-step"),
            text: String::from("The next step is ready"),
        },
    );
    nodes.insert(String::from("n19"), NodeKind::End);

    let edges = vec![
        FlowEdge::next("n00", "n01"),
        FlowEdge::next("n03", "n06"),
        FlowEdge::next("n07", "n09"),
        FlowEdge::next("n09", "n10"),
        FlowEdge::next("n15", "n17"),
    ];
    let terminals = BTreeMap::from([
        (
            String::from("n04"),
            Terminal::Referred {
                target: String::from("docs"),
            },
        ),
        (
            String::from("n06"),
            Terminal::HandedOff {
                target: String::from("support"),
            },
        ),
        (String::from("n08"), Terminal::Completed),
        (
            String::from("n12"),
            Terminal::Referred {
                target: String::from("help"),
            },
        ),
        (
            String::from("n14"),
            Terminal::HandedOff {
                target: String::from("specialist"),
            },
        ),
        (
            String::from("n16"),
            Terminal::Refused {
                reason: String::from("fallback refused"),
            },
        ),
        (
            String::from("n18"),
            Terminal::Referred {
                target: String::from("next-step"),
            },
        ),
        (String::from("n19"), Terminal::Completed),
    ]);
    FlowSpec::new(vars, "n00", nodes, edges, terminals, FlowBounds::new(64))
}

/// Build a single terminal flow.
fn terminal_flow(kind: NodeKind, terminal: Option<Terminal>) -> Result<FlowSpec, BotError> {
    let terminals = terminal.map_or_else(BTreeMap::new, |outcome| {
        BTreeMap::from([(String::from("start"), outcome)])
    });
    FlowSpec::new(
        BTreeMap::new(),
        "start",
        BTreeMap::from([(String::from("start"), kind)]),
        Vec::new(),
        terminals,
        FlowBounds::new(8),
    )
}

/// Build a two-node ask/end flow for validation tests.
fn ask_end_flow(
    vars: BTreeMap<String, VarType>,
    routes: BTreeMap<String, String>,
) -> Result<FlowSpec, BotError> {
    FlowSpec::new(
        vars,
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("answer"),
                    options: vec![String::from("yes"), String::from("no")],
                    routes,
                },
            ),
            (String::from("end"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(8),
    )
}

#[test]
fn flow_fixture_has_twenty_nodes_and_every_node_kind() -> TestResult {
    let flow = twenty_node_flow()?;
    assert_eq!(flow.nodes().len(), 20);
    let kinds = flow
        .nodes()
        .values()
        .map(|node| match *node {
            NodeKind::Say { .. } => "say",
            NodeKind::Ask { .. } => "ask",
            NodeKind::Branch { .. } => "branch",
            NodeKind::Handoff { .. } => "handoff",
            NodeKind::Refer { .. } => "refer",
            NodeKind::Route { .. } => "route",
            NodeKind::End => "end",
            _ => "unknown",
        })
        .collect::<Vec<_>>();
    for expected in ["say", "ask", "branch", "handoff", "refer", "route", "end"] {
        assert!(kinds.contains(&expected), "fixture is missing {expected}");
    }
    let json = flow.to_json()?;
    let decoded = FlowSpec::from_json(&json)?;
    assert_eq!(decoded.nodes().len(), 20);
    Ok(())
}

#[test]
fn scripted_sessions_reach_each_terminal_kind() -> TestResult {
    let mut referred = Session::new(
        "referred",
        terminal_flow(
            NodeKind::Refer {
                target: String::from("docs"),
                text: String::from("read this"),
            },
            None,
        )?,
    )?;
    assert_eq!(
        referred.terminal(),
        Some(&Terminal::Referred {
            target: String::from("docs"),
        })
    );
    let answer_after_terminal = referred.answer("unused");
    assert!(matches!(
        answer_after_terminal,
        Err(BotError::SessionTerminated)
    ));

    let handed_off = Session::new(
        "handoff",
        terminal_flow(
            NodeKind::Handoff {
                target: String::from("support"),
            },
            None,
        )?,
    )?;
    assert_eq!(
        handed_off.terminal(),
        Some(&Terminal::HandedOff {
            target: String::from("support"),
        })
    );

    let completed = Session::new("completed", terminal_flow(NodeKind::End, None)?)?;
    assert_eq!(completed.terminal(), Some(&Terminal::Completed));

    let refused = Session::new(
        "refused",
        terminal_flow(
            NodeKind::End,
            Some(Terminal::Refused {
                reason: String::from("not eligible"),
            }),
        )?,
    )?;
    assert_eq!(
        refused.terminal(),
        Some(&Terminal::Refused {
            reason: String::from("not eligible"),
        })
    );
    Ok(())
}

#[test]
fn reask_keeps_the_cursor_until_a_resolver_match() -> TestResult {
    let flow = ask_end_flow(
        BTreeMap::from([(
            String::from("answer"),
            VarType::Choice(vec![String::from("yes"), String::from("no")]),
        )]),
        BTreeMap::from([
            (String::from("yes"), String::from("end")),
            (String::from("no"), String::from("end")),
        ]),
    )?;
    let mut session = Session::new("reask", flow)?;
    assert_eq!(session.current(), Some("ask"));
    session.answer("maybe")?;
    assert_eq!(session.current(), Some("ask"));
    assert_eq!(session.terminal(), None);
    assert_eq!(session.transcript().len(), 3);
    session.answer("yes")?;
    assert_eq!(session.terminal(), Some(&Terminal::Completed));
    assert_eq!(
        session.scope().get("answer"),
        Some(&Value::Choice(String::from("yes")))
    );
    Ok(())
}

#[test]
fn cycle_over_budget_returns_without_hanging() -> TestResult {
    let flow = FlowSpec::new(
        BTreeMap::from([(
            String::from("answer"),
            VarType::Choice(vec![String::from("yes")]),
        )]),
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("answer"),
                    options: vec![String::from("yes")],
                    routes: BTreeMap::from([(String::from("yes"), String::from("loop"))]),
                },
            ),
            (
                String::from("loop"),
                NodeKind::Route {
                    dispatch: String::from("loop"),
                    fallback: String::from("loop"),
                },
            ),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(4),
    )?;
    let mut session = Session::new("budget", flow)?;
    let error = session.answer("yes");
    assert!(matches!(
        error,
        Err(BotError::SessionBudgetExceeded {
            steps: 5,
            budget: 4
        })
    ));
    Ok(())
}

#[test]
fn validation_rejects_an_undeclared_transition_target() {
    let result = FlowSpec::new(
        BTreeMap::new(),
        "start",
        BTreeMap::from([(String::from("start"), NodeKind::End)]),
        vec![FlowEdge::next("start", "missing")],
        BTreeMap::new(),
        FlowBounds::new(4),
    );
    assert!(matches!(
        result,
        Err(BotError::InvalidTransitionTarget { .. })
    ));
}

#[test]
fn validation_rejects_an_ask_option_without_a_route() {
    let result = ask_end_flow(
        BTreeMap::from([(
            String::from("answer"),
            VarType::Choice(vec![String::from("yes"), String::from("no")]),
        )]),
        BTreeMap::from([(String::from("yes"), String::from("end"))]),
    );
    assert!(matches!(result, Err(BotError::MissingAskRoute { .. })));
}

#[test]
fn validation_rejects_a_declared_variable_without_a_writer() {
    let result = FlowSpec::new(
        BTreeMap::from([(
            String::from("answer"),
            VarType::Choice(vec![String::from("yes")]),
        )]),
        "end",
        BTreeMap::from([(String::from("end"), NodeKind::End)]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(4),
    );
    assert!(matches!(result, Err(BotError::VariableNeverWritten { .. })));
}

#[test]
fn validation_rejects_an_undeclared_template_variable() {
    let result = FlowSpec::new(
        BTreeMap::new(),
        "say",
        BTreeMap::from([
            (
                String::from("say"),
                NodeKind::Say {
                    text: String::from("Hello ${missing}"),
                },
            ),
            (String::from("end"), NodeKind::End),
        ]),
        vec![FlowEdge::next("say", "end")],
        BTreeMap::new(),
        FlowBounds::new(4),
    );
    assert!(matches!(result, Err(BotError::UndeclaredVariable { .. })));
}

#[test]
fn validation_rejects_an_unreachable_node() {
    let result = FlowSpec::new(
        BTreeMap::new(),
        "start",
        BTreeMap::from([
            (String::from("start"), NodeKind::End),
            (String::from("orphan"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(4),
    );
    assert!(matches!(result, Err(BotError::UnreachableNode { .. })));
}

#[test]
fn validation_rejects_nodes_over_the_declared_budget() {
    let result = FlowSpec::new(
        BTreeMap::new(),
        "start",
        BTreeMap::from([
            (String::from("start"), NodeKind::End),
            (String::from("second"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(1),
    );
    assert!(matches!(result, Err(BotError::FlowBudgetExceeded { .. })));
}

#[test]
fn validation_rejects_an_unknown_node_kind() -> TestResult {
    let source = r#"{
        "vars": {},
        "entry": "start",
        "nodes": {"start": {"kind": "teleport"}},
        "edges": [],
        "terminals": {},
        "bounds": {"budget": 4}
    }"#;
    let result = FlowSpec::from_json(source);
    assert!(matches!(result, Err(BotError::UnknownNodeKind { .. })));
    Ok(())
}

/// A resolver that always reports the same verdict, so a session's behaviour
/// can be observed against each [`Resolution`] variant in isolation.
struct FixedResolver(Resolution);

impl Resolver for FixedResolver {
    fn resolve(&self, _utterance: &str, _options: &[String]) -> Resolution {
        self.0.clone()
    }
}

/// One `Ask` node whose options route to an `End`, so a resolved answer is
/// observable as the session leaving the node.
fn one_question_flow() -> Result<FlowSpec, BotError> {
    let mut vars = BTreeMap::new();
    vars.insert(
        String::from("choice"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    );
    FlowSpec::new(
        vars,
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("choice"),
                    options: vec![String::from("yes"), String::from("no")],
                    routes: BTreeMap::from([
                        (String::from("yes"), String::from("done")),
                        (String::from("no"), String::from("done")),
                    ]),
                },
            ),
            (String::from("done"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(8),
    )
}

/// One `Ask` node over a single variable, routing every option to an `End`.
fn single_ask_flow(declared: VarType, options: Vec<String>) -> Result<FlowSpec, BotError> {
    let routes = options
        .iter()
        .map(|option| (option.clone(), String::from("done")))
        .collect();
    FlowSpec::new(
        BTreeMap::from([(String::from("answer"), declared)]),
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("answer"),
                    options,
                    routes,
                },
            ),
            (String::from("done"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(8),
    )
}

/// The declared-type label of one stored value.
///
/// Written out here rather than read from [`VarType::label`] so the assertion
/// is about the value that was actually stored: comparing a stored value to the
/// declaration through one function would agree with itself whatever it stored.
fn value_kind(value: &Value) -> &'static str {
    match *value {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Boolean(_) => "boolean",
        Value::Choice(_) => "choice",
        // `Value` is `#[non_exhaustive]`. A variant added later must be named
        // above before this can claim to know what the variable stored; until
        // then it reports a string no declared type matches, so the assertion
        // that reads it fails rather than passing by accident.
        _ => "unrecognised",
    }
}

/// The five facts an ask-candidate refusal must carry, or a description of why
/// the flow was not refused that way.
fn ask_refusal(result: Result<FlowSpec, BotError>) -> Result<[String; 5], String> {
    match result {
        Err(BotError::AskOptionNotAssignable {
            node,
            variable,
            option,
            expected,
            cause,
        }) => Ok([node, variable, option, String::from(expected), cause]),
        Err(other) => Err(format!("refused for the wrong reason: {other}")),
        Ok(spec) => Err(format!(
            "accepted a flow that offers options it cannot store ({} nodes)",
            spec.nodes().len()
        )),
    }
}

/// One `Ask` node whose options are ordinary English, so a phrase that shares no
/// letter with either of them is the only way to reach the semantic tier.
fn order_flow() -> Result<FlowSpec, BotError> {
    let options = vec![String::from("Repeat last order"), String::from("Cancel")];
    // Cloned once because the declaration and the node both name the options;
    // the node then takes ownership rather than cloning a second time.
    let declared = VarType::Choice(options.clone());
    FlowSpec::new(
        BTreeMap::from([(String::from("order"), declared)]),
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("order"),
                    options,
                    routes: BTreeMap::from([
                        (String::from("Repeat last order"), String::from("end")),
                        (String::from("Cancel"), String::from("end")),
                    ]),
                },
            ),
            (String::from("end"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(8),
    )
}

/// Whether every embedding an [`OrderEmbedder`] returns carries a direction.
///
/// A named type rather than a `bool` on the embedder, so the test that reads it
/// says which state it is asking for and the compiler checks that the two
/// states are the two that exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measurability {
    /// Every embedding is a direction, so every comparison that is attempted
    /// produces a score.
    Complete,
    /// One competitor embeds to the zero vector: the right width, so the
    /// resolver's dimension check accepts it, and no direction, so no angle
    /// exists between it and anything else.
    DegenerateCompetitor,
}

/// An embedder that places `"the usual"` next to `"Repeat last order"` and
/// everything else orthogonally, so the test's geometry is its assertion.
struct OrderEmbedder {
    identity: EmbedderIdentity,
    measurability: Measurability,
}

impl Embedder for OrderEmbedder {
    type Error = std::convert::Infallible;

    fn identity(&self) -> &EmbedderIdentity {
        &self.identity
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Self::Error> {
        let vector = match (text, self.measurability) {
            ("the usual" | "Repeat last order", _) => vec![1.0, 0.0],
            ("Cancel", Measurability::DegenerateCompetitor) => vec![0.0, 0.0],
            _ => vec![0.0, 1.0],
        };
        Ok(vector)
    }
}

#[test]
fn a_session_resolves_a_phrase_only_a_model_can_relate() -> TestResult {
    // The claim this test exists to check is the integration one: a semantic
    // resolver is a `Resolver`, so it drops into `Session` with no change to the
    // session, the flow document, or the four verbs. The unit tests in
    // `semantic.rs` prove the tier's arithmetic; only this proves it plugs in.
    let embedder = OrderEmbedder {
        identity: EmbedderIdentity::new("test-model", "digest-test", 2)?,
        measurability: Measurability::Complete,
    };
    let mut session =
        Session::with_resolver("semantic", order_flow()?, SemanticResolver::new(embedder))?;

    // The premise, asserted rather than assumed: the default resolver cannot
    // reach this phrase. Without it the test could pass on a lexicon match and
    // prove nothing about the model.
    let mut default = Session::new("default", order_flow()?)?;
    default.answer("the usual")?;
    assert_eq!(
        default.current(),
        Some("ask"),
        "the lexicon alone must re-ask, or this test proves nothing about the model"
    );

    session.answer("the usual")?;

    assert_eq!(
        session.terminal(),
        Some(&Terminal::Completed),
        "the model's resolution advanced the session"
    );
    assert_eq!(
        session.scope().get("order"),
        Some(&Value::Choice(String::from("Repeat last order")))
    );
    Ok(())
}

#[test]
fn a_degraded_resolver_reasks_without_claiming_absence() -> TestResult {
    let session = Session::with_resolver(
        "degraded",
        one_question_flow()?,
        FixedResolver(Resolution::Degraded {
            reason: DegradedReason::EmbedderUnavailable,
        }),
    )?;
    let mut session = session;
    session.answer("yes")?;

    assert_eq!(
        session.current(),
        Some("ask"),
        "a degraded resolver re-asks the same node rather than advancing"
    );
    assert_eq!(session.terminal(), None);

    let roles: Vec<&str> = session
        .transcript()
        .iter()
        .map(TranscriptEntry::role)
        .collect();
    assert!(
        roles.contains(&"resolver-degraded"),
        "the cause is recorded under its own role, got {roles:?}"
    );
    let recorded = session
        .transcript()
        .iter()
        .find(|entry| entry.role() == "resolver-degraded")
        .map(TranscriptEntry::text)
        .unwrap_or_default();
    assert_eq!(
        recorded, "Resolver unavailable: the embedder is unavailable",
        "the record names the cause rather than reporting an empty score"
    );
    Ok(())
}

#[test]
fn an_unmeasurable_competitor_does_not_advance_the_session() -> TestResult {
    // End to end through the real `SemanticResolver`, not a fixed verdict. The
    // utterance and the competitor both embed to the zero vector: the width is
    // right, so the dimension check passes, and no angle can be drawn from
    // either. Before the repair this resolved to `Repeat last order` with a lead
    // equal to its whole score — a winner over a one-option field it was never
    // compared against — and the session advanced on it.
    let identity = || EmbedderIdentity::new("test-model", "digest-test", 2);
    let mut session = Session::with_resolver(
        "semantic",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: identity()?,
            measurability: Measurability::DegenerateCompetitor,
        }),
    )?;

    session.answer("the usual")?;

    assert_eq!(
        session.current(),
        Some("ask"),
        "an incomplete comparison set must not advance the session"
    );
    assert_eq!(session.terminal(), None);
    assert_eq!(
        session.scope().get("order"),
        None,
        "no option was chosen, so the variable is not bound"
    );
    let recorded = session
        .transcript()
        .iter()
        .find(|entry| entry.role() == "resolver-degraded")
        .map(TranscriptEntry::text)
        .unwrap_or_default();
    assert_eq!(
        recorded, "Resolver unavailable: an embedding could not be measured",
        "the record names the unmeasurable embedding, not a missing dependency"
    );

    // The same flow with a healthy competitor set: one option is measured
    // against both competitors and the session completes. Without this half the
    // test could pass on a resolver that never advances at all.
    let mut healthy = Session::with_resolver(
        "semantic",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: identity()?,
            measurability: Measurability::Complete,
        }),
    )?;
    healthy.answer("the usual")?;
    assert_eq!(
        healthy.terminal(),
        Some(&Terminal::Completed),
        "the same utterance resolves once every comparison can be made"
    );
    assert_eq!(
        healthy.scope().get("order"),
        Some(&Value::Choice(String::from("Repeat last order")))
    );
    Ok(())
}

#[test]
fn a_degraded_verdict_is_distinguishable_from_an_absent_one() -> TestResult {
    let mut absent = Session::with_resolver(
        "absent",
        one_question_flow()?,
        FixedResolver(Resolution::Absent { best_score: 0.0 }),
    )?;
    absent.answer("yes")?;
    let mut degraded = Session::with_resolver(
        "degraded",
        one_question_flow()?,
        FixedResolver(Resolution::Degraded {
            reason: DegradedReason::EmbedderUnavailable,
        }),
    )?;
    degraded.answer("yes")?;

    // Both re-ask, and that is the point: the *session* behaviour is the same,
    // while what is recorded is not. A two-valued verdict would leave these two
    // transcripts identical, and an operator reading a session that repeats the
    // same question would have no way to tell an unclear person from a resolver
    // that never ran.
    assert_eq!(
        degraded.transcript().len(),
        absent.transcript().len() + 1,
        "the degraded re-ask carries one record the absent one does not"
    );
    let absent_roles: Vec<&str> = absent
        .transcript()
        .iter()
        .map(TranscriptEntry::role)
        .collect();
    let degraded_roles: Vec<&str> = degraded
        .transcript()
        .iter()
        .map(TranscriptEntry::role)
        .collect();
    assert!(!absent_roles.contains(&"resolver-degraded"));
    assert!(degraded_roles.contains(&"resolver-degraded"));
    assert_ne!(absent_roles, degraded_roles);
    Ok(())
}

#[test]
fn the_boolean_ask_from_the_report_is_refused_by_from_json() -> TestResult {
    // The document is the one in the defect report, verbatim. It used to
    // validate, construct a session, reach the ask, and then fail *every*
    // answer: `Continue` and `Cancel` are exact resolver matches that a boolean
    // variable cannot store, and answering `true` cannot repair the flow
    // because the session stores the selected option, not the raw utterance.
    let document = r#"
    {
      "vars": {"decision": {"kind": "boolean"}},
      "entry": "ask",
      "nodes": {
        "ask": {
          "kind": "ask",
          "var": "decision",
          "options": ["Continue", "Cancel"],
          "routes": {"Continue": "done", "Cancel": "done"}
        },
        "done": {"kind": "end"}
      }
    }
    "#;

    // `from_json` is the whole proof: the refusal is a load-time verdict, so no
    // session is constructed and no journal entry is written.
    let [node, variable, option, expected, cause] = ask_refusal(FlowSpec::from_json(document))?;
    assert_eq!(
        node, "ask",
        "the refusal names the node carrying the candidate"
    );
    assert_eq!(
        variable, "decision",
        "the refusal names the variable the ask writes"
    );
    assert_eq!(
        option, "Continue",
        "the refusal names the candidate it could not store"
    );
    assert_eq!(expected, "boolean", "the refusal names the declared type");
    assert_eq!(
        cause,
        AnswerRejection::NotABoolean.to_string(),
        "the cause is the decoder's own verdict rather than a second opinion about it"
    );
    Ok(())
}

#[test]
fn a_candidate_after_a_storable_one_is_still_refused() -> TestResult {
    // A check that stopped at the first option would accept this document and
    // leave `Cancel` unanswerable: the same defect, one option later.
    let facts = ask_refusal(single_ask_flow(
        VarType::Boolean,
        vec![String::from("yes"), String::from("Cancel")],
    ))?;
    let [_, _, option, _, _] = facts;
    assert_eq!(
        option, "Cancel",
        "every candidate is checked, not only the first"
    );
    Ok(())
}

#[test]
fn every_declared_type_refuses_a_candidate_it_cannot_store() -> TestResult {
    // `String` has no refusal case to pin: it stores any candidate, which is
    // the free-form declaration doing its job. The other three each have a
    // candidate that a person could reasonably author and that the variable
    // cannot hold, plus the two integer range edges.
    let choice = VarType::Choice(vec![String::from("yes"), String::from("no")]);
    let cases: Vec<(VarType, Vec<String>, &str, AnswerRejection)> = vec![
        (
            VarType::Boolean,
            vec![String::from("Continue"), String::from("Cancel")],
            "Continue",
            AnswerRejection::NotABoolean,
        ),
        (
            VarType::Integer,
            vec![String::from("7.5")],
            "7.5",
            AnswerRejection::NotAnInteger,
        ),
        (
            VarType::Integer,
            vec![String::from("9223372036854775808")],
            "9223372036854775808",
            AnswerRejection::IntegerOutOfRange,
        ),
        (
            VarType::Integer,
            vec![String::from("-9223372036854775809")],
            "-9223372036854775809",
            AnswerRejection::IntegerOutOfRange,
        ),
        (
            choice,
            vec![String::from("approve"), String::from("deny")],
            "approve",
            AnswerRejection::NotADeclaredChoice,
        ),
    ];

    for (declared, options, expected_option, rejection) in cases {
        let expected_label = declared.label();
        let [node, variable, option, expected, cause] =
            ask_refusal(single_ask_flow(declared, options))?;
        assert_eq!(node, "ask", "the refusal names the ask node");
        assert_eq!(
            variable, "answer",
            "the refusal names the variable the ask writes"
        );
        assert_eq!(
            option, expected_option,
            "the refusal names the candidate it cannot store"
        );
        assert_eq!(
            expected, expected_label,
            "the refusal names the declared type"
        );
        assert_eq!(
            cause,
            rejection.to_string(),
            "the cause is the decoder's own verdict"
        );
    }
    Ok(())
}

#[test]
fn every_option_of_every_accepted_ask_is_storable() -> TestResult {
    // Four asks, one per declared type, chained so every node is reachable:
    // each ask routes all of its options to the next ask, and the last one to
    // the end. The candidates disagree with the declaration in spelling on
    // purpose (`SMALL` against `small`, `007` against a plain integer), because
    // those are exactly the candidates a stricter check would refuse and a
    // person would still expect to work.
    let asks = [
        (
            String::from("words"),
            VarType::String,
            vec![String::from("Continue"), String::from("Cancel")],
        ),
        (
            String::from("count"),
            VarType::Integer,
            vec![
                String::from("7"),
                String::from(" -12 "),
                String::from("007"),
            ],
        ),
        (
            String::from("confirmed"),
            VarType::Boolean,
            vec![
                String::from("yes"),
                String::from("FALSE"),
                String::from("No"),
            ],
        ),
        (
            String::from("size"),
            VarType::Choice(vec![String::from("small"), String::from("large")]),
            vec![String::from("SMALL"), String::from("Large")],
        ),
    ];

    let ids: Vec<String> = asks.iter().map(|ask| format!("ask_{}", ask.0)).collect();
    let mut vars = BTreeMap::new();
    let mut nodes = BTreeMap::new();
    for (position, ask) in asks.iter().enumerate() {
        let (variable, declared, options) = (&ask.0, &ask.1, &ask.2);
        vars.insert(variable.clone(), declared.clone());
        let target = ids
            .get(position + 1)
            .cloned()
            .unwrap_or_else(|| String::from("end"));
        let routes = options
            .iter()
            .map(|option| (option.clone(), target.clone()))
            .collect();
        nodes.insert(
            format!("ask_{variable}"),
            NodeKind::Ask {
                var: variable.clone(),
                options: options.clone(),
                routes,
            },
        );
    }
    nodes.insert(String::from("end"), NodeKind::End);
    let spec = FlowSpec::from_nodes(vars, "ask_words", nodes, FlowBounds::new(64))?;

    // The enumeration is over the *spec*, not over the `asks` array, so it
    // proves the property of the document that was accepted rather than of the
    // table the document was built from.
    let mut enumerated = 0_usize;
    for node_id in spec.nodes().keys() {
        let Some(NodeKind::Ask { var, options, .. }) = spec.node(node_id).cloned() else {
            continue;
        };
        let Some(declared) = spec.vars().get(&var) else {
            return Err(format!("ask node {node_id} writes undeclared variable {var}").into());
        };
        for option in &options {
            let decoded = declared.decode_answer(option).map_err(|rejection| {
                format!(
                    "accepted ask {node_id} offers {option:?} for a {} variable, \
                     which cannot store it: {rejection}",
                    declared.label()
                )
            })?;
            assert_eq!(
                value_kind(&decoded),
                declared.label(),
                "ask {node_id} option {option:?} stores a {} value",
                value_kind(&decoded)
            );
            enumerated = enumerated.saturating_add(1);
        }
    }
    assert_eq!(
        enumerated, 10,
        "the fixture's four accepted asks offer ten candidates in total"
    );
    Ok(())
}

#[test]
fn a_display_label_stores_the_value_the_declared_type_names() -> TestResult {
    // A candidate is a label the resolver matches *and* a value the variable
    // stores, and the two are not always the same string. Each case pins one
    // mapping a person reading the document would expect: the label is routed
    // on as written, and what lands in the variable is the declared type's
    // value.
    let cases: Vec<(VarType, &str, Value)> = vec![
        (VarType::Boolean, "yes", Value::Boolean(true)),
        (VarType::Boolean, "No", Value::Boolean(false)),
        (VarType::Integer, "007", Value::Integer(7)),
        (VarType::Integer, " -12 ", Value::Integer(-12)),
        (
            VarType::Choice(vec![String::from("small")]),
            "SMALL",
            Value::Choice(String::from("small")),
        ),
        (
            VarType::String,
            "  padded  ",
            Value::String(String::from("  padded  ")),
        ),
    ];

    for (declared, option, expected) in cases {
        let label = declared.label();
        let spec = single_ask_flow(declared, vec![String::from(option)])?;
        let mut session = Session::new("mapping", spec)?;
        session.answer(option)?;
        assert_eq!(
            session.scope().get("answer"),
            Some(&expected),
            "answering {option:?} for a {label} variable stores {expected:?}"
        );
    }
    Ok(())
}

#[test]
fn exact_ambiguous_and_absent_answers_behave_as_before() -> TestResult {
    // The load-time check must not change what happens to an answer at runtime.
    let mut exact = Session::new("exact", one_question_flow()?)?;
    exact.answer("yes")?;
    assert_eq!(
        exact.terminal(),
        Some(&Terminal::Completed),
        "an exact match still routes on a validated ask"
    );
    assert_eq!(
        exact.scope().get("choice"),
        Some(&Value::Choice(String::from("yes"))),
        "the exact match stores the option the route was keyed by"
    );

    let mut ambiguous = Session::with_resolver(
        "ambiguous",
        one_question_flow()?,
        FixedResolver(Resolution::Ambiguous {
            tied: vec![0, 1],
            score: 0.8,
        }),
    )?;
    ambiguous.answer("maybe")?;
    assert_eq!(
        ambiguous.current(),
        Some("ask"),
        "an ambiguous answer leaves the cursor on the ask"
    );
    assert_eq!(
        ambiguous.scope().get("choice"),
        None,
        "an ambiguous answer stores no value"
    );
    assert_eq!(
        ambiguous.transcript().last().map(TranscriptEntry::text),
        Some("Choose one: yes, no"),
        "the re-ask repeats the options still in play"
    );

    let mut absent = Session::with_resolver(
        "absent",
        one_question_flow()?,
        FixedResolver(Resolution::Absent { best_score: 0.0 }),
    )?;
    absent.answer("maybe")?;
    assert_eq!(
        absent.current(),
        Some("ask"),
        "an unrecognized answer leaves the cursor on the ask"
    );
    assert_eq!(
        absent.scope().get("choice"),
        None,
        "an unrecognized answer stores no value"
    );
    Ok(())
}

/// One journal record: the path node, the role label, and the rendered text.
type JournalRecord = (String, String, String);

/// The buffer a [`SharedJournal`] writes into, and the handle a test reads.
type JournalRecords = Rc<RefCell<Vec<JournalRecord>>>;

/// A journal whose records the caller keeps a handle to.
///
/// A session owns its journal, so a test that inspects the session after a
/// refused construction has nothing left to inspect: the error path returns no
/// session. Sharing the record buffer instead makes "refused before anything
/// was written" observable rather than asserted.
#[derive(Clone)]
struct SharedJournal {
    records: JournalRecords,
}

impl SharedJournal {
    /// An empty journal and the handle that reads it.
    fn new() -> (Self, JournalRecords) {
        let records: JournalRecords = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                records: Rc::clone(&records),
            },
            records,
        )
    }
}

impl Journal for SharedJournal {
    fn record(&mut self, path_node: &str, role: &str, text: &str) {
        self.records
            .borrow_mut()
            .push((path_node.to_owned(), role.to_owned(), text.to_owned()));
    }
}

/// One flow per terminal node kind, paired with the outcome that node kind
/// carries before the terminal map is consulted.
///
/// The targets are deliberately ordinary strings: the cross product below uses
/// `elsewhere`, so a test that stopped comparing targets would accept a
/// declaration that hands the person to a different place than the node names.
fn terminal_kinds() -> Vec<(&'static str, NodeKind, Terminal)> {
    vec![
        ("end", NodeKind::End, Terminal::Completed),
        (
            "handoff",
            NodeKind::Handoff {
                target: String::from("external-support"),
            },
            Terminal::HandedOff {
                target: String::from("external-support"),
            },
        ),
        (
            "refer",
            NodeKind::Refer {
                target: String::from("docs"),
                text: String::from("Read the guide"),
            },
            Terminal::Referred {
                target: String::from("docs"),
            },
        ),
    ]
}

/// Every declared outcome the cross product tries against every terminal node
/// kind.
///
/// One per `Terminal` variant, each with a payload no node above carries, so
/// the comparison that decides accept-or-refuse is doing work on all four: a
/// rule that only compared variant names would accept `HandedOff` against a
/// `refer` node, and a rule that ignored targets would accept a handoff to the
/// wrong place.
fn declared_outcomes() -> Vec<Terminal> {
    vec![
        Terminal::Completed,
        Terminal::Referred {
            target: String::from("elsewhere"),
        },
        Terminal::HandedOff {
            target: String::from("elsewhere"),
        },
        Terminal::Refused {
            reason: String::from("not authorized"),
        },
    ]
}

/// The three facts a conflicting-terminal refusal must carry, or a description
/// of why the document was not refused that way.
///
/// Generic over the success type because the same refusal is checked at both
/// doors: `FlowSpec`'s constructors, and `Session::with_components`, which
/// re-validates a flow a caller may have deserialized without going through
/// `FlowSpec::from_json`.
fn conflict_refusal<T>(
    result: Result<T, BotError>,
) -> Result<(String, Terminal, Terminal), String> {
    match result {
        Err(BotError::ConflictingTerminalDeclaration {
            node,
            intrinsic,
            declared,
        }) => Ok((node, intrinsic, declared)),
        Err(other) => Err(format!("refused for the wrong reason: {other}")),
        Ok(_) => Err(String::from(
            "accepted a document whose terminal declaration contradicts its node",
        )),
    }
}

/// The disposition a downstream consumer reads from one terminal outcome.
///
/// Routed through the public classifier rather than matched here, because the
/// defect's consequence was downstream: a consumer switches on this, so this is
/// what the assertion has to read.
fn dispatch_of(terminal: &Terminal) -> Disposition {
    terminal.outcome(EffectLedger::new(0, 0)).disposition()
}

#[test]
fn the_handoff_refusal_from_the_report_is_refused_by_from_json() -> TestResult {
    // The document is the one in the defect report. It used to validate, build
    // a session, and complete with `HandedOff { target: "external-support" }`
    // — the authored refusal was accepted, round-tripped through the document,
    // and then discarded at the moment the node ran.
    let document = r#"
    {
      "vars": {},
      "entry": "handoff",
      "nodes": {
        "handoff": {"kind": "handoff", "target": "external-support"}
      },
      "terminals": {
        "handoff": {"kind": "refused", "reason": "handoff is not authorized"}
      }
    }
    "#;

    let (node, intrinsic, declared) = conflict_refusal(FlowSpec::from_json(document))?;
    assert_eq!(
        node, "handoff",
        "the refusal names the node carrying both outcomes"
    );
    assert_eq!(
        intrinsic,
        Terminal::HandedOff {
            target: String::from("external-support")
        },
        "the refusal names the outcome the node's kind carries"
    );
    assert_eq!(
        declared,
        Terminal::Refused {
            reason: String::from("handoff is not authorized")
        },
        "the refusal names the outcome the document declared"
    );
    Ok(())
}

#[test]
fn a_json_terminal_declaration_is_executed_and_not_merely_accepted() -> TestResult {
    // The other half of the rule: a declaration that agrees with its node is
    // accepted, and the session then executes it. The existing acceptance
    // fixtures already put matching duplicates on `handoff` and `refer` nodes;
    // they could not tell the difference between a declaration that is read
    // and one that is ignored, which is the blind spot this pins.
    let document = r#"
    {
      "vars": {},
      "entry": "handoff",
      "nodes": {
        "handoff": {"kind": "handoff", "target": "external-support"}
      },
      "terminals": {
        "handoff": {"kind": "handed_off", "target": "external-support"}
      }
    }
    "#;

    let spec = FlowSpec::from_json(document)?;
    let declared = Terminal::HandedOff {
        target: String::from("external-support"),
    };
    assert_eq!(
        spec.effective_terminal("handoff"),
        Some(declared.clone()),
        "the validated document exposes the outcome it declares"
    );
    let session = Session::new("json", spec)?;
    assert_eq!(
        session.terminal(),
        Some(&declared),
        "the session executes the outcome the validated document exposes"
    );
    Ok(())
}

#[test]
fn every_terminal_node_kind_agrees_with_every_declared_outcome() -> TestResult {
    // The cross product: three terminal node kinds against the four `Terminal`
    // variants, plus each node's own outcome, which is the declaration a
    // `handoff` or `refer` node has to be able to repeat. Fourteen
    // combinations, and every one of them is decided here rather than left to
    // the happy path:
    //
    //   - an `end` node accepts all four declared outcomes, because it has no
    //     destination to contradict;
    //   - a `handoff` or `refer` node accepts only the outcome it carries
    //     itself, and refuses the other four.
    //
    // Six accepted and eight refused. The rule is restated here rather than
    // read from the crate because this is the test that says the rule is what a
    // reader would expect: the assertions below are still about behaviour —
    // what the accepted document exposes and executes, and what the refusal
    // names.
    let mut accepted = 0_usize;
    let mut refused = 0_usize;

    for (label, kind, intrinsic) in terminal_kinds() {
        let mut attempts = declared_outcomes();
        if !attempts.contains(&intrinsic) {
            attempts.push(intrinsic.clone());
        }
        for declared in attempts {
            let result = terminal_flow(kind.clone(), Some(declared.clone()));
            if matches!(kind, NodeKind::End) || intrinsic == declared {
                let spec = result.map_err(|error| {
                    format!("{label} refused the outcome {declared:?} it should accept: {error}")
                })?;
                assert_eq!(
                    spec.effective_terminal("start"),
                    Some(declared.clone()),
                    "{label} exposes the outcome it declares"
                );
                let session = Session::new("cross", spec)?;
                assert_eq!(
                    session.current(),
                    None,
                    "{label} stops the cursor at the terminal node"
                );
                assert_eq!(
                    session.terminal(),
                    Some(&declared),
                    "{label} executes the outcome it declares"
                );
                match session.terminal() {
                    Some(terminal) => assert_eq!(
                        dispatch_of(terminal),
                        dispatch_of(&declared),
                        "{label} dispatches as the outcome it declares"
                    ),
                    None => return Err(format!("{label} reached no terminal outcome").into()),
                }
                accepted = accepted.saturating_add(1);
            } else {
                let (node, got_intrinsic, got_declared) = conflict_refusal(result)?;
                assert_eq!(
                    node, "start",
                    "{label} names the node carrying both outcomes"
                );
                assert_eq!(
                    got_intrinsic, intrinsic,
                    "{label} reports the outcome its kind carries"
                );
                assert_eq!(
                    got_declared, declared,
                    "{label} reports the outcome the document declared"
                );
                refused = refused.saturating_add(1);
            }
        }
    }

    assert_eq!(
        (accepted, refused),
        (6, 8),
        "the cross product is complete: every combination is decided"
    );
    Ok(())
}

#[test]
fn a_conflicting_terminal_is_refused_before_the_session_writes_anything() -> TestResult {
    // `json::from_str` is the public door that skips `FlowSpec::from_json`'s
    // validation, so this is a document a caller can genuinely hold:
    // deserialized and never validated. It is therefore the document `Session`
    // has to refuse for itself, and the assertion is that it does so before a
    // single record reaches the journal or the transcript.
    let document = r#"
    {
      "vars": {},
      "entry": "handoff",
      "nodes": {"handoff": {"kind": "handoff", "target": "external-support"}},
      "terminals": {
        "handoff": {"kind": "refused", "reason": "handoff is not authorized"}
      }
    }
    "#;

    let (journal, records) = SharedJournal::new();
    let unvalidated: FlowSpec = lgwks_bot::json::from_str(document)?;
    let outcome = Session::with_components(
        "conflict",
        unvalidated,
        lgwks_bot::language::LanguageResolver::new(),
        journal,
    );
    let (node, intrinsic, declared) = conflict_refusal(outcome)?;
    assert_eq!(
        node, "handoff",
        "the refusal names the node carrying both outcomes"
    );
    assert_eq!(
        intrinsic,
        Terminal::HandedOff {
            target: String::from("external-support")
        },
        "the refusal names the outcome the node's kind carries"
    );
    assert_eq!(
        declared,
        Terminal::Refused {
            reason: String::from("handoff is not authorized")
        },
        "the refusal names the outcome the document declared"
    );
    assert!(
        records.borrow().is_empty(),
        "a refused document reaches the journal with no records: {:?}",
        records.borrow()
    );

    // The same document through the ordinary constructor, since that is how a
    // caller who never touched the facade would arrive at it.
    let unvalidated: FlowSpec = lgwks_bot::json::from_str(document)?;
    assert!(
        matches!(
            Session::new("conflict", unvalidated),
            Err(BotError::ConflictingTerminalDeclaration { .. })
        ),
        "Session::new refuses the same contradiction"
    );
    Ok(())
}

#[test]
fn a_refused_declaration_never_dispatches_as_a_handoff() -> TestResult {
    // The downstream consequence the report names. A consumer switches on the
    // returned disposition, and the bug handed it a permissive handoff for a
    // document that had explicitly refused. `End` is where a refusal is
    // supported, so that is where the dispatch assertion belongs.
    let spec = terminal_flow(
        NodeKind::End,
        Some(Terminal::Refused {
            reason: String::from("not authorized"),
        }),
    )?;
    let session = Session::new("refused", spec)?;
    let Some(terminal) = session.terminal() else {
        return Err("an end-only flow reached no terminal outcome".into());
    };
    assert_eq!(
        terminal,
        &Terminal::Refused {
            reason: String::from("not authorized")
        },
        "the refusal is what the document declared"
    );
    assert_eq!(
        dispatch_of(terminal),
        Disposition::Refused,
        "a refused document dispatches as refused"
    );
    assert_ne!(
        dispatch_of(terminal),
        Disposition::HandedOff,
        "a refused document is never classified as a handoff"
    );

    // And the same refusal declared on a handoff node is refused outright,
    // rather than run with the refusal discarded — which is the only other way
    // this defect could have been "fixed".
    let conflicting = terminal_flow(
        NodeKind::Handoff {
            target: String::from("external-support"),
        },
        Some(Terminal::Refused {
            reason: String::from("not authorized"),
        }),
    );
    conflict_refusal(conflicting)?;
    Ok(())
}

#[test]
fn a_non_terminal_node_has_no_effective_outcome() -> TestResult {
    // The `None` half of the centralised calculation, which is what keeps it
    // from inventing an outcome for a node that never ends a run.
    let flow = one_question_flow()?;
    assert_eq!(
        flow.effective_terminal("ask"),
        None,
        "an ask node has no effective outcome"
    );
    assert_eq!(
        flow.effective_terminal("done"),
        Some(Terminal::Completed),
        "an end node with no declaration completes"
    );
    assert_eq!(
        flow.effective_terminal("no-such-node"),
        None,
        "a missing node has no effective outcome"
    );
    Ok(())
}

// ── Issue 40: the byte ceilings a flow's shape does not bound ───────────────
//
// Validation bounds a document's *shape* and `FlowBounds` bounds its *steps*.
// Neither bounds bytes, and the two are independent: `${answer}` written forty
// thousand times is a two-hundred-kilobyte document that interpolates to a
// hundred and sixty megabytes, and one `say` node is one step however much
// text it renders. The tests below use ceilings small enough to run in a test
// binary and pin the byte arithmetic a shipped ceiling applies to the same
// shape, so the amplification is exercised rather than described.

/// A flow that asks `answer` once, speaks `say_text`, and ends.
///
/// The shape the amplification needs: the spoken text is built from an answer
/// the person supplies, so the document can be small while the expansion is
/// not.
fn speak_back_flow(say_text: &str, options: &[String]) -> Result<FlowSpec, BotError> {
    let routes = options
        .iter()
        .map(|option| (option.clone(), String::from("say")))
        .collect();
    FlowSpec::new(
        BTreeMap::from([(String::from("answer"), VarType::String)]),
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("answer"),
                    options: options.to_vec(),
                    routes,
                },
            ),
            (
                String::from("say"),
                NodeKind::Say {
                    text: say_text.to_owned(),
                },
            ),
            (String::from("done"), NodeKind::End),
        ]),
        vec![FlowEdge::next("say", "done")],
        BTreeMap::new(),
        FlowBounds::new(16),
    )
}

/// A JSON document for [`speak_back_flow`], carrying the supplied `bounds`.
///
/// Written as text rather than serialized so the test reaches the parser a
/// caller reaches — [`FlowSpec::from_json`] — on a document shaped like a
/// caller's. Only the template and the candidate list are interpolated, and a
/// caller of this helper passes strings with no characters the format has to
/// escape.
fn speak_back_document(say_text: &str, options: &[String], bounds: &str) -> String {
    let quoted = options
        .iter()
        .map(|option| format!("\"{option}\""))
        .collect::<Vec<String>>()
        .join(", ");
    let routes = options
        .iter()
        .map(|option| format!("\"{option}\": \"say\""))
        .collect::<Vec<String>>()
        .join(", ");
    format!(
        "{{\"vars\": {{\"answer\": {{\"kind\": \"string\"}}}}, \
         \"entry\": \"ask\", \
         \"nodes\": {{ \
         \"ask\": {{\"kind\": \"ask\", \"var\": \"answer\", \"options\": [{quoted}], \
         \"routes\": {{{routes}}}}}, \
         \"say\": {{\"kind\": \"say\", \"text\": \"{say_text}\"}}, \
         \"done\": {{\"kind\": \"end\"}}}}, \
         \"edges\": [{{\"kind\": \"next\", \"from\": \"say\", \"to\": \"done\"}}], \
         \"bounds\": {bounds}}}"
    )
}

/// A flow whose ask loops back through a say node until the person stops.
///
/// The repetition the aggregate ceiling exists for: each turn is a handful of
/// individually acceptable records, and the sum of them is what needs a bound.
fn loop_flow() -> Result<FlowSpec, BotError> {
    FlowSpec::new(
        BTreeMap::from([(String::from("answer"), VarType::String)]),
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("answer"),
                    options: vec![String::from("again"), String::from("stop")],
                    routes: BTreeMap::from([
                        (String::from("again"), String::from("say")),
                        (String::from("stop"), String::from("done")),
                    ]),
                },
            ),
            (
                String::from("say"),
                NodeKind::Say {
                    text: String::from("tick"),
                },
            ),
            (String::from("done"), NodeKind::End),
        ]),
        vec![FlowEdge::next("say", "ask")],
        BTreeMap::new(),
        FlowBounds::new(64),
    )
}

/// The bytes a session retains, recounted from its public surface.
///
/// Written out here rather than read from `Session::retained_bytes` so the
/// assertion compares the counter to what the session actually holds: a
/// comparison of the counter with itself would agree with itself whatever it
/// counted.
fn retained_recorded_bytes(session: &Session) -> usize {
    let records = session.transcript().iter().fold(0usize, |total, entry| {
        total
            .saturating_add(entry.text().len())
            .saturating_add(entry.path_node().len())
            .saturating_add(entry.role().len())
    });
    session
        .visited()
        .iter()
        .fold(records, |total, node| total.saturating_add(node.len()))
}

#[test]
fn a_placeholder_is_charged_the_bytes_it_expands_to() {
    // Multibyte text is charged its UTF-8 bytes, not its characters: the
    // expansion pastes bytes into a buffer, and a ceiling counted in characters
    // would let a document four times its own limit through.
    let multibyte = Value::String("\u{e9}".repeat(512));
    assert_eq!(
        multibyte.rendered_bytes(),
        1024,
        "512 two-byte characters occupy 1024 bytes"
    );
    assert_eq!(
        "\u{e9}".repeat(512).chars().count(),
        512,
        "the character count is half the byte count, which is why the bytes are what is charged"
    );
    // Integers are charged the width of their decimal spelling — what
    // interpolation writes — and not the width of the candidate that produced
    // them.
    assert_eq!(
        Value::Integer(0).rendered_bytes(),
        1,
        "zero renders as one digit"
    );
    assert_eq!(
        Value::Integer(-7).rendered_bytes(),
        2,
        "a negative integer renders its sign"
    );
    assert_eq!(
        Value::Integer(12_345).rendered_bytes(),
        5,
        "five digits render five bytes"
    );
    assert_eq!(
        Value::Integer(i64::MAX).rendered_bytes(),
        19,
        "the widest positive integer is nineteen bytes"
    );
    assert_eq!(
        Value::Integer(i64::MIN).rendered_bytes(),
        20,
        "the widest negative integer is twenty, and its magnitude has no positive counterpart"
    );
    assert_eq!(
        Value::Boolean(true).rendered_bytes(),
        4,
        "true renders as four bytes"
    );
    assert_eq!(
        Value::Boolean(false).rendered_bytes(),
        5,
        "false renders as five bytes"
    );
}

#[test]
fn a_repeated_placeholder_is_refused_at_its_computed_size() -> TestResult {
    // The report's defect at a scale a test binary can afford. Twenty thousand
    // repetitions of a value at the shipped utterance ceiling is a two-hundred-
    // kilobyte document whose expansion is a hundred and sixty-three megabytes:
    // the document validates, the budget of sixteen steps is untouched, and the
    // refusal reports the size of a string that was never built. The value is
    // as large as the ingress ceiling allows, which is the most a document can
    // get out of one answer and therefore the worst case for this shape.
    let value = "z".repeat(MAX_UTTERANCE_BYTES);
    let repeats = 20_000usize;
    let template = "${answer}".repeat(repeats);
    let expansion = repeats
        .checked_mul(value.len())
        .ok_or("the fixture's expansion overflows usize")?;
    // Above the prompt's own size, which carries the candidate in full, and far
    // below the expansion the template asks for.
    let ceiling = 16_384usize;
    assert!(
        expansion > MAX_RECORD_BYTES,
        "the fixture's expansion ({expansion} bytes) is over the shipped record ceiling too, \
         so the contraction is not an artefact of a deliberately tiny bound"
    );
    let limits = ResourceLimits::shipped().tighten(ResourceAxis::Record, ceiling);
    let (journal, records) = SharedJournal::new();
    let mut session = Session::with_components_and_limits(
        "amplify",
        speak_back_flow(&template, std::slice::from_ref(&value))?,
        LanguageResolver::new(),
        journal,
        limits,
    )?;
    let written_before = records.borrow().len();
    let retained_before = session.retained_bytes();
    assert_eq!(
        session.limits().get(ResourceAxis::Record),
        ceiling,
        "the session enforces the tightened ceiling"
    );
    match session.answer(&value) {
        Err(BotError::TemplateExpansionTooLarge { node, bytes, limit }) => {
            assert_eq!(node, "say", "the refusal names the template's node");
            assert_eq!(
                bytes, expansion,
                "the refusal reports the exact expansion, computed rather than measured: \
                 164 MB from a 200 KB document"
            );
            assert_eq!(limit, ceiling, "the refusal reports the ceiling in force");
        }
        other => {
            return Err(format!("expected the expansion refusal, got {other:?}").into());
        }
    }
    assert!(
        records.borrow().len() > written_before,
        "the answer's own record was written before the say node was reached"
    );
    assert_eq!(
        records.borrow().len(),
        session.transcript().len(),
        "the journal and the transcript hold the same records: the refused write left no \
         partial record in either"
    );
    assert!(
        records.borrow().iter().all(|record| record.0 != "say"),
        "the refused say record was never handed to the journal"
    );
    assert_eq!(
        session.current(),
        Some("say"),
        "the session is left at the node whose text it could not speak"
    );
    assert!(
        session.visited().iter().any(|node| node == "say"),
        "the say node was entered, charged, and then refused its own text"
    );
    assert_eq!(
        session.retained_bytes(),
        retained_recorded_bytes(&session),
        "the retained counter is the sum of what the session holds"
    );
    assert!(
        session.retained_bytes() > retained_before,
        "the accepted part of the answer is retained"
    );
    assert_eq!(
        session.terminal(),
        None,
        "a refused expansion does not end the run: the session is not completed, \
         it is stuck at the node whose text it could not speak"
    );
    Ok(())
}

#[test]
fn an_expansion_exactly_at_the_ceiling_succeeds_and_one_byte_over_is_refused() -> TestResult {
    // The boundary, from both sides. Sixteen repetitions of a sixty-four-byte
    // value is exactly the ceiling; seventeen is one byte past it.
    let value = "z".repeat(64);
    let repeats = 16usize;
    let ceiling = 1_024usize;
    assert_eq!(
        repeats.checked_mul(value.len()),
        Some(ceiling),
        "the fixture fills the ceiling exactly"
    );
    let limits = ResourceLimits::shipped().tighten(ResourceAxis::Record, ceiling);
    let fits = speak_back_flow(&"${answer}".repeat(repeats), std::slice::from_ref(&value))?;
    let mut session = Session::with_limits("fits", fits, limits)?;
    session.answer(&value)?;
    let spoken = session
        .transcript()
        .iter()
        .find(|entry| entry.path_node() == "say")
        .ok_or("the say node recorded the text it spoke")?;
    assert_eq!(
        spoken.text().len(),
        ceiling,
        "the record fills the ceiling exactly and is accepted"
    );
    assert_eq!(
        session.terminal(),
        Some(&Terminal::Completed),
        "a record of exactly the ceiling is spoken and the flow completes"
    );

    let over = repeats
        .checked_add(1)
        .ok_or("the fixture's repeat count overflows")?;
    let refused_bytes = over
        .checked_mul(value.len())
        .ok_or("the fixture's expansion overflows usize")?;
    let (journal, records) = SharedJournal::new();
    let mut session = Session::with_components_and_limits(
        "over",
        speak_back_flow(&"${answer}".repeat(over), std::slice::from_ref(&value))?,
        LanguageResolver::new(),
        journal,
        limits,
    )?;
    match session.answer(&value) {
        Err(BotError::TemplateExpansionTooLarge { node, bytes, limit }) => {
            assert_eq!(node, "say", "the refusal names the template's node");
            assert_eq!(bytes, refused_bytes, "one byte over the ceiling");
            assert_eq!(limit, ceiling, "the refusal reports the ceiling in force");
        }
        other => {
            return Err(format!("expected the expansion refusal, got {other:?}").into());
        }
    }
    assert!(
        records.borrow().iter().all(|record| record.0 != "say"),
        "the refused record was never handed to the journal"
    );
    assert!(
        session
            .transcript()
            .iter()
            .all(|entry| entry.path_node() != "say"),
        "and it is not in the transcript either"
    );
    assert_eq!(
        session.retained_bytes(),
        retained_recorded_bytes(&session),
        "the refused expansion retained nothing the session does not hold"
    );
    Ok(())
}

#[test]
fn an_utterance_at_the_ceiling_is_accepted_and_one_byte_over_is_refused_before_anything_is_written()
-> TestResult {
    let ceiling = 8usize;
    let limits = ResourceLimits::shipped().tighten(ResourceAxis::Utterance, ceiling);
    let option = String::from("x");
    let at_ceiling = "12345678";
    assert_eq!(
        at_ceiling.len(),
        ceiling,
        "the fixture is exactly the ceiling"
    );

    let (journal, records) = SharedJournal::new();
    let mut session = Session::with_components_and_limits(
        "at",
        speak_back_flow("${answer}", std::slice::from_ref(&option))?,
        LanguageResolver::new(),
        journal,
        limits,
    )?;
    let written_before = records.borrow().len();
    // Not the option, so the ask re-asks: the point is that the size ceiling
    // did not refuse it.
    session.answer(at_ceiling)?;
    assert!(
        records.borrow().len() > written_before,
        "an utterance of exactly the ceiling reaches the conversation"
    );

    let (journal, records) = SharedJournal::new();
    let mut session = Session::with_components_and_limits(
        "over",
        speak_back_flow("${answer}", std::slice::from_ref(&option))?,
        LanguageResolver::new(),
        journal,
        limits,
    )?;
    let written_before = records.borrow().len();
    let steps_before = session.steps();
    let retained_before = session.retained_bytes();
    let one_over = "123456789";
    match session.answer(one_over) {
        Err(BotError::UtteranceTooLarge { bytes, limit }) => {
            assert_eq!(
                bytes,
                one_over.len(),
                "the refusal counts the bytes offered"
            );
            assert_eq!(limit, ceiling, "the refusal reports the ceiling in force");
        }
        other => {
            return Err(format!("expected the utterance refusal, got {other:?}").into());
        }
    }
    assert_eq!(
        records.borrow().len(),
        written_before,
        "the refused utterance wrote no record"
    );
    assert_eq!(
        session.steps(),
        steps_before,
        "and charged no step: the ingress ceiling is checked before the step is charged"
    );
    assert_eq!(
        session.retained_bytes(),
        retained_before,
        "and retained nothing, so it is not in the transcript"
    );
    Ok(())
}

#[test]
fn an_ask_candidate_too_wide_for_the_session_is_refused_at_load() -> TestResult {
    // The load-time half, and the reason it exists: the candidate list and the
    // applied ceiling are both known while the document loads, so an ask whose
    // answer could never be stored is refused there rather than after the
    // person has typed it.
    let option = "x".repeat(MAX_VALUE_BYTES);
    let at_ceiling = single_ask_flow(VarType::String, vec![option.clone()])?;
    Session::with_limits("at", at_ceiling, ResourceLimits::shipped())?;

    // One byte over, and it is the document that refuses it: the flow never
    // becomes a session, so the candidate is refused before anyone is asked it.
    let one_over = "y".repeat(MAX_VALUE_BYTES.saturating_add(1));
    let refusal = single_ask_flow(VarType::String, vec![one_over.clone()])
        .err()
        .ok_or("a document offering a candidate wider than the shipped ceiling loaded")?;
    match refusal {
        BotError::AskOptionTooLarge {
            node,
            variable,
            option: offered,
            bytes,
            limit,
        } => {
            assert_eq!(node, "ask", "the refusal names the ask node");
            assert_eq!(
                variable, "answer",
                "the refusal names the variable the ask writes"
            );
            assert_eq!(offered, one_over, "the refusal names the candidate");
            assert_eq!(bytes, one_over.len(), "one byte over the shipped ceiling");
            assert_eq!(limit, MAX_VALUE_BYTES, "the shipped value ceiling");
        }
        other => {
            return Err(format!("expected the candidate refusal, got {other:?}").into());
        }
    }

    // A tighter session ceiling refuses a candidate the shipped one accepts.
    // The document is well-formed; it is the session that cannot run it, and
    // that is why the check runs a second time inside the constructor rather
    // than only where the document was parsed.
    let narrow = single_ask_flow(VarType::String, vec![option.clone()])?;
    let tightened = ResourceLimits::shipped().tighten(ResourceAxis::Value, 32);
    let refusal = Session::with_limits("narrow", narrow, tightened)
        .err()
        .ok_or("a candidate the session cannot store was accepted")?;
    match refusal {
        BotError::AskOptionTooLarge { bytes, limit, .. } => {
            assert_eq!(bytes, option.len(), "the candidate's stored width");
            assert_eq!(limit, 32, "the ceiling the session applies");
        }
        other => {
            return Err(format!("expected the candidate refusal, got {other:?}").into());
        }
    }
    Ok(())
}

#[test]
fn a_template_whose_literals_alone_exceed_the_ceiling_is_refused_at_load() -> TestResult {
    // The one size claim that is true whatever the variables turn out to hold:
    // every render contains the literal bytes, so a template whose literals are
    // over the ceiling can never render. It is refused where it is authored
    // rather than at the node that would speak it.
    let flow = speak_back_flow(&"L".repeat(100), &[String::from("x")])?;
    // Under the shipped ceiling the document is fine — the flow is not
    // malformed, it just cannot be spoken under this session's bound.
    let strict = ResourceLimits::shipped().tighten(ResourceAxis::Record, 64);
    let refusal = Session::with_limits("strict", flow.clone(), strict)
        .err()
        .ok_or("a template whose literals are over the ceiling was accepted")?;
    match refusal {
        BotError::TemplateExpansionTooLarge { node, bytes, limit } => {
            assert_eq!(node, "say", "the refusal names the template's node");
            assert_eq!(
                bytes, 100,
                "the literal floor, which no render can go below"
            );
            assert_eq!(limit, 64, "the ceiling the session applies");
        }
        other => {
            return Err(format!("expected the literal refusal, got {other:?}").into());
        }
    }
    Session::with_limits("shipped", flow, ResourceLimits::shipped())?;
    Ok(())
}

#[test]
fn a_document_may_tighten_its_own_ceilings_and_may_not_raise_them() -> TestResult {
    // The public path: JSON document, validated flow, session.
    let modest = speak_back_document(
        "${answer}",
        &[String::from("yes")],
        "{\"budget\": 16, \"resources\": {\"record_bytes\": 1024}}",
    );
    let flow = FlowSpec::from_json(&modest)?;
    assert_eq!(
        flow.bounds()
            .resources()
            .map(|limits| limits.get(ResourceAxis::Record)),
        Some(1_024),
        "the document's requested ceiling survives parsing"
    );
    let session = Session::new("modest", flow)?;
    assert_eq!(
        session.limits().get(ResourceAxis::Record),
        1_024,
        "the session enforces the narrower of the document's request and the operator's ceiling"
    );
    assert_eq!(
        session.limits().get(ResourceAxis::Utterance),
        MAX_UTTERANCE_BYTES,
        "an axis the document did not name keeps the operator's ceiling"
    );

    let greedy = speak_back_document(
        "${answer}",
        &[String::from("yes")],
        "{\"budget\": 16, \"resources\": {\"session_bytes\": 4294967296}}",
    );
    match FlowSpec::from_json(&greedy) {
        Err(BotError::ResourceLimitAboveCeiling {
            axis,
            requested,
            ceiling,
        }) => {
            assert_eq!(axis, ResourceAxis::Session, "the refusal names the axis");
            assert_eq!(
                requested, 4_294_967_296,
                "the ceiling the document asked for"
            );
            assert_eq!(
                ceiling, MAX_SESSION_BYTES,
                "the operator's hard ceiling, unchanged by the request"
            );
        }
        other => {
            return Err(format!("expected the ceiling refusal at load, got {other:?}").into());
        }
    }
    Ok(())
}

#[test]
fn a_configured_ceiling_above_the_operators_is_refused_rather_than_clamped() -> TestResult {
    // A bound that is silently ignored is a bound nobody can reason about, and
    // an operator that asked for no ceiling must not run believing it has one.
    let flow = speak_back_flow("${answer}", &[String::from("x")])?;
    let unbounded = ResourceLimits::shipped().tighten(ResourceAxis::Session, usize::MAX);
    let refusal = Session::with_limits("unbounded", flow, unbounded)
        .err()
        .ok_or("a ceiling above the operator's was accepted")?;
    match refusal {
        BotError::ResourceLimitAboveCeiling {
            axis,
            requested,
            ceiling,
        } => {
            assert_eq!(axis, ResourceAxis::Session, "the refusal names the axis");
            assert_eq!(requested, usize::MAX, "the ceiling that was asked for");
            assert_eq!(ceiling, MAX_SESSION_BYTES, "the operator's hard ceiling");
        }
        other => {
            return Err(format!("expected the ceiling refusal, got {other:?}").into());
        }
    }
    // And an operator ceiling below the document's request wins, per axis.
    let document = speak_back_document(
        "${answer}",
        &[String::from("x")],
        "{\"budget\": 16, \"resources\": {\"record_bytes\": 1024}}",
    );
    let limits = ResourceLimits::shipped().tighten(ResourceAxis::Record, 64);
    let session = Session::with_limits("narrow", FlowSpec::from_json(&document)?, limits)?;
    assert_eq!(
        session.limits().get(ResourceAxis::Record),
        64,
        "the operator's smaller ceiling is the one in force"
    );
    Ok(())
}

#[test]
fn cumulative_records_are_charged_against_the_session_ceiling() -> TestResult {
    // Every turn of this loop is a handful of individually acceptable records.
    // The per-record ceiling never refuses one; the aggregate is what bounds
    // the conversation.
    let (journal, records) = SharedJournal::new();
    let mut session = Session::with_components_and_limits(
        "generous",
        loop_flow()?,
        LanguageResolver::new(),
        journal,
        ResourceLimits::shipped(),
    )?;
    session.answer("again")?;
    session.answer("again")?;
    session.answer("stop")?;
    let total = session.retained_bytes();
    let full = records.borrow().clone();
    assert_eq!(
        total,
        retained_recorded_bytes(&session),
        "the retained counter is the sum of the records and the path"
    );
    assert!(
        session
            .transcript()
            .iter()
            .filter(|entry| entry.role() == "assistant")
            .count()
            > 1,
        "the loop recorded repeated prompts, which is what accumulates"
    );

    // A ceiling of exactly that total admits the whole script: the boundary
    // from the accepting side.
    let exact = ResourceLimits::shipped().tighten(ResourceAxis::Session, total);
    let (journal, _) = SharedJournal::new();
    let mut session = Session::with_components_and_limits(
        "exact",
        loop_flow()?,
        LanguageResolver::new(),
        journal,
        exact,
    )?;
    session.answer("again")?;
    session.answer("again")?;
    session.answer("stop")?;
    assert_eq!(
        session.retained_bytes(),
        total,
        "the script that fits exactly is run to completion"
    );
    assert_eq!(
        session.terminal(),
        Some(&Terminal::Completed),
        "and reaches its terminal"
    );

    // Half of it does not, and what was written before the refusal is a prefix
    // of what the full run wrote: no partial record, no hole.
    let half = total
        .checked_div(2)
        .ok_or("half of a measured positive total is never zero")?;
    assert!(
        half > 0 && half < total,
        "the fixture's half-ceiling ({half}) is a genuine tightening of {total}"
    );
    let (journal, records) = SharedJournal::new();
    let mut session = match Session::with_components_and_limits(
        "half",
        loop_flow()?,
        LanguageResolver::new(),
        journal,
        ResourceLimits::shipped().tighten(ResourceAxis::Session, half),
    ) {
        Ok(session) => session,
        // Construction records the opening prompt, so a half-ceiling below that
        // one record would fail before the loop this test is about. That is a
        // defect in the fixture's arithmetic, and it says so rather than
        // reporting a refusal that the bound did not cause.
        Err(BotError::SessionRetentionExceeded { .. }) => {
            return Err(
                "the fixture's first record does not fit half of the run's own total; the \
                 fixture is wrong, not the bound"
                    .into(),
            );
        }
        Err(other) => return Err(format!("expected a session, got {other:?}").into()),
    };
    let mut refusal = None;
    for _attempt in 0..8 {
        if let Err(error) = session.answer("again") {
            refusal = Some(error);
            break;
        }
    }
    let Some(BotError::SessionRetentionExceeded { bytes, limit }) = refusal else {
        return Err(format!("expected the retention refusal, got {refusal:?}").into());
    };
    assert_eq!(limit, half, "the refusal reports the ceiling in force");
    assert!(
        bytes > limit,
        "the refusal reports the total the write would have reached ({bytes}), which is past \
         the {limit}-byte ceiling it was checked against"
    );
    assert!(
        session.retained_bytes() <= half,
        "nothing past the ceiling was retained: {} stayed within {half}",
        session.retained_bytes()
    );
    let written = records.borrow();
    assert!(
        written.len() < full.len(),
        "the run that hit the ceiling wrote fewer records ({}) than the run that did not ({})",
        written.len(),
        full.len()
    );
    assert_eq!(
        written.as_slice(),
        &full[..written.len()],
        "and what it wrote is a prefix of the full run, with no partial record at the end"
    );
    drop(written);
    assert_eq!(
        session.retained_bytes(),
        retained_recorded_bytes(&session),
        "the counter and the retained records agree after the refusal"
    );
    Ok(())
}

#[test]
fn a_session_that_cannot_retain_its_first_record_writes_nothing() -> TestResult {
    // The construction path, where the aggregate ceiling is hit before any
    // conversation exists. The refusal is observable through the journal rather
    // than only asserted, because the error path returns no session to inspect.
    let (journal, records) = SharedJournal::new();
    let result = Session::with_components_and_limits(
        "tiny",
        loop_flow()?,
        LanguageResolver::new(),
        journal,
        ResourceLimits::shipped().tighten(ResourceAxis::Session, 8),
    );
    match result {
        Err(BotError::SessionRetentionExceeded { bytes, limit }) => {
            assert!(
                bytes > limit,
                "the refusal reports a total past a ceiling of {limit}"
            );
        }
        Ok(_session) => {
            return Err("expected the retention refusal, got a session".into());
        }
        Err(other) => {
            return Err(format!("expected the retention refusal, got {other:?}").into());
        }
    }
    assert!(
        records.borrow().is_empty(),
        "a session refused during construction wrote no record at all"
    );
    Ok(())
}
