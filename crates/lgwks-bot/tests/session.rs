//! Public acceptance tests for the synchronous flow/session surface.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use lgwks_bot::{
    Alias, AnswerRejection, BotError, DecisionReceipt, DegradedReason, Disposition, EffectLedger,
    Embedder, EmbedderIdentity, FlowBounds, FlowEdge, FlowSpec, Journal, JournalError,
    LanguageResolver, MAX_RECORD_BYTES, MAX_SESSION_BYTES, MAX_UTTERANCE_BYTES, MAX_VALUE_BYTES,
    MatchTier, MemoryJournal, NodeKind, PolicyVersion, Predicate, Provenance, Question,
    RECEIPT_VERSION, ReceiptAcceptance, Resolution, Resolver, ResourceAxis, ResourceLimits,
    SemanticPolicy, SemanticResolver, Session, Terminal, TranscriptEntry, Value, ValueExpr,
    VarType, Verdict,
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

/// The policy version a [`FixedResolver`] attributes its verdicts to.
///
/// Constant rather than freshly digested per resolver: these resolvers return a
/// scripted resolution, so what varies between them is the resolution and never
/// the provenance. A test that needs the provenance to vary constructs its own
/// [`Verdict`] instead of reaching through this helper.
fn fixed_policy() -> PolicyVersion {
    PolicyVersion::new("test-fixed", &[1.0])
}

/// A resolver that always reports the same verdict, so a session's behaviour
/// can be observed against each [`Resolution`] variant in isolation.
struct FixedResolver(Resolution);

