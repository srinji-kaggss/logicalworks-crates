//! Public acceptance tests for the synchronous flow/session surface.

use std::collections::BTreeMap;

use lgwks_bot::{
    BotError, FlowBounds, FlowEdge, FlowSpec, NodeKind, Predicate, Session, Terminal, Value,
    ValueExpr, VarType,
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
