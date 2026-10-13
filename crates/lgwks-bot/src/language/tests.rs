//! Tests for the language resolver, moved out of `language.rs` so the source file stays under the size law.

/// The two spellings of the input bound are one number.
///
/// The policy digest records the bound at `u32` and the resolver bounds its
/// input at `usize`, and nothing in the type system ties them together. If
/// they ever disagree, a receipt names a bound the resolver is not using —
/// which is the failure a policy version exists to make visible, produced by
/// the very mechanism meant to make it visible.
#[test]
fn the_digest_input_bound_is_the_shipped_bound() {
    assert_eq!(
        usize::try_from(u64::from(INPUT_BOUND_CHARS)).ok(),
        Some(MAX_UTTERANCE_CHARS),
        "INPUT_BOUND_CHARS and MAX_UTTERANCE_CHARS are one bound under two \
             spellings; if they differ, the policy digest names a bound the \
             resolver does not apply"
    );
}

use super::*;
use crate::session::Resolver;

/// The sentence the cross-tier tests resolve, and the two competitors that
/// sit below an exact match of it.
///
/// `IDENTICAL` is the utterance itself. `PHONETIC_TWIN` keeps a *different*
/// spelling of one word, so it lands on the phonetic tier, and `FUZZY_TWIN`
/// transposes the first two words, which leaves the token set identical and
/// the edit distance at four characters — a fuzzy score above the phonetic
/// tier's `0.9`, which is exactly the ordering these tests exist to pin.
const IDENTICAL: &str = "a monthly account statement should be sent to my email address";
const PHONETIC_TWIN: &str = "a monthlee account statement should be sent to my email address";
const FUZZY_TWIN: &str = "monthly a account statement should be sent to my email address";

/// Builds the option list most tests resolve against.
fn options() -> Vec<String> {
    vec![
        String::from("Yes, continue"),
        String::from("No, go back"),
        String::from("Speak to a person"),
    ]
}

/// Names the question most tests resolve against.
fn ask(options: &[String]) -> Question<'_> {
    Question::new("ask", options)
}

/// Returns the option and tier a verdict selected, or `None` for any other
/// verdict — so a test can assert identity and tier in one comparison
/// without a `panic!`, which this workspace forbids.
fn selected(verdict: &Verdict) -> Option<(usize, MatchTier)> {
    match *verdict.resolution() {
        Resolution::Resolved { index, tier, .. } => Some((index, tier)),
        _ => None,
    }
}

/// Returns the tied options and the tier they tied in, or `None`.
fn tied(verdict: &Verdict) -> Option<(Vec<usize>, MatchTier)> {
    match *verdict.resolution() {
        Resolution::Ambiguous { ref tied, tier, .. } => Some((tied.clone(), tier)),
        _ => None,
    }
}

/// Asserts two scores agree to within the tolerance float arithmetic needs.
fn assert_close(left: f64, right: f64) {
    assert!(
        (left - right).abs() < 1e-9,
        "expected {right}, observed {left}"
    );
}

#[test]
fn normalization_folds_case_punctuation_and_accents() {
    assert_eq!(normalize("  Yes,  CONTINUE! "), "yes continue");
    assert_eq!(normalize("Café"), "cafe");
    assert_eq!(
        normalize("Œuvre"),
        "ouvre",
        "a ligature folds to its base letter, not its expansion"
    );
    assert_eq!(normalize("Straße"), "strase");
    assert_eq!(normalize("!!!"), "");
}

#[test]
fn normalization_treats_unsupported_scripts_as_passthrough() {
    // Not a failure: the character survives so the fuzzy tier can still see
    // it, rather than being silently dropped to nothing.
    assert_eq!(normalize("日本"), "日本");
}

#[test]
fn phonetic_keys_collapse_spelling_variation() {
    assert_eq!(phonetic_key("Smith"), phonetic_key("Smyth"));
    assert_eq!(phonetic_key("there"), phonetic_key("their"));
    assert_eq!(phonetic_key("Straße"), phonetic_key("Strasse"));
    assert_eq!(phonetic_key("Kathryn"), phonetic_key("Kathrin"));
}

#[test]
fn phonetic_keys_preserve_the_initial_and_so_do_not_collapse_catherine() {
    // Recorded as a test rather than left implicit: this is the deliberate
    // false-negative side of the Soundex trade, and a future change that
    // "fixes" it would also collapse `cat`/`sat`.
    assert_eq!(phonetic_key("Catherine"), "C365");
    assert_eq!(phonetic_key("Kathryn"), "K365");
    // Same tail, different initial: not collapsed, which is the same rule.
    assert_ne!(phonetic_key("cat"), phonetic_key("kat"));
    assert_ne!(phonetic_key("cat"), phonetic_key("sat"));
}