impl Resolver for FixedResolver {
    fn resolve(&self, _utterance: &str, _question: &Question<'_>) -> Verdict {
        Verdict::new(self.0.clone(), Provenance::without_model(fixed_policy()))
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

// --- Decision receipts (#52) --------------------------------------------
//
// The defect these pin: a resolution carried no provenance, so two runs that
// reached the same verdict by different means — a lexicon versus a model, one
// weight set versus another, one threshold versus a retuned one — left records
// that were identical. The tier and the score were reported; *on whose
// authority* was not, and the session transcript cannot hold it, because a
// transcript is a rendering of what was said and not of what was decided.

/// A journal that keeps every receipt it is handed, and can be told to refuse.
///
/// The state is behind `Rc` because `Session` owns its journal for the session's
/// lifetime and offers no route back to it — which is right for a sink, and the
/// reason this double is shared with the test rather than read back out of the
/// session.
#[derive(Clone)]
struct TestJournal {
    /// Receipts this sink accepted, in the order it accepted them.
    accepted: Rc<RefCell<Vec<DecisionReceipt>>>,
    /// Receipts this sink refused, in the order it refused them.
    refused: Rc<RefCell<Vec<DecisionReceipt>>>,
    /// How many further receipts to refuse before accepting again.
    refusals: Rc<Cell<usize>>,
    /// What this sink reports when it accepts one.
    acceptance: ReceiptAcceptance,
}

impl TestJournal {
    /// A sink that accepts everything and reports `acceptance`.
    fn accepting(acceptance: ReceiptAcceptance) -> Self {
        Self {
            accepted: Rc::new(RefCell::new(Vec::new())),
            refused: Rc::new(RefCell::new(Vec::new())),
            refusals: Rc::new(Cell::new(0)),
            acceptance,
        }
    }

    /// A sink that refuses its next `refusals` receipts and then accepts.
    fn refusing(acceptance: ReceiptAcceptance, refusals: usize) -> Self {
        let sink = Self::accepting(acceptance);
        sink.refusals.set(refusals);
        sink
    }

    /// Returns the receipts this sink accepted.
    fn accepted(&self) -> Vec<DecisionReceipt> {
        self.accepted.borrow().clone()
    }

    /// Returns the receipts this sink refused.
    fn refused(&self) -> Vec<DecisionReceipt> {
        self.refused.borrow().clone()
    }
}

impl Journal for TestJournal {
    fn record(&mut self, _path_node: &str, _role: &str, _text: &str) {
        // This double exists for the receipt channel. Path records are the
        // transcript's copy of the same conversation, and every assertion about
        // them is made through `Session::transcript`.
    }

    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError> {
        if self.refusals.get() > 0 {
            self.refusals.set(self.refusals.get().saturating_sub(1));
            self.refused.borrow_mut().push(receipt.clone());
            return Err(JournalError::new("test-sink", "the sink refused the write"));
        }
        self.accepted.borrow_mut().push(receipt.clone());
        Ok(self.acceptance)
    }
}

/// The tier a resolution was reached at, or `None` when it did not resolve.
///
/// The wildcard arm is reachable by a variant this crate adds later, and reports
/// `None`: a tier this test does not know is not a tier it may claim.
fn tier_of(resolution: &Resolution) -> Option<MatchTier> {
    match *resolution {
        Resolution::Resolved { tier, .. } => Some(tier),
        _ => None,
    }
}

/// The identity of the model the semantic tests below drive.
fn test_identity(digest: &str) -> Result<EmbedderIdentity, Box<dyn std::error::Error>> {
    Ok(EmbedderIdentity::new("test-model", digest, 2)?)
}

/// Runs `order_flow` for one utterance through a semantic resolver and returns
/// the session plus the one receipt it recorded.
///
/// Uses the shipped policy; see [`receipt_for_policy`] to vary it.
fn receipt_for(
    utterance: &str,
    embedder: OrderEmbedder,
) -> Result<(Session, DecisionReceipt), Box<dyn std::error::Error>> {
    receipt_for_policy(utterance, embedder, SemanticPolicy::DEFAULT)
}

/// As [`receipt_for`], with the tier's acceptance policy chosen by the caller.
fn receipt_for_policy(
    utterance: &str,
    embedder: OrderEmbedder,
    policy: SemanticPolicy,
) -> Result<(Session, DecisionReceipt), Box<dyn std::error::Error>> {
    let journal = TestJournal::accepting(ReceiptAcceptance::Durable);
    let mut session = Session::with_components(
        "receipt",
        order_flow()?,
        SemanticResolver::with_policy(embedder, policy),
        journal,
    )?;
    session.answer(utterance)?;
    let receipt = session
        .decisions()
        .first()
        .map(|entry| entry.receipt().clone())
        .ok_or_else(|| String::from("an accepted answer recorded no decision receipt"))?;
    Ok((session, receipt))
}

/// One `Ask` node whose options are `order_flow`'s in the opposite order, so a
/// selected option occupies a different index.
fn reversed_order_flow() -> Result<FlowSpec, BotError> {
    let options = vec![String::from("Cancel"), String::from("Repeat last order")];
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

/// Routes every option to one target, which is all a routing test needs.
fn routes_to(options: &[String], target: &str) -> BTreeMap<String, String> {
    options
        .iter()
        .map(|option| (option.clone(), String::from(target)))
        .collect()
}

/// Two ask nodes with different vocabularies, the second reachable only by
/// answering the first.
///
/// The shape the multi-ask tests need: a phrase confirmed at `ask_one` has a
/// *different* candidate set at `ask_two`, and the person's answer at the first
/// question is what makes the second observable.
fn two_question_flow(first: &[&str], second: &[&str]) -> Result<FlowSpec, BotError> {
    let first_options: Vec<String> = first.iter().map(|name| (*name).to_owned()).collect();
    let second_options: Vec<String> = second.iter().map(|name| (*name).to_owned()).collect();
    FlowSpec::new(
        BTreeMap::from([
            (
                String::from("first"),
                VarType::Choice(first_options.clone()),
            ),
            (
                String::from("second"),
                VarType::Choice(second_options.clone()),
            ),
        ]),
        "ask_one",
        BTreeMap::from([
            (
                String::from("ask_one"),
                NodeKind::Ask {
                    var: String::from("first"),
                    options: first_options.clone(),
                    routes: routes_to(&first_options, "ask_two"),
                },
            ),
            (
                String::from("ask_two"),
                NodeKind::Ask {
                    var: String::from("second"),
                    options: second_options.clone(),
                    routes: routes_to(&second_options, "done"),
                },
            ),
            (String::from("done"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(8),
    )
}

#[test]
fn a_receipt_names_the_model_only_when_a_model_decided() -> TestResult {
    let (_, lexical) = receipt_for(
        "Cancel",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;
    let (_, semantic) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;

    // "Cancel" is one of the options verbatim, so the lexicon answered it and no
    // model was consulted. Naming a model here would attribute a reproducible
    // verdict to a model that never ran.
    assert_eq!(
        lexical.provenance().model(),
        None,
        "a lexicon verdict must not name a model"
    );
    assert_eq!(
        semantic.provenance().model().map(EmbedderIdentity::name),
        Some("test-model"),
        "a verdict only the model could reach must name the model"
    );
    assert_eq!(
        semantic.provenance().model().map(EmbedderIdentity::digest),
        Some("digest-a"),
        "a name alone cannot tell two weight sets apart"
    );
    assert_eq!(
        semantic
            .provenance()
            .model()
            .map(EmbedderIdentity::dimension),
        Some(2),
        "and the width is what makes two scores comparable at all"
    );

    // The two decisions differ in provenance and in nothing else the record
    // holds: one flow, one node, one candidate list, one session.
    assert_eq!(lexical.flow(), semantic.flow(), "one flow revision");
    assert_eq!(lexical.node(), semantic.node(), "one ask node");
    assert_eq!(lexical.session(), semantic.session(), "one session");
    assert_eq!(
        lexical.options(),
        semantic.options(),
        "one candidate list was offered to both"
    );
    assert_ne!(
        tier_of(lexical.resolution()),
        tier_of(semantic.resolution()),
        "one resolved at the lexical tier and the other at the semantic one"
    );
    Ok(())
}

/// Reads the single transcript record written under `role`, or an empty string.
fn recorded_under(session: &Session, role: &str) -> String {
    session
        .transcript()
        .iter()
        .find(|entry| entry.role() == role)
        .map(TranscriptEntry::text)
        .unwrap_or_default()
        .to_owned()
}

/// The filed counterexample for #25, at session level: one resolver, two
/// questions. Before the fix, the same phrase selected index 0 at *both*, which
/// at the second question meant `Delete account` — an action nobody confirmed.
#[test]
fn an_alias_confirmed_at_one_ask_cannot_answer_another() -> TestResult {
    let resolver = LanguageResolver::with_aliases(vec![Alias::new(
        "ask_one",
        "the usual",
        "Repeat last order",
    )]);
    let mut session = Session::with_resolver(
        "scope",
        two_question_flow(
            &["Repeat last order", "Cancel"],
            &["Delete account", "Keep account"],
        )?,
        resolver,
    )?;

    session.answer("the usual")?;
    assert_eq!(
        session.scope().get("first"),
        Some(&Value::Choice(String::from("Repeat last order"))),
        "the confirmation is honoured at the question it was made in"
    );
    assert_eq!(session.current(), Some("ask_two"));

    session.answer("the usual")?;
    assert_eq!(
        session.current(),
        Some("ask_two"),
        "the second question must not consume the first question's confirmation"
    );
    assert_eq!(
        session.scope().get("second"),
        None,
        "and must certainly not select the option sitting at its old index"
    );
    Ok(())
}

#[test]
fn the_policy_that_decided_is_named_on_the_verdict_it_produced() -> TestResult {
    let (_, lexical) = receipt_for(
        "Cancel",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;
    let (_, semantic) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;

    // Every verdict names a policy, including the one whose tier consulted no
    // model: "the lexicon decided this" is itself a claim about a rule, and a
    // receipt that named no rule could not be read against a later release.
    assert_eq!(
        lexical.provenance().policy().label(),
        "lexicon",
        "the tier that decided is the tier whose policy is named"
    );
    assert_eq!(
        semantic.provenance().policy().label(),
        "semantic",
        "the semantic tier's own parameters are the ones in force"
    );
    assert_ne!(
        lexical.provenance().policy(),
        semantic.provenance().policy(),
        "two independently tuned tiers are two policies"
    );
    assert!(
        !lexical.provenance().policy().revision().is_empty(),
        "a policy version with no revision is not a version"
    );
    Ok(())
}

#[test]
fn two_models_that_score_identically_are_distinguishable_in_the_record() -> TestResult {
    let (first_session, first) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;
    let (second_session, second) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-b")?,
            measurability: Measurability::Complete,
        },
    )?;

    // The two runs are indistinguishable everywhere a two-valued record looked:
    // same conversation, same verdict, same chosen text, same route.
    assert_eq!(
        first_session.transcript(),
        second_session.transcript(),
        "the same words produced the same transcript"
    );
    assert_eq!(first.resolution(), second.resolution(), "the same geometry");
    assert_eq!(first.selected(), second.selected(), "the same option text");
    assert_eq!(first.route(), second.route(), "the same transition");

    // And they are different runs, which only the model's own identity can say.
    assert_eq!(
        first.provenance().model().map(EmbedderIdentity::name),
        second.provenance().model().map(EmbedderIdentity::name),
        "both runs name the same model"
    );
    assert_ne!(
        first.provenance().model().map(EmbedderIdentity::digest),
        second.provenance().model().map(EmbedderIdentity::digest),
        "same name, different weights, different runs"
    );
    Ok(())
}

#[test]
fn a_retuned_policy_is_visible_when_the_resolution_is_unchanged() -> TestResult {
    let (_, shipped) = receipt_for_policy(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
        SemanticPolicy::DEFAULT,
    )?;
    let (_, retuned) = receipt_for_policy(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
        SemanticPolicy::new(0.70, 0.05)?,
    )?;

    assert_eq!(
        shipped.resolution(),
        retuned.resolution(),
        "the same answer was reached under both thresholds"
    );
    assert_eq!(
        shipped.provenance().model().map(EmbedderIdentity::digest),
        retuned.provenance().model().map(EmbedderIdentity::digest),
        "and against the same weights"
    );
    assert_ne!(
        shipped.provenance().policy().revision(),
        retuned.provenance().policy().revision(),
        "a different threshold is a different policy, and the revision says so"
    );
    Ok(())
}

#[test]
fn reordering_the_options_changes_the_identity_and_not_the_choice() -> TestResult {
    let (_, ordered) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;

    let journal = TestJournal::accepting(ReceiptAcceptance::Durable);
    let mut reversed = Session::with_components(
        "reversed",
        reversed_order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        }),
        journal,
    )?;
    reversed.answer("the usual")?;
    let reversed_receipt = reversed
        .decisions()
        .first()
        .map(|entry| entry.receipt().clone())
        .ok_or_else(|| String::from("the reversed run recorded no receipt"))?;

    // The same text was chosen and the same node was reached, so a record that
    // kept only the chosen text would call these two runs identical.
    assert_eq!(
        ordered.selected(),
        reversed_receipt.selected(),
        "the same option text was chosen from both lists"
    );
    assert_eq!(
        ordered.route(),
        reversed_receipt.route(),
        "and both route to the same node"
    );

    // They are not the same question. The option-set identity digests the
    // ordered list, and the verdict's index is a position within it.
    assert_ne!(
        ordered.options(),
        reversed_receipt.options(),
        "a reordering is a different candidate list"
    );
    assert_ne!(
        ordered.resolution(),
        reversed_receipt.resolution(),
        "an index is a position in a list, and the position moved"
    );
    Ok(())
}

#[test]
fn a_receipt_exists_for_an_answer_that_did_not_resolve() -> TestResult {
    let journal = TestJournal::accepting(ReceiptAcceptance::InMemory);
    let mut session = Session::with_components(
        "unresolved",
        one_question_flow()?,
        FixedResolver(Resolution::Ambiguous {
            tied: vec![0, 1],
            tier: MatchTier::Fuzzy,
            score: 0.6,
        }),
        journal.clone(),
    )?;
    session.answer("maybe")?;

    assert_eq!(
        session.current(),
        Some("ask"),
        "an ambiguous answer re-asks"
    );
    assert_eq!(
        session.decisions().len(),
        1,
        "a question that was re-asked is still a decision, so it is still recorded"
    );
    let recorded = session
        .decisions()
        .first()
        .map(|entry| entry.receipt().clone())
        .ok_or_else(|| String::from("the re-ask recorded no receipt"))?;

    assert_eq!(recorded.selected(), None, "nothing was selected");
    assert_eq!(recorded.route(), None, "and nothing was routed");
    assert_eq!(
        recorded.resolution(),
        &Resolution::Ambiguous {
            tied: vec![0, 1],
            tier: MatchTier::Fuzzy,
            score: 0.6,
        },
        "the verdict is carried verbatim, including the tie"
    );
    assert_eq!(
        journal.accepted().len(),
        1,
        "the sink holds the record of the question that had to be asked again"
    );
    Ok(())
}

/// The filed counterexample for #25, second half: the bound option is removed
/// and its successor is the dangerous one. Teaching row 0 and then replacing row
/// 0's text must not transfer the confirmation to whatever moved in.
#[test]
fn a_confirmation_whose_option_was_removed_reasks_instead_of_selecting_its_successor() -> TestResult
{
    let resolver = LanguageResolver::with_aliases(vec![Alias::new(
        "ask_one",
        "the usual",
        "Repeat last order",
    )]);
    // Version two of the same question: `Repeat last order` is gone and
    // `Delete account` now sits where it was.
    let mut session = Session::with_resolver(
        "stale",
        two_question_flow(&["Delete account", "Cancel"], &["Yes", "No"])?,
        resolver,
    )?;

    session.answer("the usual")?;

    assert_eq!(
        session.current(),
        Some("ask_one"),
        "a withdrawn confirmation re-asks rather than advancing"
    );
    assert_eq!(
        session.scope().get("first"),
        None,
        "no value is stored for an answer the resolver refused to read"
    );
    assert_eq!(
        recorded_under(&session, "resolver-stale-alias"),
        "Superseded alias: question ask_one no longer offers \"Repeat last order\"",
        "the record names both halves of the broken binding, because either \
         alone is unactionable"
    );

    // The person can still answer: the withdrawal is not a trap, it is a
    // withdrawal. Typing without the withdrawn phrase resolves normally.
    session.answer("Cancel")?;
    assert_eq!(
        session.scope().get("first"),
        Some(&Value::Choice(String::from("Cancel")))
    );
    Ok(())
}

/// An ask/route flow whose every option routes to a `Refer` terminal named
/// after the option itself.
///
/// The option text is the route identity on purpose: "the session went where
/// the value says" then reads as one equality against the option, and a test
/// never has to know a node id to assert a route.
fn option_routed_flow(
    var: &str,
    declared: VarType,
    options: &[&str],
) -> Result<FlowSpec, BotError> {
    let offered: Vec<String> = options.iter().map(|option| (*option).to_owned()).collect();
    let mut routes = BTreeMap::new();
    let mut nodes = BTreeMap::new();
    let mut terminals = BTreeMap::new();
    for (position, option) in offered.iter().enumerate() {
        let target = format!("routed_{position}");
        routes.insert(option.clone(), target.clone());
        nodes.insert(
            target.clone(),
            NodeKind::Refer {
                target: option.clone(),
                text: String::from("Noted"),
            },
        );
        terminals.insert(
            target,
            Terminal::Referred {
                target: option.clone(),
            },
        );
    }
    nodes.insert(
        String::from("ask"),
        NodeKind::Ask {
            var: String::from(var),
            options: offered,
            routes,
        },
    );
    FlowSpec::new(
        BTreeMap::from([(String::from(var), declared)]),
        "ask",
        nodes,
        Vec::new(),
        terminals,
        FlowBounds::new(16),
    )
}

/// Reads the route a session took as the option text it named.
fn routed_to(session: &Session) -> Option<String> {
    session.terminal().and_then(|terminal| match *terminal {
        Terminal::Referred { ref target } => Some(target.clone()),
        _ => None,
    })
}

/// The filed counterexample for #38, at session level: `-5` is not one of the
/// two numbers this question offers.
///
/// Before the fix, normalization folded the sign away, `-5` matched `"5"` at
/// the Exact tier, and the session stored `Value::Integer(5)` and advanced down
/// the positive route — an answer the person never gave, reported at maximum
/// confidence.
#[test]
fn a_negative_answer_is_not_folded_into_the_positive_option() -> TestResult {
    let mut session = Session::new(
        "sign",
        option_routed_flow("amount", VarType::Integer, &["5", "10"])?,
    )?;

    session.answer("-5")?;

    assert_eq!(
        session.scope().get("amount"),
        None,
        "a number the question does not offer must not be stored as one it does"
    );
    assert_eq!(
        session.current(),
        Some("ask"),
        "the question is re-asked rather than answered by a different number"
    );
    assert_eq!(
        session.terminal(),
        None,
        "and no route is taken, so nothing downstream acts on the invented value"
    );
    Ok(())
}

/// The value, not the spelling, decides: every reading of one number lands on
/// that number's option, and both signed choices stay distinguishable.
#[test]
fn an_integer_answer_stores_its_value_and_takes_its_own_route() -> TestResult {
    for (utterance, expected_value, expected_option) in [
        ("-5", -5_i64, "-5"),
        ("5", 5, "5"),
        ("+5", 5, "5"),
        (" 5 ", 5, "5"),
        ("0", 0, "0"),
        ("10", 10, "10"),
    ] {
        let mut session = Session::new(
            "values",
            option_routed_flow("amount", VarType::Integer, &["-5", "5", "0", "10"])?,
        )?;
        session.answer(utterance)?;
        assert_eq!(
            session.scope().get("amount"),
            Some(&Value::Integer(expected_value)),
            "{utterance:?} must store the parsed value, not the option's spelling"
        );
        assert_eq!(
            routed_to(&session).as_deref(),
            Some(expected_option),
            "{utterance:?} must take the route of the option holding that value"
        );
    }
    Ok(())
}

/// The ends of the range are values like any other, and one past them is not a
/// value at all.
#[test]
fn the_integer_boundaries_round_trip_and_overflow_is_refused() -> TestResult {
    let low = i64::MIN.to_string();
    let high = i64::MAX.to_string();
    for (utterance, expected) in [(low.as_str(), i64::MIN), (high.as_str(), i64::MAX)] {
        let mut session = Session::new(
            "bounds",
            option_routed_flow("amount", VarType::Integer, &[low.as_str(), high.as_str()])?,
        )?;
        session.answer(utterance)?;
        assert_eq!(
            session.scope().get("amount"),
            Some(&Value::Integer(expected)),
            "{utterance:?} is representable and must be stored exactly"
        );
        assert_eq!(
            routed_to(&session).as_deref(),
            Some(utterance),
            "{utterance:?} must take the route of the option holding that value"
        );
    }

    // One past each end parses as nothing, so it matches nothing: the failure
    // is a re-ask, never a wrapped or clamped value.
    for utterance in ["9223372036854775808", "-9223372036854775809"] {
        let mut session = Session::new(
            "overflow",
            option_routed_flow("amount", VarType::Integer, &["5", "10"])?,
        )?;
        session.answer(utterance)?;
        assert_eq!(
            session.scope().get("amount"),
            None,
            "{utterance:?} is out of range and must not be stored"
        );
        assert_eq!(
            session.current(),
            Some("ask"),
            "{utterance:?} must re-ask rather than wrap into a nearby value"
        );
    }
    Ok(())
}

/// The control for #38: folding punctuation away is still what a *label*
/// question wants, and the fix must not have withdrawn it.
#[test]
fn punctuation_tolerant_matching_still_answers_a_label_question() -> TestResult {
    let mut session = Session::new(
        "labels",
        option_routed_flow("answer", VarType::Boolean, &["yes", "no"])?,
    )?;

    session.answer("yes!")?;

    assert_eq!(
        session.scope().get("answer"),
        Some(&Value::Boolean(true)),
        "an ordinary word answer keeps the tolerant reading it has always had"
    );
    assert_eq!(
        routed_to(&session).as_deref(),
        Some("yes"),
        "and still takes the route of the option it matched"
    );
    Ok(())
}

/// The policy is what decides, so the same authored strings are one answer or
/// two depending on the declared type: `-5` and `5` are two values and one
/// label, and `5` and `05` are two spellings of one value.
#[test]
fn option_collisions_are_judged_under_the_declared_policy() -> TestResult {
    let as_values = option_routed_flow("amount", VarType::Integer, &["-5", "5"])?;
    assert_eq!(
        as_values.nodes().len(),
        3,
        "a signed question offering both signs is accepted: two values"
    );

    let as_labels = option_routed_flow("answer", VarType::String, &["-5", "5"]);
    assert!(
        matches!(as_labels, Err(BotError::MalformedFlow { .. })),
        "the same two strings are one label once the sign folds, and a flow that \
         offers them that way can never tell the person's answer apart: {as_labels:?}"
    );

    let duplicate_values = option_routed_flow("amount", VarType::Integer, &["5", "05"]);
    assert!(
        matches!(duplicate_values, Err(BotError::MalformedFlow { .. })),
        "two options holding one value are the numeric form of the same \
         collision: {duplicate_values:?}"
    );
    Ok(())
}

#[test]
fn a_refused_receipt_is_not_an_accepted_answer() -> TestResult {
    let journal = TestJournal::refusing(ReceiptAcceptance::Durable, 1);
    let mut session = Session::with_components(
        "refused",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        }),
        journal.clone(),
    )?;

    let refusal = session.answer("the usual");
    let rendered = format!("{refusal:?}");
    let Err(BotError::ReceiptNotRecorded { node, cause }) = refusal else {
        return Err(format!("expected a receipt refusal, got {rendered}").into());
    };
    assert_eq!(
        node, "ask",
        "the refusal names the node whose decision it was"
    );
    assert_eq!(
        cause.sink(),
        "test-sink",
        "the refusal names the sink that refused"
    );
    assert_eq!(
        cause.cause(),
        "the sink refused the write",
        "and carries the sink's own description of the failure"
    );

    // The answer was not taken. Every observable the session has says so.
    assert_eq!(session.current(), Some("ask"), "the cursor did not move");
    assert_eq!(session.terminal(), None, "the session did not complete");
    assert_eq!(
        session.scope().get("order"),
        None,
        "the variable was not written"
    );
    assert!(
        session.decisions().is_empty(),
        "the session does not hold a receipt the sink never took"
    );
    assert!(
        !session
            .transcript()
            .iter()
            .any(|entry| entry.text() == "the usual"),
        "an answer the session did not accept is not recorded as having been said"
    );
    assert_eq!(
        journal.refused().len(),
        1,
        "the sink did refuse one receipt"
    );
    assert!(journal.accepted().is_empty(), "and accepted none");

    // The refusal is not a dead end: the same answer is taken once the sink
    // takes it.
    session.answer("the usual")?;
    assert_eq!(
        session.terminal(),
        Some(&Terminal::Completed),
        "the session advanced once the receipt was taken"
    );
    assert_eq!(
        session.scope().get("order"),
        Some(&Value::Choice(String::from("Repeat last order")))
    );
    assert_eq!(session.decisions().len(), 1, "and holds the receipt");
    assert_eq!(journal.accepted().len(), 1, "which the sink took");
    Ok(())
}

#[test]
fn the_sink_says_whether_a_receipt_outlives_the_process() -> TestResult {
    // One session identity, one flow document, one utterance, two sinks. The
    // only thing that differs between the runs is what the sink does with the
    // receipt, so any difference in the record is the sink's doing.
    let mut in_memory = Session::with_resolver(
        "same",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        }),
    )?;
    in_memory.answer("the usual")?;

    let mut durable = Session::with_components(
        "same",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        }),
        TestJournal::accepting(ReceiptAcceptance::Durable),
    )?;
    durable.answer("the usual")?;

    let held = in_memory
        .decisions()
        .first()
        .ok_or_else(|| String::from("the in-memory run recorded no receipt"))?;
    let written = durable
        .decisions()
        .first()
        .ok_or_else(|| String::from("the durable run recorded no receipt"))?;

    assert_eq!(
        held.acceptance(),
        ReceiptAcceptance::InMemory,
        "a receipt held in the session's own journal is gone when the process is"
    );
    assert_eq!(
        written.acceptance(),
        ReceiptAcceptance::Durable,
        "and a sink that writes it through says so"
    );
    // The two sinks were handed the same record. Only the answer about
    // persistence differs, which is why that answer is stored beside the receipt
    // rather than inside it: one field of the receipt would then differ between
    // two sinks that were given identical bytes.
    assert_eq!(
        held.receipt(),
        written.receipt(),
        "both sinks were handed the same decision record"
    );
    Ok(())
}

