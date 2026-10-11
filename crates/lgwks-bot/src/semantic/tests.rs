//! Tests for the semantic tier: the resolver against stub embedders.

use super::{Alias, Embedder, EmbedderIdentity, SemanticError, SemanticPolicy, SemanticResolver};
use crate::session::{AnswerDomain, DegradedReason, MatchTier, Question, Resolution, Resolver};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// The failure a stub embedder reports, to exercise the degraded path.
#[derive(Debug)]
struct StubFailure;

impl fmt::Display for StubFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("stub embedder failure")
    }
}

impl std::error::Error for StubFailure {}

/// An embedder whose vectors the test chooses, counting its own calls.
///
/// The counter is shared through an `Rc` rather than exposed by an accessor
/// on the resolver: the point of several of these tests is that the model is
/// *not* consulted, and a public accessor added only to observe that would
/// be API surface justified by a test.
struct StubEmbedder {
    identity: EmbedderIdentity,
    vectors: BTreeMap<String, Vec<f32>>,
    fallback: Vec<f32>,
    calls: Rc<Cell<usize>>,
    failing: bool,
}

impl Embedder for StubEmbedder {
    type Error = StubFailure;

    fn identity(&self) -> &EmbedderIdentity {
        &self.identity
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Self::Error> {
        self.calls.set(self.calls.get().saturating_add(1));
        if self.failing {
            let refusal = Err(StubFailure);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "embed: returning an error to the caller");
            return refusal;
        }
        match self.vectors.get(text) {
            Some(vector) => Ok(vector.clone()),
            // The stub's declared contract, not a stand-in: a text it was not
            // handed answers with the fallback, which a test can make
            // deliberately wrong-length to reach the mismatch path.
            None => Ok(self.fallback.clone()),
        }
    }
}

const DIMENSION: usize = 2;

/// The two-option field most of these tests ask about.
///
/// Named once because the option *texts* are part of each premise: a test
/// that misspells one is measuring a field the lexicon resolves differently,
/// and the misspelling is invisible in a diff full of vectors.
fn the_field() -> Vec<String> {
    options(&["Repeat last order", "Cancel"])
}

/// Resolve `"the usual"` against [`the_field`] with the default policy.
///
/// The question and the field are the same in every test that only varies
/// the vectors, so the thing under test is the embedder and not a
/// differently-worded question.
fn resolve_with(embedder: StubEmbedder) -> crate::session::Verdict {
    SemanticResolver::new(embedder).resolve("the usual", &ask(&the_field()))
}

/// Two candidates whose cosines against `"the usual"` are `0.73` and `0.71`.
///
/// The second component of each is the positive root that makes the cosine
/// exact, so the margin these tests assert is the margin the arithmetic
/// produces rather than one the vectors were rounded to.
fn near_tie_vectors() -> Vec<(&'static str, Vec<f32>)> {
    vec![
        ("the usual", vec![1.0, 0.0]),
        (
            "Repeat last order",
            vec![0.73, (1.0_f32 - 0.73_f32.powi(2)).sqrt()],
        ),
        ("Cancel", vec![0.71, (1.0_f32 - 0.71_f32.powi(2)).sqrt()]),
    ]
}

/// Builds a stub embedder and the call counter the test watches.
///
/// `fallback` is returned for any text not in `vectors`, so a test can put a
/// deliberately wrong-length vector there and reach the mismatch path.
fn stub(
    vectors: Vec<(&str, Vec<f32>)>,
    fallback: Vec<f32>,
    failing: bool,
) -> Result<(StubEmbedder, Rc<Cell<usize>>), SemanticError> {
    let calls = Rc::new(Cell::new(0));
    let embedder = StubEmbedder {
        identity: EmbedderIdentity::new("stub-model", "digest-0123456789", DIMENSION)?,
        vectors: vectors
            .into_iter()
            .map(|(text, vector)| (text.to_owned(), vector))
            .collect(),
        fallback,
        calls: Rc::clone(&calls),
        failing,
    };
    Ok((embedder, calls))
}

fn options(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Names the question the resolver tests resolve against.
fn ask(options: &[String]) -> Question<'_> {
    Question::new("ask", options)
}