#[test]
fn phonetic_keys_distinguish_unrelated_words() {
    assert_ne!(phonetic_key("continue"), phonetic_key("person"));
}

#[test]
fn an_input_with_no_letters_has_an_empty_phonetic_key() {
    assert_eq!(phonetic_key("123"), "");
    assert_eq!(phonetic_key("!!!"), "");
}

/// Whether a resolved verdict's lead clears the margin — the property that
/// makes `Resolved` the right variant rather than `Ambiguous`.
///
/// Note what this does *not* assert. A lead equal to the score is not by
/// itself evidence of the discarded-runner-up defect: when the best score
/// is `0.9` and the runner-up legitimately measured `0.0`, the gap really is
/// `0.9`. The two states are told apart by the score the runner-up was
/// given, not by the width of the gap, which is why the regression tests
/// below build the candidate field explicitly instead of inferring it from a
/// real phrase pair.
fn clears_the_margin(lead: f64) -> bool {
    lead >= MATCH_MARGIN
}

#[test]
fn an_exact_normalized_match_resolves_at_the_exact_tier() {
    let resolution = LanguageResolver::new()
        .resolve("  yes, CONTINUE ", &ask(&options()))
        .into_resolution();
    // The other two options only ever reach the fuzzy tier, and `score_all`
    // reduces the field to the winning tier before `decide` measures
    // anything, so the rival field this winner is measured against is empty
    // and the lead is the whole of its score — the same honest equality a
    // lone option reports. It is not the discarded-runner-up defect: that was
    // a *same-tier* competitor deleted by the threshold before the lead was
    // taken, and `the_lead_is_measured_against_the_highest_other_measured_score`
    // below pins the arithmetic that fixes it. Cross-tier, the gap would be
    // a subtraction between two numbers that do not mean the same thing.
    assert!(
        matches!(
            resolution,
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score,
                lead,
            } if (score - 1.0).abs() < 1e-9
                && (lead - score).abs() < 1e-9
                && clears_the_margin(lead)
        ),
        "expected an exact resolution of option 0 over a field with no exact rival, got {resolution:?}"
    );
}

#[test]
fn a_spelling_variation_resolves_at_the_phonetic_tier() {
    let choices = vec![String::from("Smyth"), String::from("Marcus")];
    let resolution = LanguageResolver::new()
        .resolve("Smith", &ask(&choices))
        .into_resolution();
    assert_eq!(
        resolution,
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Phonetic,
            score: 0.9,
            lead: 0.9,
        }
    );
}

#[test]
fn an_unrecognized_answer_is_absent() {
    let resolution = LanguageResolver::new()
        .resolve("maybe later", &ask(&options()))
        .into_resolution();
    assert!(
        matches!(resolution, Resolution::Absent { .. }),
        "expected Absent, got {resolution:?}"
    );
}

#[test]
fn two_equally_close_options_are_ambiguous_and_both_are_tied() {
    // Both options share a sound-alike key with the utterance, so both are
    // phonetic candidates at the same score and neither leads.
    let choices = vec![
        String::from("Accept the offer"),
        String::from("Accept the order"),
    ];
    let verdict = LanguageResolver::new().resolve("accept the", &ask(&choices));
    assert_eq!(
        tied(&verdict),
        Some((vec![0, 1], MatchTier::Phonetic)),
        "both options must stay in play, and the tie names its tier: {verdict:?}"
    );
}

#[test]
fn a_fuzzy_tie_is_reported_at_the_fuzzy_tier() {
    // The margin still has its intended meaning *inside* a tier: two
    // candidates that fold to the same text score identically, and the
    // resolver reports the tie rather than letting index order decide.
    let choices = vec![
        String::from(FUZZY_TWIN),
        String::from("monthly a account statement should be sent to my email address!"),
    ];
    let resolution = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
    assert_eq!(
        tied(&resolution),
        Some((vec![0, 1], MatchTier::Fuzzy)),
        "two equally fuzzy candidates stay ambiguous: {resolution:?}"
    );
}

