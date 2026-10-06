//! The seed-driven half of the simulation substrate: entropy and the replay
//! receipt, and nothing else.
//!
//! Split out of `sim/mod.rs` so a crate that is not `lgwks_bot` can drive its
//! own deterministic simulations on the same generator and the same trace
//! hash. `sim/mod.rs` also carries the dispatch rig, which names `lgwks_bot`
//! types, so another crate's test cannot include it; this file is `std` only
//! and is included by path (`#[path = ".../sim/seed.rs"] mod seed;`). One
//! copy, so a seed recorded against one crate's suite means the same draw
//! sequence in every other.

// ── Randomness ────────────────────────────────────────────────────────────

/// A xoshiro256** generator, seeded through splitmix64.
///
/// The same stream as the public `lgwks_std::seeded::Seeded`, kept here as a
/// `std`-only copy because `lgwks_ast` includes this file and does not depend on
/// `lgwks_std`. `sim_substrate::the_substrate_and_lgwks_std_seeded_draw_one_stream`
/// holds the two to one stream.
///
/// Chosen for two reasons that matter here rather than in the abstract: the
/// state is four `u64`s, so a scenario's entire random history is a `u64` and
/// a seed, and the generator is a fixed, fully specified algorithm, so a hash
/// recorded today still means the same thing after a toolchain upgrade. A
/// generator whose output depends on the platform would make the replay
/// receipt meaningless.
#[derive(Clone, Debug)]
pub struct Rng {
    state: [u64; 4],
}

impl Rng {
    /// The generator for `seed`.
    ///
    /// A zero state is remapped, because xoshiro cannot leave an all-zero
    /// state — it is the one fixed point of the update — and a scenario that
    /// silently stopped varying after a zeroed draw would be worse than one
    /// that failed.
    pub fn new(seed: u64) -> Self {
        let mut splitmix = seed;
        let mut state = [0u64; 4];
        for lane in state.iter_mut() {
            splitmix = splitmix.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = splitmix;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            *lane = z ^ (z >> 31);
        }
        if state.iter().all(|lane| *lane == 0) {
            state[0] = 0x9e37_79b9_7f4a_7c15;
        }
        Self { state }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let shift = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= shift;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }

    /// A uniform value in `0..bound`, by Lemire's multiply-shift.
    ///
    /// Thirty-two bits of the stream become a product with the bound, the top
    /// half of that product is the answer, and a draw is rejected only when it
    /// lands in the low `threshold` residues. The rejection threshold is
    /// `(2^32 - bound) mod bound`, not `bound` itself: the residues that are
    /// over-represented are exactly the ones below that threshold, so testing
    /// against the bound instead of the threshold rejects almost every draw
    /// and the loop never returns. The acceptance rate is at worst one half
    /// and is one for a bound of one, so a draw is never expensive.
    ///
    /// Multiplied rather than reduced modulo `bound` because a modulo both
    /// biases toward the low residues and is raw arithmetic that wraps the day
    /// a constant changes.
    pub fn below(&mut self, bound: u32) -> u32 {
        // Two to the thirty-second: the width of the draw this consumes.
        let width = u64::from(u32::MAX).saturating_add(1);
        let range = u64::from(bound);
        // A zero bound is the only one with no residue, and nothing lies below it.
        let Some(threshold) = width.checked_rem(range) else {
            return 0;
        };
        let entropy = u64::from(u32::MAX);
        loop {
            let draw = self.next_u64() & entropy;
            // The product of two values below 2^32 always fits in a `u64`, so
            // the wrap is unreachable rather than merely unlikely.
            let product = draw.wrapping_mul(range);
            if product < threshold {
                continue;
            }
            // The high half of the product, read as its four high bytes.
            let [.., b4, b5, b6, b7] = product.to_le_bytes();
            return u32::from_le_bytes([b4, b5, b6, b7]);
        }
    }

    /// A value in `low..=high`.
    pub fn between(&mut self, low: u32, high: u32) -> u32 {
        if high <= low {
            return low;
        }
        low.saturating_add(self.below(high.saturating_sub(low).saturating_add(1)))
    }
}

// ── The trace ─────────────────────────────────────────────────────────────

/// An append-only record of what a scenario actually did.
///
/// The point is not debugging output. The point is that a scenario's *claims*
/// are data, so two runs can be compared without reading either: if the trace
/// hash differs, the runs differed, and the assertions are only a second line
/// of defence over a fact the hash already established.
#[derive(Clone, Debug, Default)]
pub struct Trace {
    /// Every fact recorded, framed; [`super::seed_helpers`] reads whether it is
    /// empty, so the field is visible to the substrate around this file.
    pub(super) bytes: Vec<u8>,
    /// How many facts were recorded, mixed into each frame.
    len: u64,
}

impl Trace {
    /// The empty trace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one labelled fact.
    ///
    /// A separator and a running counter go in before every label so that
    /// `record("ab")` and `record("a"); record("b")` hash differently. Without
    /// that, a scenario that split one event into two would be
    /// indistinguishable from one that did not, and the replay receipt would
    /// be weaker than it looks.
    pub fn record(&mut self, fact: &str) {
        self.len = self.len.wrapping_add(1);
        self.bytes.push(b'|');
        self.bytes.extend_from_slice(&self.len.to_le_bytes()[..4]);
        self.bytes.extend_from_slice(fact.as_bytes());
    }

    /// Append a labelled number, which is what most facts are.
    ///
    /// Written as its decimal digits, whatever its width: a count, an id and a
    /// draw of equal value trace identically on every target, so a `usize` on a
    /// 32-bit host and a `u64` on a 64-bit one produce the same receipt, and
    /// there is no widening to fail.
    pub fn record_number(&mut self, label: &str, value: impl std::fmt::Display) {
        self.record(label);
        self.record(&value.to_string());
    }

    /// The FNV-1a 64 digest of everything recorded.
    ///
    /// FNV-1a rather than a cryptographic digest on purpose: this is a
    /// determinism receipt, not a security boundary, and it must be readable
    /// in three lines of dependency-free code whose behaviour cannot change
    /// across a toolchain bump.
    pub fn hash(&self) -> u64 {
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        for byte in &self.bytes {
            digest ^= u64::from(*byte);
            digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
        }
        digest
    }
}
