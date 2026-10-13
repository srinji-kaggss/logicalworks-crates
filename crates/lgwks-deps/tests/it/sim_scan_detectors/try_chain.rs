//! The long-try-chain detector: the three-operator threshold, let bindings, closures, if conditions, exempting attributes, nested functions and trait default methods.

use super::*;

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
