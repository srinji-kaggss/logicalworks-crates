//! Which chain of a wave may yield once, so one seed decides one stall set on any host.
//!
//! A chain goes `Pending` on its first turn when it is wedged or yields. The first
//! chain to do so starts the wave's reaper, whose deadline is wall time by design
//! (INV-BOT-123). Every chain turned after it can find that deadline already spent
//! when a loaded host deschedules the polling thread for longer than the 1 ms the
//! family runs under. A wedged chain is reported stalled either way, but a yielder
//! cut off there is reported stalled on scheduling alone, and the same seed then
//! decides a different stall set on a loaded host than on an idle one.
//!
//! Source: main run 39060016758 (`Tests (lgwks-bot no default features)`, load
//! average 65), `the_same_seed_replays_a_deadline_wave`, seed 8 of band 40. Commit
//! `88ee8c6b5` closed the same race for a yield turned after a wedge; this closes it
//! for a yield turned after another yield.

use super::*;

/// Whether a chain of the current tick has already gone `Pending` on its first turn.
#[derive(Debug, Default)]
pub(super) struct PendingTurn {
    /// Set once a chain that is wedged, or that keeps a yield, has been armed.
    seen: bool,
}

impl PendingTurn {
    /// The number of polls the next chain yields for.
    ///
    /// `drawn` while no chain before it goes `Pending`, and none after: that chain
    /// is the one that starts the reaper, so its first turn cannot follow the
    /// deadline. `wedged` is whether this chain is wedged; a wedged chain parks
    /// whatever it was drawn, so it ends the run of chains that may yield.
    pub(super) fn arm(&mut self, wedged: bool, drawn: u32) -> u32 {
        let yields = if self.seen { 0 } else { drawn };
        self.seen = self.seen || wedged || yields > 0;
        yields
    }
}

/// The yields one tick arms for chains with these wedges and draws, in order.
fn armed(wedged: &[bool], drawn: &[u32]) -> Vec<u32> {
    let mut turn = PendingTurn::default();
    wedged
        .iter()
        .zip(drawn)
        .map(|(&wedge, &draw)| turn.arm(wedge, draw))
        .collect()
}

/// One seeded tick: how many chains, which are wedged, and what each draws.
fn drawn_tick(rng: &mut Rng) -> (Vec<bool>, Vec<u32>) {
    let width = rng.between(1, 8);
    let mut wedged = Vec::new();
    let mut drawn = Vec::new();
    for _ in 0..width {
        wedged.push(rng.chance(250));
        drawn.push(rng.between(0, 2));
    }
    (wedged, drawn)
}

/// Across 2,000 seeded ticks, no chain is armed to yield after one that went `Pending`.
///
/// Source: the same race as the module note (main run 39060016758). The unit cases
/// below pin four hand-picked orders; this sweeps widths 1 to 8, a 25% wedge rate and
/// draws of 0 to 2, so an order nobody wrote down is checked too. The oracle is the
/// rule itself, stated independently of `PendingTurn`: scan the chains in order, and
/// once one is wedged or keeps a yield, every later armed yield must be zero.
#[test]
fn no_chain_yields_after_one_that_went_pending_across_seeded_ticks() {
    const SEED: u64 = 0x1250_0021;
    let mut rng = Rng::new(SEED);
    let mut ended_by_wedge = 0_usize;
    let mut ended_by_yield = 0_usize;
    for tick in 0..2_000_u32 {
        let (wedged, drawn) = drawn_tick(&mut rng);
        let got = armed(&wedged, &drawn);
        let mut pending = false;
        for (index, &yields) in got.iter().enumerate() {
            let expected = if pending { 0 } else { drawn[index] };
            assert_eq!(
                yields, expected,
                "seed {SEED} tick {tick} chain {index}: wedged {wedged:?} drawn {drawn:?}"
            );
            if !pending && (wedged[index] || yields > 0) {
                pending = true;
                if wedged[index] {
                    ended_by_wedge = ended_by_wedge.saturating_add(1);
                } else {
                    ended_by_yield = ended_by_yield.saturating_add(1);
                }
            }
        }
    }
    assert!(
        ended_by_wedge > 0 && ended_by_yield > 0,
        "seed {SEED}: the sweep reached both ways a run of yielders ends, so the oracle is not vacuous"
    );
}

/// The arming replays: the same seed yields the same armed set, twice over.
///
/// The arming is a function of the draws alone, with no clock, so the trace a
/// family records from it is the run and not the host.
#[test]
fn the_arming_of_a_seeded_wave_replays() -> TestResult {
    sim::assert_replays(sim::band_of(41), |sim| {
        let ticks = sim.rng().between(2, 6);
        for tick in 0..ticks {
            let (wedged, drawn) = drawn_tick(sim.rng());
            sim.record(&format!("tick {tick} armed {:?}", armed(&wedged, &drawn)));
        }
        Ok(())
    })
}

/// A yield after another yield is not armed: the first one started the reaper.
#[test]
fn a_yield_after_a_yield_is_not_armed() {
    assert_eq!(
        armed(&[false, false, false], &[1, 1, 1]),
        [1, 0, 0],
        "only the first chain to go Pending keeps its yield"
    );
    assert_eq!(
        armed(&[false, false, false], &[0, 1, 1]),
        [0, 1, 0],
        "a chain that draws no yield does not start the reaper, so the next may"
    );
}

/// A yield after a wedge is not armed (commit 88ee8c6b5's half of the rule).
#[test]
fn a_yield_after_a_wedge_is_not_armed() {
    assert_eq!(
        armed(&[true, false, false], &[1, 1, 1]),
        [1, 0, 0],
        "the wedged chain keeps its own draw, which it never reads, and ends the run"
    );
    assert_eq!(
        armed(&[false, true, false], &[0, 0, 1]),
        [0, 0, 0],
        "a wedge ends the run of chains that may yield even when nothing yielded before it"
    );
}

/// With nothing wedged and nothing drawn, the arming changes nothing.
#[test]
fn a_tick_with_no_yield_and_no_wedge_arms_none() {
    assert_eq!(armed(&[false, false, false], &[0, 0, 0]), [0, 0, 0]);
}
