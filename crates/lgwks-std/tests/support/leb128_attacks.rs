//! The attacks on a LEB128 reader, stated once for all four integer widths.
//!
//! A varint reader sits on the wire, so its attacker is the peer: it controls
//! every byte and every length. The attacks are the ones that have broken real
//! readers: a second spelling of one value (malleability, which forges a hash or
//! a signature over "the same" message), a run of continuation bytes that never
//! ends (an unbounded loop), a final group with bits above the width (silent
//! truncation), and a prefix cut mid-value (a half-read integer).
//!
//! Sources: the WebAssembly binary format section 5.2.2 (the canonical LEB128
//! the codec admits), CWE-190 (integer overflow), CWE-835 (loop with an
//! unreachable exit), CWE-20 (improper input validation), and the 2018
//! protobuf varint malleability reports. Every answer is checked against a
//! reference length written here from the definition of the encoding.
//!
//! Each property is a function over [`Width`], so the same attack is mounted on
//! `u64`, `u32`, `i64` and `i32` and a width cannot be quietly left out.

use lgwks_std::leb128::{
    DecodeError, decode_i32, decode_i64, decode_u32, decode_u64, encode_i32, encode_i64,
    encode_u32, encode_u64,
};

use crate::adversary::{Attacker, TestResult, sweep};

/// One integer width of the codec.
pub trait Width: Copy + PartialEq + std::fmt::Debug {
    /// The width's name, for messages.
    const NAME: &'static str;
    /// The width in bits.
    const BITS: u32;
    /// The longest canonical encoding of the width, in bytes.
    const MAX_BYTES: usize;
    /// Whether values of the width are signed.
    const SIGNED: bool;
    /// Appends the canonical encoding of the value.
    fn encode(self, out: &mut Vec<u8>);
    /// Decodes one value from the front of `input`.
    fn decode(input: &[u8]) -> Result<(Self, usize), DecodeError>;
    /// A value of the width spelled by the low bits of `word`.
    fn from_word(word: u64) -> Self;
    /// The value widened without loss.
    fn wide(self) -> i128;
}