#[test]
fn a_learned_alias_resolves_exactly_and_is_revocable() {
    let mut resolver = LanguageResolver::new();
    assert_eq!(resolver.learned(), 0);
    assert_eq!(
        resolver.learn("ask", "the usual", "Speak to a person"),
        None,
        "a fresh alias"
    );
    assert_eq!(resolver.learned(), 1);

    assert_eq!(
        resolver
            .resolve("The usual!", &ask(&options()))
            .into_resolution(),
        Resolution::Resolved {
            index: 2,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        },
        "an alias is a confirmed exact match, not a fuzzy guess"
    );

    let replaced = resolver.learn("ask", "the usual", "No, go back");
    assert_eq!(
        replaced.as_ref().map(Alias::option),
        Some("Speak to a person"),
        "re-teaching returns the binding it replaced, so a correction is \
             distinguishable from a first lesson"
    );
    assert_eq!(
        resolver
            .forget("ask", "the usual")
            .as_ref()
            .map(Alias::option),
        Some("No, go back"),
        "forgetting a learned alias returns the row it revoked"
    );
    assert_eq!(
        resolver.forget("ask", "the usual"),
        None,
        "forgetting a learned alias twice revokes nothing"
    );
    assert_eq!(resolver.learned(), 0);
}

#[test]
fn a_shipped_alias_table_is_normalized_on_load() {
    let resolver = LanguageResolver::with_aliases(vec![Alias::new(
        "ask",
        "  The Usual ",
        "Speak to a person",
    )]);
    assert_eq!(
        resolver
            .resolve("the usual", &ask(&options()))
            .into_resolution(),
        Resolution::Resolved {
            index: 2,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        }
    );
}

/// The filed counterexample for #25, first half: the confirmation was made
/// about an option, not about row 0, so it follows the option when the flow
/// reorders its choices.
#[test]
fn an_alias_follows_its_option_to_a_new_position() {
    let resolver =
        LanguageResolver::with_aliases(vec![Alias::new("ask", "the usual", "Repeat last order")]);
    for (choices, expected) in [
        (vec!["Repeat last order", "Cancel"], 0_usize),
        (vec!["Cancel", "Repeat last order"], 1),
    ] {
        let options = choices
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let verdict = resolver.resolve("the usual", &ask(&options));
        assert_eq!(
            selected(&verdict),
            Some((expected, MatchTier::Exact)),
            "the confirmation must travel with its option, not its index: \
                 {choices:?} gave {verdict:?}"
        );
    }
}

/// The filed counterexample for #25, second half: a binding made in one
/// question's vocabulary is not spent in another's — even when the other
/// question offers the identical option list. Without the scope this is
/// `Resolved` at index 0 with score 1.0, which is how `"the usual"` became
/// `"Delete account"`.
#[test]
fn an_alias_is_not_visible_from_another_question() {
    let resolver =
        LanguageResolver::with_aliases(vec![Alias::new("ask", "the usual", "Repeat last order")]);
    let choices = vec![
        String::from("Repeat last order"),
        String::from("Cancel"),
        String::from("Delete account"),
    ];
    let elsewhere = resolver.resolve("the usual", &Question::new("some_other_ask", &choices));
    assert_eq!(
        selected(&elsewhere),
        None,
        "another question must not consume this question's confirmation: {elsewhere:?}"
    );
    assert!(
        matches!(
            elsewhere.resolution(),
            Resolution::Absent { best_score }
                if *best_score > 0.0 && *best_score < MATCH_THRESHOLD
        ),
        "the phrase is simply unrecognized there — measured, and nowhere near \
             the threshold — not quietly bound to row 0: {elsewhere:?}"
    );
}

/// An option that is removed does not hand its confirmation to the option
/// that replaces it, and the withdrawal is reported rather than swallowed.
#[test]
fn a_superseded_alias_is_reported_rather_than_spent_on_the_new_option() {
    let resolver =
        LanguageResolver::with_aliases(vec![Alias::new("ask", "the usual", "Repeat last order")]);
    let choices = vec![String::from("Delete account"), String::from("Keep account")];
    let verdict = resolver.resolve("the usual", &ask(&choices));
    assert_eq!(
        verdict.resolution(),
        &Resolution::StaleAlias {
            question: String::from("ask"),
            option: String::from("Repeat last order"),
        },
        "the binding names the option that went away, not the one at its old index"
    );
    assert!(
        !matches!(verdict.resolution(), Resolution::Resolved { .. }),
        "a withdrawn confirmation must never select an option"
    );
}

