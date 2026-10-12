//! The seeded draw stream the crate's in-module simulations share.
//!
//! In-module simulations (`sim_*.rs` beside the code they test) cannot reach
//! the integration suite's `tests/sim` substrate, so they draw from this one
//! stream instead of each carrying a copy: one word of SplitMix64 state, so a
//! seed is a replayable trace, and every draw refuses rather than guesses when
//! asked for an index into nothing.

/// SplitMix64 over one word of state.
pub(crate) struct Stream(u64);

impl Stream {
    /// The stream for `seed`.
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next word.
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A drawn index in `0..bound`, from the word's low sixteen bits: every
    /// bound a simulation here asks for is far below 65,536, so the bias is
    /// under one part in a hundred. An empty range has no index, and is
    /// refused.
    pub(crate) fn below(&mut self, bound: usize) -> Result<usize, String> {
        let [low, high, ..] = self.next().to_le_bytes();
        usize::from(u16::from_le_bytes([low, high]))
            .checked_rem(bound)
            .ok_or_else(|| "an index drawn from an empty range".to_owned())
    }

    /// A drawn element of `items`.
    pub(crate) fn choose<T: Copy>(&mut self, items: &[T]) -> Result<T, String> {
        let at = self.below(items.len())?;
        items
            .get(at)
            .copied()
            .ok_or_else(|| format!("{at} is outside {} items", items.len()))
    }
}

/// The FNV-1a 64 fold of `bytes` into `digest`: the replay receipt every
/// in-module simulation hashes its verdicts into.
pub(crate) fn fold(digest: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(digest, |digest, byte| {
        (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The FNV-1a 64 offset basis, where every receipt starts.
pub(crate) const RECEIPT_START: u64 = 0xcbf2_9ce4_8422_2325;
