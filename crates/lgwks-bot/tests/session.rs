//! Public acceptance tests for the synchronous flow/session surface.

use std::collections::BTreeMap;

use lgwks_bot::{
    Alias, BotError, DegradedReason, Embedder, EmbedderIdentity, FlowBounds, FlowEdge, FlowSpec,
    LanguageResolver, NodeKind, Predicate, Question, Resolution, Resolver, SemanticResolver,
    Session, Terminal, TranscriptEntry, Value, ValueExpr, VarType,
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
    fn resolve(&self, _utterance: &str, _question: &Question<'_>) -> Resolution {
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

/// An embedder that places `"the usual"` next to `"Repeat last order"` and
/// everything else orthogonally, so the test's geometry is its assertion.
struct OrderEmbedder {
    identity: EmbedderIdentity,
}

impl Embedder for OrderEmbedder {
    type Error = std::convert::Infallible;

    fn identity(&self) -> &EmbedderIdentity {
        &self.identity
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Self::Error> {
        Ok(match text {
            "the usual" | "Repeat last order" => vec![1.0, 0.0],
            _ => vec![0.0, 1.0],
        })
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
