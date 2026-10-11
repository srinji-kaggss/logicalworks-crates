//! Deterministic simulation of the zero-gate source detectors.
//!
//! `scan_source` is the gate every estate repository runs over its own code, so
//! a false positive blocks a correct commit and a false negative passes the
//! defect the gate exists to catch. Its unit tests pin one example per rule;
//! this family generates whole files. One seed draws a run of blocks — an
//! `allow` list, a function of `?` statements, a documented function, or
//! filler — and each block carries what an independent model, written from the
//! rules' contracts rather than from the scanner, says the three syntax-only
//! detectors must report for it:
//!
//! - ALLOW-SILENCE for every clippy path an `allow` names, in order, wherever
//!   the attribute is written, test functions included;
//! - long-try-chain for a statement other than a `let` whose own expression
//!   holds more than three `?`, judged in the innermost enclosing function and
//!   never in one marked `test` or `test_*`;
//! - tautological-doc for a `pub` function, not a test, whose doc carries one
//!   to six content tokens of which at least half share a stem with its name.
//!
//! The scan must agree finding for finding: line, rule and evidence text. The
//! generator never writes the two error detectors' triggers (`return Err`, a
//! dropped `Result`), so a finding outside the model is a false positive and a
//! model finding the scan missed is a false negative.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `mixed_band_00..07` | 1,024 mixed files over eight bands agree with the model, replay to one trace, and reach every rule |
//! | `the_mixed_bands_cover_every_seed_exactly_once` | the bands partition the seed space |
//! | `allow_*`, `an_allow_*`, `a_lint_*`, `deny_*` | ALLOW-SILENCE: clippy paths only, `allow` only, every placement |
//! | `a_statement_*` .. `a_production_function_*` | long-try-chain: the threshold, `let`, closures, conditions, exemptions, nesting |
//! | `a_short_paraphrase_*` .. `a_test_function_is_never_judged` | tautological-doc: visibility, the six-token ceiling, stop words, methods, tests |
//! | `clean_files_*` .. `the_same_seed_*` | no false positives, permutation, line shifts, order, tenants, malformed input, disk, replay |

#![cfg(feature = "scan")]

use std::collections::BTreeSet;

use lgwks_ast::scan::{ScanError, scan_path, scan_source};
use lgwks_std::{hex, random};

use crate::scan_sim_model::{
    ALLOW, BANDS, Block, CHAIN, DOC, DocSpec, Drawn, File, Finding, LOUD_LEVELS, Lints,
    MAX_TRY_CHAIN, MIXED_SEEDS, PLACEMENTS, Placement, ROUNDS, SEEDS, SHAPES, SPREAD, Seen, Shape,
    TENANTS, TestResult, agree, allow_block, assemble, band_seeds, chain_block, doc_block,
    filler_block, mixed_band, mixed_blocks, mixed_file, mixed_receipt, mixed_seed, scanned,
    several, shuffle, some_line_exceeds, some_line_holds, sweep, swept_tally, trait_block,
};
use crate::sim::Rng;

macro_rules! mixed_family {
    ($($name:ident => $band:expr);+ $(;)?) => {
        $(
            /// A seeded band of mixed files, each agreeing with the model.
            #[test]
            fn $name() -> TestResult {
                mixed_band($band)
            }
        )+
    };
}

