//! Tests of the session module. Split from `session.rs` (#526).

use super::*;

/// Return a small validated flow used by focused unit tests.
fn end_flow() -> Result<FlowSpec, BotError> {
    FlowSpec::new(
        BTreeMap::new(),
        "end",
        BTreeMap::from([(String::from("end"), NodeKind::End)]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(4),
    )
}

#[test]
fn predicates_compare_typed_values() -> Result<(), BotError> {
    let mut declarations = BTreeMap::new();
    declarations.insert(String::from("answer"), VarType::Integer);
    let mut scope = VarScope::new(declarations)?;
    scope.set_from_answer("answer", "7")?;
    let predicate = Predicate::Gt(
        ValueExpr::Var(String::from("answer")),
        ValueExpr::Literal(Value::Integer(3)),
    );
    assert!(predicate.evaluate(&scope)?);
    Ok(())
}

#[test]
fn templates_use_scope_values() -> Result<(), BotError> {
    let mut declarations = BTreeMap::new();
    declarations.insert(String::from("name"), VarType::String);
    let mut scope = VarScope::new(declarations)?;
    scope.set_from_answer("name", "Ada")?;
    let rendered = scope.interpolate("Hello ${name}")?;
    assert_eq!(rendered, "Hello Ada");
    Ok(())
}

/// One declared type paired with candidate answer strings and the value
/// each must decode to.
fn decode_cases() -> Vec<(VarType, &'static str, Result<Value, AnswerRejection>)> {
    let choice = || VarType::Choice(vec![String::from("yes"), String::from("no")]);
    vec![
        // A string variable accepts anything, untrimmed: the declared type
        // is what decides, not a literal spelling.
        (
            VarType::String,
            "Continue",
            Ok(Value::String(String::from("Continue"))),
        ),
        (
            VarType::String,
            "  padded  ",
            Ok(Value::String(String::from("  padded  "))),
        ),
        (VarType::Integer, "7", Ok(Value::Integer(7))),
        (VarType::Integer, " -12 ", Ok(Value::Integer(-12))),
        (VarType::Integer, "007", Ok(Value::Integer(7))),
        (VarType::Integer, "7.5", Err(AnswerRejection::NotAnInteger)),
        (VarType::Integer, "", Err(AnswerRejection::NotAnInteger)),
        (
            VarType::Integer,
            "99999999999999999999",
            Err(AnswerRejection::IntegerOutOfRange),
        ),
        (VarType::Boolean, "yes", Ok(Value::Boolean(true))),
        (VarType::Boolean, " TRUE ", Ok(Value::Boolean(true))),
        (VarType::Boolean, "No", Ok(Value::Boolean(false))),
        (
            VarType::Boolean,
            "Continue",
            Err(AnswerRejection::NotABoolean),
        ),
        (choice(), "yes", Ok(Value::Choice(String::from("yes")))),
        (choice(), " No ", Ok(Value::Choice(String::from("no")))),
        (
            choice(),
            "approve",
            Err(AnswerRejection::NotADeclaredChoice),
        ),
    ]
}

#[test]
fn the_answer_decoder_is_one_function_for_validation_and_assignment() {
    // The decode table is the whole statement of what "this candidate can
    // be stored" means, and both the load-time check and the runtime
    // assignment read it. Every branch of every declared type is here,
    // including the two integer failures, which are distinct verdicts: a
    // typo and a value too large to store need different repairs.
    for (declared, answer, expected) in decode_cases() {
        assert_eq!(
            declared.decode_answer(answer),
            expected,
            "decoding {answer:?} as {} disagreed with the table",
            declared.label()
        );
    }
}

#[test]
fn assignment_uses_the_same_decoder_validation_does() -> Result<(), BotError> {
    // The property that makes the load-time check worth anything: for every
    // case the decoder accepts, assigning supplies the answer to a scope
    // and stores exactly the value the decoder named. A second, separately
    // written assignment path is what let a flow load and then be
    // unanswerable.
    for (declared, answer, expected) in decode_cases() {
        let mut declarations = BTreeMap::new();
        declarations.insert(String::from("slot"), declared.clone());
        let mut scope = VarScope::new(declarations)?;

        let assigned = scope.set_from_answer("slot", answer);
        match expected {
            Ok(value) => {
                assigned?;
                assert_eq!(
                    scope.get("slot"),
                    Some(&value),
                    "assigning {answer:?} as {} stored the wrong value",
                    declared.label()
                );
            }
            Err(rejection) => {
                assert!(
                    matches!(assigned, Err(BotError::InvalidVariableValue { .. })),
                    "assigning {answer:?} as {} must refuse with the typed value error, got \
                     {assigned:?} ({rejection})",
                    declared.label()
                );
                assert!(
                    scope.get("slot").is_none(),
                    "a refused assignment must not write the variable"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn every_rejection_renders_a_cause_a_reader_can_act_on() {
    let rejections = [
        AnswerRejection::NotAnInteger,
        AnswerRejection::IntegerOutOfRange,
        AnswerRejection::NotABoolean,
        AnswerRejection::NotADeclaredChoice,
    ];
    for rejection in rejections {
        let rendered = rejection.to_string();
        assert!(
            !rendered.is_empty() && rendered.chars().any(char::is_alphabetic),
            "a rejection is rendered into a diagnostic, so it cannot be empty: {rendered:?}"
        );
    }
}

#[test]
fn an_ask_whose_variable_cannot_hold_its_options_is_refused_at_load() -> Result<(), BotError> {
    // The counterexample the review filed: a boolean variable and a
    // question offering "Continue"/"Cancel". Every candidate is decodable
    // at load time, and neither one is storable, so the document is refused
    // rather than accepted and then unanswerable.
    let outcome = FlowSpec::new(
        BTreeMap::from([(String::from("decision"), VarType::Boolean)]),
        "ask",
        BTreeMap::from([
            (
                String::from("ask"),
                NodeKind::Ask {
                    var: String::from("decision"),
                    options: vec![String::from("Continue"), String::from("Cancel")],
                    routes: BTreeMap::from([
                        (String::from("Continue"), String::from("done")),
                        (String::from("Cancel"), String::from("done")),
                    ]),
                },
            ),
            (String::from("done"), NodeKind::End),
        ]),
        Vec::new(),
        BTreeMap::new(),
        FlowBounds::new(8),
    );
    let BotError::AskOptionNotAssignable {
        node,
        variable,
        option,
        expected,
        cause,
    } = (match outcome {
        Err(error) => error,
        Ok(spec) => {
            return Err(BotError::MalformedFlow {
                cause: format!("accepted a flow with {} nodes", spec.nodes().len()),
            });
        }
    })
    else {
        return Err(BotError::MalformedFlow {
            cause: String::from("refused for the wrong reason"),
        });
    };

    // The five facts the diagnostic has to carry: where, which variable,
    // which candidate, what type was expected, and why.
    assert_eq!(node, "ask", "the refusal names the ask node");
    assert_eq!(variable, "decision", "the refusal names the variable");
    assert_eq!(
        option, "Continue",
        "the refusal names the candidate, not just the node"
    );
    assert_eq!(expected, "boolean", "the refusal names the expected type");
    assert_eq!(
        cause,
        AnswerRejection::NotABoolean.to_string(),
        "the refusal carries the decoder's own reason"
    );
    Ok(())
}

#[test]
fn an_ask_the_variable_can_hold_is_still_accepted() -> Result<(), BotError> {
    // The other half of the rule, so the check above cannot be satisfied by
    // refusing everything: a boolean variable offered boolean literals, a
    // choice variable offered its own declared values, and an integer
    // variable offered integers all load.
    let accepted = [
        (
            VarType::Boolean,
            vec![String::from("yes"), String::from("no")],
        ),
        (
            VarType::Integer,
            vec![String::from("0"), String::from("-1")],
        ),
        (
            VarType::Choice(vec![String::from("yes"), String::from("no")]),
            vec![String::from("Yes"), String::from("NO")],
        ),
        (
            VarType::String,
            vec![String::from("anything"), String::from("")],
        ),
    ];
    for (declared, options) in accepted {
        let routes = options
            .iter()
            .map(|option| (option.clone(), String::from("done")))
            .collect();
        let spec = FlowSpec::new(
            BTreeMap::from([(String::from("slot"), declared.clone())]),
            "ask",
            BTreeMap::from([
                (
                    String::from("ask"),
                    NodeKind::Ask {
                        var: String::from("slot"),
                        options,
                        routes,
                    },
                ),
                (String::from("done"), NodeKind::End),
            ]),
            Vec::new(),
            BTreeMap::new(),
            FlowBounds::new(8),
        )?;
        assert_eq!(
            spec.nodes().len(),
            2,
            "a {} ask over storable candidates must load",
            declared.label()
        );
    }
    Ok(())
}

#[test]
fn the_default_resolver_has_an_explicit_unrecognized_case() {
    let options = vec![String::from("yes"), String::from("no")];
    let question = Question::new("ask", &options);
    let resolver = crate::language::LanguageResolver::new();
    assert!(
        matches!(
            resolver.resolve("maybe", &question).into_resolution(),
            Resolution::Absent { .. }
        ),
        "an unrecognized answer is Absent, which is not the same as Ambiguous"
    );
    assert_eq!(
        resolver.resolve("yes", &question).into_resolution(),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        }
    );
}

#[test]
fn end_session_stops_at_terminal() -> Result<(), BotError> {
    let session = Session::new("test", end_flow()?)?;
    assert_eq!(session.current(), None);
    assert_eq!(session.terminal(), Some(&Terminal::Completed));
    Ok(())
}

/// The four terminals, each paired with the disposition it must report.
fn terminals() -> [(Terminal, Disposition); 4] {
    [
        (Terminal::Completed, Disposition::Completed),
        (
            Terminal::Referred {
                target: String::from("tier-2"),
            },
            Disposition::Referred,
        ),
        (
            Terminal::HandedOff {
                target: String::from("agent"),
            },
            Disposition::HandedOff,
        ),
        (
            Terminal::Refused {
                reason: String::from("out of scope"),
            },
            Disposition::Refused,
        ),
    ]
}

#[test]
fn disposition_and_effect_knowledge_are_reported_independently() {
    // The cross-product: every terminal against every effect history. Each
    // axis comes back unchanged whatever the other says, because they are
    // two facts rather than two arms of one choice.
    let histories = [
        EffectLedger::new(0, 0), // nothing attempted
        EffectLedger::new(1, 0), // one effect, confirmed
        EffectLedger::new(0, 1), // one attempted, occurrence unknown
        EffectLedger::new(2, 1), // confirmed and unknown at once
        EffectLedger::new(3, 2),
    ];

    for (terminal, expected) in terminals() {
        for ledger in histories {
            let report = terminal.outcome(ledger);
            assert_eq!(
                report.disposition(),
                expected,
                "no effect history may change what the workflow decided"
            );
            assert_eq!(
                report.effects(),
                ledger,
                "no disposition may change what is known about the effects"
            );
            assert_eq!(
                report.needs_reconciliation(),
                ledger.unsettled() > 0,
                "reconciliation follows the unsettled count and nothing else"
            );
        }
    }
}

#[test]
fn a_refusal_no_longer_erases_an_unsettled_effect() {
    // The counterexample from the review, kept as a regression. Two runs
    // refused for the same reason, differing only in whether an earlier
    // effect was left in doubt. The old classifier returned `Failure` for
    // both, which is the one answer that cannot be acted on: only the
    // second one needs an effect reconciled before anyone retries.
    let refused = Terminal::Refused {
        reason: String::from("later step denied"),
    };

    let settled = refused.outcome(EffectLedger::new(0, 0));
    let unknown = refused.outcome(EffectLedger::new(0, 1));

    assert_eq!(
        settled.disposition(),
        unknown.disposition(),
        "both runs were refused, and the report says so in both cases"
    );
    assert!(
        !settled.needs_reconciliation(),
        "nothing was attempted, so there is nothing to reconcile"
    );
    assert!(
        unknown.needs_reconciliation(),
        "an earlier effect may be live; a refusal does not settle it"
    );
    assert_ne!(
        settled, unknown,
        "histories differing on a possibly-live effect must stay distinguishable"
    );
}

#[test]
fn an_unsettled_effect_does_not_imply_a_confirmed_one() {
    // The second inference error the review named. `unsettled > 0` was
    // documented as "part of the output exists", which does not follow: one
    // attempted effect whose response was lost may have produced none, and
    // the report has to say what is known rather than the flattering half.
    let report = Terminal::Completed.outcome(EffectLedger::new(0, 1));

    assert_eq!(
        report.effects().confirmed(),
        0,
        "nothing was confirmed, so nothing may be claimed to exist"
    );
    assert_eq!(
        report.effects().unsettled(),
        1,
        "the unknown effect is still counted"
    );
    assert!(
        report.needs_reconciliation(),
        "the run needs reconciling, which is not the same statement as having \
         partly succeeded"
    );
}

#[test]
fn an_indeterminate_effect_is_what_puts_a_run_in_doubt() {
    // The law tying the two halves of this change together: the error says
    // an effect may have happened, and that is exactly what puts the run in
    // doubt. Neither half is reachable in practice without the other.
    let errors = [BotError::EffectIndeterminate {
        domain: String::from("gh::merge"),
        cause: String::from("request timed out"),
    }];
    let unsettled = errors
        .iter()
        .filter(|error| matches!(**error, BotError::EffectIndeterminate { .. }))
        .count();

    assert_eq!(unsettled, 1, "the run left one effect unsettled");
    let report = Terminal::Completed.outcome(EffectLedger::new(0, unsettled));
    assert_eq!(
        report.disposition(),
        Disposition::Completed,
        "an unsettled effect does not change what the workflow decided"
    );
    assert!(
        report.needs_reconciliation(),
        "an indeterminate effect is what puts a completed run in doubt"
    );
}

#[test]
fn a_report_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
    // A report is written to a journal and read back by whoever reconciles
    // it, so it has to survive the trip with both facts intact.
    let report = Terminal::Refused {
        reason: String::from("denied"),
    }
    .outcome(EffectLedger::new(2, 1));

    let json = crate::json::to_string(&report)?;
    let back: Outcome = crate::json::from_str(&json)?;

    assert_eq!(
        back, report,
        "a report has to survive the journal it is written to"
    );
    assert_eq!(
        back.disposition(),
        Disposition::Refused,
        "the refusal survives the round trip"
    );
    assert!(back.needs_reconciliation(), "so does the unresolved effect");
    Ok(())
}
