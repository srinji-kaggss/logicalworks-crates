//! The ALLOW-SILENCE detector: clippy paths in order, non-clippy lints, loud levels, every placement, and allows on test functions.

use super::*;

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