/// The low four bytes of a word.
fn low_four(word: u64) -> [u8; 4] {
    let bytes = word.to_le_bytes();
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

impl Width for u64 {
    const NAME: &'static str = "u64";
    const BITS: u32 = 64;
    const MAX_BYTES: usize = 10;
    const SIGNED: bool = false;
    fn encode(self, out: &mut Vec<u8>) {
        encode_u64(self, out);
    }
    fn decode(input: &[u8]) -> Result<(Self, usize), DecodeError> {
        decode_u64(input)
    }
    fn from_word(word: u64) -> Self {
        word
    }
    fn wide(self) -> i128 {
        i128::from(self)
    }
}

impl Width for u32 {
    const NAME: &'static str = "u32";
    const BITS: u32 = 32;
    const MAX_BYTES: usize = 5;
    const SIGNED: bool = false;
    fn encode(self, out: &mut Vec<u8>) {
        encode_u32(self, out);
    }
    fn decode(input: &[u8]) -> Result<(Self, usize), DecodeError> {
        decode_u32(input)
    }
    fn from_word(word: u64) -> Self {
        u32::from_le_bytes(low_four(word))
    }
    fn wide(self) -> i128 {
        i128::from(self)
    }
}

impl Width for i64 {
    const NAME: &'static str = "i64";
    const BITS: u32 = 64;
    const MAX_BYTES: usize = 10;
    const SIGNED: bool = true;
    fn encode(self, out: &mut Vec<u8>) {
        encode_i64(self, out);
    }
    fn decode(input: &[u8]) -> Result<(Self, usize), DecodeError> {
        decode_i64(input)
    }
    fn from_word(word: u64) -> Self {
        i64::from_le_bytes(word.to_le_bytes())
    }
    fn wide(self) -> i128 {
        i128::from(self)
    }
}

impl Width for i32 {
    const NAME: &'static str = "i32";
    const BITS: u32 = 32;
    const MAX_BYTES: usize = 5;
    const SIGNED: bool = true;
    fn encode(self, out: &mut Vec<u8>) {
        encode_i32(self, out);
    }
    fn decode(input: &[u8]) -> Result<(Self, usize), DecodeError> {
        decode_i32(input)
    }
    fn from_word(word: u64) -> Self {
        i32::from_le_bytes(low_four(word))
    }
    fn wide(self) -> i128 {
        i128::from(self)
    }
}

/// The canonical encoding of `value`.
pub fn encoded<W: Width>(value: W) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// A value of the width at a random magnitude: the draw is shifted right by a
/// random amount, so small values and huge values are equally likely.
fn draw_value<W: Width>(attacker: &mut Attacker) -> W {
    let word = attacker.word();
    W::from_word(word.wrapping_shr(attacker.field(0, 64)))
}

/// The fewest 7-bit groups that hold `value`, written from the definition: `n`
/// groups hold `0..2^(7n)` unsigned and `-2^(7n-1)..2^(7n-1)` signed.
fn reference_len(value: i128, signed: bool) -> usize {
    let mut groups = 1_usize;
    let mut capacity = 128_i128;
    while groups < 10 {
        let half = capacity.div_euclid(2);
        let (low, high) = if signed {
            (half.saturating_neg(), half.saturating_sub(1))
        } else {
            (0, capacity.saturating_sub(1))
        };
        if (low..=high).contains(&value) {
            return groups;
        }
        groups = groups.saturating_add(1);
        capacity = capacity.saturating_mul(128);
    }
    groups
}

/// Whether `value` is inside the range of the width `Target`.
fn narrow_holds<Target: Width>(value: i128) -> bool {
    let (low, high) = if Target::SIGNED {
        let half = 2_i128.pow(Target::BITS.saturating_sub(1));
        (half.saturating_neg(), half.saturating_sub(1))
    } else {
        (0, 2_i128.pow(Target::BITS).saturating_sub(1))
    };
    (low..=high).contains(&value)
}

/// Checks that whatever the decoder accepted is the canonical spelling of what
/// it returned and no longer than the width allows, and reports whether it
/// accepted.
fn check_canonical<W: Width>(seed: u64, round: usize, bytes: &[u8]) -> bool {
    let Ok((value, used)) = W::decode(bytes) else {
        return false;
    };
    assert!(
        (1..=W::MAX_BYTES).contains(&used) && used <= bytes.len(),
        "seed {seed:#x} round {round}: {} consumed {used} of {} bytes",
        W::NAME,
        bytes.len()
    );
    assert_eq!(
        encoded(value),
        bytes[..used].to_vec(),
        "seed {seed:#x} round {round}: {bytes:02x?} was accepted as {value:?} but is not its canonical spelling"
    );
    true
}

/// Random short byte strings: whatever is accepted is the one canonical
/// spelling of its value (no second spelling forges a hash), and a reasonable
/// share of the draws is accepted so the property is not vacuous.
pub fn canonical_on_random_bytes<W: Width>() -> TestResult {
    let mut accepted = 0_usize;
    sweep(2_048, |seed, round, attacker| {
        let len = 1_usize.saturating_add(attacker.below(12));
        if check_canonical::<W>(seed, round, &attacker.bytes(len)) {
            accepted = accepted.saturating_add(1);
        }
        Ok(())
    })?;
    assert!(
        accepted > 1_500,
        "{}: only {accepted} draws were accepted",
        W::NAME
    );
    Ok(())
}

/// Valid encodings after two rounds of byte mutation: the attacker starts from
/// something the reader accepts and nudges it, which is how real malleability
/// reports are found.
pub fn mutated_encodings_stay_canonical<W: Width>() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let value = draw_value::<W>(attacker);
        let hostile = attacker.mutate_n(&encoded(value), 2);
        check_canonical::<W>(seed, round, &hostile);
        Ok(())
    })
}

