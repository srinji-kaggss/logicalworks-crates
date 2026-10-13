//! The tautological-doc detector: which docs are judged, the six-token ceiling, stop words, methods and test functions.

use super::*;

// ── tautological-doc ────────────────────────────────────────────────────────

/// A doc whose short content is at least half the name's stems is reported on
/// a `pub` function, and one under half is not.
#[test]
fn a_short_paraphrase_on_a_pub_function_is_reported() -> TestResult {
    let swept = sweep(0xd0c0_0001, |rng| {
        several(rng, |rng, index| doc_block(rng, index, PUBLIC_DOC))
    })?;
    let reported = swept_tally(&swept, DOC);
    let documented = swept
        .iter()
        .map(|swept| swept.0.source.matches("pub fn").count())
        .sum::<usize>();
    assert!(
        reported > 0 && reported < documented,
        "both verdicts were reached: {reported} of {documented}"
    );
    Ok(())
}

/// `pub(crate)` and private functions are not this rule's concern.
#[test]
fn crate_visible_and_private_functions_are_never_judged() -> TestResult {
    let swept = sweep(0xd0c0_0002, |rng| {
        several(rng, |rng, index| {
            let seen = if rng.coin() {
                Seen::Crate
            } else {
                Seen::Private
            };
            doc_block(
                rng,
                index,
                DocSpec {
                    seen,
                    paraphrase: true,
                    ..PUBLIC_DOC
                },
            )
        })
    })?;
    assert_eq!(swept_tally(&swept, DOC), 0, "only pub is judged");
    Ok(())
}

/// Above six content tokens a doc says more than its name, even when every
/// token is one of the name's stems.
#[test]
fn a_doc_over_six_content_tokens_is_never_judged() -> TestResult {
    let swept = sweep(0xd0c0_0003, |rng| {
        several(rng, |rng, index| {
            doc_block(
                rng,
                index,
                DocSpec {
                    fewest: 7,
                    most: 10,
                    paraphrase: true,
                    ..PUBLIC_DOC
                },
            )
        })
    })?;
    assert_eq!(swept_tally(&swept, DOC), 0, "a long doc is never judged");
    Ok(())
}

/// Stop words are removed before anything is counted: they neither dilute a
/// paraphrase nor make an empty doc into evidence.
#[test]
fn stop_words_never_count_toward_the_ratio() -> TestResult {
    let swept = sweep(0xd0c0_0004, |rng| {
        several(rng, |rng, index| {
            doc_block(
                rng,
                index,
                DocSpec {
                    fewest: 0,
                    stops: 10,
                    ..PUBLIC_DOC
                },
            )
        })
    })?;
    assert!(
        swept_tally(&swept, DOC) > 0,
        "a padded paraphrase was reported"
    );
    Ok(())
}

/// A `pub` method in an inherent `impl` is judged like a free function.
#[test]
fn methods_are_judged_like_free_functions() -> TestResult {
    let swept = sweep(0xd0c0_0005, |rng| {
        several(rng, |rng, index| {
            doc_block(
                rng,
                index,
                DocSpec {
                    method: true,
                    ..PUBLIC_DOC
                },
            )
        })
    })?;
    assert!(
        swept_tally(&swept, DOC) > 0,
        "a method paraphrase was reported"
    );
    Ok(())
}

/// A test function's doc is never judged, however much it paraphrases.
#[test]
fn a_test_function_is_never_judged() -> TestResult {
    let swept = sweep(0xd0c0_0006, |rng| {
        several(rng, |rng, index| {
            doc_block(
                rng,
                index,
                DocSpec {
                    test: true,
                    paraphrase: true,
                    ..PUBLIC_DOC
                },
            )
        })
    })?;
    assert_eq!(swept_tally(&swept, DOC), 0, "a test is never judged");
    Ok(())
}