/// The two vectors a paraphrase relationship is built from, plus an
/// unrelated one. `"the usual"` and `"Repeat last order"` share no word and
/// no sound-alike key, which is the point: the lexicon cannot relate them,
/// and the embedder can.
fn paraphrase_vectors() -> Vec<(&'static str, Vec<f32>)> {
    vec![
        ("the usual", vec![1.0, 0.0]),
        ("Repeat last order", vec![1.0, 0.1]),
        ("Cancel", vec![0.0, 1.0]),
        ("Later", vec![-1.0, 0.0]),
    ]
}

/// Asserts two vectors match component-wise, without comparing floats by
/// equality: `float_cmp` is forbidden workspace-wide, and a paraphrase
/// assertion is about proximity rather than identity.
fn assert_vector(actual: &[f32], expected: &[f32]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "vector length changed: {actual:?} vs {expected:?}"
    );
    for (index, (actual_component, expected_component)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (actual_component - expected_component).abs() < 1e-6,
            "component {index} was {actual_component}, expected {expected_component}"
        );
    }
}

#[test]
fn an_exact_lexicon_match_never_consults_the_model() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let resolver = SemanticResolver::new(embedder);

    let resolution = resolver
        .resolve("yes", &ask(&options(&["yes", "no"])))
        .into_resolution();

    assert_eq!(
        resolution,
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        },
        "the lexicon's verdict is returned unchanged"
    );
    assert_eq!(
        calls.get(),
        0,
        "an exact match must not reach the embedder at all"
    );
    Ok(())
}

#[test]
fn a_lexicon_tie_is_not_rescored_by_the_model() -> Result<(), Box<dyn std::error::Error>> {
    // `Jaccard` scores both options identically against a shared word, so the
    // lexicon reports a tie. The model is far more decisive, and its opinion
    // is exactly what must not be heard here: a tie is a question to put back
    // to the person, not a case for a model to settle.
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let resolver = SemanticResolver::new(embedder);

    let resolution = resolver
        .resolve("order", &ask(&options(&["order", "order"])))
        .into_resolution();

    assert!(
        matches!(resolution, Resolution::Ambiguous { .. }),
        "the lexicon's ambiguity survives, got {resolution:?}"
    );
    assert_eq!(
        calls.get(),
        0,
        "an ambiguous verdict must not reach the model"
    );
    Ok(())
}

#[test]
fn a_phrase_the_lexicon_cannot_reach_resolves_semantically()
-> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let resolver = SemanticResolver::new(embedder);
    let options = options(&["Repeat last order", "Cancel"]);

    // Establish the premise: the lexicon alone cannot reach this phrase.
    let lexicon = crate::language::LanguageResolver::new();
    assert!(
        matches!(
            lexicon
                .resolve("the usual", &ask(&options))
                .into_resolution(),
            Resolution::Absent { .. }
        ),
        "\"the usual\" must not be reachable by letters alone, or this test proves nothing"
    );

    let resolution = resolver
        .resolve("the usual", &ask(&options))
        .into_resolution();

    assert!(
        matches!(
            resolution,
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Semantic,
                ..
            }
        ),
        "expected a semantic resolution of option 0, got {resolution:?}"
    );
    assert!(
        calls.get() > 0,
        "the model is what resolved it, so it must have been consulted"
    );
    Ok(())
}