/// A redundant final group, in the sign-extending form the width calls
/// redundant, is refused where the redundancy sits (or at the last group when
/// the padded value would be too long).
pub fn padding_is_refused<W: Width>() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let value = draw_value::<W>(attacker);
        let mut padded = encoded(value);
        let used = padded.len();
        if let Some(last) = padded.last_mut() {
            *last |= 0x80;
        }
        padded.push(if W::SIGNED && value.wide() < 0 {
            0x7f
        } else {
            0x00
        });
        let expected = if used < W::MAX_BYTES {
            DecodeError::NonMinimal { at: used }
        } else {
            DecodeError::Overflow {
                at: W::MAX_BYTES.saturating_sub(1),
            }
        };
        assert_eq!(
            W::decode(&padded),
            Err(expected),
            "seed {seed:#x} round {round}: {value:?} padded to {padded:02x?}"
        );
        Ok(())
    })
}

/// A value cut at every length is an unexpected end at the cut, never a
/// shorter integer, and the whole encoding decodes to its value.
pub fn every_proper_prefix_is_an_unexpected_end<W: Width>() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let value = draw_value::<W>(attacker);
        let whole = encoded(value);
        for cut in 0..whole.len() {
            assert_eq!(
                W::decode(&whole[..cut]),
                Err(DecodeError::UnexpectedEnd { at: cut }),
                "seed {seed:#x} round {round}: {value:?} cut to {cut} bytes"
            );
        }
        assert_eq!(
            W::decode(&whole),
            Ok((value, whole.len())),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// Bytes after a value are the caller's: the reader consumes exactly the value,
/// whatever follows, including bytes that would extend it if it read on.
pub fn trailing_bytes_are_left_alone<W: Width>() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let value = draw_value::<W>(attacker);
        let mut input = encoded(value);
        let used = input.len();
        let tail = attacker.below(33);
        input.extend(attacker.bytes(tail));
        input.extend([0x80_u8, 0xff, 0x00]);
        assert_eq!(
            W::decode(&input),
            Ok((value, used)),
            "seed {seed:#x} round {round}: {value:?} followed by {tail} hostile bytes"
        );
        Ok(())
    })
}

/// Zero, and minus one for the signed widths, padded to every depth the width
/// allows: each is refused at the first redundant group.
pub fn padding_to_any_depth_is_refused<W: Width>() {
    for pad in 1..W::MAX_BYTES {
        let mut zero = vec![0x80; pad];
        zero.push(0x00);
        assert_eq!(
            W::decode(&zero),
            Err(DecodeError::NonMinimal { at: pad }),
            "{}: zero padded by {pad} groups",
            W::NAME
        );
        if W::SIGNED {
            let mut minus_one = vec![0xff; pad];
            minus_one.push(0x7f);
            assert_eq!(
                W::decode(&minus_one),
                Err(DecodeError::NonMinimal { at: pad }),
                "{}: minus one padded by {pad} groups",
                W::NAME
            );
        }
    }
}

/// A run of continuation bytes at least as long as the width is an overflow at
/// the last group the width has, however long the run is: the loop is bounded
/// by the width, not by the attacker's patience.
pub fn a_run_of_continuations_overflows_at_the_last_group<W: Width>() {
    let last = W::MAX_BYTES.saturating_sub(1);
    for run in W::MAX_BYTES..=W::MAX_BYTES.saturating_add(90) {
        let mut bytes = vec![0x80; run];
        bytes.push(0x00);
        assert_eq!(
            W::decode(&bytes),
            Err(DecodeError::Overflow { at: last }),
            "{}: a run of {run} continuations",
            W::NAME
        );
    }
}

