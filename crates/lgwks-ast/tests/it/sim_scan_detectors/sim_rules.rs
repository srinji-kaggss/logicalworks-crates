//! The per-rule families: ALLOW-SILENCE, long-try-chain and tautological-doc,
//! each swept over its own seeds and held to the model.

use crate::sim::Rng;

use super::allow::{allow_block, cfg_attr_block, include_block, spelled_block, unreadable_block};
use super::model::{
    ALLOW, Block, CHAIN, DOC, DocSpec, Drawn, LOUD_LEVELS, Lints, PLACEMENTS, PUBLIC_DOC,
    Placement, SEEDS, SPELLINGS, SPREAD, Seen, Shape, Spelling, TestResult, agree, assemble,
    chain_block, doc_block, scanned, several, some_line_exceeds, some_line_holds, sweep,
    swept_tally, trait_block,
};

/// The spellings that write a raw identifier somewhere.
const RAW_SPELLINGS: [Spelling; 3] = [Spelling::RawHead, Spelling::RawLints, Spelling::RawBoth];

/// Placements inside a macro: the body of a `macro_rules!`, and the arguments of
/// an invocation.
const MACRO_PLACEMENTS: [Placement; 2] = [Placement::MacroBody, Placement::MacroArgument];

/// Code that holds attribute-shaped and `#`-shaped tokens inside macros and
/// silences nothing: other attributes, a loud level, interpolation, and a call to
/// a method named like a silencing level.
const MACRO_CODE: &str = r#"macro_rules! carrier {
    ($name:ident) => {
        #[derive(Debug)]
        #[doc = "carries"]
        #[cfg(test)]
        #[must_use]
        #[allow]
        #[deny(dead_code)]
        #![warn(unused)]
        struct $name;
    };
}
fn interpolate(value: Option<u8>, name: &str) {
    let first = quote! { #name };
    let second = quote! { #(#name)* };
    assert!(value.expect(name) > 0);
    drop((first, second));
}
"#;

// ── ALLOW-SILENCE ───────────────────────────────────────────────────────────

/// What the scan reports for `block` alone in a file, as evidence text, once it
/// agrees with the model.
fn reported_by(seed: u64, block: Block) -> Drawn<Vec<String>> {
    let file = assemble(&[block], &[0], &[0])?;
    Ok(agree(seed, &file)?
        .into_iter()
        .map(|(_, _, snippet)| snippet)
        .collect())
}

/// Whether every list in `reported` is the same as the one after it.
fn all_alike(reported: &[Vec<String>]) -> bool {
    reported.windows(2).all(|pair| match *pair {
        [ref left, ref right] => left == right,
        _ => false,
    })
}

/// An `allow` reports every lint it names, in order, clippy's and rustc's
/// alike: a silenced lint is a silenced lint, whatever tool owns it (#699).
#[test]
fn allow_lists_report_every_lint_in_order() -> TestResult {
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

/// A rustc lint — `dead_code`, `unused_variables` — is silence exactly as a
/// clippy lint is.
#[test]
fn a_rustc_lint_is_silence() -> TestResult {
    let swept = sweep(0xa110_0002, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            allow_block(rng, index, placement, "allow", Lints::OtherOnly)
        })
    })?;
    assert!(swept_tally(&swept, ALLOW) > 0, "a rustc lint is silence");
    Ok(())
}

/// `expect` suppresses the emission the way `allow` does, so it is the same
/// finding.
#[test]
fn an_expect_is_silence_as_surely_as_an_allow() -> TestResult {
    let swept = sweep(0xa110_0006, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            allow_block(rng, index, placement, "expect", Lints::Mixed)
        })
    })?;
    assert!(
        swept_tally(&swept, ALLOW) > 0,
        "an expectation silences a lint"
    );
    Ok(())
}

/// The same attribute inside a `cfg_attr` silences under a condition, which is
/// a silence all the same.
#[test]
fn a_cfg_attr_attribute_is_silence() -> TestResult {
    let swept = sweep(0xa110_0007, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            cfg_attr_block(rng, index, placement, "allow", Lints::Mixed)
        })
    })?;
    assert!(
        swept_tally(&swept, ALLOW) > 0,
        "a cfg_attr allow reported a silence"
    );
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
            reported.push(reported_by(seed, block)?);
        }
        assert!(
            all_alike(&reported),
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

/// A raw identifier is the plain identifier it prefixes, so `#[r#allow(..)]`,
/// `#[r#cfg_attr(.., r#allow(..))]` and `#[allow(r#dead_code)]` are the real
/// attributes to rustc and silence exactly as the plain ones do (#758).
#[test]
fn a_raw_identifier_spelling_is_still_silence() -> TestResult {
    let swept = sweep(0xa110_0008, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            let level = if rng.coin() { "allow" } else { "expect" };
            let spelling = *rng.pick_named("raw spellings", &RAW_SPELLINGS)?;
            let nested = rng.coin();
            spelled_block(rng, index, placement, level, Lints::Mixed, nested, spelling)
        })
    })?;
    assert!(
        swept_tally(&swept, ALLOW) > 0,
        "a raw spelling was reported"
    );
    for raw in ["r#allow(", "r#expect(", "r#cfg_attr(", "r#dead_code"] {
        assert!(
            swept.iter().any(|swept| swept.0.source.contains(raw)),
            "the sweep wrote `{raw}`"
        );
    }
    Ok(())
}

