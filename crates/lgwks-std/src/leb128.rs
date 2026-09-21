//! `leb128` owns variable-length integer encoding and decoding for WebAssembly
//! and canonical binary serialization, enforcing INV-LEB128-MINIMAL: all
//! decoders reject non-minimal or overflowing encodings to prevent malleability.

use std::error::Error;
use std::fmt;

/// Why an LEB128 sequence could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Input ended unexpectedly before the terminating byte.
    UnexpectedEnd {
        /// Offset where input terminated.
        at: usize,
    },
    /// Value exceeds the target integer width (overflow).
    Overflow {
        /// Offset where overflow was encountered.
        at: usize,
    },
    /// The encoding is not canonically minimal (e.g. redundant padding bytes).
    NonMinimal {
        /// Offset of the redundant non-minimal byte.
        at: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnexpectedEnd { at } => {
                write!(f, "unexpected end of LEB128 input at offset {at}")
            }
            Self::Overflow { at } => {
                write!(f, "LEB128 value exceeds integer capacity at offset {at}")
            }
            Self::NonMinimal { at } => {
                write!(f, "non-minimal LEB128 encoding rejected at offset {at}")
            }
        }
    }
}

impl Error for DecodeError {}

/// Narrows one masked 7-bit group (`value & 0x7f`) to the byte that carries it.
///
/// Every caller masks with `0x7f` first, so the argument is always in
/// `0..=127` and the narrowing is lossless for `u8`, `u32`, `u64`, `i32` and
/// `i64` alike. The `unwrap_or` arm therefore cannot be reached; it exists
/// because the target type is narrower than every source type and this crate
/// bans `as`, not because a truncation is expected.
fn group_byte<T: TryInto<u8>>(group: T) -> u8 {
    group.try_into().ok().unwrap_or(0)
}

/// Emits the low 7-bit group of `integer` and shifts it down by 7 in place.
///
/// Returns the byte to write and whether the encoding is now complete. The
/// continuation bit (0x80) is set by the caller's step only while more groups
/// remain, so the byte here is always a payload byte.
fn encode_u64_step(integer: &mut u64) -> (u8, bool) {
    let mut byte = group_byte(*integer & 0x7f);
    *integer >>= 7;
    let done = *integer == 0;
    if !done {
        byte |= 0x80;
    }
    (byte, done)
}

/// Encodes an unsigned 64-bit integer into unsigned LEB128 (varuint) bytes.
pub fn encode_u64(mut integer: u64, out: &mut Vec<u8>) {
    loop {
        let (byte, done) = encode_u64_step(&mut integer);
        out.push(byte);
        if done {
            break;
        }
    }
}

