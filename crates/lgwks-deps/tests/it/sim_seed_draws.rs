//! Seeded sweeps over the draws and receipts every policy family relies on.
//!
//! The policy families draw fixtures with `pick` and close each run with
//! `receipt`. Those two are this crate's additions to the shared seed
//! substrate, so their contracts are swept here rather than assumed by every
//! family: a draw can reach every element of its table, a draw consumes the
//! same stream whatever table it was asked about (so a seed replays the same
//! sequence after a fixture table grows or empties), and a receipt is refused
//! exactly when the run recorded nothing.

use crate::sim::{Rng, Trace, receipt};

use std::error::Error;
use std::ops::Range;

type TestResult = Result<(), Box<dyn Error>>;

/// Seeds per range: each test sweeps one contiguous, disjoint slice.
const SEEDS_PER_RANGE: u64 = 64;

/// The `index`-th slice of the seed space.
fn seeds(index: u64) -> Range<u64> {
    let first = index.saturating_mul(SEEDS_PER_RANGE);
    first..first.saturating_add(SEEDS_PER_RANGE)
}

#[test]
fn pick_reaches_every_element_range_0() -> TestResult {
    pick_reaches_every_element(seeds(0))
}

#[test]
fn pick_reaches_every_element_range_1() -> TestResult {
    pick_reaches_every_element(seeds(1))
}

#[test]
fn pick_reaches_every_element_range_2() -> TestResult {
    pick_reaches_every_element(seeds(2))
}

#[test]
fn pick_reaches_every_element_range_3() -> TestResult {
    pick_reaches_every_element(seeds(3))
}

#[test]
fn pick_consumes_one_draw_whatever_the_table_range_0() -> TestResult {
    pick_consumes_one_draw_whatever_the_table(seeds(0))
}

#[test]
fn pick_consumes_one_draw_whatever_the_table_range_1() -> TestResult {
    pick_consumes_one_draw_whatever_the_table(seeds(1))
}

#[test]
fn pick_consumes_one_draw_whatever_the_table_range_2() -> TestResult {
    pick_consumes_one_draw_whatever_the_table(seeds(2))
}

#[test]
fn pick_consumes_one_draw_whatever_the_table_range_3() -> TestResult {
    pick_consumes_one_draw_whatever_the_table(seeds(3))
}

#[test]
fn a_receipt_refuses_only_an_empty_trace_range_0() -> TestResult {
    a_receipt_refuses_only_an_empty_trace(seeds(0))
}

#[test]
fn a_receipt_refuses_only_an_empty_trace_range_1() -> TestResult {
    a_receipt_refuses_only_an_empty_trace(seeds(1))
}

#[test]
fn a_receipt_refuses_only_an_empty_trace_range_2() -> TestResult {
    a_receipt_refuses_only_an_empty_trace(seeds(2))
}

#[test]
fn a_receipt_refuses_only_an_empty_trace_range_3() -> TestResult {
    a_receipt_refuses_only_an_empty_trace(seeds(3))
}

/// Every element of a table of one to eight entries is drawn at least once in
/// 256 draws, on every seed.
fn pick_reaches_every_element(range: Range<u64>) -> TestResult {
    for seed in range {
        let mut rng = Rng::new(seed);
        for width in 1..=PREFIXES.len() {
            prefix_is_fully_drawn(seed, &mut rng, width)?;
        }
    }
    Ok(())
}

/// The table every prefix is cut from: each element is its own index.
const PREFIXES: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

/// 256 draws from the first `width` entries of [`PREFIXES`] reach every one.
fn prefix_is_fully_drawn(seed: u64, rng: &mut Rng, width: usize) -> TestResult {
    let table = PREFIXES.get(..width).ok_or("a prefix of the table")?;
    let mut seen = [false; 8];
    for _ in 0..256 {
        let drawn = *rng.pick_named("prefix", table)?;
        mark(&mut seen, drawn)?;
    }
    let missed = seen.get(..width).ok_or("the seen prefix")?;
    assert!(
        missed.iter().all(|hit| *hit),
        "seed {seed}: a table of {width} was never fully drawn: {missed:?}"
    );
    Ok(())
}

/// Marks `drawn` as seen, refusing an element outside the table.
fn mark(seen: &mut [bool; 8], drawn: u8) -> TestResult {
    let slot = seen
        .get_mut(usize::from(drawn))
        .ok_or("a drawn element inside the table")?;
    *slot = true;
    Ok(())
}

/// A draw from a table of any length — empty included — advances the stream
/// by exactly one step.
fn pick_consumes_one_draw_whatever_the_table(range: Range<u64>) -> TestResult {
    let tables: [&[u8]; 4] = [&[], &[1], &[1, 2, 3], &[0; 97]];
    for seed in range {
        let mut reference = Rng::new(seed);
        let mut drawing = Rng::new(seed);
        for step in 0..64_usize {
            let table = tables
                .get(
                    step.checked_rem(tables.len())
                        .ok_or("a non-empty set of tables")?,
                )
                .ok_or("a table index inside the set")?;
            let drawn = drawing.pick(table);
            assert_eq!(
                drawn.is_none(),
                table.is_empty(),
                "seed {seed} step {step}: only an empty table refuses a draw"
            );
            reference.next_u64();
            assert_eq!(
                reference.next_u64(),
                drawing.next_u64(),
                "seed {seed} step {step}: a draw from a table of {} moved the stream differently",
                table.len()
            );
        }
    }
    Ok(())
}

/// A receipt is refused exactly when the run recorded nothing, and one seed's
/// run always yields one receipt.
fn a_receipt_refuses_only_an_empty_trace(range: Range<u64>) -> TestResult {
    let run = |seed: u64| -> (u32, Trace) {
        let mut rng = Rng::new(seed);
        let facts = rng.below(4);
        let mut trace = Trace::new();
        for fact in 0..facts {
            trace.record_number("fact", rng.below(1_000).saturating_add(fact));
        }
        (facts, trace)
    };
    for seed in range {
        let (facts, trace) = run(seed);
        let (_, again) = run(seed);
        match (facts, receipt(&trace)) {
            (0, Err(_)) => {}
            (0, Ok(hash)) => {
                let refusal: TestResult =
                    Err(format!("seed {seed}: an empty trace was given receipt {hash:#x}").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_receipt_refuses_only_an_empty_trace: an empty run was receipted");
                return refusal;
            }
            (_, verdict) => {
                assert_eq!(
                    verdict?,
                    receipt(&again)?,
                    "seed {seed}: one seed's run yielded two receipts"
                );
            }
        }
    }
    Ok(())
}