/// Staleness is reported where it would decide the outcome, not ahead of an
/// answer the current list genuinely supports.
#[test]
fn a_visible_option_outranks_a_superseded_alias() {
    let resolver = LanguageResolver::with_aliases(vec![Alias::new("ask", "yes", "No, go back")]);
    let choices = vec![String::from("Yes")];
    let verdict = resolver.resolve("yes", &ask(&choices));
    assert_eq!(
        selected(&verdict),
        Some((0, MatchTier::Exact)),
        "the phrase is on screen and the reading does not come from the \
             alias at all; refusing an answer the person can see to report a \
             fact about the alias table would be the tail wagging the dog: \
             {verdict:?}"
    );
}

/// The same phrase, the same option list, two questions: the meaning is a
/// property of the question, which is what the scope makes expressible.
#[test]
fn one_phrase_can_mean_two_things_in_two_questions() {
    let mut resolver = LanguageResolver::new();
    resolver.learn("ask_one", "the usual", "Repeat last order");
    assert_eq!(
        resolver.learn("ask_two", "the usual", "Cancel"),
        None,
        "a second question's first lesson has no predecessor in *that* question"
    );
    assert_eq!(resolver.learned(), 2, "both questions kept their own row");

    let choices = vec![String::from("Repeat last order"), String::from("Cancel")];
    assert_eq!(
        selected(&resolver.resolve("the usual", &Question::new("ask_one", &choices))),
        Some((0, MatchTier::Exact)),
        "in the first question the phrase means the first option"
    );
    assert_eq!(
        selected(&resolver.resolve("the usual", &Question::new("ask_two", &choices))),
        Some((1, MatchTier::Exact)),
        "in the second question it means the second"
    );
}

/// Two spellings that fold to one form are one row, and the row it replaced
/// is returned rather than dropped.
#[test]
fn a_normalized_collision_rebinds_one_row_and_returns_the_replaced_one() {
    let mut resolver = LanguageResolver::new();
    resolver.learn("ask", "The Usual", "Speak to a person");
    let replaced = resolver.learn("ask", "the usual!", "No, go back");
    assert_eq!(
        replaced.as_ref().map(Alias::option),
        Some("Speak to a person"),
        "the collision is a rebinding of one phrase, and it is visible"
    );
    assert_eq!(resolver.learned(), 1, "one phrase is one row");
    assert_eq!(
        selected(&resolver.resolve("the usual", &ask(&options()))),
        Some((1, MatchTier::Exact)),
        "the later binding is the live one"
    );
}

/// The learned vocabulary is data, so it can be reviewed and moved. A table
/// that survives the round trip is the difference between a house vocabulary
/// an estate can ship and one trapped inside a running process.
#[test]
fn an_exported_table_reloads_unchanged() {
    let mut resolver = LanguageResolver::new();
    resolver.learn("ask", "the usual", "Repeat last order");
    resolver.learn("ask", "nah", "Cancel");

    let table = resolver.aliases();
    assert_eq!(table.len(), 2, "both rows export");
    assert!(
        table.iter().any(|alias| alias.question() == "ask"
            && alias.utterance() == "the usual"
            && alias.option() == "Repeat last order"),
        "a row is readable without knowing how it was stored: {table:?}"
    );

    let reloaded = LanguageResolver::with_aliases(table);
    assert_eq!(reloaded.learned(), resolver.learned());
    let choices = vec![String::from("Repeat last order"), String::from("Cancel")];
    for utterance in ["the usual", "nah"] {
        assert_eq!(
            reloaded.resolve(utterance, &ask(&choices)),
            resolver.resolve(utterance, &ask(&choices)),
            "a migrated table resolves exactly as the one it came from"
        );
    }
}

#[test]
fn the_fuzzy_weights_account_for_the_whole_score() {
    assert_close(TOKEN_WEIGHT + DISTANCE_WEIGHT, 1.0);
}

#[test]
fn a_long_utterance_degrades_rather_than_panicking() {
    let long = "yes ".repeat(MAX_UTTERANCE_CHARS);
    // `EditDistance` refuses an over-limit input and scores it 0.0 rather
    // than allocating a quadratic table, so a hostile input degrades to the
    // token tier instead of hanging. The bound is the contract.
    let resolution = LanguageResolver::new()
        .resolve(&long, &ask(&options()))
        .into_resolution();
    assert!(
        matches!(
            resolution,
            Resolution::Resolved { .. } | Resolution::Absent { .. }
        ),
        "an over-limit input must produce a verdict, got {resolution:?}"
    );
}

#[test]
fn an_empty_option_list_is_absent_not_a_panic() {
    assert_eq!(
        LanguageResolver::new()
            .resolve("yes", &ask(&[]))
            .into_resolution(),
        Resolution::Absent { best_score: 0.0 }
    );
}

