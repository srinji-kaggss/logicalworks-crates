//! The attacker the adversarial simulation families share.
//!
//! An adversarial family states who controls the input, what that party may do
//! to it, and the invariant the shipped code must keep whatever they do. The
//! party is [`Attacker`]: one seed, one stream, so a failure names the seed that
//! reproduces the attack, and the same seed mounts the same attack on every
//! target. The stream is the crate's own [`lgwks_std::seeded::Seeded`]
//! (INV-STD-SEEDED-1); there is no second generator here.
//!
//! The attacker carries no knowledge of the code it attacks. It mutates valid
//! input the way a wire fault, a fuzzer or a hostile peer does: one flipped bit,
//! a deleted or duplicated byte, a boundary byte where a digit was, a cut, a
//! tail. What each target must do about it is written in the family, against a
//! reference derived from the documented contract.
//!
//! Its own tests are `sim_adversary_fixture`, rather than an `allow`: not every
//! family calls every operator, and a fixture whose unused half is silenced is a
//! fixture nobody has read.

use lgwks_std::seeded::Seeded;

use crate::seeded_bytes::{below, fold_bytes, next_bytes, next_text};
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, initial_trace, next_seed,
    seeded_stream,
};

/// How many rounds a campaign trace folds.
const CAMPAIGN_ROUNDS: usize = 256;

/// Bytes at the edges of every byte-oriented grammar: the NUL, the smallest and
/// largest 7-bit values, the first and last continuation bytes of UTF-8, the
/// start of a three-byte UTF-8 sequence, and the largest byte.
const BOUNDARY_BYTES: [u8; 8] = [0x00, 0x01, 0x7f, 0x80, 0xbf, 0xc0, 0xe0, 0xff];

/// The eight single-bit masks of a byte, so a flip needs no shift.
const BIT_MASKS: [u8; 8] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80];

/// How many mutation operators [`Attacker::mutate`] chooses between.
const MUTATION_KINDS: usize = 8;

/// One party who controls the bytes a parser is handed.
pub struct Attacker {
    /// The stream every choice the attacker makes is drawn from.
    stream: Seeded,
}

impl Attacker {
    /// The attacker `seed` names.
    pub fn new(seed: u64) -> Self {
        Self {
            stream: seeded_stream(seed),
        }
    }

    /// The next word of the attacker's stream.
    pub fn word(&mut self) -> u64 {
        next_seed(&mut self.stream)
    }

    /// A value in `0..bound`, drawn without bias; a zero bound yields `0`.
    pub fn below(&mut self, bound: usize) -> usize {
        below(&mut self.stream, bound)
    }

    /// A value in `low..high` as the `u32` a calendar field is held in.
    ///
    /// An empty range yields `low`, the one value it can be said to contain, and
    /// draws nothing.
    pub fn field(&mut self, low: u32, high: u32) -> u32 {
        let width = u64::from(high.saturating_sub(low));
        match self.stream.below(width).map(u32::try_from) {
            Ok(Ok(offset)) => low.saturating_add(offset),
            Ok(Err(_)) | Err(_) => low,
        }
    }

    /// A payload of `len` bytes drawn from the attacker's stream, the raw material
    /// of a hostile message or a key.
    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        next_bytes(&mut self.stream, len)
    }

    /// `LEN` bytes as a fixed-width array: a key, an identifier, a digest.
    pub fn array<const LEN: usize>(&mut self) -> [u8; LEN] {
        let mut out = [0_u8; LEN];
        for (slot, byte) in out.iter_mut().zip(self.bytes(LEN)) {
            *slot = byte;
        }
        out
    }

    /// A message of `below(max)` random bytes with its `spell`ing as text.
    pub fn spelled(&mut self, max: usize, spell: impl Fn(&[u8]) -> String) -> (Vec<u8>, String) {
        let len = self.below(max);
        let bytes = self.bytes(len);
        let text = spell(&bytes);
        (bytes, text)
    }

    /// `len` printable ASCII characters.
    pub fn text(&mut self, len: usize) -> String {
        next_text(&mut self.stream, len)
    }

    /// One of the bytes at the edge of a byte grammar.
    pub fn boundary_byte(&mut self) -> u8 {
        BOUNDARY_BYTES[self.below(BOUNDARY_BYTES.len())]
    }

    /// One element of a table that has at least one element.
    pub fn pick<'table, Item>(&mut self, table: &'table [Item]) -> &'table Item {
        &table[self.below(table.len())]
    }

    /// `input` with one hostile change: a flipped bit, a deleted, duplicated,
    /// inserted, overwritten or swapped byte, a cut, or a random tail.
    pub fn mutate(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = input.to_vec();
        let at = self.below(out.len().max(1));
        match self.below(MUTATION_KINDS) {
            0 => {
                let mask = BIT_MASKS[self.below(BIT_MASKS.len())];
                if let Some(byte) = out.get_mut(at) {
                    *byte ^= mask;
                }
            }
            1 => {
                out = out
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, byte)| (index != at).then_some(byte))
                    .collect();
            }
            2 => {
                if let Some(&byte) = out.get(at) {
                    out.insert(at, byte);
                }
            }
            3 => {
                let byte = self.boundary_byte();
                out.insert(at.min(out.len()), byte);
            }
            4 => out.truncate(at),
            5 => {
                let tail = 1_usize.saturating_add(self.below(16));
                out.extend(self.bytes(tail));
            }
            6 => {
                let byte = self.boundary_byte();
                if let Some(slot) = out.get_mut(at) {
                    *slot = byte;
                }
            }
            _ => {
                let next = at.saturating_add(1);
                if next < out.len() {
                    out.swap(at, next);
                }
            }
        }
        out
    }

    /// `input` after `rounds` successive mutations, each applied to the last.
    pub fn mutate_n(&mut self, input: &[u8], rounds: usize) -> Vec<u8> {
        let mut out = input.to_vec();
        for _ in 0..rounds {
            out = self.mutate(&out);
        }
        out
    }
}

