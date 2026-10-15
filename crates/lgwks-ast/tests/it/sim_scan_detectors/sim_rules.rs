//! The per-rule families: ALLOW-SILENCE, long-try-chain and tautological-doc,
//! each swept over its own seeds and held to the model.

use crate::sim::Rng;

use super::model::{
    ALLOW, CHAIN, DOC, DocSpec, LOUD_LEVELS, Lints, PLACEMENTS, PUBLIC_DOC, Placement, SEEDS,
    SPREAD, Seen, Shape, TestResult, agree, allow_block, assemble, chain_block, doc_block, several,
    some_line_exceeds, some_line_holds, sweep, swept_tally, trait_block,
};

// ── ALLOW-SILENCE ───────────────────────────────────────────────────────────

/// An `allow` reports each clippy path it names, in order, and nothing for
/// the rest of its list.
#[test]
fn allow_lists_report_every_clippy_path_in_order() -> TestResult {
    let swept = sweep(0xa110_0001, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            allow_block(rng, index, placement, "allow", Lints::Mixed)
        })
    })?;
    assert!(
        swept_tally(&swept, ALLOW) > 0,
        "the family reported a silence"
    );
    Ok(())
}

/// A lint that is not clippy's — rustc's own, or another tool's path — is
/// never silence, however the list is written.
#[test]
fn a_lint_outside_clippy_is_never_silence() -> TestResult {
    let swept = sweep(0xa110_0002, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            allow_block(rng, index, placement, "allow", Lints::OtherOnly)
        })
    })?;
    assert_eq!(swept_tally(&swept, ALLOW), 0, "no rustc lint is silence");
    Ok(())
}

/// `deny`, `warn` and `forbid` raise a clippy lint rather than silence it.
#[test]
fn deny_warn_and_forbid_silence_nothing() -> TestResult {
    let swept = sweep(0xa110_0003, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            let level = *rng.pick_named("loud levels", &LOUD_LEVELS)?;
            allow_block(rng, index, placement, level, Lints::ClippyOnly)
        })
    })?;
    assert_eq!(
        swept_tally(&swept, ALLOW),
        0,
        "a raised lint is not silence"
    );
    Ok(())
}

/// One list written on an item, a `let`, a method and a test function
/// reports the same paths: the placement never hides a silence.
#[test]
fn an_allow_is_found_wherever_it_is_written() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xa110_0004 ^ index.wrapping_mul(SPREAD);
        let mut reported = Vec::new();
        for placement in PLACEMENTS {
            // A fresh stream per placement: the list is drawn before the
            // placement is read, so every placement writes the same list.
            let mut rng = Rng::new(seed);
            let block = allow_block(&mut rng, 0, placement, "allow", Lints::Mixed)?;
            let file = assemble(&[block], &[0], &[0])?;
            let snippets: Vec<String> = agree(seed, &file)?
                .into_iter()
                .map(|(_, _, snippet)| snippet)
                .collect();
            reported.push(snippets);
        }
        assert!(
            reported.windows(2).all(|pair| match *pair {
                [ref left, ref right] => left == right,
                _ => false,
            }),
            "seed {seed:#x}: placements reported different paths: {reported:?}"
        );
    }
    Ok(())
}

/// The rule has no test exemption: an `allow` on a test function is the same
/// silence as one on production code.
#[test]
fn an_allow_on_a_test_function_is_still_reported() -> TestResult {
    let swept = sweep(0xa110_0005, |rng| {
        several(rng, |rng, index| {
            allow_block(
                rng,
                index,
                Placement::TestFunction,
                "allow",
                Lints::ClippyOnly,
            )
        })
    })?;
    assert!(
        swept_tally(&swept, ALLOW) > 0,
        "a test function's allow was reported"
    );
    Ok(())
}

// ── long-try-chain ──────────────────────────────────────────────────────────

/// A statement holding three `?` passes and one holding four fails: the
/// threshold is contractual, and the sweep reaches both sides of it.
#[test]
fn a_statement_fires_only_above_three_operators() -> TestResult {
    let swept = sweep(0xc4a1_0001, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, false, &[Shape::Call], 7)
        })
    })?;
    assert!(swept_tally(&swept, CHAIN) > 0, "a long chain was reported");
    assert!(
        some_line_holds(&swept, 3) && some_line_holds(&swept, 4),
        "the sweep reached both sides of the threshold"
    );
    Ok(())
}