#[test]
fn a_receipt_binds_itself_to_the_flow_revision_that_produced_it() -> TestResult {
    let (session, receipt) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;

    assert_eq!(
        receipt.flow(),
        session.flow_revision(),
        "the receipt names the digest of the flow document it came from"
    );

    // Two flows with different structure are two revisions, so a receipt cannot
    // be read against the wrong document.
    let other = Session::new("other", one_question_flow()?)?;
    assert_ne!(
        receipt.flow(),
        other.flow_revision(),
        "a different flow document is a different revision"
    );
    Ok(())
}

#[test]
fn a_receipt_is_self_describing_when_exported() -> TestResult {
    let (_, receipt) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
            measurability: Measurability::Complete,
        },
    )?;

    assert_eq!(
        receipt.version(),
        RECEIPT_VERSION,
        "the record declares the schema that wrote it"
    );

    let exported = lgwks_bot::json::to_string(&receipt)?;
    for field in [
        "version",
        "session",
        "flow",
        "node",
        "options",
        "resolution",
        "provenance",
        "selected",
        "route",
    ] {
        assert!(
            exported.contains(field),
            "the exported record names its {field} field: {exported}"
        );
    }

    let reimported: DecisionReceipt = lgwks_bot::json::from_str(&exported)?;
    assert_eq!(
        reimported, receipt,
        "a record that does not survive the trip out of its own process is not a record"
    );
    Ok(())
}