mixed_family!(
    mixed_band_00 => 0; mixed_band_01 => 1; mixed_band_02 => 2; mixed_band_03 => 3;
    mixed_band_04 => 4; mixed_band_05 => 5; mixed_band_06 => 6; mixed_band_07 => 7;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_mixed_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = BTreeSet::new();
    for band in 0..BANDS {
        for index in band_seeds(band)? {
            assert!(covered.insert(index), "seed {index} is in two bands");
        }
    }
    assert_eq!(
        u64::try_from(covered.len())?,
        MIXED_SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}

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

/// The doc spec every tautology family starts from: a public free function
/// whose doc carries one to six content tokens.
const PUBLIC_DOC: DocSpec = DocSpec {
    seen: Seen::Public,
    method: false,
    test: false,
    fewest: 1,
    most: 6,
    paraphrase: false,
    stops: 2,
};

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

// ── Cross-cutting ───────────────────────────────────────────────────────────

/// Files drawn only from the clean side of every rule report nothing: no
/// false positive across the seed space.
#[test]
fn clean_files_report_nothing() -> TestResult {
    let swept = sweep(0xc1ea_0001, |rng| {
        several(rng, |rng, index| match rng.below(4) {
            0 => {
                let placement = *rng.pick_named("placements", &PLACEMENTS)?;
                allow_block(rng, index, placement, "allow", Lints::OtherOnly)
            }
            1 => chain_block(rng, index, false, &SHAPES, MAX_TRY_CHAIN),
            2 => doc_block(
                rng,
                index,
                DocSpec {
                    fewest: 7,
                    most: 10,
                    ..PUBLIC_DOC
                },
            ),
            _ => filler_block(rng),
        })
    })?;
    assert!(
        swept.iter().all(|swept| swept.1.is_empty()),
        "a clean file reported a finding"
    );
    Ok(())
}

/// Each finding as `(block, offset into it, rule)`: what stays fixed when the
/// blocks move.
fn by_block(
    blocks: &[Block],
    file: &File,
    found: &[Finding],
) -> Drawn<Vec<(usize, usize, &'static str)>> {
    let mut placed = Vec::new();
    for &(line, rule, _) in found {
        let owner = file
            .starts
            .iter()
            .zip(blocks)
            .position(|(&start, block)| {
                line >= start && line < start.saturating_add(block.lines.len())
            })
            .ok_or("a finding outside every block")?;
        let start = file.starts.get(owner).copied().ok_or("a start per block")?;
        placed.push((owner, line.saturating_sub(start), rule));
    }
    placed.sort_unstable();
    Ok(placed)
}

/// A finding belongs to its item, not to its position: the same blocks in a
/// shuffled order report the same findings, each inside its own block.
#[test]
fn findings_follow_their_items_under_any_permutation() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xc1ea_0002 ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let blocks = mixed_blocks(&mut rng)?;
        let natural: Vec<usize> = (0..blocks.len()).collect();
        let mut shuffled = natural.clone();
        shuffle(&mut rng, &mut shuffled)?;
        let gaps = vec![0; blocks.len()];
        let mut placements = Vec::new();
        for order in [&natural, &shuffled] {
            let file = assemble(&blocks, order, &gaps)?;
            let found = agree(seed, &file)?;
            placements.push(by_block(&blocks, &file, &found)?);
        }
        assert_eq!(
            placements.first(),
            placements.last(),
            "seed {seed:#x}: findings moved between blocks under {shuffled:?}"
        );
    }
    Ok(())
}

/// Lines inserted before a block shift every finding in it by exactly their
/// count, and its function lines with it.
#[test]
fn inserted_lines_shift_every_finding_by_exactly_their_count() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xc1ea_0003 ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let blocks = mixed_blocks(&mut rng)?;
        let order: Vec<usize> = (0..blocks.len()).collect();
        let tight = assemble(&blocks, &order, &vec![0; blocks.len()])?;
        let loose_gaps: Vec<u32> = order.iter().map(|_| rng.between(1, 6)).collect();
        let loose = assemble(&blocks, &order, &loose_gaps)?;
        let tight_found = agree(seed, &tight)?;
        let loose_found = agree(seed, &loose)?;
        assert_eq!(
            by_block(&blocks, &tight, &tight_found)?,
            by_block(&blocks, &loose, &loose_found)?,
            "seed {seed:#x}: a gap moved a finding inside its block"
        );
    }
    Ok(())
}

/// Findings come back by line, then by rule, so a printer reads them in file
/// order without sorting.
#[test]
fn findings_are_ordered_by_line_then_rule() -> TestResult {
    for index in 0..SEEDS {
        let seed = mixed_seed(index ^ 0x0de4);
        let found = scanned(&mixed_file(seed)?.source, "sim.rs")?;
        assert!(
            found.windows(2).all(|pair| match *pair {
                [ref left, ref right] => (left.0, left.1) <= (right.0, right.1),
                _ => false,
            }),
            "seed {seed:#x}: findings out of order: {found:#?}"
        );
    }
    Ok(())
}