/// Text from bytes an attacker produced: the bytes themselves when they are
/// UTF-8, and the replacement-character reading when they are not, so a parser
/// that takes `&str` is still handed everything the attacker built.
#[must_use]
pub fn as_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// What a family's test returns: the first refusal of a generated input that
/// must have been valid ends the sweep and prints itself.
pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The trace hash of one campaign against a decoder.
///
/// Each round the attacker builds `valid` input, mutates it three times, and the
/// text `outcome` gives of what the decoder made of it is folded into the trace.
/// Two runs of one seed fold the same outcomes in the same order, so the hash is
/// the replay oracle every family states once.
pub fn decoder_campaign(
    seed: u64,
    valid: impl Fn(&mut Attacker) -> Vec<u8>,
    outcome: impl Fn(&[u8]) -> String,
) -> u64 {
    let mut attacker = Attacker::new(seed);
    let mut trace = initial_trace();
    for _ in 0..CAMPAIGN_ROUNDS {
        let base = valid(&mut attacker);
        let hostile = attacker.mutate_n(&base, 3);
        fold_bytes(&mut trace, outcome(&hostile).as_bytes());
    }
    trace
}

/// Asserts a campaign replays bit for bit under every sweep seed.
pub fn replays_under_every_seed(campaign: impl Fn(u64) -> u64) {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(&campaign, seed);
    }
}

/// Asserts a campaign differs between seeds: two pairs, so one coincidence of a
/// pair cannot pass a campaign that ignores its seed.
pub fn diverges_between_seeds(campaign: impl Fn(u64) -> u64) {
    assert_distinct_seeds_diverge(&campaign, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
    assert_distinct_seeds_diverge(&campaign, SWEEP_SEEDS[2], SWEEP_SEEDS[3]);
}

/// `bytes` with bit number `bit` flipped, counting from the first byte's lowest
/// bit: the single-bit fault every checksum and signature must notice.
pub fn flip_bit(bytes: &[u8], bit: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    if let Some(byte) = out.get_mut(bit.div_euclid(8)) {
        *byte ^= BIT_MASKS[bit.rem_euclid(8)];
    }
    out
}

/// Calls `check` with `template` after each of the 256 byte values has replaced
/// each of its positions: the exhaustive single-byte attack on a short grammar.
pub fn every_byte_at_every_position<const LEN: usize>(
    template: [u8; LEN],
    mut check: impl FnMut([u8; LEN], usize, u8),
) {
    for position in 0..LEN {
        for candidate in 0..=u8::MAX {
            let mut input = template;
            input[position] = candidate;
            check(input, position, candidate);
        }
    }
}

/// Mounts `rounds` rounds of an attack under every sweep seed.
///
/// `body` receives the seed, the round and the attacker, so a failing assertion
/// can name the seed and round that reproduce it. One attacker serves all of a
/// seed's rounds, so round `n` is the same attack on every run of that seed.
pub fn sweep(
    rounds: usize,
    mut body: impl FnMut(u64, usize, &mut Attacker) -> TestResult,
) -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut attacker = Attacker::new(seed);
        for round in 0..rounds {
            body(seed, round, &mut attacker)?;
        }
    }
    Ok(())
}
