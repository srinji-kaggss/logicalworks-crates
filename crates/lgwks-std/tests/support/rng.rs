//! Shared deterministic generator for the seeded simulation families.
//!
//! The wire and http families both draw from one xorshift64* stream, so the
//! generator lives here once rather than being copied into each test target.
#![allow(
    dead_code,
    reason = "each test target uses a subset of the generator's methods"
)]

/// A deterministic xorshift64* generator: one seed, one stream.
pub struct Rng(u64);

impl Rng {
    /// Seed the stream, avoiding the all-zero fixed point.
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    /// The next value in the stream.
    pub fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A value below `bound`, in `0..bound`.
    pub fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).unwrap_or(u64::MAX);
        if bound == 0 {
            return 0;
        }
        usize::try_from(self.next().checked_rem(bound).unwrap_or(0)).unwrap_or(0)
    }
}