/// Runs `one_question_flow` for one scripted verdict and returns the session
/// plus the receipt it recorded.
fn receipt_for_verdict(
    resolution: Resolution,
) -> Result<(Session, DecisionReceipt), Box<dyn std::error::Error>> {
    let journal = TestJournal::accepting(ReceiptAcceptance::InMemory);
    let mut session = Session::with_components(
        "outcomes",
        one_question_flow()?,
        FixedResolver(resolution),
        journal,
    )?;
    session.answer("yes")?;
    let receipt = session
        .decisions()
        .first()
        .map(|entry| entry.receipt().clone())
        .ok_or_else(|| String::from("the decision recorded no receipt"))?;
    Ok((session, receipt))
}

#[test]
fn the_four_verdicts_are_four_distinguishable_receipts() -> TestResult {
    let (resolved_session, resolved) = receipt_for_verdict(Resolution::Resolved {
        index: 0,
        tier: MatchTier::Fuzzy,
        score: 0.81,
        lead: 0.22,
    })?;
    let (_, ambiguous) = receipt_for_verdict(Resolution::Ambiguous {
        tied: vec![0, 1],
        tier: MatchTier::Fuzzy,
        score: 0.6,
    })?;
    let (_, absent) = receipt_for_verdict(Resolution::Absent { best_score: 0.1 })?;
    let (_, degraded) = receipt_for_verdict(Resolution::Degraded {
        reason: DegradedReason::EmbedderUnavailable,
    })?;

    // The verdict is carried verbatim: the tier, the score and the lead survive,
    // not only the index the session needed.
    assert_eq!(
        resolved.resolution(),
        &Resolution::Resolved {
            index: 0,
            tier: MatchTier::Fuzzy,
            score: 0.81,
            lead: 0.22,
        },
        "a record holding the index alone cannot say what produced it"
    );
    assert_eq!(
        absent.resolution(),
        &Resolution::Absent { best_score: 0.1 },
        "the best score observed is kept"
    );
    assert_eq!(
        degraded.resolution(),
        &Resolution::Degraded {
            reason: DegradedReason::EmbedderUnavailable,
        },
        "and a degraded verdict carries no score at all"
    );

    // Four verdicts, four different records. "Nothing was close" and "nothing
    // was compared" are the two a two-valued verdict renders identically, and
    // they need different repairs.
    assert_ne!(
        absent.resolution(),
        degraded.resolution(),
        "incomplete matching and an unavailable dependency are different records"
    );
    assert_ne!(resolved.resolution(), ambiguous.resolution());
    assert_ne!(ambiguous.resolution(), absent.resolution());

    // Only the resolved verdict selected a route, and what the receipt says it
    // selected is what the session actually stored.
    assert_eq!(resolved.selected(), Some("yes"), "the chosen option's text");
    assert_eq!(resolved.route(), Some("done"), "the node it routed to");
    assert_eq!(
        resolved_session.scope().get("choice"),
        Some(&Value::Choice(String::from("yes"))),
        "the receipt's selection agrees with the value the session stored"
    );
    assert_eq!(
        resolved_session.terminal(),
        Some(&Terminal::Completed),
        "and with where the session ended up"
    );
    for (label, receipt) in [
        ("ambiguous", &ambiguous),
        ("absent", &absent),
        ("degraded", &degraded),
    ] {
        assert_eq!(
            receipt.selected(),
            None,
            "a {label} verdict selected nothing, and says so by absence"
        );
        assert_eq!(receipt.route(), None, "and routed nowhere");
    }
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
    //
    // The three integer candidates name three *different* values, and they have
    // to: two options naming one value are refused at load
    // (`option_collisions_are_judged_under_the_declared_policy`), because the
    // resolver can only ever report the tie and the person cannot answer the
    // question. So the zero-padded spelling here is a pad of no other candidate.
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
                String::from("8"),
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
            tier: MatchTier::Fuzzy,
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
    /// Where the receipts go.
    ///
    /// This double exists to observe the path records, and delegating the
    /// receipts to the crate's own in-memory sink rather than a second
    /// hand-rolled store is what keeps the two from drifting. Delegated rather
    /// than accepted-and-dropped because a sink that drops a receipt and
    /// reports success is the failure the [`Journal`] contract names in as many
    /// words.
    receipts: MemoryJournal,
}

