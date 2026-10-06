//! `seeded` owns the estate's one deterministic stream: a generator whose whole
//! output is a function of a `u64` seed, so a simulation that fails on a seed
//! replays exactly from that seed.
//!
//! It is not randomness. Nothing here reads the OS, a clock or a counter, and
//! the stream is predictable to anyone who knows the seed. That is the point for
//! a simulation and the opposite of what a key, a token or an id needs, which is
//! why this is a module of its own rather than part of `random`: `random` holds
//! INV-RANDOM-ONE-SOURCE, and a seeded stream is a second source by design. A
//! caller that needs unpredictability reads `lgwks_std::random` (feature
//! `random`); a caller that needs replay reads this, which is in `core` and has
//! no dependency.
//!
//! # The algorithm, and why the stream is stable
//!
//! xoshiro256\*\* (Blackman and Vigna, 2018), its four state words filled by four
//! consecutive splitmix64 outputs of the seed. Both are fixed, fully specified
//! integer algorithms with no platform-dependent step, so a seed names one
//! stream on every target and in every release: a change to either is a
//! breaking change to this crate, and the reference vectors in this module's
//! tests pin it. It is the same stream as the estate's test-side substrate
//! (`lgwks-bot/tests/sim/seed.rs`, which `lgwks_ast` includes because it does
//! not depend on this crate), so a seed recorded against any crate's
//! simulations draws the same words here.
//!
//! splitmix64 is a bijection of its counter, so at most one of the four filling
//! words can be zero and the all-zero state — xoshiro's one fixed point — is
//! unreachable from any seed.
//!
//! # Bounded draws
//!
//! [`Seeded::below`][below] is unbiased: Lemire's multiply-shift over the full 64-bit
//! word with the exact rejection threshold `(2^64 - bound) mod bound`, so every
//! value in `0..bound` has probability exactly `1 / bound`. A plain
//! `next_u64() % bound` is not: for a bound of two thirds of `2^64` it lands in
//! the lower half of the range two times in three. A rejection costs one more
//! word, happens at worst one draw in two, and never for a power of two.
//!
//! [below]: crate::seeded::Seeded::below
//!
//! ```rust
//! use lgwks_std::seeded::{DrawError, Seeded};
//!
//! let mut stream = Seeded::from_seed(7);
//! let mut replay = Seeded::from_seed(7);
//! assert_eq!(stream.next_u64(), replay.next_u64());
//!
//! let roll = stream.below(6)?;
//! assert!(roll < 6);
//! let lanes = ["fmt", "clippy", "tests"];
//! assert!(lanes.get(stream.index(lanes.len())?).is_some());
//!
//! // A range with no value in it is refused, never answered with a substitute.
//! assert_eq!(stream.below(0), Err(DrawError::EmptyRange));
//! # Ok::<(), DrawError>(())
//! ```

use std::error::Error;
use std::fmt;

/// splitmix64's increment: the 64-bit golden ratio.
const GOLDEN_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

/// One step of splitmix64 over `counter`, returning the mixed word.
fn splitmix64(counter: &mut u64) -> u64 {
    *counter = counter.wrapping_add(GOLDEN_GAMMA);
    let mut mixed = *counter;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

/// Why a bounded draw has no value to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DrawError {
    /// The range `0..0` holds no value.
    EmptyRange,
    /// The length does not fit in the stream's 64-bit word. No target Rust
    /// supports today has a `usize` that wide; the arm exists so a future one
    /// is refused rather than truncated into a different range.
    WiderThanStream,
}

impl fmt::Display for DrawError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::EmptyRange => f.write_str("a draw below zero has no value to return"),
            Self::WiderThanStream => {
                f.write_str("a draw over a length wider than 64 bits cannot be made exactly")
            }
        }
    }
}

impl Error for DrawError {}

/// A deterministic xoshiro256\*\* stream: one seed, one sequence, on every target.
///
/// A value, not a handle: [`Clone`] forks the stream at its current position,
/// and a simulation that draws for several actors keeps one per actor, so that
/// adding a draw to one actor does not shift every other actor's sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seeded {
    /// xoshiro256\*\*'s four state words, never all zero.
    words: [u64; 4],
}