#[test]
fn two_equally_close_options_are_ambiguous_rather_than_guessed()
-> Result<(), Box<dyn std::error::Error>> {
    let (embedder, _calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let resolver = SemanticResolver::new(embedder);

    // Both options are the same vector, so no margin can separate them. The
    // tier must report the tie rather than let index order decide, which is
    // the whole reason the verdict is three-way.
    let resolution = resolver
        .resolve(
            "the usual",
            &ask(&options(&["Repeat last order", "Repeat last order"])),
        )
        .into_resolution();

    assert!(
        matches!(&resolution, Resolution::Ambiguous { tied, .. } if *tied == vec![0, 1]),
        "both options are still in play, got {resolution:?}"
    );
    Ok(())
}

#[test]
fn a_runner_up_below_threshold_still_counts_against_the_margin()
-> Result<(), Box<dyn std::error::Error>> {
    // Two cosines constructed to be exactly `0.73` and `0.71` against the
    // utterance: the required margin is `0.05` and the observed lead is
    // `0.02`. The runner-up misses the `0.72` threshold by one hundredth,
    // which is exactly what used to delete it. A threshold says an option
    // may not win; it does not make the proximity that was measured vanish.
    let (embedder, _) = stub(near_tie_vectors(), vec![0.0, 1.0], false)?;
    let verdict = resolve_with(embedder);
    assert!(
        matches!(verdict.resolution(), Resolution::Ambiguous { tied, .. } if *tied == vec![0, 1]),
        "the near tie must be reported as one, got {verdict:?}"
    );
    Ok(())
}

#[test]
fn a_below_threshold_runner_up_is_reported_in_the_lead() -> Result<(), Box<dyn std::error::Error>> {
    // The same field with a wide margin: the winner is decisive, and the
    // number it reports must be the `0.02` it actually holds, not the `0.73`
    // of its own score reported over an emptied field.
    let (embedder, _) = stub(near_tie_vectors(), vec![0.0, 1.0], false)?;
    let policy = SemanticPolicy::new(0.72, 0.01)?;
    let verdict =
        SemanticResolver::with_policy(embedder, policy).resolve("the usual", &ask(&the_field()));
    assert!(
        matches!(
            verdict.resolution(),
            Resolution::Resolved {
                index: 0,
                score,
                lead,
                ..
            } if (*score - 0.73).abs() < 1e-6 && (*lead - 0.02).abs() < 1e-6
        ),
        "the lead must be the measured gap over the runner-up, got {verdict:?}"
    );
    Ok(())
}

#[test]
fn an_option_below_the_threshold_is_not_a_candidate() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let resolver = SemanticResolver::new(embedder);

    // Both options are orthogonal or opposed to the utterance, so the tier
    // has nothing to say and must say nothing rather than pick the least bad.
    let resolution = resolver
        .resolve("the usual", &ask(&options(&["Cancel", "Later"])))
        .into_resolution();

    assert!(
        matches!(resolution, Resolution::Absent { .. }),
        "expected the semantic tier to stay silent, got {resolution:?}"
    );
    assert!(calls.get() > 0, "the tier did look, and found nothing");
    Ok(())
}

#[test]
fn a_broken_competitor_cannot_manufacture_semantic_confidence()
-> Result<(), Box<dyn std::error::Error>> {
    // GitHub issue #50's counterexample. The utterance and the winning
    // option are the same vector, and the competitor's embedding is
    // all-zero — the width is right, so the dimension check waves it
    // through. Skipping it used to leave a one-option field, and the
    // survivor reported its entire score as a lead: `Resolved { index: 0,
    // tier: Semantic, score: 1.0, lead: 1.0 }`. That lead was never held
    // over anything. Missing evidence is not evidence that the competitor
    // is distant.
    let (embedder, _) = stub(
        vec![
            ("the usual", vec![1.0, 0.0]),
            ("Repeat last order", vec![1.0, 0.0]),
            ("Cancel", vec![0.0, 0.0]),
        ],
        vec![0.0, 1.0],
        false,
    )?;
    let verdict = resolve_with(embedder);
    assert_eq!(
        verdict.into_resolution(),
        Resolution::Degraded {
            reason: DegradedReason::UnmeasurableEmbedding,
        },
        "an unmeasured competitor cannot leave the field intact"
    );
    Ok(())
}