impl SharedJournal {
    /// An empty journal and the handle that reads it.
    fn new() -> (Self, JournalRecords) {
        let records: JournalRecords = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                records: Rc::clone(&records),
                receipts: MemoryJournal::new(),
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

    /// Forwarded whole to [`MemoryJournal`], which answers `InMemory` and says
    /// why in its own implementation.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError> {
        self.receipts.record_decision(receipt)
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

// ── Definite assignment over the execution graph (issue #36) ────────────────

/// A journal that keeps every record it receives, so a test can assert what
/// the runner emitted. Shared rather than moved so the assertion can read the
/// records back after [`Session`] has taken ownership of its sink.
#[derive(Clone, Default)]
struct RecordingJournal {
    records: Arc<Mutex<Vec<String>>>,
    /// Where the receipts go, for the same reason [`SharedJournal`] delegates
    /// them: this double is about the records, and a receipt it accepted must
    /// actually be held somewhere rather than reported accepted and dropped.
    receipts: MemoryJournal,
}

impl RecordingJournal {
    fn new() -> Self {
        Self::default()
    }

    /// Every record received so far, oldest first.
    ///
    /// A poisoned lock yields the empty vector; the positive control in
    /// `validation_refuses_before_any_journal_record` fails if that happens,
    /// so an empty read cannot pass for the wrong reason.
    fn records(&self) -> Vec<String> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }
}

impl Journal for RecordingJournal {
    fn record(&mut self, path_node: &str, role: &str, text: &str) {
        if let Ok(mut records) = self.records.lock() {
            records.push(format!("{path_node}/{role}/{text}"));
        }
    }