/// Sixty-four files scanned at once on their own threads, eight times each,
/// each report exactly their own findings: the scanner keeps no state between
/// calls for one tenant's file to leak into another's.
#[test]
fn concurrent_scans_of_distinct_files_each_get_their_own_findings() -> TestResult {
    let mut files = Vec::new();
    for index in 0..TENANTS {
        let seed = mixed_seed(index ^ 0x7e4a_0000);
        files.push((seed, mixed_file(seed)?));
    }
    let failures = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for &(seed, ref file) in &files {
            handles.push(scope.spawn(move || -> Result<(), String> {
                for round in 0..ROUNDS {
                    let observed = scanned(&file.source, &format!("tenant-{seed:x}.rs"))
                        .map_err(|error| format!("seed {seed:#x} round {round}: {error}"))?;
                    let verdict = if observed == file.expected {
                        Ok(())
                    } else {
                        Err(format!("seed {seed:#x} round {round}: {observed:#?}"))
                    };
                    verdict?;
                }
                Ok(())
            }));
        }
        let mut failures = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(failure)) => failures.push(failure),
                Err(_) => failures.push("a scanner thread panicked".to_owned()),
            }
        }
        failures
    });
    assert!(failures.is_empty(), "tenants disagreed: {failures:#?}");
    Ok(())
}

/// The findings depend on the source alone: the same text under two path
/// labels, scanned twice each, reports one list.
#[test]
fn a_scan_depends_on_the_source_alone() -> TestResult {
    for index in 0..SEEDS {
        let seed = mixed_seed(index ^ 0x1de0);
        let file = mixed_file(seed)?;
        let first = scanned(&file.source, "a.rs")?;
        for path in ["a.rs", "tenant/b.rs"] {
            assert_eq!(
                scanned(&file.source, path)?,
                first,
                "seed {seed:#x}: {path} reported differently"
            );
        }
    }
    Ok(())
}

/// A torn file — one unclosed parenthesis at a seeded block boundary — is
/// refused with its own path, never passed as clean.
#[test]
fn a_malformed_file_is_refused_with_its_path() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xbad0_0001 ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let mut blocks = mixed_blocks(&mut rng)?;
        let at = usize::try_from(rng.below(u32::try_from(blocks.len().saturating_add(1))?))?;
        let mut torn = Block::default();
        torn.line("fn torn(");
        blocks.insert(at, torn);
        let order: Vec<usize> = (0..blocks.len()).collect();
        let file = assemble(&blocks, &order, &vec![0; blocks.len()])?;
        let path = format!("tenant-{seed:x}.rs");
        let result = scan_source(&file.source, &path);
        assert!(
            matches!(result, Err(ScanError::Unparseable { path: ref named, .. }) if *named == path),
            "seed {seed:#x}: a torn file must be refused, not passed: {result:?}"
        );
    }
    Ok(())
}

/// A file on disk reports what its text reports, and a path that does not
/// exist is refused rather than passed.
#[test]
fn scan_path_reads_what_scan_source_reads() -> TestResult {
    let directory = std::env::temp_dir();
    // A drawn nonce, not the process id: the OS reuses ids, and several
    // checkouts on one host share this directory.
    let nonce = hex::encode(random::bytes::<8>()?);
    for index in 0..64_u64 {
        let seed = mixed_seed(index ^ 0xd15c);
        let file = mixed_file(seed)?;
        let path = directory.join(format!("lgwks-sim-scan-{nonce}-{seed:x}.rs"));
        std::fs::write(&path, &file.source)?;
        let from_disk = scan_path(&path);
        std::fs::remove_file(&path)?;
        let from_disk: Vec<Finding> = from_disk?
            .iter()
            .map(|hit| (hit.line(), hit.rule(), hit.snippet().to_owned()))
            .collect();
        assert_eq!(
            from_disk,
            agree(seed, &file)?,
            "seed {seed:#x}: disk and text differ"
        );
    }
    let missing = directory.join(format!("lgwks-sim-scan-{nonce}-missing.rs"));
    assert!(
        matches!(scan_path(&missing), Err(ScanError::Unparseable { .. })),
        "a missing file is refused"
    );
    Ok(())
}

/// The same seed replays to the same trace; distinct seeds diverge.
#[test]
fn the_same_seed_replays_and_distinct_seeds_diverge() -> TestResult {
    let mut hashes = BTreeSet::new();
    for index in 0..SEEDS {
        let seed = mixed_seed(index ^ 0x5eed_0000);
        let first = mixed_receipt(seed)?;
        assert_eq!(
            first,
            mixed_receipt(seed)?,
            "seed {seed:#x} replayed differently"
        );
        hashes.insert(first);
    }
    assert!(
        u64::try_from(hashes.len())? == SEEDS,
        "{SEEDS} seeds gave {} traces",
        hashes.len()
    );
    Ok(())
}
