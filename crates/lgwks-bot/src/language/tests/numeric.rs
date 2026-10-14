//! The numeric-domain and margin tests: an integer question selects a value, never its spelling, and a lead is measured against the highest other score.

use super::*;

/// Builds an option list from literals, for the numeric-domain tests below.
fn amounts(choices: &[&str]) -> Vec<String> {
    choices.iter().map(|choice| (*choice).to_owned()).collect()
}

/// Names an integer question over `options`.
fn numeric(options: &[String]) -> Question<'_> {
    Question::new("amount", options).with_domain(AnswerDomain::Integer)
}

#[test]
fn integer_decoding_trims_surrounding_space_and_reads_a_leading_sign() {
    for (raw, expected) in [
        ("5", Some(5_i64)),
        ("+5", Some(5)),
        ("-5", Some(-5)),
        (" 5 ", Some(5)),
        ("0", Some(0)),
        ("-0", Some(0)),
        ("007", Some(7)),
        ("", None),
        ("five", None),
        ("5.0", None),
        ("5,000", None),
        ("9223372036854775807", Some(i64::MAX)),
        ("-9223372036854775808", Some(i64::MIN)),
        ("9223372036854775808", None),
        ("-9223372036854775809", None),
    ] {
        assert_eq!(
            decode_integer(raw),
            expected,
            "{raw:?} must decode to {expected:?} and nothing else"
        );
    }
}

#[test]
fn the_label_fold_is_exactly_why_a_number_needs_its_own_domain() {
    // The premise of the defect, stated as an assertion: the normalization
    // every label match runs through erases the sign, so a numeric answer
    // read as a label cannot tell "-5" from "5".
    assert_eq!(
        normalize("-5"),
        normalize("5"),
        "the fold collapses the sign, which is why it must not be applied to a value"
    );
    // And that is what the label domain does with the same two options: it
    // reports a tie rather than claiming either one. (A validated flow
    // refuses to offer them as labels at all — see `session.rs`.)
    let options = amounts(&["-5", "5"]);
    let verdict = LanguageResolver::new().resolve("-5", &ask(&options));
    assert_eq!(
        tied(&verdict),
        Some((vec![0, 1], MatchTier::Exact)),
        "under the label domain the sign is punctuation: {verdict:?}"
    );
}

#[test]
fn a_numeric_question_selects_the_value_and_never_the_spelling() {
    let resolver = LanguageResolver::new();
    let options = amounts(&["-5", "5", "0", "10"]);
    let question = numeric(&options);
    for (utterance, expected) in [
        ("-5", 0_usize),
        ("5", 1),
        ("+5", 1),
        (" 5 ", 1),
        ("0", 2),
        ("10", 3),
    ] {
        let verdict = resolver.resolve(utterance, &question);
        assert_eq!(
            selected(&verdict),
            Some((expected, MatchTier::Exact)),
            "{utterance:?} must select the option holding that value: {verdict:?}"
        );
    }
}

#[test]
fn a_number_no_option_holds_is_absent_rather_than_a_nearby_option() {
    let resolver = LanguageResolver::new();
    let options = amounts(&["5", "10"]);
    let question = numeric(&options);

    // The filed counterexample, reproduced: the same utterance against the
    // same options, read the way the defect read every question. This is the
    // verdict #38 reported — `-5` becoming option 0 at the Exact tier — and
    // it is asserted rather than removed, because it is the behaviour the
    // domain exists to withhold. Delete the domain and this is what the
    // assertion below starts returning.
    assert_eq!(
        selected(&resolver.resolve("-5", &ask(&options))),
        Some((0, MatchTier::Exact)),
        "the untyped reading of this question is the defect, exactly as filed"
    );

    for utterance in ["-5", "five", "5.0", "9223372036854775808", ""] {
        assert_eq!(
            resolver.resolve(utterance, &question).into_resolution(),
            Resolution::Absent { best_score: 0.0 },
            "{utterance:?} is not one of the offered values and no lexical, \
                 phonetic or fuzzy tier may turn it into one that is"
        );
    }
}

#[test]
fn a_numeric_reading_is_not_vetoed_by_a_similar_number() {
    // No fuzzy competitor exists in this domain: "-50" is not a near miss
    // for "-5", it is a different value.
    let options = amounts(&["-5", "-50"]);
    let verdict = LanguageResolver::new().resolve("-5", &numeric(&options));
    assert_eq!(
        selected(&verdict),
        Some((0, MatchTier::Exact)),
        "the value, not its similarity: {verdict:?}"
    );
}