    /// Forwarded whole to [`MemoryJournal`], for the reason its sibling above
    /// gives.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError> {
        self.receipts.record_decision(receipt)
    }
}

/// The literal accepted-but-failing document from issue #36.
///
/// Every node is reachable and `name` has a writer, so the global writer set is
/// satisfied; the only writer runs *after* the read, and the session starts
/// with an empty scope, so `Session::new` failed on the first step with
/// `VariableUnset` before the user could reach the ask.
const ENTRY_READ_BEFORE_WRITE: &str = r#"{
  "vars": {"name": {"kind": "string"}},
  "entry": "greet",
  "nodes": {
    "greet": {"kind": "say", "text": "Hello ${name}"},
    "ask": {
      "kind": "ask",
      "var": "name",
      "options": ["Ada"],
      "routes": {"Ada": "end"}
    },
    "end": {"kind": "end"}
  },
  "edges": [{"kind": "next", "from": "greet", "to": "ask"}],
  "bounds": {"budget": 8}
}"#;

/// Build a flow from a node map, an entry, and explicit continuations.
fn flow_with(
    vars: BTreeMap<String, VarType>,
    entry: &str,
    nodes: BTreeMap<String, NodeKind>,
    edges: Vec<FlowEdge>,
    terminals: BTreeMap<String, Terminal>,
) -> Result<FlowSpec, BotError> {
    FlowSpec::new(vars, entry, nodes, edges, terminals, FlowBounds::new(32))
}