/// Rejects a 64-bit group that would not fit the destination width.
///
/// The tenth group (`shift == 63`) has only one usable bit, so its payload must
/// be 0 or 1 *and* it must terminate the sequence; anything wider is the
/// overflow this decoder exists to refuse. `shift > 63` catches a sequence
/// longer than ten bytes, which INV-LEB128-MINIMAL rejects as malleable.
fn check_overflow_u64(shift: usize, chunk: u64, byte: u8, index: usize) -> Result<(), DecodeError> {
    if (shift == 63 && (chunk > 1 || (byte & 0x80) != 0)) || shift > 63 {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

/// Rejects a padding group that a shorter encoding would have avoided.
///
/// A zero payload byte after the first group says the encoder wrote a redundant
/// leading zero, so the encoding is non-minimal and a second, different byte
/// string would decode to the same integer. Rejecting it keeps every value's
/// byte representation unique.
fn check_minimal_u64(index: usize, byte: u8) -> Result<(), DecodeError> {
    if index > 0 && byte == 0x00 {
        Err(DecodeError::NonMinimal { at: index })
    } else {
        Ok(())
    }
}

/// Folds one input byte into the 64-bit accumulator.
///
/// Returns `Ok(Some(consumed))` when the terminating byte has been read, where
/// `consumed` counts bytes through and including it, and `Ok(None)` while the
/// continuation bit says more bytes follow. Validation happens before the
/// accumulated value is written, so a rejected group never contaminates
/// `result`.
fn decode_u64_byte(
    byte: u8,
    index: usize,
    shift: usize,
    result: &mut u64,
) -> Result<Option<usize>, DecodeError> {
    let chunk = u64::from(byte & 0x7f);
    check_overflow_u64(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_u64(index, byte)?;
        // `index` is a valid byte offset into the input, so it is below
        // `isize::MAX` and the increment cannot overflow; saturating keeps it
        // total without a panic path.
        Ok(Some(index.saturating_add(1)))
    } else {
        Ok(None)
    }
}

/// Decodes an unsigned 64-bit integer from unsigned LEB128 bytes.
/// Returns the decoded integer and the number of bytes consumed.
pub fn decode_u64(input: &[u8]) -> Result<(u64, usize), DecodeError> {
    let mut result: u64 = 0;
    for (index, &byte) in input.iter().enumerate() {
        // Seven bits per group. A sequence long enough to overflow `index * 7`
        // cannot describe a valid u64, so saturating to `usize::MAX` makes the
        // overflow check below reject it rather than letting a wrapped shift
        // amount resurrect a value the encoder never wrote.
        let shift = index.saturating_mul(7);
        if let Some(consumed) = decode_u64_byte(byte, index, shift, &mut result)? {
            return Ok((result, consumed));
        }
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

/// Emits the low 7-bit group of `integer` and shifts it down by 7 in place.
///
/// The 32-bit twin of [`encode_u64_step`], and identical in shape: the value
/// width is the only difference, so the same bytes are produced for any integer
/// that fits both types.
fn encode_u32_step(integer: &mut u32) -> (u8, bool) {
    let mut byte = group_byte(*integer & 0x7f);
    *integer >>= 7;
    let done = *integer == 0;
    if !done {
        byte |= 0x80;
    }
    (byte, done)
}

/// Encodes an unsigned 32-bit integer into unsigned LEB128 (varuint) bytes.
pub fn encode_u32(mut integer: u32, out: &mut Vec<u8>) {
    loop {
        let (byte, done) = encode_u32_step(&mut integer);
        out.push(byte);
        if done {
            break;
        }
    }
}

/// Rejects a 32-bit group that would not fit the destination width.
///
/// The fifth group (`shift == 28`) has four usable bits, so its payload must
/// stay within `0x0f` and must terminate the sequence. `shift > 28` catches a
/// longer run, mirroring [`check_overflow_u64`].
fn check_overflow_u32(shift: usize, chunk: u32, byte: u8, index: usize) -> Result<(), DecodeError> {
    if (shift == 28 && (chunk > 0x0f || (byte & 0x80) != 0)) || shift > 28 {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

/// Rejects a padding group that a shorter 32-bit encoding would have avoided.
///
/// See [`check_minimal_u64`]: a redundant zero group after the first would make
/// two distinct byte strings decode to the same integer.
fn check_minimal_u32(index: usize, byte: u8) -> Result<(), DecodeError> {
    if index > 0 && byte == 0x00 {
        Err(DecodeError::NonMinimal { at: index })
    } else {
        Ok(())
    }
}

/// Folds one input byte into the 32-bit accumulator.
///
/// Returns `Ok(Some(consumed))` once the terminating byte has been read and
/// `Ok(None)` while the continuation bit says more bytes follow. The overflow
/// and minimality checks run before the value is committed, so a rejected group
/// leaves `result` untouched.
fn decode_u32_byte(
    byte: u8,
    index: usize,
    shift: usize,
    result: &mut u32,
) -> Result<Option<usize>, DecodeError> {
    let chunk = u32::from(byte & 0x7f);
    check_overflow_u32(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_u32(index, byte)?;
        // `index` indexes the input, so it stays below `isize::MAX`; the
        // saturating increment cannot saturate and keeps the path panic-free.
        Ok(Some(index.saturating_add(1)))
    } else {
        Ok(None)
    }
}

/// Decodes an unsigned 32-bit integer from unsigned LEB128 bytes.
/// Returns the decoded integer and the number of bytes consumed.
pub fn decode_u32(input: &[u8]) -> Result<(u32, usize), DecodeError> {
    let mut result: u32 = 0;
    for (index, &byte) in input.iter().enumerate() {
        // See `decode_u64`: saturating to `usize::MAX` routes an impossibly long
        // sequence into the overflow check instead of wrapping the shift amount.
        let shift = index.saturating_mul(7);
        if let Some(consumed) = decode_u32_byte(byte, index, shift, &mut result)? {
            return Ok((result, consumed));
        }
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

/// Emits the low 7-bit group of `integer` and shifts it down by 7 in place.
///
/// Signed LEB128 must terminate as soon as the remaining value is either all
/// zero bits or all one bits *and* the emitted group's sign bit already says so.
/// That test is why `sign_bit` is read back from the payload byte: without it,
/// a positive value whose top payload bit happens to be set would be read as
/// negative by the decoder.
fn encode_i64_step(integer: &mut i64) -> (u8, bool) {
    let mut byte = group_byte(*integer & 0x7f);
    *integer >>= 7;
    let sign_bit = (byte & 0x40) != 0;
    let done = (*integer == 0 && !sign_bit) || (*integer == -1 && sign_bit);
    if !done {
        byte |= 0x80;
    }
    (byte, done)
}

/// Encodes a signed 64-bit integer into signed LEB128 (varint) bytes.
pub fn encode_i64(mut integer: i64, out: &mut Vec<u8>) {
    loop {
        let (byte, done) = encode_i64_step(&mut integer);
        out.push(byte);
        if done {
            break;
        }
    }
}

/// Rejects a signed 64-bit group that would not fit the destination width.
///
/// The tenth group (`shift == 63`) carries a single useful bit and must agree
/// with the sign of the accumulated value: only an all-zero or all-one payload
/// is a valid extension, and it must terminate the sequence. `shift > 63`
/// rejects an over-long sequence.
fn check_overflow_i64(shift: usize, chunk: u64, byte: u8, index: usize) -> Result<(), DecodeError> {
    if (shift == 63 && ((chunk != 0 && chunk != 0x7f) || (byte & 0x80) != 0)) || shift > 63 {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

/// Rejects a sign-extension group a shorter encoding would have avoided.
///
/// A signed value may be padded only with copies of its sign bit: `0x00` after
/// a positive group, `0x7f` after a negative one. `prev_byte` supplies that
/// sign, read from bit 6 of the preceding payload because that bit is the sign
/// of the 7-bit group. Anything else is a redundant byte and two encodings
/// would name the same integer.
fn check_minimal_i64(index: usize, byte: u8, prev_byte: u8) -> Result<(), DecodeError> {
    if index > 0 {
        let prev_sign = (prev_byte & 0x40) != 0;
        if (!prev_sign && byte == 0x00) || (prev_sign && byte == 0x7f) {
            Err(DecodeError::NonMinimal { at: index })
        } else {
            Ok(())
        }
    } else {
        Ok(())
    }
}

/// Widens the accumulated groups into a signed 64-bit value.
///
/// The `as` this replaces was a bit-pattern reinterpretation, not a numeric
/// conversion: once the groups above the sign bit are folded in, the u64 may
/// hold values such as `i64::MIN + 1`'s raw form that a checked conversion would
/// reject. `from_le_bytes`/`to_le_bytes` preserves those bits exactly, so every
/// input the caller accepted still yields the same integer.
///
/// The arithmetic right shift of `!0i64` fills the bits above the final group
/// with ones to extend a negative sign. The `shift < 63` guard keeps
/// `shift + 7` inside the shift width, so the saturating add is a formality.
fn sign_extend_i64(raw_val: u64, shift: usize, byte: u8) -> i64 {
    let mut signed_res = i64::from_le_bytes(raw_val.to_le_bytes());
    if shift < 63 && (byte & 0x40) != 0 {
        signed_res |= (!0i64) << shift.saturating_add(7);
    }
    signed_res
}

/// Folds one input byte into the 64-bit accumulator and sign-extends on the
/// terminating byte.
///
/// `prev_byte` is the previously consumed byte and is used only by the
/// minimality check, so the first byte may pass `0`. On the terminating byte
/// the accumulated groups are widened by [`sign_extend_i64`] and returned with
/// the consumed count; otherwise `Ok(None)` asks the caller for more input.
fn decode_i64_byte(
    byte: u8,
    index: usize,
    shift: usize,
    prev_byte: u8,
    result: &mut u64,
) -> Result<Option<(i64, usize)>, DecodeError> {
    let chunk = u64::from(byte & 0x7f);
    check_overflow_i64(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_i64(index, byte, prev_byte)?;
        let val = sign_extend_i64(*result, shift, byte);
        // `index` indexes the input and so stays below `isize::MAX`; the
        // saturating increment is exact here and only keeps the expression free
        // of the bare `+` this crate forbids on integers.
        Ok(Some((val, index.saturating_add(1))))
    } else {
        Ok(None)
    }
}

/// Decodes a signed 64-bit integer from signed LEB128 bytes.
/// Returns the decoded integer and the number of bytes consumed.
pub fn decode_i64(input: &[u8]) -> Result<(i64, usize), DecodeError> {
    let mut result: u64 = 0;
    let mut prev_byte: u8 = 0;
    for (index, &byte) in input.iter().enumerate() {
        // See `decode_u64`: saturating routes an impossibly long sequence into
        // the overflow check instead of wrapping the shift amount.
        let shift = index.saturating_mul(7);
        if let Some(res) = decode_i64_byte(byte, index, shift, prev_byte, &mut result)? {
            return Ok(res);
        }
        prev_byte = byte;
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

/// Emits the low 7-bit group of `integer` and shifts it down by 7 in place.
///
/// The 32-bit twin of [`encode_i64_step`], including the sign-bit test that
/// decides when a signed encoding may stop.
fn encode_i32_step(integer: &mut i32) -> (u8, bool) {
    let mut byte = group_byte(*integer & 0x7f);
    *integer >>= 7;
    let sign_bit = (byte & 0x40) != 0;
    let done = (*integer == 0 && !sign_bit) || (*integer == -1 && sign_bit);
    if !done {
        byte |= 0x80;
    }
    (byte, done)
}

/// Encodes a signed 32-bit integer into signed LEB128 (varint) bytes.
pub fn encode_i32(mut integer: i32, out: &mut Vec<u8>) {
    loop {
        let (byte, done) = encode_i32_step(&mut integer);
        out.push(byte);
        if done {
            break;
        }
    }
}

/// Rejects a signed 32-bit group that would not fit the destination width.
///
/// The fifth group (`shift == 28`) carries four useful bits and must be a sign
/// extension of the value below it: `0x00`-shaped (bits 3..6 clear) for a
/// positive result, `0x78`-shaped (bits 3..6 set) for a negative one, and it
/// must terminate the sequence. `shift > 28` rejects an over-long run.
fn check_overflow_i32(shift: usize, chunk: u32, byte: u8, index: usize) -> Result<(), DecodeError> {
    if shift > 28
        || (shift == 28
            && (((chunk & 0x78) != 0x00 && (chunk & 0x78) != 0x78) || (byte & 0x80) != 0))
    {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

/// Rejects a sign-extension group a shorter 32-bit encoding would have avoided.
///
/// See [`check_minimal_i64`]: only padding that repeats the sign of the
/// preceding group is canonical.
fn check_minimal_i32(index: usize, byte: u8, prev_byte: u8) -> Result<(), DecodeError> {
    if index > 0 {
        let prev_sign = (prev_byte & 0x40) != 0;
        if (!prev_sign && byte == 0x00) || (prev_sign && byte == 0x7f) {
            Err(DecodeError::NonMinimal { at: index })
        } else {
            Ok(())
        }
    } else {
        Ok(())
    }
}

/// Widens the accumulated groups into a signed 32-bit value.
///
/// As with [`sign_extend_i64`], the `as` this replaces reinterpreted the bit
/// pattern rather than converting a number: the u32 may hold the raw form of
/// `i32::MIN`, which a checked conversion would reject. `from_le_bytes` on the
/// same four bytes reproduces the identical integer for every accepted input.
///
/// The `shift < 28` guard keeps `shift + 7` within the shift width of an i32.
fn sign_extend_i32(raw_val: u32, shift: usize, byte: u8) -> i32 {
    let mut signed_res = i32::from_le_bytes(raw_val.to_le_bytes());
    if shift < 28 && (byte & 0x40) != 0 {
        signed_res |= (!0i32) << shift.saturating_add(7);
    }
    signed_res
}

/// Folds one input byte into the 32-bit accumulator and sign-extends on the
/// terminating byte.
///
/// `prev_byte` is the previously consumed byte, used only by the minimality
/// check; the first byte may pass `0`. On the terminating byte the result is
/// widened by [`sign_extend_i32`] and returned with the consumed count,
/// otherwise `Ok(None)` asks for more input.
fn decode_i32_byte(
    byte: u8,
    index: usize,
    shift: usize,
    prev_byte: u8,
    result: &mut u32,
) -> Result<Option<(i32, usize)>, DecodeError> {
    let chunk = u32::from(byte & 0x7f);
    check_overflow_i32(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_i32(index, byte, prev_byte)?;
        let val = sign_extend_i32(*result, shift, byte);
        // `index` indexes the input and so cannot reach a saturating offset;
        // saturating_add is used only to keep the expression free of the bare
        // `+` this crate forbids on integers.
        Ok(Some((val, index.saturating_add(1))))
    } else {
        Ok(None)
    }
}

/// Decodes a signed 32-bit integer from signed LEB128 bytes.
/// Returns the decoded integer and the number of bytes consumed.
pub fn decode_i32(input: &[u8]) -> Result<(i32, usize), DecodeError> {
    let mut result: u32 = 0;
    let mut prev_byte: u8 = 0;
    for (index, &byte) in input.iter().enumerate() {
        // See `decode_u64`: saturating routes an impossibly long sequence into
        // the overflow check instead of wrapping the shift amount.
        let shift = index.saturating_mul(7);
        if let Some(res) = decode_i32_byte(byte, index, shift, prev_byte, &mut result)? {
            return Ok(res);
        }
        prev_byte = byte;
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // These tests return `Result` rather than unwrapping: a LEB128 refusal
    // reports its own `Debug` on failure, which is the same report `.unwrap`
    // would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn u64_roundtrip_vectors() -> Result<(), DecodeError> {
        let cases = [0u64, 1, 127, 128, 255, 624485, u64::MAX];
        for integer in cases {
            let mut buf = Vec::new();
            encode_u64(integer, &mut buf);
            let (decoded, consumed) = decode_u64(&buf)?;
            assert_eq!(decoded, integer);
            assert_eq!(consumed, buf.len());
        }
        Ok(())
    }

    #[test]
    fn i64_roundtrip_vectors() -> Result<(), DecodeError> {
        let cases = [
            0i64,
            1,
            -1,
            63,
            -64,
            127,
            -128,
            624485,
            -624485,
            i64::MIN,
            i64::MAX,
        ];
        for integer in cases {
            let mut buf = Vec::new();
            encode_i64(integer, &mut buf);
            let (decoded, consumed) = decode_i64(&buf)?;
            assert_eq!(decoded, integer, "failed for {integer}");
            assert_eq!(consumed, buf.len());
        }
        Ok(())
    }

    #[test]
    fn standard_wasm_vectors() {
        // Standard LEB128 624485 = 0xE5 0x8E 0x26
        let mut buf = Vec::new();
        encode_u64(624485, &mut buf);
        assert_eq!(buf, vec![0xe5, 0x8e, 0x26]);

        // -624485 = 0x9B 0xF1 0x59
        let mut sbuf = Vec::new();
        encode_i64(-624485, &mut sbuf);
        assert_eq!(sbuf, vec![0x9b, 0xf1, 0x59]);
    }

    #[test]
    fn rejects_non_minimal_padding() {
        // 0 encoded as 2 bytes: [0x80, 0x00]
        let non_minimal = [0x80, 0x00];
        assert_eq!(
            decode_u64(&non_minimal),
            Err(DecodeError::NonMinimal { at: 1 })
        );
        assert_eq!(
            decode_i64(&non_minimal),
            Err(DecodeError::NonMinimal { at: 1 })
        );

        // -1 encoded as 2 bytes: [0xFF, 0x7F]
        let non_minimal_signed = [0xff, 0x7f];
        assert_eq!(
            decode_i64(&non_minimal_signed),
            Err(DecodeError::NonMinimal { at: 1 })
        );
    }

    #[test]
    fn rejects_overflow_and_never_panics() {
        // 11 continuation bytes
        let malformed = vec![0x80; 11];
        assert_eq!(decode_u64(&malformed), Err(DecodeError::Overflow { at: 9 }));
        assert_eq!(decode_i64(&malformed), Err(DecodeError::Overflow { at: 9 }));

        // 10 continuation bytes + 0x00
        let mut malformed_term = vec![0x80; 10];
        malformed_term.push(0x00);
        assert_eq!(
            decode_u64(&malformed_term),
            Err(DecodeError::Overflow { at: 9 })
        );
        assert_eq!(
            decode_i64(&malformed_term),
            Err(DecodeError::Overflow { at: 9 })
        );
    }

    #[test]
    fn u32_roundtrip_vectors() -> Result<(), DecodeError> {
        let cases = [0u32, 1, 127, 128, 255, 624485, u32::MAX];
        for integer in cases {
            let mut buf = Vec::new();
            encode_u32(integer, &mut buf);
            let (decoded, consumed) = decode_u32(&buf)?;
            assert_eq!(decoded, integer);
            assert_eq!(consumed, buf.len());
        }
        Ok(())
    }

    #[test]
    fn i32_roundtrip_vectors() -> Result<(), DecodeError> {
        let cases = [
            0i32,
            1,
            -1,
            63,
            -64,
            127,
            -128,
            624485,
            -624485,
            i32::MIN,
            i32::MAX,
        ];
        for integer in cases {
            let mut buf = Vec::new();
            encode_i32(integer, &mut buf);
            let (decoded, consumed) = decode_i32(&buf)?;
            assert_eq!(decoded, integer, "failed for {integer}");
            assert_eq!(consumed, buf.len());
        }
        Ok(())
    }

    #[test]
    fn u32_i32_fixed_vectors() {
        // 624485 = 0xE5 0x8E 0x26 (unsigned, same bytes at every width)
        let mut buf = Vec::new();
        encode_u32(624485, &mut buf);
        assert_eq!(buf, vec![0xe5, 0x8e, 0x26]);

        // -1 is a single 0x7f at every signed width
        let mut neg = Vec::new();
        encode_i32(-1, &mut neg);
        assert_eq!(neg, vec![0x7f]);

        // 64 needs the sign-clearing second byte: [0xC0, 0x00]
        let mut sixty_four = Vec::new();
        encode_i32(64, &mut sixty_four);
        assert_eq!(sixty_four, vec![0xc0, 0x00]);
    }

    #[test]
    fn u32_i32_reject_non_minimal_and_overflow() {
        // 0 encoded as 2 bytes: [0x80, 0x00]
        let non_minimal = [0x80, 0x00];
        assert_eq!(
            decode_u32(&non_minimal),
            Err(DecodeError::NonMinimal { at: 1 })
        );
        assert_eq!(
            decode_i32(&non_minimal),
            Err(DecodeError::NonMinimal { at: 1 })
        );

        // 6 continuation bytes: overflows 32-bit widths at index 4
        let malformed = vec![0x80; 6];
        assert_eq!(decode_u32(&malformed), Err(DecodeError::Overflow { at: 4 }));
        assert_eq!(decode_i32(&malformed), Err(DecodeError::Overflow { at: 4 }));

        // 5 continuation bytes + 0x00: the 5th continuation already overflows
        let mut malformed_term = vec![0x80; 5];
        malformed_term.push(0x00);
        assert_eq!(
            decode_u32(&malformed_term),
            Err(DecodeError::Overflow { at: 4 })
        );
        assert_eq!(
            decode_i32(&malformed_term),
            Err(DecodeError::Overflow { at: 4 })
        );
    }
}