/// One list, written in each spelling, reports the same lints: spelling is
/// never evidence of anything but how the author typed it (#758).
#[test]
fn a_spelling_never_changes_what_is_reported() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xa110_0009 ^ index.wrapping_mul(SPREAD);
        let mut reported = Vec::new();
        for spelling in SPELLINGS {
            // A fresh stream per spelling: the list is drawn before the spelling
            // is read, so every spelling writes the same list.
            let mut rng = Rng::new(seed);
            let nested = index % 2 == 0;
            let block = spelled_block(
                &mut rng,
                0,
                Placement::Item,
                "allow",
                Lints::Mixed,
                nested,
                spelling,
            )?;
            reported.push(reported_by(seed, block)?);
        }
        assert!(
            all_alike(&reported),
            "seed {seed:#x}: spellings reported different lints: {reported:?}"
        );
    }
    Ok(())
}

/// An attribute inside a `macro_rules!` body or an invocation's arguments is
/// inert text until the macro expands, and expansion leaves it on the item, so
/// it silences the lint all the same (#758).
#[test]
fn an_allow_inside_a_macro_is_silence() -> TestResult {
    let swept = sweep(0xa110_000a, |rng| {
        several(rng, |rng, index| {
            let placement = *rng.pick_named("macro placements", &MACRO_PLACEMENTS)?;
            let level = if rng.coin() { "allow" } else { "expect" };
            let spelling = *rng.pick_named("spellings", &SPELLINGS)?;
            let nested = rng.coin();
            spelled_block(rng, index, placement, level, Lints::Mixed, nested, spelling)
        })
    })?;
    assert!(
        swept_tally(&swept, ALLOW) > 0,
        "a macro's allow was reported"
    );
    assert!(
        swept
            .iter()
            .all(|swept| swept.0.source.contains("macro_rules!")),
        "every file under test holds a macro"
    );
    Ok(())
}

/// A silencing attribute whose lints are a macro's metavariables cannot be read
/// to a lint, and a list that cannot be read is a silence that cannot be shown
/// empty: it is reported, with its lints shown as `..`, never skipped (#758).
#[test]
fn a_silencing_list_the_scan_cannot_read_is_reported_not_skipped() -> TestResult {
    let swept = sweep(0xa110_000b, |rng| several(rng, unreadable_block))?;
    let unread = swept
        .iter()
        .flat_map(|swept| swept.1.iter())
        .filter(|finding| finding.2.contains("(..)"))
        .count();
    assert_eq!(
        unread,
        swept_tally(&swept, ALLOW),
        "every finding of an unreadable list says so"
    );
    assert!(unread > 0, "the sweep reached a silencing level");
    assert!(
        swept.iter().any(|swept| swept.1.is_empty()),
        "and a loud level, which silences nothing"
    );
    Ok(())
}

/// The walker reads `.rs` files, so `include!` and `#[path]` of any other file
/// pull in source nothing scans; the file can hold a silence, and the scan says
/// it cannot see it. A `.rs` file is read, and data or generated code is not
/// authored source (#758).
#[test]
fn a_file_the_scan_never_reads_is_reported_wherever_it_is_pulled_in() -> TestResult {
    let swept = sweep(0xa110_000c, |rng| several(rng, include_block))?;
    let reported = swept_tally(&swept, ALLOW);
    let pulled_in = swept
        .iter()
        .map(|swept| {
            swept
                .0
                .source
                .lines()
                .filter(|line| line.contains("include") || line.contains("#[path"))
                .count()
        })
        .sum::<usize>();
    assert!(
        reported > 0 && reported < pulled_in,
        "both verdicts were reached: {reported} of {pulled_in}"
    );
    Ok(())
}

/// Attribute-shaped and `#`-shaped tokens inside a macro that silence nothing
/// report nothing: the token-tree reader accuses only a silencing level over a
/// bracket group, never `quote!`'s interpolation or a method named `expect`.
#[test]
fn macro_code_that_silences_nothing_reports_nothing() -> TestResult {
    assert_eq!(
        scanned(MACRO_CODE, "macro.rs")?,
        Vec::new(),
        "no finding in code that silences nothing"
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
