//! Public acceptance tests for the synchronous flow/session surface.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use lgwks_bot::{
    BotError, DecisionReceipt, DegradedReason, Embedder, EmbedderIdentity, FlowBounds, FlowEdge,
    FlowSpec, Journal, JournalError, MatchTier, NodeKind, PolicyVersion, Predicate, Provenance,
    RECEIPT_VERSION, ReceiptAcceptance, Resolution, Resolver, SemanticPolicy, SemanticResolver,
    Session, Terminal, TranscriptEntry, Value, ValueExpr, VarType, Verdict,
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
    fn resolve(&self, _utterance: &str, _options: &[String]) -> Verdict {
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

#[test]
fn a_receipt_names_the_model_only_when_a_model_decided() -> TestResult {
    let (_, lexical) = receipt_for(
        "Cancel",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
        },
    )?;
    let (_, semantic) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
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

#[test]
fn the_policy_that_decided_is_named_on_the_verdict_it_produced() -> TestResult {
    let (_, lexical) = receipt_for(
        "Cancel",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
        },
    )?;
    let (_, semantic) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
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
        },
    )?;
    let (second_session, second) = receipt_for(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-b")?,
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
        },
        SemanticPolicy::DEFAULT,
    )?;
    let (_, retuned) = receipt_for_policy(
        "the usual",
        OrderEmbedder {
            identity: test_identity("digest-a")?,
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
        },
    )?;

    let journal = TestJournal::accepting(ReceiptAcceptance::Durable);
    let mut reversed = Session::with_components(
        "reversed",
        reversed_order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
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

#[test]
fn a_refused_receipt_is_not_an_accepted_answer() -> TestResult {
    let journal = TestJournal::refusing(ReceiptAcceptance::Durable, 1);
    let mut session = Session::with_components(
        "refused",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
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
        }),
    )?;
    in_memory.answer("the usual")?;

    let mut durable = Session::with_components(
        "same",
        order_flow()?,
        SemanticResolver::new(OrderEmbedder {
            identity: test_identity("digest-a")?,
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
