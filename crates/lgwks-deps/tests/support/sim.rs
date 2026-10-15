//! `lgwks_deps`' draws over the estate's one seed substrate.
//!
//! The generator and the trace are `lgwks_bot`'s `sim/seed.rs`, included by
//! path beside its `seed_helpers.rs`, as `lgwks_ast` includes them: one
//! xoshiro stream and one FNV-1a receipt for every crate above `lgwks_bot`, so
//! a seed and a hash mean the same thing in each. `lgwks_std` sits below
//! `lgwks_bot`, and its tests do not reach up into a crate that depends on it;
//! they draw the same stream from `lgwks_std::seeded::Seeded` directly. This
//! file adds only what the policy families need on top: a draw from a named
//! table, and a receipt that refuses a run that recorded nothing.

use std::num::NonZeroU64;

pub use crate::seed::{Rng, Trace};

impl Rng {
    /// An element of `values`, drawn from the sequence, or `None` when
    /// `values` is empty.
    ///
    /// An empty table has no element to draw and no remainder to reduce a draw
    /// by, so the draw is refused rather than answered with a substituted
    /// length: a simulation whose fixture table had been emptied would
    /// otherwise test a value its seed never chose. The draw is consumed before
    /// the table is inspected, so the sequence a seed produces does not depend
    /// on how long the table it drew from was — the same seed replays the same
    /// sequence whatever it is asked to pick from.
    pub fn pick<'a, T>(&mut self, values: &'a [T]) -> Option<&'a T> {
        let draw = self.next_u64();
        // A table this target cannot measure in `u64` and an empty table are the
        // two cases with no index; `NonZeroU64` is the one check that refuses
        // both without naming a substitute length.
        let bound = NonZeroU64::new(u64::try_from(values.len()).ok()?)?;
        // `checked_rem` is the one reduction a bounded draw needs: it refuses
        // rather than rounding, and the remainder is below `values.len()`, so
        // the narrowed index is inside this target's reach.
        let index = usize::try_from(draw.checked_rem(bound.get())?).ok()?;
        values.get(index)
    }

    /// An element of `values`, drawn from the sequence, naming the table in
    /// the refusal.
    ///
    /// The same refusal as [`Rng::pick`] with the one thing a bare `Option`
    /// drops: which table was empty. Every table a suite draws from is a
    /// constant or a slice whose emptiness the caller already tested, so this
    /// names a fixture defect — and a fixture defect has to be a *named* one,
    /// because the alternative is a suite that silently tests the first element
    /// of a table it meant to draw from.
    pub fn pick_named<'a, T>(
        &mut self,
        table: &'static str,
        values: &'a [T],
    ) -> Result<&'a T, EmptyTable> {
        self.pick(values).ok_or(EmptyTable { table })
    }

    /// A fair coin, from the substrate's weighted one.
    ///
    /// One spelling for every family, so every coin in the suite is the same
    /// draw from the same stream rather than a bit each family chose to read.
    pub fn coin(&mut self) -> bool {
        self.chance(500)
    }
}

/// The replay receipt of one run: its trace's hash, or a refusal when the run
/// recorded nothing.
///
/// An empty trace hashes to the same value whatever the run did, so two runs
/// that both recorded nothing would "replay" without having compared anything.
///
/// # Errors
///
/// [`EmptyTrace`] when nothing was recorded.
pub fn receipt(trace: &Trace) -> Result<u64, EmptyTrace> {
    if trace.is_empty() {
        let refusal = Err(EmptyTrace);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "receipt: refusing an empty trace");
        return refusal;
    }
    Ok(trace.hash())
}

/// A draw asked a table that holds no elements.
///
/// A typed refusal rather than a substituted element: the harness refuses to
/// invent a value its seed did not choose, and names the table so the fixture
/// that was emptied can be found without re-reading every draw site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyTable {
    /// The table the draw was asked for, as the call site named it.
    pub table: &'static str,
}

impl std::fmt::Display for EmptyTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the {} table holds no elements to draw",
            self.table
        )
    }
}

impl std::error::Error for EmptyTable {}

/// A run asked for its receipt having recorded nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyTrace;

impl std::fmt::Display for EmptyTrace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the run recorded nothing, so its replay hash proves nothing")
    }
}

impl std::error::Error for EmptyTrace {}
