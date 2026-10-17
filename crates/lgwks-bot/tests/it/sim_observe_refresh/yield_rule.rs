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