/// The `FUZZY_TWIN` reading of `IDENTICAL` must really be a *fuzzy* reading
/// scoring above the phonetic tier, or the precedence tests below would be
/// comparing nothing. This test states that premise so a change to the
/// similarity arithmetic cannot quietly hollow them out.
#[test]
fn the_fuzzy_competitor_really_outscores_the_phonetic_tier() {
    let alone = vec![String::from(FUZZY_TWIN)];
    let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&alone));
    assert!(
        matches!(
            verdict.resolution(),
            Resolution::Resolved {
                tier: MatchTier::Fuzzy,
                score,
                ..
            } if *score > 0.9
        ),
        "the fuzzy competitor must score above the phonetic tier's 0.9: {verdict:?}"
    );
}

#[test]
fn an_exact_answer_is_not_vetoed_by_a_fuzzy_competitor() {
    // The filed counterexample: the person typed the complete text of
    // option 0, and a transposed competitor one tier down scored high
    // enough to sit inside the margin.
    let choices = vec![String::from(IDENTICAL), String::from(FUZZY_TWIN)];
    let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
    assert_eq!(
        selected(&verdict),
        Some((0, MatchTier::Exact)),
        "a unique exact answer must not be vetoed by a lower tier: {verdict:?}"
    );
}

#[test]
fn an_exact_answer_is_not_vetoed_by_a_phonetic_competitor() {
    let choices = vec![String::from(IDENTICAL), String::from(PHONETIC_TWIN)];
    let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
    assert_eq!(
        selected(&verdict),
        Some((0, MatchTier::Exact)),
        "the phonetic tier ranks below the exact tier: {verdict:?}"
    );
}

#[test]
fn a_phonetic_candidate_outranks_a_higher_scoring_fuzzy_candidate() {
    let choices = vec![String::from(PHONETIC_TWIN), String::from(FUZZY_TWIN)];
    let verdict = LanguageResolver::new().resolve(IDENTICAL, &ask(&choices));
    assert_eq!(
        selected(&verdict),
        Some((0, MatchTier::Phonetic)),
        "precedence is the tier order, not the numeric score: {verdict:?}"
    );
}

#[test]
fn two_normalized_equal_exact_options_tie_rather_than_taking_the_first() {
    // Both options fold to `yes continue`, so both are exact. Picking the
    // first would be an arbitrary choice between two options the person's
    // answer genuinely does not distinguish.
    let choices = vec![String::from("Yes, continue"), String::from("yes continue!")];
    let verdict = LanguageResolver::new().resolve("yes continue", &ask(&choices));
    assert_eq!(
        tied(&verdict),
        Some((vec![0, 1], MatchTier::Exact)),
        "an exact collision stays ambiguous at the exact tier: {verdict:?}"
    );
}

#[test]
fn a_learned_alias_conflicting_with_a_normalized_exact_option_ties() {
    // The confirmation says `yes` means `No`; the folding says it means
    // `Yes`. Both are exact-tier readings, so the resolver reports the
    // conflict rather than letting the alias table silently outrank the
    // option text.
    let mut resolver = LanguageResolver::new();
    resolver.learn("ask", "yes", "No");
    let choices = vec![String::from("Yes"), String::from("No")];
    let verdict = resolver.resolve("yes", &ask(&choices));
    assert_eq!(
        tied(&verdict),
        Some((vec![0, 1], MatchTier::Exact)),
        "a conflicting confirmation is a tie, not an override: {verdict:?}"
    );
}

#[test]
fn tier_precedence_survives_option_permutation() {
    let resolver = LanguageResolver::new();
    for (choices, expected) in [
        (
            vec![String::from(IDENTICAL), String::from(FUZZY_TWIN)],
            (0_usize, MatchTier::Exact),
        ),
        (
            vec![String::from(FUZZY_TWIN), String::from(IDENTICAL)],
            (1, MatchTier::Exact),
        ),
        (
            vec![String::from(PHONETIC_TWIN), String::from(FUZZY_TWIN)],
            (0, MatchTier::Phonetic),
        ),
        (
            vec![String::from(FUZZY_TWIN), String::from(PHONETIC_TWIN)],
            (1, MatchTier::Phonetic),
        ),
    ] {
        let verdict = resolver.resolve(IDENTICAL, &ask(&choices));
        assert_eq!(
            selected(&verdict),
            Some(expected),
            "the winning option must follow its text, not its position: {choices:?} gave {verdict:?}"
        );
    }
}

mod numeric;
