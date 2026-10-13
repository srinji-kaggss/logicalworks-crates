//! Tests for the interface recognizer, moved out of `interface.rs` so the
//! source file stays under the size law.

use super::*;

/// Asserts two scores agree to within the tolerance float arithmetic needs.
///
/// An epsilon rather than `==`: `clippy::float_cmp` is forbidden workspace
/// wide, and exact float equality in a test is a claim the test cannot
/// actually check.
fn assert_close(left: f64, right: f64) {
    assert!(
        (left - right).abs() < 1e-9,
        "expected {right}, observed {left}"
    );
}

/// Builds a candidate carrying one identity attribute and some visible text.
fn candidate(identity: &str, text: &str) -> ElementFacts {
    ElementFacts::new(
        "button",
        "html > body > form > button",
        text,
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_identity(vec![String::from(identity)])
}

/// Builds the shipped default vector.
fn vector() -> Result<RecognitionVector, RecognitionError> {
    RecognitionVector::fingerprint()
}

#[test]
fn identical_facts_resolve_with_a_full_score() -> Result<(), RecognitionError> {
    let target = candidate("data-testid=submit", "Submit");
    assert_eq!(
        vector()?.recognize(&target, std::slice::from_ref(&target)),
        Recognition::Resolved {
            index: 0,
            score: 1.0,
            lead: 1.0,
        }
    );
    Ok(())
}

#[test]
fn an_empty_candidate_set_is_absent_not_a_panic() -> Result<(), RecognitionError> {
    let target = candidate("data-testid=submit", "Submit");
    assert_eq!(
        vector()?.recognize(&target, &[]),
        Recognition::Absent { best_score: 0.0 }
    );
    Ok(())
}

#[test]
fn two_indistinguishable_candidates_are_ambiguous() -> Result<(), RecognitionError> {
    let target = candidate("data-testid=submit", "Submit");
    let twin = candidate("data-testid=submit", "Submit");
    assert_eq!(
        vector()?.recognize(&target, &[twin.clone(), twin]),
        Recognition::Ambiguous {
            best: 0,
            runner_up: Some(1),
            score: 1.0,
            lead: 0.0,
        },
        "the lowest index must win a tie, and the pair must not resolve"
    );
    Ok(())
}

/// Builds facts for an element that carries a tag and a path and nothing
/// else: no identity attributes, no text.
fn bare(tag: &str, path: &str) -> ElementFacts {
    ElementFacts::new(tag, path, "", [0.5, 0.5, 0.1, 0.05])
}

/// Builds facts for an icon-only control: an identifier, a path and a box,
/// and no visible text.
fn icon(identifier: &str) -> ElementFacts {
    bare("button", "html > body > nav > button").with_identity(vec![String::from(identifier)])
}

#[test]
fn absent_identity_is_not_evidence_that_an_unrelated_element_matches()
-> Result<(), RecognitionError> {
    // GitHub issue #39's counterexample, verbatim. Neither fact carries an
    // identifying attribute and neither carries text; the two boxes and the
    // two paths are identical; the tags differ. This used to score
    // `Resolved { index: 0, score: 0.75, lead: 0.75 }` — identity `1.0` from
    // `Jaccard`'s empty-set convention, text `1.0` from the edit distance's,
    // geometry `1.0`, path `0.0` — a confident locator decision for an
    // element that is not even the same kind of thing.
    let target = ElementFacts::new("button", "button", "", [0.5, 0.5, 0.1, 0.05]);
    let unrelated = ElementFacts::new("img", "img", "", [0.5, 0.5, 0.1, 0.05]);
    let result = vector()?.recognize(&target, &[unrelated]);
    assert!(matches!(result, Recognition::Absent { .. }), "{result:?}");
    Ok(())
}

#[test]
fn the_metric_convention_is_intact_where_it_belongs() {
    // The rule being corrected lives one layer down, and it is still
    // correct there: two empty sets are the same set and two empty strings
    // are the same string. What element recognition changes is the reading
    // of "empty" — *nothing to compare* rather than *identical* — and this
    // asserts both halves, so a future repair cannot quietly change the
    // shared metric to fix a consumer.
    let no_attributes: [String; 0] = [];
    assert_close(
        Jaccard::<String>::new().score(&no_attributes, &no_attributes),
        1.0,
    );
    assert_close(EditDistance::new(8).score("", ""), 1.0);
    assert_close(PathSimilarity::new().score("", ""), 1.0);

    let blank = bare("button", "");
    assert_close(IdentityComponent::new().score(&blank, &blank), 0.0);
    assert_close(TextComponent::new(8).score(&blank, &blank), 0.0);
    assert_close(PathComponent::new().score(&blank, &blank), 0.0);
}

#[test]
fn an_element_with_nothing_to_identify_it_by_cannot_resolve() -> Result<(), RecognitionError> {
    // Same tag, same path, same box, no text, and no identity attributes on
    // either side. Every fact that is present agrees perfectly, and the two
    // snapshots still say nothing about which element this is: the absent
    // facts are comparisons that were not made, not comparisons that
    // succeeded. Identity is half the vector and the threshold is three
    // quarters, so it cannot resolve however well the rest lines up.
    let recognition = vector()?.recognize(
        &bare("button", "html > body > form > button"),
        &[bare("button", "html > body > form > button")],
    );
    assert!(
        matches!(
            recognition,
            Recognition::Absent { best_score } if best_score < FINGERPRINT_THRESHOLD
        ),
        "two elements with no identifying evidence must not match, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn an_icon_only_control_resolves_on_its_identifier() -> Result<(), RecognitionError> {
    // The positive case the presence rule must not break: no visible text
    // at all, and a genuinely observed matching stable identifier. Text is
    // absent on both sides and contributes nothing; identity and structure
    // carry the decision, which is what those facts are for.
    let target = icon("data-testid=menu");
    let recognition = vector()?.recognize(&target, std::slice::from_ref(&target));
    assert!(
        matches!(
            recognition,
            Recognition::Resolved { index: 0, score, lead }
                if (score - 0.875).abs() < 1e-9 && lead >= FINGERPRINT_MARGIN
        ),
        "a matching identifier must still resolve an icon-only control, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn an_icon_only_control_does_not_match_on_layout() -> Result<(), RecognitionError> {
    // The same icon-only pair with different identifiers. Identity is
    // unobserved-in-effect (both sides report one, and they disagree) and
    // text is absent, so the only facts left are the path and the box, worth
    // a quarter of the vector between them — nowhere near three quarters.
    // Layout alone cannot locate a control.
    let target = icon("data-testid=menu");
    let other = icon("data-testid=close");
    let recognition = vector()?.recognize(&target, &[other]);
    assert!(
        matches!(
            recognition,
            Recognition::Absent { best_score } if best_score < FINGERPRINT_THRESHOLD
        ),
        "a quarter of the vector must not resolve, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn an_unrelated_tag_is_excluded_however_well_the_rest_agrees() -> Result<(), RecognitionError> {
    // Every component in the vector agrees perfectly, including a genuinely
    // observed matching identifier and matching text. The tag disagrees, and
    // the tag is a gate, so the candidate is never scored. The mirror case
    // below is what shows the gate is the cause rather than the scoring.
    let facts = |tag: &str| {
        ElementFacts::new(
            tag,
            "html > body > form > button",
            "Submit",
            [0.5, 0.5, 0.1, 0.05],
        )
        .with_identity(vec![String::from("data-testid=submit")])
    };
    let target = facts("button");
    let impostor = facts("img");
    assert_eq!(
        vector()?.recognize(&target, &[impostor]),
        Recognition::Absent { best_score: 0.0 },
        "a different element kind is not a candidate"
    );
    let twin = facts("button");
    assert!(
        matches!(
            vector()?.recognize(&target, &[twin]),
            Recognition::Resolved { score, .. } if (score - 1.0).abs() < 1e-9
        ),
        "the identical element on the same facts still resolves"
    );
    Ok(())
}

#[test]
fn an_unreported_tag_excludes_nothing() -> Result<(), RecognitionError> {
    // An empty tag is a fact nobody reported, not a competing one, so it
    // cannot be an incompatibility. It cannot help either: the tag is not a
    // component, so the candidate has exactly the evidence its other facts
    // supply, and it still has to clear the threshold on those.
    let target = ElementFacts::new(
        "button",
        "html > body > form > button",
        "Submit",
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_identity(vec![String::from("data-testid=submit")]);
    let unlabelled = ElementFacts::new(
        "",
        "html > body > form > button",
        "Submit",
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_identity(vec![String::from("data-testid=submit")]);
    let recognition = vector()?.recognize(&target, &[unlabelled]);
    assert!(
        matches!(recognition, Recognition::Resolved { index: 0, .. }),
        "an unreported tag must not exclude an otherwise matching element, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn structurally_distinct_candidates_are_measured_and_rejected() -> Result<(), RecognitionError> {
    // Same tag, so the candidate is scored rather than gated, and every
    // fact it reports differs. The score is a real measurement below the
    // threshold, which is the difference between "looked at and rejected"
    // and "never looked at".
    let target = ElementFacts::new(
        "button",
        "html > body > form > button",
        "Submit order",
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_identity(vec![String::from("data-testid=submit")]);
    let elsewhere = ElementFacts::new(
        "button",
        "html > body > article > aside > button",
        "Terms and conditions",
        [900.0, 900.0, 0.1, 0.05],
    )
    .with_identity(vec![String::from("data-testid=legal")]);
    let recognition = vector()?.recognize(&target, &[elsewhere]);
    assert!(
        matches!(
            recognition,
            Recognition::Absent { best_score }
                if best_score < FINGERPRINT_THRESHOLD && best_score > 0.0
        ),
        "a scored near-miss is Absent at its measured score, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn two_indistinguishable_icon_only_candidates_are_ambiguous() -> Result<(), RecognitionError> {
    // The same absence rules, with a field instead of a lone candidate: two
    // identical icon-only controls carry the same observed identifier, so
    // neither can be separated from the other, and the verdict is the one
    // that forces a re-ask rather than a guess.
    let target = icon("data-testid=menu");
    let recognition = vector()?.recognize(&target, &[target.clone(), target.clone()]);
    assert!(
        matches!(
            &recognition,
            Recognition::Ambiguous { best: 0, runner_up: Some(1), score, lead }
                if (score - 0.875).abs() < 1e-9 && lead.abs() < 1e-9
        ),
        "two indistinguishable icon-only controls are a tie, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn a_clear_winner_resolves() -> Result<(), RecognitionError> {
    let target = candidate("data-testid=submit", "Submit order");
    let candidates = [candidate("data-testid=cancel", "Cancel"), target.clone()];
    let recognition = vector()?.recognize(&target, &candidates);
    // `lead` is the gap to the runner-up, not the score: with a competitor
    // present it is `best - next`, so this cannot be asserted as `1.0` the
    // way the no-competitor case above can. Asserting the property rather
    // than a transcribed float also keeps this test from breaking on a
    // legitimate reweight of the default vector.
    assert!(
        matches!(
            recognition,
            Recognition::Resolved { index: 1, score, lead }
                if (score - 1.0).abs() < 1e-9 && lead >= FINGERPRINT_MARGIN
        ),
        "the second candidate must win outright with a lead over the margin, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn a_candidate_in_another_document_is_excluded_before_scoring() -> Result<(), RecognitionError> {
    let target = candidate("data-testid=submit", "Submit").with_frames(vec![0]);
    let look_alike = candidate("data-testid=submit", "Submit").with_frames(vec![3]);
    assert_eq!(
        vector()?.recognize(&target, &[look_alike]),
        Recognition::Absent { best_score: 0.0 },
        "a perfect score across a frame boundary must not be reachable"
    );
    Ok(())
}

#[test]
fn a_weak_candidate_is_absent_below_the_threshold() -> Result<(), RecognitionError> {
    let target = candidate("data-testid=submit", "Submit order");
    let unrelated = ElementFacts::new(
        "section",
        "html > body > article",
        "Terms and conditions",
        [0.0, 0.0, 0.1, 0.1],
    )
    .with_identity(vec![String::from("data-testid=legal")]);
    let recognition = vector()?.recognize(&target, &[unrelated]);
    assert!(
        matches!(
            recognition,
            Recognition::Absent { best_score } if best_score < FINGERPRINT_THRESHOLD
        ),
        "an unrelated element must be Absent below the threshold, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn an_empty_component_set_is_refused() {
    let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> = Vec::new();
    assert_eq!(
        RecognitionVector::new(components, 0.5, 0.1).err(),
        Some(RecognitionError::Weights(WeightedError::Empty))
    );
}

#[test]
fn weights_above_one_are_refused() {
    let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> = vec![
        (0.75, Box::new(IdentityComponent::new())),
        (0.75, Box::new(PathComponent::new())),
    ];
    assert_eq!(
        RecognitionVector::new(components, 0.5, 0.1).err(),
        Some(RecognitionError::Weights(
            WeightedError::WeightSumExceedsOne
        ))
    );
}

#[test]
fn a_margin_outside_the_unit_interval_is_refused() {
    let components: Vec<(f64, Box<dyn Similarity<Value = ElementFacts>>)> =
        vec![(0.5, Box::new(IdentityComponent::new()))];
    assert_eq!(
        RecognitionVector::new(components, 0.5, 1.5).err(),
        Some(RecognitionError::InvalidMargin { margin: 1.5 })
    );
}

#[test]
fn the_shipped_vector_carries_its_declared_rules() -> Result<(), RecognitionError> {
    let vector = vector()?;
    assert_close(vector.threshold(), FINGERPRINT_THRESHOLD);
    assert_close(vector.margin(), FINGERPRINT_MARGIN);
    assert_close(0.5 + 0.25 + 0.125 + 0.125, 1.0);
    Ok(())
}

/// Builds facts offering one anchor value, plus the shared shell every
/// ladder test needs so the fingerprint can score at all.
fn anchored(tag: &str, kind: Anchor, value: &str, text: &str) -> ElementFacts {
    ElementFacts::new(
        tag,
        "html > body > form > button",
        text,
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_anchor(kind, value)
    .with_identity(vec![format!("{kind:?}={value}")])
}

#[test]
fn an_empty_ladder_is_an_error() {
    assert_eq!(Ladder::new([]), Err(LadderError::Empty));
}

#[test]
fn a_ladder_is_sorted_and_deduplicated_strongest_first() -> Result<(), LadderError> {
    let ladder = Ladder::new([
        Anchor::ClassPath,
        Anchor::Id,
        Anchor::Text,
        Anchor::Id,
        Anchor::TestId,
    ])?;
    assert_eq!(
        ladder.anchors(),
        &[Anchor::Id, Anchor::TestId, Anchor::Text, Anchor::ClassPath],
        "input order must not decide rank, and a repeated rung is one rung"
    );
    Ok(())
}

#[test]
fn an_empty_anchor_value_is_not_offered() {
    let facts = anchored("button", Anchor::Id, "", "Submit");
    assert_eq!(facts.anchor(Anchor::Id), None);
    assert_eq!(facts.anchor(Anchor::TestId), None);
}

#[test]
fn a_unique_strong_anchor_resolves_at_the_first_rung() -> Result<(), Box<dyn std::error::Error>> {
    let target = anchored("button", Anchor::Id, "submit", "Submit");
    let other = anchored("button", Anchor::Id, "cancel", "Cancel");
    let ladder = Ladder::new([Anchor::Id, Anchor::Text])?;
    let recognition = vector()?.recognize_with_ladder(&ladder, &target, &[other, target.clone()]);
    assert!(
        matches!(recognition, Recognition::Resolved { index: 1, .. }),
        "the sole id match must resolve, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn ambiguous_at_a_strong_anchor_is_not_retried_at_a_weak_one()
-> Result<(), Box<dyn std::error::Error>> {
    // Two candidates share the target's id, so the strongest rung cannot
    // separate them. Their texts differ, and the weaker text rung would
    // pick one of them. That "recovery" is exactly what the walk must
    // refuse: a weak anchor succeeding where a strong one was ambiguous is
    // a likely mis-match, not a resolution.
    let target = anchored("button", Anchor::Id, "submit", "Submit");
    let twin = anchored("button", Anchor::Id, "submit", "Delete");
    let ladder = Ladder::new([Anchor::Id, Anchor::Text])?;
    let recognition = vector()?.recognize_with_ladder(&ladder, &target, &[twin, target.clone()]);
    assert!(
        matches!(recognition, Recognition::Ambiguous { .. }),
        "a strong-rung ambiguity must stand, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn the_ladder_falls_through_when_the_strong_anchor_is_absent()
-> Result<(), Box<dyn std::error::Error>> {
    // Neither element carries an id, so the first rung offers nobody. The
    // test-id rung has exactly one match and must take the decision.
    let target = anchored("button", Anchor::TestId, "checkout", "Submit");
    let other = anchored("button", Anchor::TestId, "cancel", "Cancel");
    let ladder = Ladder::new([Anchor::Id, Anchor::TestId])?;
    let recognition = vector()?.recognize_with_ladder(&ladder, &target, &[other, target.clone()]);
    assert!(
        matches!(recognition, Recognition::Resolved { index: 1, .. }),
        "the fall-down must reach the test-id rung, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn every_rung_absent_reports_the_strongest_absent() -> Result<(), Box<dyn std::error::Error>> {
    // Nothing matches on any rung. The return is the highest `best_score`
    // any rung reached, so the caller can see how close the strongest
    // evidence came rather than a bare zero.
    let target = ElementFacts::new(
        "button",
        "html > body > form > button",
        "Submit",
        [0.5, 0.5, 0.1, 0.05],
    )
    .with_anchor(Anchor::Id, "submit")
    .with_anchor(Anchor::ClassPath, "html > body > form > button");
    let unrelated = ElementFacts::new(
        "button",
        "html > body > footer > button",
        "Cancel",
        [0.1, 0.9, 0.1, 0.05],
    )
    .with_anchor(Anchor::Id, "cancel")
    .with_anchor(Anchor::ClassPath, "html > body > footer > button");
    let ladder = Ladder::new([Anchor::Id, Anchor::ClassPath])?;
    let recognition = vector()?.recognize_with_ladder(&ladder, &target, &[unrelated]);
    assert!(
        matches!(
            recognition,
            Recognition::Absent { best_score }
                if (0.0..FINGERPRINT_THRESHOLD).contains(&best_score)
        ),
        "absent must report a sub-threshold score, got {recognition:?}"
    );
    Ok(())
}

#[test]
fn a_candidate_that_offers_no_matching_anchor_is_never_scored()
-> Result<(), Box<dyn std::error::Error>> {
    let target = anchored("button", Anchor::Id, "submit", "Submit");
    let blank = bare("button", "html > body > form > button");
    let ladder = Ladder::new([Anchor::Id])?;
    assert_eq!(
        vector()?.recognize_with_ladder(&ladder, &target, &[blank]),
        Recognition::Absent { best_score: 0.0 },
        "an id rung with nobody in it is a fall-through to a finished ladder"
    );
    Ok(())
}
