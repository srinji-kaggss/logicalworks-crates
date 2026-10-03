//! Shared scaffolding for the deterministic simulation suites.
//!
//! One seed fully determines a family's sequence: no wall clock, no OS entropy,
//! no dependency. `sim_origin.rs` and `sim_dependency_policy.rs` include this
//! module (via `#[path]`) so the generator is written once.

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

    /// An index into `values`, drawn from the sequence.
    pub fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        let len = u64::try_from(values.len()).unwrap_or(u64::MAX).max(1);
        let index = usize::try_from(self.next_u64().checked_rem(len).unwrap_or(0)).unwrap_or(0);
        &values[index]
    }
}