#[test]
fn a_degenerate_utterance_degrades_rather_than_finding_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    // The other half of issue #50: when the *utterance* vector is zero or
    // non-finite, every comparison is invalid, and the old code reported
    // `Absent { best_score: 0.0 }` — the same verdict a healthy model
    // returns when it genuinely finds no matching meaning. The flow then
    // blamed the person's phrasing instead of reporting the failure.
    let options = options(&["Repeat last order", "Cancel"]);
    // The premise the whole test rests on: neither phrase is reachable by
    // letters alone, so the model is what the verdict comes from.
    let lexicon = crate::language::LanguageResolver::new();
    for phrase in ["the usual", "a second phrasing"] {
        assert!(
            matches!(
                lexicon.resolve(phrase, &ask(&options)).into_resolution(),
                Resolution::Absent { .. }
            ),
            "{phrase:?} must not be reachable by the lexicon, or this proves nothing"
        );
    }

    for broken in [
        vec![0.0, 0.0],
        vec![f32::NAN, 0.0],
        vec![f32::INFINITY, 0.0],
    ] {
        let (embedder, _) = stub(
            vec![
                ("the usual", broken.clone()),
                ("a second phrasing", vec![1.0, 0.1]),
                ("Repeat last order", vec![1.0, 0.1]),
                ("Cancel", vec![0.0, 1.0]),
            ],
            vec![0.0, 1.0],
            false,
        )?;
        let resolver = SemanticResolver::new(embedder);

        let verdict = resolver.resolve("the usual", &ask(&options));
        assert_eq!(
            verdict.into_resolution(),
            Resolution::Degraded {
                reason: DegradedReason::UnmeasurableEmbedding,
            },
            "a {broken:?} utterance vector is not an absence of meaning"
        );

        // Recovery with valid measurements: the same resolver, an utterance
        // whose vector carries a direction, resolves normally. The tier is
        // not poisoned by the previous failure.
        let healthy = resolver.resolve("a second phrasing", &ask(&options));
        assert!(
            matches!(
                healthy.resolution(),
                Resolution::Resolved {
                    index: 0,
                    tier: MatchTier::Semantic,
                    score,
                    lead,
                } if (*score - 1.0).abs() < 1e-6 && *lead > 0.8
            ),
            "a valid utterance still resolves after a degraded one, got {healthy:?}"
        );
    }
    Ok(())
}

#[test]
fn an_invalid_embedding_at_either_end_of_the_field_degrades()
-> Result<(), Box<dyn std::error::Error>> {
    // Position must not matter. A first-competitor failure and a
    // last-competitor failure are the same failure: the comparison set is
    // one member short.
    let cases = [
        ("Repeat last order", vec![0.0, 0.0]),
        ("Later", vec![f32::INFINITY, 0.0]),
    ];
    for (broken_option, broken_vector) in cases {
        let (embedder, _) = stub(
            vec![
                ("the usual", vec![1.0, 0.0]),
                ("Repeat last order", vec![1.0, 0.1]),
                ("Cancel", vec![0.0, 1.0]),
                ("Later", vec![-1.0, 0.0]),
                (broken_option, broken_vector),
            ],
            vec![0.0, 1.0],
            false,
        )?;
        let verdict = SemanticResolver::new(embedder).resolve(
            "the usual",
            &ask(&options(&["Repeat last order", "Cancel", "Later"])),
        );
        assert_eq!(
            verdict.into_resolution(),
            Resolution::Degraded {
                reason: DegradedReason::UnmeasurableEmbedding,
            },
            "an invalid embedding for {broken_option} must stop the decision"
        );
    }
    Ok(())
}

#[test]
fn a_field_of_unmeasurable_vectors_is_degraded_not_absent() -> Result<(), Box<dyn std::error::Error>>
{
    // Every candidate invalid. The old code skipped all of them and
    // returned `Absent { best_score: 0.0 }`, which is the number a healthy
    // model returns for a field it looked at and rejected.
    let (embedder, _) = stub(
        vec![
            ("the usual", vec![1.0, 0.0]),
            ("Repeat last order", vec![0.0, 0.0]),
            ("Cancel", vec![f32::NAN, 0.0]),
        ],
        vec![0.0, 1.0],
        false,
    )?;
    let verdict = resolve_with(embedder);
    assert_eq!(
        verdict.into_resolution(),
        Resolution::Degraded {
            reason: DegradedReason::UnmeasurableEmbedding,
        },
        "no comparison was made, so no best score exists"
    );
    Ok(())
}

#[test]
fn an_unmeasurable_embedding_is_named_apart_from_a_missing_embedder() {
    // Two different repairs: restore the dependency, versus find whatever
    // produced a vector with no direction. The verdict has to say which,
    // and the rendered record is what an operator reads.
    assert_ne!(
        DegradedReason::UnmeasurableEmbedding,
        DegradedReason::EmbedderUnavailable,
        "the two causes must not collapse into one verdict"
    );
    assert_ne!(
        DegradedReason::UnmeasurableEmbedding.to_string(),
        DegradedReason::EmbedderUnavailable.to_string(),
        "and must not render identically in the transcript"
    );
}

