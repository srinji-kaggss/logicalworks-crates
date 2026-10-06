//! Shared deterministic generator for the seeded simulation families.
//!
//! The wire and http families both draw from one xorshift64* stream, so the
//! generator lives here once rather than being copied into each test target.
//! It holds its state by value, unlike [`crate::seeded_sweep::next_seed`]'s
//! by-reference stream, because a family that draws from several streams at
//! once keeps one generator per stream.
//!
//! It carries its own tests rather than an `allow`: not every binary that
//! includes this module calls [`Rng::below`], and a fixture whose unused half
//! is silenced is a fixture nobody has read.

/// A deterministic xorshift64* generator: one seed, one stream.
///
/// The finalising multiply is load-bearing rather than decorative: an
/// unfinalised linear-congruential step has period `2**k` in its low `k` bits,
/// so an index drawn from the low eight bits of one would repeat every 256
/// draws.
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
    ///
    /// The whole word is drawn and read at the platform's index width byte by
    /// byte, because `as` is `as_conversions = forbid`, `From<usize>` for `u64`
    /// does not exist and a sentinel would be an index the draw invented. A
    /// zero bound has no value below it and yields `0`, which is the one value
    /// an empty range can be said to contain.
    pub fn below(&mut self, bound: usize) -> usize {
        let mut folded = 0_usize;
        for byte in self.next().to_le_bytes() {
            folded = folded.wrapping_mul(31).wrapping_add(usize::from(byte));
        }
        folded.rem_euclid(bound.max(1))
    }
}

#[cfg(test)]
mod tests {
    /// Three named seeds, so this fixture's replay property is stated over
    /// inputs it names rather than over another fixture's. They are non-zero,
    /// which is what keeps the stream off xorshift's fixed point.
    const SEEDS: [u64; 3] = [
        0xA5A5_0000_0000_0001,
        0xA5A5_0000_0000_0002,
        0xA5A5_FFFF_FFFF_FFFF,
    ];

    use super::Rng;

    #[test]
    /// One seed, one stream.
    fn sim_one_seed_replays_one_stream() {
        for seed in SEEDS {
            let mut first = Rng::new(seed);
            let mut second = Rng::new(seed);
            let drawn: Vec<u64> = (0..32).map(|_| first.next()).collect();
            let replayed: Vec<u64> = (0..32).map(|_| second.next()).collect();
            assert_eq!(drawn, replayed, "seed {seed:#x} must replay its own stream");
        }
    }

    #[test]
    /// Two named seeds draw two streams.
    fn sim_distinct_seeds_draw_distinct_streams() {
        let mut first = Rng::new(SEEDS[0]);
        let mut second = Rng::new(SEEDS[1]);
        let drawn: Vec<u64> = (0..32).map(|_| first.next()).collect();
        let other: Vec<u64> = (0..32).map(|_| second.next()).collect();
        assert_ne!(drawn, other, "two named seeds drew one stream");
    }

    #[test]
    /// A draw is inside its bound at every width a family uses.
    fn sim_a_draw_is_inside_its_bound_at_every_bound_a_family_uses() {
        for bound in [1_usize, 2, 17, 32, 64, 4_096] {
            let mut rng = Rng::new(SEEDS[2]);
            for _ in 0..1_024 {
                assert!(
                    rng.below(bound) < bound,
                    "a value drawn for bound {bound} was not inside it"
                );
            }
        }
    }

    #[test]
    /// The defect the finaliser prevents, observed at the byte a family draws.
    fn sim_the_low_byte_of_the_stream_has_no_period_of_its_own_width() {
        // An unfinalised linear-congruential step has period 256 in its low 8
        // bits, so `bytes[i]` equals `bytes[i + 256]` for ever and a 4 KiB
        // payload drawn one byte per word was sixteen copies of one block.
        let mut rng = Rng::new(SEEDS[0]);
        let bytes: Vec<u8> = (0..1_024).map(|_| rng.next().to_le_bytes()[0]).collect();
        assert_ne!(
            &bytes[..768],
            &bytes[256..1_024],
            "the low byte repeated with a period of 256 draws"
        );
    }

    #[test]
    /// A thousand draws of one seed are a thousand different words.
    fn sim_a_thousand_draws_are_a_thousand_distinct_words() {
        let mut rng = Rng::new(SEEDS[0]);
        let mut words = std::collections::BTreeSet::new();
        for _ in 0..1_024 {
            words.insert(rng.next());
        }
        assert_eq!(
            words.len(),
            1_024,
            "a thousand draws collapsed onto {} distinct words",
            words.len()
        );
    }

    #[test]
    /// No value is below zero, so the draw yields the only one it can.
    fn a_zero_bound_yields_zero_rather_than_dividing_by_zero() {
        assert_eq!(
            Rng::new(SEEDS[0]).below(0),
            0,
            "no value is below zero, so the draw must yield the only one it can"
        );
    }
}
