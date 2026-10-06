//! Shared scaffolding for the deterministic simulation suites.
//!
//! One seed fully determines a family's sequence: no wall clock, no OS entropy,
//! no dependency. `sim_origin.rs` and `sim_dependency_policy.rs` include this
//! module (via `#[path]`) so the generator is written once.

use std::num::NonZeroU64;

/// A tiny LCG so the seed fully determines the sequence. `wrapping_*` keeps the
/// arithmetic total.
pub struct Rng(u64);

impl Rng {
    /// A generator whose whole sequence is fixed by `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// The next 64-bit value in the sequence.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

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
