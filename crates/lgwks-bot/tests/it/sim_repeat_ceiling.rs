//! Simulation family: repeated unchanged failure reaches a finite typed
//! intervention (acceptance row T29).
//!
//! Row T29 of `docs/orchestration-acceptance.spec.md`: "Repeated unchanged
//! failure reaches finite typed intervention, not infinite tool/model repairs;
//! new evidence is recorded and does not erase root spend."
//!
//! The row is the retry bound the Google SRE book asks of a client and the trip
//! threshold of a circuit breaker: failures are tolerated up to a declared
//! count, and past it the caller is told so by a typed value instead of being
//! handed another attempt. `proposal::repeated_unchanged_failure_reaches_a_finite_intervention_t29`
//! pins one ceiling of three. This family draws the repeat ceiling, the distinct
//! bound, where the evidence lands and how many attempts come after the ceiling.
//!
//! | Property | What a seed checks |
//! |---|---|
//! | tolerated up to the ceiling | the first `repeat` sightings of a fingerprint are admitted and counted exactly |
//! | typed past it | every later sighting is `NoProgress` naming the fingerprint and its running count, and `may_repair` is false |
//! | evidence is beside the spend | recording evidence moves neither the repetition count nor the root spend, and is itself counted |
//! | a different failure is different | distinct fingerprints are admitted while the ledger has room, and one more is `LedgerFull` naming how many it holds |
//! | the spend is the whole record | the root spend is every failure recorded, intervention or not |
//!
//! # What is real and what is seeded
//!
//! The ledger, its limits and its provenance are the shipped types. The seed
//! draws only the numbers; no clock is read and nothing waits.

#![cfg(feature = "script")]

use crate::band_family;
use crate::proposal_fixtures as support;
use crate::sim;

use std::error::Error;

use lgwks_bot::proposal::{Intervention, LedgerLimits, RepairLedger};

use sim::Band;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The largest repeat ceiling a seed draws.
const MOST_REPEAT: u32 = 6;

/// The largest number of distinct fingerprints a seed lets the ledger hold.
const MOST_DISTINCT: u32 = 8;

/// The most attempts a seed makes after the ceiling.
const MOST_LATER: u32 = 8;

/// The ledger tolerates `repeat` unchanged failures, then returns a typed
/// intervention for every further one, whatever evidence arrived in between.
fn unchanged_failure_reaches_a_finite_intervention(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let seed = sim.seed;
        let repeat = sim.rng().between(1, MOST_REPEAT);
        let distinct = usize::try_from(u64::from(sim.rng().between(2, MOST_DISTINCT)))?;
        let later = sim.rng().between(1, MOST_LATER);
        let fingerprint = format!("boom-{seed}");
        let payload = format!("op=read-report\nnote=seed-{seed}\n");
        let mut ledger = RepairLedger::new(
            LedgerLimits::new(repeat, distinct),
            support::provenance(payload.as_bytes()),
        );
        let mut evidence = 0_u64;

        for attempt in 1..=repeat {
            let outcome = ledger.record_failure(&fingerprint);
            assert!(
                outcome.is_admitted(),
                "seed {seed}: attempt {attempt} of {repeat} may still be repaired: {outcome}"
            );
            assert_eq!(
                ledger.repetitions(&fingerprint),
                attempt,
                "seed {seed}: the count is the number of sightings"
            );
            if sim.rng().chance(500) {
                let spent = ledger.spent();
                assert!(
                    ledger.record_evidence(&fingerprint),
                    "seed {seed}: evidence for a fingerprint that failed is recorded"
                );
                evidence = evidence.saturating_add(1);
                assert_eq!(
                    ledger.spent(),
                    spent,
                    "seed {seed}: evidence is not a failure, so it costs no spend"
                );
                assert_eq!(
                    ledger.repetitions(&fingerprint),
                    attempt,
                    "seed {seed}: and it does not lower the count the ceiling is measured against"
                );
                assert!(
                    ledger.has_progress(&fingerprint),
                    "seed {seed}: it is visible as progress"
                );
            }
        }
        assert!(
            ledger.may_repair(&fingerprint),
            "seed {seed}: at the ceiling the fingerprint may still be repaired, not past it"
        );

        for extra in 1..=later {
            let outcome = ledger.record_failure(&fingerprint);
            let expected = Intervention::NoProgress {
                fingerprint: fingerprint.clone(),
                repetitions: repeat.saturating_add(extra),
            };
            assert_eq!(
                outcome.intervention(),
                Some(&expected),
                "seed {seed}: sighting {extra} past the ceiling is a typed intervention: {outcome}"
            );
            assert!(
                !ledger.may_repair(&fingerprint),
                "seed {seed}: and the ledger never returns to repairing an unchanged failure"
            );
        }
        let unchanged = u64::from(repeat).saturating_add(u64::from(later));
        assert_eq!(
            ledger.spent(),
            unchanged,
            "seed {seed}: every attempt is charged to the root spend, intervention or not"
        );
        assert_eq!(
            ledger.evidence(),
            evidence,
            "seed {seed}: and the evidence recorded is still recorded"
        );

        for index in 1..distinct {
            let other = format!("other-{seed}-{index}");
            let outcome = ledger.record_failure(&other);
            assert!(
                outcome.is_admitted(),
                "seed {seed}: a different failure is a different fingerprint, admitted while the ledger has room: {outcome}"
            );
            assert_eq!(
                ledger.repetitions(&other),
                1,
                "seed {seed}: a first sighting counts once"
            );
            if sim.rng().chance(300) {
                assert!(
                    ledger.record_evidence(&other),
                    "seed {seed}: evidence for a fingerprint that failed is recorded"
                );
                evidence = evidence.saturating_add(1);
            }
        }
        let overflow = format!("overflow-{seed}");
        let outcome = ledger.record_failure(&overflow);
        assert_eq!(
            outcome.intervention(),
            Some(&Intervention::LedgerFull { held: distinct }),
            "seed {seed}: one fingerprint past the {distinct} the ledger holds is its own intervention: {outcome}"
        );
        assert_eq!(
            ledger.repetitions(&overflow),
            0,
            "seed {seed}: a fingerprint the full ledger refused is not recorded"
        );
        assert!(
            !ledger.record_evidence(&overflow),
            "seed {seed}: so evidence for it is refused: the run has not got there"
        );
        assert!(
            !ledger.record_evidence(&format!("never-failed-{seed}")),
            "seed {seed}: and so is evidence for a fingerprint nobody failed under"
        );

        let others = u64::try_from(distinct.saturating_sub(1))?;
        assert_eq!(
            ledger.spent(),
            unchanged.saturating_add(others).saturating_add(1),
            "seed {seed}: the root spend is every failure the run recorded"
        );
        assert_eq!(
            ledger.repetitions(&fingerprint),
            repeat.saturating_add(later),
            "seed {seed}: nothing recorded since moved the original count"
        );
        assert_eq!(
            ledger.evidence(),
            evidence,
            "seed {seed}: the evidence count is every piece the run recorded and none it was refused"
        );

        sim.record(&format!(
            "repeat={repeat} distinct={distinct} later={later} evidence={evidence} spent={}",
            ledger.spent()
        ));
        Ok(())
    })
}