#[test]
fn a_failing_embedder_is_degraded_rather_than_absent() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, _calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], true)?;
    let resolution = resolve_with(embedder).into_resolution();

    assert_eq!(
        resolution,
        Resolution::Degraded {
            reason: DegradedReason::EmbedderUnavailable,
        },
        "a failed embedder is not a person who said something unclear"
    );
    Ok(())
}

#[test]
fn a_vector_of_the_wrong_length_is_degraded_rather_than_scored()
-> Result<(), Box<dyn std::error::Error>> {
    // The fallback is three wide against a declared dimension of two, so any
    // option not named in `vectors` comes back the wrong size. Scoring it
    // would compare unlike things; this asserts it is reported instead.
    let (embedder, _calls) = stub(
        vec![("the usual", vec![1.0, 0.0])],
        vec![0.0, 1.0, 2.0],
        false,
    )?;
    let resolution = resolve_with(embedder).into_resolution();

    assert_eq!(
        resolution,
        Resolution::Degraded {
            reason: DegradedReason::EmbedderUnavailable,
        },
        "a provider that contradicts its own declared width is broken, not silent"
    );
    Ok(())
}

#[test]
fn a_learned_alias_moves_a_phrase_off_the_model() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let mut resolver = SemanticResolver::new(embedder);
    let options = options(&["Repeat last order", "Cancel"]);

    assert_eq!(
        resolver.learn("ask", "the usual", "Repeat last order"),
        None,
        "the first binding for a phrase has no predecessor"
    );

    let resolution = resolver
        .resolve("the usual", &ask(&options))
        .into_resolution();
    assert!(
        matches!(
            resolution,
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score,
                lead,
            } if (score - 1.0).abs() < 1e-9 && (lead - score).abs() < 1e-9
        ),
        "a confirmed phrase resolves exactly from then on. The lead is the whole \
         score and that is the measurement, not a gap left unmeasured: an alias \
         puts exactly one candidate in the exact tier, and the lead is held over \
         the runner-up in the tier the question is answered in, so a lone exact \
         winner has none to lead. A below-tier competitor does not count against \
         it — see `the_lead_is_measured_against_the_highest_other_measured_score` \
         for the measurement itself. Got {resolution:?}"
    );
    assert_eq!(
        calls.get(),
        0,
        "and a confirmed correction leaves the model out of it entirely"
    );
    assert_eq!(resolver.learned(), 1);
    assert_eq!(
        resolver
            .forget("ask", "the usual")
            .as_ref()
            .map(Alias::option),
        Some("Repeat last order")
    );
    assert_eq!(resolver.learned(), 0);
    Ok(())
}

/// A withdrawn confirmation is a verdict, and the model is not asked to
/// overturn it. The embedder here is loaded with exactly the geometry that
/// would rescue the stale phrase — it places `"the usual"` next to
/// `"Repeat last order"` — and the assertion is that it is never consulted,
/// because a model that could rehabilitate a superseded binding would be a
/// second, unreviewable way for a withdrawn confirmation to select an
/// option.
#[test]
fn a_superseded_alias_is_not_handed_to_the_model() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;
    let mut resolver = SemanticResolver::new(embedder);
    assert_eq!(
        resolver.learn("ask", "the usual", "Repeat last order"),
        None
    );

    // The same question, after the option the person confirmed is gone.
    let edited = options(&["Delete account", "Keep account"]);
    let resolution = resolver
        .resolve("the usual", &ask(&edited))
        .into_resolution();
    assert_eq!(
        resolution,
        Resolution::StaleAlias {
            question: String::from("ask"),
            option: String::from("Repeat last order"),
        },
        "the semantic tier must return a stale verdict verbatim"
    );
    assert_eq!(
        calls.get(),
        0,
        "and must not spend the model trying to legitimate it"
    );
    Ok(())
}