#[test]
fn a_numeric_question_does_not_consult_the_alias_table() {
    // An alias key is a normalized utterance, and `-5` and `5` share one,
    // so reading a numeric answer through the table would put the sign loss
    // back through a second door.
    let resolver = LanguageResolver::with_aliases(vec![Alias::new("amount", "-5", "5")]);
    assert_eq!(
        resolver.learned(),
        1,
        "the premise: the alias really is held under the folded key"
    );
    let options = amounts(&["5", "10"]);
    assert_eq!(
        resolver.resolve("-5", &numeric(&options)).into_resolution(),
        Resolution::Absent { best_score: 0.0 },
        "the taught phrase must not reach a question read as values"
    );
    assert_eq!(
        selected(&resolver.resolve("5", &numeric(&options))),
        Some((0, MatchTier::Exact)),
        "and the value itself still resolves, by value rather than by alias"
    );
}

#[test]
fn two_options_holding_one_value_tie_rather_than_taking_the_first() {
    // A `FlowSpec` refuses to author this (see `session.rs`); asserted here
    // so the resolver is honest rather than silently positional even when
    // a caller constructs the question directly.
    let options = amounts(&["5", "05"]);
    let verdict = LanguageResolver::new().resolve("5", &numeric(&options));
    assert_eq!(
        tied(&verdict),
        Some((vec![0, 1], MatchTier::Exact)),
        "two options hold 5, so no option is preferred: {verdict:?}"
    );
}

/// Builds a measured candidate field from `(index, score)` pairs.
///
/// The tier is irrelevant to the decision arithmetic, so every entry is
/// `Semantic`: the point of these tests is the relationship between the
/// scores, and a tier label would be noise a reader has to skip past.
fn measured(scores: &[(usize, f64)]) -> Vec<(usize, MatchTier, f64)> {
    scores
        .iter()
        .copied()
        .map(|(index, score)| (index, MatchTier::Semantic, score))
        .collect()
}

#[test]
fn the_lead_is_measured_against_the_highest_other_measured_score() {
    let field = measured(&[(0, 0.90), (1, 0.10)]);
    assert_eq!(
        decide(&field, 0.5, 0.08),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Semantic,
            score: 0.90,
            lead: 0.80,
        },
        "the lead is the gap to the runner-up, not the score"
    );
}

#[test]
fn a_runner_up_just_below_the_threshold_still_counts_against_the_margin() {
    // The winner clears the threshold by 0.01 and the runner-up misses it by
    // 0.01. The measured lead is 0.02 against a required 0.05, so this is a
    // near tie. Filtering the runner-up away first made `decide` report the
    // winner's whole score as its lead and accept it.
    let field = measured(&[(0, 0.73), (1, 0.71)]);
    assert_eq!(
        decide(&field, 0.72, 0.05),
        Resolution::Ambiguous {
            tied: vec![0, 1],
            tier: MatchTier::Semantic,
            score: 0.73,
        },
        "a competitor below the threshold is still a competitor"
    );
}

#[test]
fn the_straddling_pair_is_not_special_to_a_whole_hundredth() {
    // The same shape with the pair separated from the threshold by an
    // arbitrarily small epsilon: `0.72` exactly is accepted, `0.72 - 1e-12`
    // is not, and either way the *other* measurement is what sets the lead.
    let below = 0.72 - 1e-12;
    let field = measured(&[(0, 0.72), (1, below)]);
    assert_eq!(
        decide(&field, 0.72, 0.05),
        Resolution::Ambiguous {
            tied: vec![0, 1],
            tier: MatchTier::Semantic,
            score: 0.72,
        },
        "a one-epsilon perturbation across the threshold must not manufacture confidence"
    );
}

#[test]
fn a_field_entirely_below_the_threshold_is_absent_at_its_best_measured_score() {
    let field = measured(&[(0, 0.40), (1, 0.30)]);
    assert_eq!(
        decide(&field, 0.72, 0.05),
        Resolution::Absent { best_score: 0.40 },
        "the closest option was measured, so `0.0` would be a value nothing produced"
    );
}