/// An input that is all continuation bytes is an unexpected end while it is
/// shorter than the width and an overflow at the last group once it is not.
pub fn dangling_continuations_end_exactly<W: Width>() {
    let last = W::MAX_BYTES.saturating_sub(1);
    for run in 1..W::MAX_BYTES {
        assert_eq!(
            W::decode(&vec![0xff; run]),
            Err(DecodeError::UnexpectedEnd { at: run }),
            "{}: {run} dangling continuations",
            W::NAME
        );
    }
    for run in W::MAX_BYTES..=W::MAX_BYTES.saturating_add(20) {
        assert_eq!(
            W::decode(&vec![0xff; run]),
            Err(DecodeError::Overflow { at: last }),
            "{}: {run} continuations",
            W::NAME
        );
    }
}

/// Encoders emit the fewest groups: the length of every drawn value equals the
/// reference length, so the canonical form the readers insist on is the form
/// the writers produce.
pub fn encodings_have_exactly_the_minimal_length<W: Width>() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let value = draw_value::<W>(attacker);
        assert_eq!(
            encoded(value).len(),
            reference_len(value.wide(), W::SIGNED),
            "seed {seed:#x} round {round}: {value:?}"
        );
        Ok(())
    })
}

/// The values either side of every power of two round-trip, with the length the
/// reference gives: the group boundaries are where encoders slip.
pub fn boundaries_around_every_power_of_two_round_trip<W: Width>() {
    for exponent in 0_u32..64 {
        let base = 1_u64.rotate_left(exponent);
        for word in [
            base.wrapping_sub(1),
            base,
            base.wrapping_add(1),
            base.wrapping_neg(),
        ] {
            let value = W::from_word(word);
            let bytes = encoded(value);
            assert_eq!(
                W::decode(&bytes),
                Ok((value, bytes.len())),
                "{}: {value:?} near 2^{exponent}",
                W::NAME
            );
            assert_eq!(
                bytes.len(),
                reference_len(value.wide(), W::SIGNED),
                "{}: {value:?} near 2^{exponent} has a non-minimal length",
                W::NAME
            );
        }
    }
}

/// A stream of hostile varints is read by advancing by what each read consumed:
/// the cursor moves at least one byte and at most the width, never past the end,
/// and the read ends, whatever the attacker put in the stream.
pub fn a_stream_read_always_advances_and_ends<W: Width>() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let mut stream = Vec::new();
        let count = 1_usize.saturating_add(attacker.below(8));
        for _ in 0..count {
            draw_value::<W>(attacker).encode(&mut stream);
        }
        let stream = attacker.mutate_n(&stream, 2);
        let (mut cursor, mut steps) = (0_usize, 0_usize);
        while cursor < stream.len() {
            let Ok((_, used)) = W::decode(&stream[cursor..]) else {
                break;
            };
            assert!(
                (1..=W::MAX_BYTES).contains(&used) && used <= stream.len().saturating_sub(cursor),
                "seed {seed:#x} round {round}: a read at {cursor} consumed {used}"
            );
            cursor = cursor.saturating_add(used);
            steps = steps.saturating_add(1);
        }
        assert!(
            steps <= stream.len(),
            "seed {seed:#x} round {round}: more reads than bytes"
        );
        Ok(())
    })
}

/// A value is accepted by a narrower reader exactly when it fits, and then it
/// is the same value: a wide encoding is never silently truncated.
pub fn a_narrower_reader_takes_exactly_what_fits<Wide: Width, Narrow: Width>() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let value = draw_value::<Wide>(attacker);
        let bytes = encoded(value);
        let read = Narrow::decode(&bytes);
        assert_eq!(
            read.is_ok(),
            narrow_holds::<Narrow>(value.wide()),
            "seed {seed:#x} round {round}: {value:?} read as {} from {bytes:02x?}",
            Narrow::NAME
        );
        if let Ok((narrow, used)) = read {
            assert_eq!(
                narrow.wide(),
                value.wide(),
                "seed {seed:#x} round {round}: truncated"
            );
            assert_eq!(used, bytes.len(), "seed {seed:#x} round {round}: consumed");
        }
        Ok(())
    })
}