band_family::band_family! {
    unchanged_failure_reaches_a_finite_intervention_band_00_t29 => unchanged_failure_reaches_a_finite_intervention, 0;
    unchanged_failure_reaches_a_finite_intervention_band_01_t29 => unchanged_failure_reaches_a_finite_intervention, 1;
    unchanged_failure_reaches_a_finite_intervention_band_02_t29 => unchanged_failure_reaches_a_finite_intervention, 2;
    unchanged_failure_reaches_a_finite_intervention_band_03_t29 => unchanged_failure_reaches_a_finite_intervention, 3;
    unchanged_failure_reaches_a_finite_intervention_band_04_t29 => unchanged_failure_reaches_a_finite_intervention, 4;
    unchanged_failure_reaches_a_finite_intervention_band_05_t29 => unchanged_failure_reaches_a_finite_intervention, 5;
    unchanged_failure_reaches_a_finite_intervention_band_06_t29 => unchanged_failure_reaches_a_finite_intervention, 6;
    unchanged_failure_reaches_a_finite_intervention_band_07_t29 => unchanged_failure_reaches_a_finite_intervention, 7;
    unchanged_failure_reaches_a_finite_intervention_band_08_t29 => unchanged_failure_reaches_a_finite_intervention, 8;
    unchanged_failure_reaches_a_finite_intervention_band_09_t29 => unchanged_failure_reaches_a_finite_intervention, 9;
    unchanged_failure_reaches_a_finite_intervention_band_10_t29 => unchanged_failure_reaches_a_finite_intervention, 10;
    unchanged_failure_reaches_a_finite_intervention_band_11_t29 => unchanged_failure_reaches_a_finite_intervention, 11;
    unchanged_failure_reaches_a_finite_intervention_band_12_t29 => unchanged_failure_reaches_a_finite_intervention, 12;
    unchanged_failure_reaches_a_finite_intervention_band_13_t29 => unchanged_failure_reaches_a_finite_intervention, 13;
    unchanged_failure_reaches_a_finite_intervention_band_14_t29 => unchanged_failure_reaches_a_finite_intervention, 14;
    unchanged_failure_reaches_a_finite_intervention_band_15_t29 => unchanged_failure_reaches_a_finite_intervention, 15;
}