/// A `let` breaks the chain by contract, whatever its initializer holds.
#[test]
fn a_let_binding_never_fires_whatever_its_count() -> TestResult {
    let swept = sweep(0xc4a1_0002, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, false, &[Shape::Let], 12)
        })
    })?;
    assert_eq!(swept_tally(&swept, CHAIN), 0, "a let is never a chain");
    assert!(some_line_exceeds(&swept), "a let held more than three");
    Ok(())
}

/// A closure's `?` belong to the closure: the call that passes it counts
/// none of them, and each statement inside is judged on its own.
#[test]
fn a_closure_body_is_judged_statement_by_statement() -> TestResult {
    let swept = sweep(0xc4a1_0003, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, false, &[Shape::Closure], 7)
        })
    })?;
    assert!(swept_tally(&swept, CHAIN) > 0, "a closure statement fired");
    Ok(())
}

/// An `if` statement counts its condition, never its block, whose
/// statements are judged on their own.
#[test]
fn an_if_condition_counts_and_its_block_does_not() -> TestResult {
    let swept = sweep(0xc4a1_0004, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, false, &[Shape::If], 7)
        })
    })?;
    assert!(swept_tally(&swept, CHAIN) > 0, "a condition or body fired");
    Ok(())
}

/// `#[test]`, `#[tokio::test]` and any `#[test_*]` exempt every statement of
/// their function, nested blocks and closures included.
#[test]
fn exempting_attributes_silence_every_chain_in_their_function() -> TestResult {
    let swept = sweep(0xc4a1_0005, |rng| {
        several(rng, |rng, index| {
            chain_block(
                rng,
                index,
                true,
                &[Shape::Call, Shape::If, Shape::Closure],
                7,
            )
        })
    })?;
    assert_eq!(swept_tally(&swept, CHAIN), 0, "an exempt function is quiet");
    assert!(some_line_exceeds(&swept), "an exempt function held a chain");
    Ok(())
}

/// A function nested in a body reports its own line, and the statements after
/// it report the outer function's line again.
#[test]
fn a_nested_function_reports_its_own_line_and_restores_the_outer() -> TestResult {
    let swept = sweep(0xc4a1_0006, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, false, &[Shape::Call, Shape::NestedFn], 7)
        })
    })?;
    assert!(swept_tally(&swept, CHAIN) > 0, "a nested chain fired");
    Ok(())
}

/// A test function nested in production code is exempt, and the production
/// statements after it are judged again.
#[test]
fn a_test_function_nested_in_production_code_is_exempt() -> TestResult {
    let swept = sweep(0xc4a1_0007, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, false, &[Shape::Call, Shape::NestedTest], 7)
        })
    })?;
    assert!(swept_tally(&swept, CHAIN) > 0, "the outer statements fired");
    Ok(())
}

/// The innermost function decides: production code nested in a test function
/// is judged like any other.
#[test]
fn a_production_function_nested_in_a_test_is_inspected() -> TestResult {
    let swept = sweep(0xc4a1_0008, |rng| {
        several(rng, |rng, index| {
            chain_block(rng, index, true, &[Shape::NestedFn], 7)
        })
    })?;
    assert!(swept_tally(&swept, CHAIN) > 0, "the nested function fired");
    Ok(())
}

/// A trait's default method is judged like a free function, at the top level
/// and nested in a body; before the walk visited trait items, a top-level
/// default method was never judged and a nested one was charged to the
/// function around it.
#[test]
fn a_trait_default_method_is_judged_like_any_function() -> TestResult {
    let swept = sweep(0xc4a1_0009, |rng| {
        several(rng, |rng, index| {
            if rng.coin() {
                trait_block(rng, index, &[Shape::Call, Shape::If, Shape::Closure], 7)
            } else {
                chain_block(rng, index, false, &[Shape::Call, Shape::NestedTrait], 7)
            }
        })
    })?;
    assert!(
        swept_tally(&swept, CHAIN) > 0,
        "a default method's chain fired"
    );
    Ok(())
}

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