impl Seeded {
    /// The stream `seed` names.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        let mut counter = seed;
        Self {
            words: [
                splitmix64(&mut counter),
                splitmix64(&mut counter),
                splitmix64(&mut counter),
                splitmix64(&mut counter),
            ],
        }
    }

    /// The next 64-bit word of the stream.
    pub fn next_u64(&mut self) -> u64 {
        let [s0, s1, s2, s3] = self.words;
        let out = s1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t2 = s2 ^ s0;
        let t3 = s3 ^ s1;
        self.words = [s0 ^ t3, s1 ^ t2, t2 ^ (s1 << 17), t3.rotate_left(45)];
        out
    }

    /// A uniform value in `0..bound`, with no bias toward any value.
    ///
    /// # Errors
    ///
    /// [`DrawError::EmptyRange`] when `bound` is zero. Nothing is drawn, so a
    /// refused call leaves the stream where it was.
    pub fn below(&mut self, bound: u64) -> Result<u64, DrawError> {
        // `(2^64 - bound) mod bound`: the residues one multiply-shift
        // over-represents. The remainder is refused exactly when `bound` is
        // zero, which is also the one bound with no value below it.
        let Some(threshold) = bound.wrapping_neg().checked_rem(bound) else {
            let refusal = Err(DrawError::EmptyRange);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "seeded below: returning an error to the caller");
            return refusal;
        };
        loop {
            // The product of two `u64`s fits in a `u128`, so the wrap is
            // unreachable rather than merely unlikely.
            let product = u128::from(self.next_u64()).wrapping_mul(u128::from(bound));
            let [
                l0,
                l1,
                l2,
                l3,
                l4,
                l5,
                l6,
                l7,
                h0,
                h1,
                h2,
                h3,
                h4,
                h5,
                h6,
                h7,
            ] = product.to_le_bytes();
            if u64::from_le_bytes([l0, l1, l2, l3, l4, l5, l6, l7]) >= threshold {
                return Ok(u64::from_le_bytes([h0, h1, h2, h3, h4, h5, h6, h7]));
            }
        }
    }

    /// A uniform index into a collection of `len` items.
    ///
    /// # Errors
    ///
    /// [`DrawError::EmptyRange`] when `len` is zero, and
    /// [`DrawError::WiderThanStream`] on a target whose `usize` is wider than
    /// 64 bits.
    pub fn index(&mut self, len: usize) -> Result<usize, DrawError> {
        let Ok(bound) = u64::try_from(len) else {
            let refusal = Err(DrawError::WiderThanStream);
            #[cfg(feature = "trace")]
            crate::trace::debug!(len, error = ?refusal.as_ref().err(), "seeded index: returning an error to the caller");
            return refusal;
        };
        let drawn = self.below(bound)?;
        // `drawn < len`, so it fits wherever `len` did.
        usize::try_from(drawn).or(Err(DrawError::WiderThanStream))
    }
}

#[cfg(test)]
mod tests {
    use super::{DrawError, Seeded};

    /// What every test here returns.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The first words of the stream for named seeds, computed from the
    /// published reference algorithms (splitmix64 filling xoshiro256\*\*). Seed
    /// zero's first word is the value the reference implementations print. A
    /// change here is a change to every recorded seed in the estate.
    const VECTORS: [(u64, [u64; 4]); 3] = [
        (
            0,
            [
                0x99ec_5f36_cb75_f2b4,
                0xbf6e_1f78_4956_452a,
                0x1a5f_849d_4933_e6e0,
                0x6aa5_94f1_262d_2d2c,
            ],
        ),
        (
            1,
            [
                0xb3f2_af6d_0fc7_10c5,
                0x853b_5596_4736_4cea,
                0x92f8_9756_082a_4514,
                0x642e_1c7b_c266_a3a7,
            ],
        ),
        (
            u64::MAX,
            [
                0x8f55_20d5_2a7e_ad08,
                0xc476_a018_caa1_802d,
                0x81de_31c0_d260_469e,
                0xbf65_8d7e_065f_3c2f,
            ],
        ),
    ];

    #[test]
    fn the_stream_matches_the_reference_vectors() {
        for (seed, expected) in VECTORS {
            let mut stream = Seeded::from_seed(seed);
            let drawn = [
                stream.next_u64(),
                stream.next_u64(),
                stream.next_u64(),
                stream.next_u64(),
            ];
            assert_eq!(drawn, expected, "seed {seed:#x} left the published stream");
        }
    }

    #[test]
    fn splitmix_fills_the_state_with_its_published_first_word() {
        let mut counter = 0;
        assert_eq!(super::splitmix64(&mut counter), 0xe220_a839_7b1d_cdaf);
    }

    #[test]
    fn a_zero_bound_and_an_empty_collection_are_refused_without_drawing() {
        let mut stream = Seeded::from_seed(3);
        let before = stream.clone();
        assert_eq!(stream.below(0), Err(DrawError::EmptyRange));
        assert_eq!(stream.index(0), Err(DrawError::EmptyRange));
        assert_eq!(stream, before, "a refused draw must not move the stream");
    }

    #[test]
    fn a_bound_of_one_always_yields_zero_and_draws_one_word() {
        let mut stream = Seeded::from_seed(11);
        let mut twin = Seeded::from_seed(11);
        for _ in 0..1_000 {
            assert_eq!(stream.below(1), Ok(0));
            twin.next_u64();
            assert_eq!(stream, twin, "a bound of one never rejects");
        }
    }

    #[test]
    fn the_rejection_threshold_removes_the_bias_a_remainder_would_have() -> TestResult {
        // Two thirds of 2^64, rounded up. A remainder maps [bound, 2^64) onto
        // the lower half again, so `% bound` lands below the midpoint two
        // times in three; Lemire's draw lands there one time in two.
        const BOUND: u64 = 0xaaaa_aaaa_aaaa_aaab;
        const HALF: u64 = BOUND >> 1;
        let mut stream = Seeded::from_seed(0x5eed_0344);
        let mut control = Seeded::from_seed(0x5eed_0344);
        let (mut lower, mut control_lower) = (0_u32, 0_u32);
        for _ in 0..60_000 {
            if stream.below(BOUND)? < HALF {
                lower = lower.saturating_add(1);
            }
            let remainder = control
                .next_u64()
                .checked_rem(BOUND)
                .ok_or("a non-zero bound has a remainder")?;
            if remainder < HALF {
                control_lower = control_lower.saturating_add(1);
            }
        }
        // 60,000 fair draws put the lower-half count within 1,000 of 30,000
        // except with probability below 1e-15; the remainder sits near 40,000.
        assert!(
            lower.abs_diff(30_000) < 1_000,
            "the unbiased draw put {lower} of 60000 below the midpoint"
        );
        assert!(
            control_lower.abs_diff(40_000) < 1_000,
            "the remainder control lost its bias ({control_lower}), so the test measures nothing"
        );
        Ok(())
    }
}
