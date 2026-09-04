//! `leb128` owns variable-length integer encoding and decoding for WebAssembly
//! and canonical binary serialization, enforcing INV-LEB128-MINIMAL: all
//! decoders reject non-minimal or overflowing encodings to prevent malleability.

use std::error::Error;
use std::fmt;

/// Why an LEB128 sequence could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        match self {
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

fn encode_u64_step(integer: &mut u64) -> (u8, bool) {
    let mut byte = (*integer & 0x7f) as u8;
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

fn check_overflow_u64(shift: usize, chunk: u64, byte: u8, index: usize) -> Result<(), DecodeError> {
    if (shift == 63 && (chunk > 1 || (byte & 0x80) != 0)) || shift > 63 {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

fn check_minimal_u64(index: usize, byte: u8) -> Result<(), DecodeError> {
    if index > 0 && byte == 0x00 {
        Err(DecodeError::NonMinimal { at: index })
    } else {
        Ok(())
    }
}

fn decode_u64_byte(
    byte: u8,
    index: usize,
    shift: usize,
    result: &mut u64,
) -> Result<Option<usize>, DecodeError> {
    let chunk = (byte & 0x7f) as u64;
    check_overflow_u64(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_u64(index, byte)?;
        Ok(Some(index + 1))
    } else {
        Ok(None)
    }
}

/// Decodes an unsigned 64-bit integer from unsigned LEB128 bytes.
/// Returns the decoded integer and the number of bytes consumed.
pub fn decode_u64(input: &[u8]) -> Result<(u64, usize), DecodeError> {
    let mut result: u64 = 0;
    for (index, &byte) in input.iter().enumerate() {
        let shift = index * 7;
        if let Some(consumed) = decode_u64_byte(byte, index, shift, &mut result)? {
            return Ok((result, consumed));
        }
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

fn encode_u32_step(integer: &mut u32) -> (u8, bool) {
    let mut byte = (*integer & 0x7f) as u8;
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

fn check_overflow_u32(shift: usize, chunk: u32, byte: u8, index: usize) -> Result<(), DecodeError> {
    if (shift == 28 && (chunk > 0x0f || (byte & 0x80) != 0)) || shift > 28 {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

fn check_minimal_u32(index: usize, byte: u8) -> Result<(), DecodeError> {
    if index > 0 && byte == 0x00 {
        Err(DecodeError::NonMinimal { at: index })
    } else {
        Ok(())
    }
}

fn decode_u32_byte(
    byte: u8,
    index: usize,
    shift: usize,
    result: &mut u32,
) -> Result<Option<usize>, DecodeError> {
    let chunk = (byte & 0x7f) as u32;
    check_overflow_u32(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_u32(index, byte)?;
        Ok(Some(index + 1))
    } else {
        Ok(None)
    }
}

/// Decodes an unsigned 32-bit integer from unsigned LEB128 bytes.
/// Returns the decoded integer and the number of bytes consumed.
pub fn decode_u32(input: &[u8]) -> Result<(u32, usize), DecodeError> {
    let mut result: u32 = 0;
    for (index, &byte) in input.iter().enumerate() {
        let shift = index * 7;
        if let Some(consumed) = decode_u32_byte(byte, index, shift, &mut result)? {
            return Ok((result, consumed));
        }
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

fn encode_i64_step(integer: &mut i64) -> (u8, bool) {
    let mut byte = (*integer & 0x7f) as u8;
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

fn check_overflow_i64(shift: usize, chunk: u64, byte: u8, index: usize) -> Result<(), DecodeError> {
    if (shift == 63 && ((chunk != 0 && chunk != 0x7f) || (byte & 0x80) != 0)) || shift > 63 {
        Err(DecodeError::Overflow { at: index })
    } else {
        Ok(())
    }
}

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

fn sign_extend_i64(raw_val: u64, shift: usize, byte: u8) -> i64 {
    let mut signed_res = raw_val as i64;
    if shift < 63 && (byte & 0x40) != 0 {
        signed_res |= (!0i64) << (shift + 7);
    }
    signed_res
}

fn decode_i64_byte(
    byte: u8,
    index: usize,
    shift: usize,
    prev_byte: u8,
    result: &mut u64,
) -> Result<Option<(i64, usize)>, DecodeError> {
    let chunk = (byte & 0x7f) as u64;
    check_overflow_i64(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_i64(index, byte, prev_byte)?;
        let val = sign_extend_i64(*result, shift, byte);
        Ok(Some((val, index + 1)))
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
        let shift = index * 7;
        if let Some(res) = decode_i64_byte(byte, index, shift, prev_byte, &mut result)? {
            return Ok(res);
        }
        prev_byte = byte;
    }
    Err(DecodeError::UnexpectedEnd { at: input.len() })
}

fn encode_i32_step(integer: &mut i32) -> (u8, bool) {
    let mut byte = (*integer & 0x7f) as u8;
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

fn sign_extend_i32(raw_val: u32, shift: usize, byte: u8) -> i32 {
    let mut signed_res = raw_val as i32;
    if shift < 28 && (byte & 0x40) != 0 {
        signed_res |= (!0i32) << (shift + 7);
    }
    signed_res
}

fn decode_i32_byte(
    byte: u8,
    index: usize,
    shift: usize,
    prev_byte: u8,
    result: &mut u32,
) -> Result<Option<(i32, usize)>, DecodeError> {
    let chunk = (byte & 0x7f) as u32;
    check_overflow_i32(shift, chunk, byte, index)?;
    *result |= chunk << shift;
    if (byte & 0x80) == 0 {
        check_minimal_i32(index, byte, prev_byte)?;
        let val = sign_extend_i32(*result, shift, byte);
        Ok(Some((val, index + 1)))
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
        let shift = index * 7;
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

    #[test]
    fn u64_roundtrip_vectors() {
        let cases = [0u64, 1, 127, 128, 255, 624485, u64::MAX];
        for integer in cases {
            let mut buf = Vec::new();
            encode_u64(integer, &mut buf);
            let (decoded, consumed) = decode_u64(&buf).unwrap();
            assert_eq!(decoded, integer);
            assert_eq!(consumed, buf.len());
        }
    }

    #[test]
    fn i64_roundtrip_vectors() {
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
            let (decoded, consumed) = decode_i64(&buf).unwrap();
            assert_eq!(decoded, integer, "failed for {integer}");
            assert_eq!(consumed, buf.len());
        }
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
    fn u32_roundtrip_vectors() {
        let cases = [0u32, 1, 127, 128, 255, 624485, u32::MAX];
        for integer in cases {
            let mut buf = Vec::new();
            encode_u32(integer, &mut buf);
            let (decoded, consumed) = decode_u32(&buf).unwrap();
            assert_eq!(decoded, integer);
            assert_eq!(consumed, buf.len());
        }
    }

    #[test]
    fn i32_roundtrip_vectors() {
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
            let (decoded, consumed) = decode_i32(&buf).unwrap();
            assert_eq!(decoded, integer, "failed for {integer}");
            assert_eq!(consumed, buf.len());
        }
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