#[test]
fn a_lone_option_reports_its_whole_score_as_the_lead() {
    // There is no runner-up, so the lead is over an empty field and the
    // score is the whole of it. This is the one case where the two are
    // equal and the equality is honest.
    let field = measured(&[(0, 0.80)]);
    assert_eq!(
        decide(&field, 0.72, 0.05),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Semantic,
            score: 0.80,
            lead: 0.80,
        }
    );
    assert_eq!(
        decide(&measured(&[(0, 0.50)]), 0.72, 0.05),
        Resolution::Absent { best_score: 0.50 },
        "a lone option that does not clear the threshold is absent, not resolved"
    );
    assert_eq!(
        decide(&[], 0.72, 0.05),
        Resolution::Absent { best_score: 0.0 },
        "no measured candidate at all is the empty field"
    );
}

#[test]
fn an_exact_tie_is_ambiguous_under_a_positive_margin() {
    let field = measured(&[(0, 0.80), (1, 0.80)]);
    assert_eq!(
        decide(&field, 0.72, 0.05),
        Resolution::Ambiguous {
            tied: vec![0, 1],
            tier: MatchTier::Semantic,
            score: 0.80,
        },
        "a tie holds no lead at all"
    );
}

#[test]
fn a_zero_margin_resolves_a_tie_at_the_lowest_index_by_declared_policy() {
    // Margin `0.0` is a caller stating that no separation is required. The
    // arithmetic is left literal rather than special-cased, so `0.0 >= 0.0`
    // accepts the tie and the lowest index wins it — the comparator rule the
    // module documents. The threshold is still enforced first: an exact tie
    // below it is absent, not a coin flip.
    let field = measured(&[(0, 0.80), (1, 0.80)]);
    assert_eq!(
        decide(&field, 0.72, 0.0),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Semantic,
            score: 0.80,
            lead: 0.0,
        }
    );
    assert_eq!(
        decide(&measured(&[(0, 0.40), (1, 0.40)]), 0.72, 0.0),
        Resolution::Absent { best_score: 0.40 }
    );
}

#[test]
fn the_tied_set_is_lowest_index_first_whatever_order_it_arrives_in() {
    // `Resolution::Ambiguous::tied` documents "lowest first". A caller's
    // candidate order must not change the verdict, and for equal scores the
    // sort alone cannot guarantee it, so the set is ordered explicitly.
    let ascending = measured(&[(0, 0.80), (1, 0.80), (2, 0.80)]);
    let descending = measured(&[(2, 0.80), (1, 0.80), (0, 0.80)]);
    let expected = Resolution::Ambiguous {
        tied: vec![0, 1, 2],
        tier: MatchTier::Semantic,
        score: 0.80,
    };
    assert_eq!(decide(&ascending, 0.72, 0.05), expected);
    assert_eq!(decide(&descending, 0.72, 0.05), expected);
}

#[test]
fn permuting_the_option_list_does_not_change_the_verdict() {
    // End to end through the resolver, where a permutation means the same
    // options in a different order. The winner is the same option, and the
    // tied set is the same set.
    let resolver = LanguageResolver::new();
    let forwards = vec![
        String::from("Accept the offer"),
        String::from("Accept the order"),
    ];
    let backwards = vec![
        String::from("Accept the order"),
        String::from("Accept the offer"),
    ];
    assert_eq!(
        resolver.resolve("accept the", &ask(&forwards)),
        resolver.resolve("accept the", &ask(&backwards)),
        "the same options in a different order must reach the same verdict"
    );
}

#[test]
fn every_option_is_measured_even_when_none_clears_the_threshold() {
    // Neither option is a candidate: "yes" is not "Yes, continue" and not
    // "No, go back" by any tier that accepts. Both were measured anyway, so
    // the reported `best_score` is the closest proximity actually observed
    // rather than the `0.0` a resolver that never ran would report.
    let choices = vec![String::from("Yes, continue"), String::from("No, go back")];
    let resolution = LanguageResolver::new().resolve("yes", &ask(&choices));
    assert!(
        matches!(
            resolution.resolution(),
            Resolution::Absent { best_score }
                if *best_score > 0.0 && *best_score < MATCH_THRESHOLD
        ),
        "expected Absent at the best measured score below the threshold, got {resolution:?}"
    );
}