/// An ask that routes every option to `target`.
fn ask_routing(var: &str, options: [&str; 2], target: &str) -> NodeKind {
    NodeKind::Ask {
        var: String::from(var),
        options: options.iter().map(|option| String::from(*option)).collect(),
        routes: options
            .iter()
            .map(|option| (String::from(*option), String::from(target)))
            .collect(),
    }
}

/// A predicate that can take both arms, so the analysis is forced to treat
/// both continuations as feasible.
fn both_arms() -> Predicate {
    Predicate::Const(true)
}

#[test]
fn validation_rejects_an_entry_read_before_any_writer() -> TestResult {
    let error = FlowSpec::from_json(ENTRY_READ_BEFORE_WRITE)
        .err()
        .ok_or("a read with no reaching assignment must be refused at load time")?;
    assert!(
        matches!(
            error,
            BotError::VariableReadBeforeInit { ref node, ref name }
                if node == "greet" && name == "name"
        ),
        "the diagnostic must name the reading node and the variable: {error}"
    );
    Ok(())
}

#[test]
fn validation_rejects_a_branch_path_that_bypasses_initialization() -> TestResult {
    // `answer` is assignable, and every node is reachable — the defect is that
    // the writer sits on one arm of the branch while the read sits after the
    // join, so the other arm reaches the read with an empty scope.
    let declarations = BTreeMap::from([(
        String::from("answer"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    )]);
    let nodes = BTreeMap::from([
        (
            String::from("start"),
            NodeKind::Branch {
                var: String::from("answer"),
                when: both_arms(),
                then: String::from("ask"),
                otherwise: String::from("read"),
            },
        ),
        (
            String::from("ask"),
            ask_routing("answer", ["yes", "no"], "read"),
        ),
        (
            String::from("read"),
            NodeKind::Say {
                text: String::from("You said ${answer}"),
            },
        ),
        (String::from("end"), NodeKind::End),
    ]);
    let error = flow_with(
        declarations,
        "start",
        nodes,
        vec![FlowEdge::next("read", "end")],
        BTreeMap::from([(String::from("end"), Terminal::Completed)]),
    )
    .err()
    .ok_or("a read reachable through an uninitialized arm must be refused")?;
    assert!(
        matches!(
            error,
            BotError::VariableReadBeforeInit { ref node, ref name }
                if node == "read" && name == "answer"
        ),
        "the diagnostic must name the reading node and the variable: {error}"
    );
    Ok(())
}

#[test]
fn validation_rejects_a_read_in_a_cycle_before_its_writer() -> TestResult {
    // The read is the entry and the writer is one edge further on, so the
    // cycle re-reads it every iteration; the fixed point must not let the
    // back edge supply the assignment to the node that precedes it.
    let declarations = BTreeMap::from([(
        String::from("answer"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    )]);
    let nodes = BTreeMap::from([
        (
            String::from("head"),
            NodeKind::Say {
                text: String::from("You said ${answer}"),
            },
        ),
        (
            String::from("ask"),
            ask_routing("answer", ["yes", "no"], "head"),
        ),
    ]);
    let error = flow_with(
        declarations,
        "head",
        nodes,
        vec![FlowEdge::next("head", "ask")],
        BTreeMap::new(),
    )
    .err()
    .ok_or("a read that only a back edge's writer reaches must be refused")?;
    assert!(
        matches!(
            error,
            BotError::VariableReadBeforeInit { ref node, ref name }
                if node == "head" && name == "answer"
        ),
        "the diagnostic must name the reading node and the variable: {error}"
    );
    Ok(())
}

#[test]
fn validation_accepts_an_ask_read_by_its_successor() -> TestResult {
    let declarations = BTreeMap::from([(
        String::from("answer"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    )]);
    let nodes = BTreeMap::from([
        (
            String::from("ask"),
            ask_routing("answer", ["yes", "no"], "read"),
        ),
        (
            String::from("read"),
            NodeKind::Say {
                text: String::from("You said ${answer}"),
            },
        ),
        (String::from("end"), NodeKind::End),
    ]);
    let flow = flow_with(
        declarations,
        "ask",
        nodes,
        vec![FlowEdge::next("read", "end")],
        BTreeMap::from([(String::from("end"), Terminal::Completed)]),
    )?;
    // The verdict is only half the claim: a session over the accepted flow
    // must actually run. It stops at the ask awaiting an answer, which is a
    // node it could only reach by executing the entry — not by failing on an
    // unassigned read.
    let session = Session::new("review", flow)?;
    assert_eq!(
        session.current(),
        Some("ask"),
        "an accepted ask-then-read flow runs to its ask"
    );
    Ok(())
}

#[test]
fn validation_accepts_a_join_where_every_predecessor_assigns() -> TestResult {
    let declarations = BTreeMap::from([(
        String::from("answer"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    )]);
    let nodes = BTreeMap::from([
        (
            String::from("start"),
            NodeKind::Branch {
                var: String::from("answer"),
                when: both_arms(),
                then: String::from("ask_a"),
                otherwise: String::from("ask_b"),
            },
        ),
        (
            String::from("ask_a"),
            ask_routing("answer", ["yes", "no"], "join"),
        ),
        (
            String::from("ask_b"),
            ask_routing("answer", ["yes", "no"], "join"),
        ),
        (
            String::from("join"),
            NodeKind::Say {
                text: String::from("You said ${answer}"),
            },
        ),
        (String::from("end"), NodeKind::End),
    ]);
    flow_with(
        declarations,
        "start",
        nodes,
        vec![FlowEdge::next("join", "end")],
        BTreeMap::from([(String::from("end"), Terminal::Completed)]),
    )?;
    Ok(())
}

#[test]
fn validation_rejects_a_join_where_one_predecessor_assigns() -> TestResult {
    let declarations = BTreeMap::from([(
        String::from("answer"),
        VarType::Choice(vec![String::from("yes"), String::from("no")]),
    )]);
    let nodes = BTreeMap::from([
        (
            String::from("start"),
            NodeKind::Branch {
                var: String::from("answer"),
                when: both_arms(),
                then: String::from("ask"),
                otherwise: String::from("skip"),
            },
        ),
        (
            String::from("ask"),
            ask_routing("answer", ["yes", "no"], "join"),
        ),
        (
            String::from("skip"),
            NodeKind::Say {
                text: String::from("No answer taken"),
            },
        ),
        (
            String::from("join"),
            NodeKind::Say {
                text: String::from("You said ${answer}"),
            },
        ),
        (String::from("end"), NodeKind::End),
    ]);
    let error = flow_with(
        declarations,
        "start",
        nodes,
        vec![
            FlowEdge::next("skip", "join"),
            FlowEdge::next("join", "end"),
        ],
        BTreeMap::from([(String::from("end"), Terminal::Completed)]),
    )
    .err()
    .ok_or("a join that one predecessor reaches unassigned must be refused")?;
    assert!(
        matches!(
            error,
            BotError::VariableReadBeforeInit { ref node, ref name }
                if node == "join" && name == "answer"
        ),
        "the diagnostic must name the reading node and the variable: {error}"
    );
    Ok(())
}

/// A resolver that never recognizes an answer, so the flows below stop at
/// their ask instead of driving past it.
const UNRESOLVING: Resolution = Resolution::Absent { best_score: 0.0 };

#[test]
fn validation_refuses_before_any_journal_record() -> TestResult {
    // The ordering claim: validation is what refuses the document, so a
    // custom sink never sees output. The valid control runs the same sink, so
    // an empty read means "the sink was never given anything" rather than
    // "the sink never works".
    let refused_sink = RecordingJournal::new();
    let refused = FlowSpec::from_json(ENTRY_READ_BEFORE_WRITE).and_then(|flow| {
        Session::with_components(
            "review",
            flow,
            FixedResolver(UNRESOLVING),
            refused_sink.clone(),
        )
        .map(|_| ())
    });
    assert!(
        matches!(refused, Err(BotError::VariableReadBeforeInit { .. })),
        "the document is refused by validation, before a session exists: {refused:?}"
    );
    assert!(
        refused_sink.records().is_empty(),
        "a refused document must reach no journal: {:?}",
        refused_sink.records()
    );

    let accepted_sink = RecordingJournal::new();
    let accepted = FlowSpec::from_json(
        r#"{
  "vars": {"name": {"kind": "string"}},
  "entry": "ask",
  "nodes": {
    "ask": {
      "kind": "ask",
      "var": "name",
      "options": ["Ada"],
      "routes": {"Ada": "greet"}
    },
    "greet": {"kind": "say", "text": "Hello ${name}"},
    "end": {"kind": "end"}
  },
  "edges": [{"kind": "next", "from": "greet", "to": "end"}],
  "bounds": {"budget": 8}
}"#,
    )
    .and_then(|flow| {
        Session::with_components(
            "review",
            flow,
            FixedResolver(Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            }),
            accepted_sink.clone(),
        )
        .map(|_| ())
    });
    accepted?;
    assert!(
        !accepted_sink.records().is_empty(),
        "the control flow must reach the sink, or the empty read above proves nothing"
    );
    Ok(())
}