/// A question read as values is never handed to the model.
///
/// The embedder here is loaded with exactly the geometry that would lose the
/// sign — `"-5"` sits closest to `"5"` — and the assertion is that it is
/// never consulted, because a similarity judgement over numerals is the
/// folded-sign defect reached through another tier.
#[test]
fn a_numeric_question_never_reaches_the_model() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(
        vec![
            ("-5", vec![1.0, 0.0]),
            ("5", vec![1.0, 0.1]),
            ("10", vec![0.0, 1.0]),
        ],
        vec![1.0, 0.0],
        false,
    )?;
    let resolver = SemanticResolver::new(embedder);
    let options = options(&["5", "10"]);
    let question = ask(&options).with_domain(AnswerDomain::Integer);

    assert_eq!(
        resolver.resolve("-5", &question).into_resolution(),
        Resolution::Absent { best_score: 0.0 },
        "a value the question does not offer is absent, not the number it resembles"
    );
    assert_eq!(
        calls.get(),
        0,
        "no tier may spend the model on relating two numbers"
    );
    assert_eq!(
        resolver.resolve("5", &question).into_resolution(),
        Resolution::Resolved {
            index: 0,
            tier: MatchTier::Exact,
            score: 1.0,
            lead: 1.0,
        },
        "and the values that are offered resolve exactly, by value"
    );
    assert_eq!(
        calls.get(),
        0,
        "with the model still untouched after a successful numeric answer"
    );
    Ok(())
}

#[test]
fn identity_refuses_a_record_that_could_not_be_reproduced() {
    assert_eq!(
        EmbedderIdentity::new("", "digest", 4),
        Err(SemanticError::UnnamedEmbedder)
    );
    assert_eq!(
        EmbedderIdentity::new("model", "", 4),
        Err(SemanticError::UndigestedEmbedder {
            name: String::from("model")
        })
    );
    assert_eq!(
        EmbedderIdentity::new("model", "digest", 0),
        Err(SemanticError::ZeroDimension {
            name: String::from("model")
        })
    );
    let identity = EmbedderIdentity::new("model", "digest", 4);
    assert!(
        identity.is_ok(),
        "a named, digested, sized model is accepted"
    );
}

#[test]
fn policy_refuses_a_threshold_outside_the_contract_interval() {
    // `NAN` is reported as its own case: an error carrying it could not be
    // compared to an identical copy of itself, so the variant that would
    // carry it must not exist.
    assert_eq!(
        SemanticPolicy::new(f64::NAN, 0.05),
        Err(SemanticError::NonFiniteThreshold)
    );
    assert_eq!(
        SemanticPolicy::new(f64::INFINITY, 0.05),
        Err(SemanticError::NonFiniteThreshold)
    );
    assert_eq!(
        SemanticPolicy::new(1.5, 0.05),
        Err(SemanticError::InvalidThreshold { threshold: 1.5 })
    );
    assert_eq!(
        SemanticPolicy::new(0.7, f64::NAN),
        Err(SemanticError::NonFiniteMargin)
    );
    assert_eq!(
        SemanticPolicy::new(0.7, 2.0),
        Err(SemanticError::InvalidMargin { margin: 2.0 })
    );
    assert!(SemanticPolicy::new(0.7, 0.05).is_ok());
    assert!(
        (SemanticPolicy::DEFAULT.threshold() - 0.72).abs() < 1e-12,
        "the shipped threshold is the declared one"
    );
    assert_eq!(SemanticPolicy::default(), SemanticPolicy::DEFAULT);
}

#[test]
fn the_default_embed_batch_preserves_input_order() -> Result<(), Box<dyn std::error::Error>> {
    let (embedder, calls) = stub(paraphrase_vectors(), vec![0.0, 1.0], false)?;

    let batch = embedder
        .embed_batch(&["Cancel", "the usual"])
        .map_err(|_| "the stub was configured not to fail")?;

    let expected = [vec![0.0_f32, 1.0], vec![1.0, 0.0]];
    assert_eq!(batch.len(), expected.len(), "batch length changed");
    for (actual, want) in batch.iter().zip(expected.iter()) {
        assert_vector(actual, want);
    }
    assert_eq!(calls.get(), 2, "the default embeds one text at a time");
    Ok(())
}
