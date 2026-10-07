//! Shared deterministic generator for the seeded simulation families.
//!
//! The wire and http families both draw from one stream, so the generator lives
//! here once rather than being copied into each test target. It holds its state
//! by value, like [`crate::seeded_sweep::next_seed`]'s stream, because a family
//! that draws from several streams at once keeps one generator per stream.
//!
//! The stream is the crate's own [`lgwks_std::seeded::Seeded`], the estate's one
//! deterministic stream (INV-STD-SEEDED-1), so a seed recorded against these
//! families draws the same words as the same seed in `lgwks_bot`'s, `lgwks_ast`'s
//! and `lgwks_deps`' simulations, and as a caller's own `Seeded`. The fixture
//! once carried a private xorshift64* generator; a second generator is a second
//! meaning for every seed.
//!
//! It carries its own tests rather than an `allow`: not every binary that
//! includes this module calls [`Rng::below`], and a fixture whose unused half
//! is silenced is a fixture nobody has read.

use lgwks_std::seeded::Seeded;

/// A deterministic generator: one seed, one stream.
pub struct Rng(Seeded);

impl Rng {
    /// The stream `seed` names.
    pub fn new(seed: u64) -> Self {
        Self(Seeded::from_seed(seed))
    }

    /// The next value in the stream.
    pub fn next(&mut self) -> u64 {
        self.0.next_u64()
    }

    /// A value below `bound`, in `0..bound`, drawn without bias.
    ///
    /// A zero bound has no value below it and yields `0`, the one value an
    /// empty range can be said to contain, without drawing: the stream's own
    /// refusal ([`lgwks_std::seeded::DrawError::EmptyRange`]) leaves it where
    /// it was, so a family's later draws do not depend on an empty range it
    /// happened to ask about. The stream's other refusal, an index wider than
    /// 64 bits, has no target to occur on.
    pub fn below(&mut self, bound: usize) -> usize {
        let Ok(drawn) = self.0.index(bound) else {
            return 0;
        };
        drawn
    }
}

#[cfg(test)]
mod tests {
    /// Three named seeds, so this fixture's replay property is stated over
    /// inputs it names rather than over another fixture's.
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
    /// The low byte of a word is the stream's entropy, not a short cycle.
    fn sim_the_low_byte_of_the_stream_has_no_period_of_its_own_width() {
        // A generator whose low 8 bits cycle every 256 draws (an unfinalised
        // linear-congruential step) made a 4 KiB payload drawn one byte per
        // word sixteen copies of one block.
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
    /// The fixture is the crate's `Seeded`, word for word: one seed, one
    /// meaning, in every simulation of the estate.
    fn sim_the_fixture_draws_the_public_seeded_stream() {
        for seed in SEEDS {
            let mut fixture = Rng::new(seed);
            let mut public = lgwks_std::seeded::Seeded::from_seed(seed);
            for draw in 0..256 {
                assert_eq!(
                    fixture.next(),
                    public.next_u64(),
                    "seed {seed:#x} left the public stream at draw {draw}"
                );
            }
        }
    }

    #[test]
    /// A zero bound is answered without moving the stream.
    fn a_zero_bound_draws_nothing() {
        let mut asked = Rng::new(SEEDS[1]);
        let mut untouched = Rng::new(SEEDS[1]);
        assert_eq!(asked.below(0), 0, "no value is below zero");
        assert_eq!(
            asked.next(),
            untouched.next(),
            "an empty range moved the stream"
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
