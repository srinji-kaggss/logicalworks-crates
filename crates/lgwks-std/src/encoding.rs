//! `encoding` owns base64 and percent-encoding, enforcing
//! INV-ENCODING-STRICT-DECODE: both decoders refuse malformed input with a
//! typed error rather than skipping bytes, so a corrupted value cannot
//! round-trip into something plausible.

// ── base64 ──────────────────────────────────────────────────────────────────

/// RFC 4648 standard base64 with `+`, `/`, and `=` padding.
pub mod base64 {
    use std::error::Error;
    use std::fmt;

    /// The 64 characters of the RFC 4648 standard alphabet in sextet order:
    /// index `n` is the character that encodes the six-bit value `n`.
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Emits the first two characters of a quantum.
    ///
    /// The first carries the top six bits of `b0`; the second carries the two
    /// low bits of `b0` above the top four bits of `b1`. Both indices are six
    /// bits wide by construction, so they always index inside `ALPHABET`.
    fn encode_first_pair(b0: u8, b1: u8, out: &mut String) {
        out.push(char::from(ALPHABET[usize::from(b0 >> 2)]));
        out.push(char::from(
            ALPHABET[usize::from(((b0 & 0x03) << 4) | (b1 >> 4))],
        ));
    }

    /// Emits the third character of a quantum, or `=` when the quantum holds
    /// only one significant byte.
    ///
    /// `has_b1` distinguishes a real second byte from the zero byte the caller
    /// substituted for a short final chunk; the emitted sextet is four bits of
    /// `b1` over the top two bits of `b2`.
    fn encode_third_char(b1: u8, b2: u8, has_b1: bool, out: &mut String) {
        if has_b1 {
            out.push(char::from(
                ALPHABET[usize::from(((b1 & 0x0f) << 2) | (b2 >> 6))],
            ));
        } else {
            out.push('=');
        }
    }

    /// Emits the fourth character of a quantum, or `=` when the quantum holds
    /// fewer than three significant bytes.
    ///
    /// `has_b2` is the caller's statement that a real third byte existed; the
    /// six bits emitted are the low bits of `b2` alone.
    fn encode_fourth_char(b2: u8, has_b2: bool, out: &mut String) {
        if has_b2 {
            out.push(char::from(ALPHABET[usize::from(b2 & 0x3f)]));
        } else {
            out.push('=');
        }
    }

    /// Encodes one chunk of at most three input bytes as its four-character
    /// quantum.
    ///
    /// Missing bytes are read as zero and then masked out by the `has_b1` and
    /// `has_b2` flags, so a short final chunk produces `=` padding instead of
    /// fabricated data. `chunk` must be non-empty; `encode` guarantees that by
    /// iterating with `chunks(3)`.
    fn encode_chunk_chars(chunk: &[u8], out: &mut String) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        encode_first_pair(b0, b1, out);
        encode_third_char(b1, b2, chunk.len() > 1, out);
        encode_fourth_char(b2, chunk.len() > 2, out);
    }

    /// Encodes bytes as padded standard base64.
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let bytes = bytes.as_ref();
        // Four characters per three input bytes, rounding up for a partial
        // final quantum. A slice length is bounded by `isize::MAX`, so the
        // product of the ceiling and four is far below `usize::MAX` and cannot
        // saturate; `saturating_mul` states that bound instead of wrapping.
        let capacity = bytes.len().div_ceil(3).saturating_mul(4);
        let mut out = String::with_capacity(capacity);
        for chunk in bytes.chunks(3) {
            encode_chunk_chars(chunk, &mut out);
        }
        out
    }

    /// Why a base64 string could not be decoded.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum DecodeError {
        /// Padded base64 arrives in four-character quanta.
        BadLength {
            /// Length of the offending input, in bytes.
            len: usize,
            /// Offset where length violation was observed.
            at: usize,
        },
        /// A character outside the standard alphabet appeared.
        NotInAlphabet {
            /// Zero-based offset of the offending character.
            at: usize,
            /// The offending byte, reported verbatim.
            byte: u8,
        },
        /// `=` appeared anywhere but the last one or two positions.
        MisplacedPadding {
            /// Zero-based offset of the offending `=`.
            at: usize,
        },
    }

    impl fmt::Display for DecodeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {
                Self::BadLength { len, at: _ } => {
                    write!(f, "base64 length {len} is not a multiple of 4")
                }
                Self::NotInAlphabet { at, byte } => {
                    write!(f, "byte {byte:#04x} at offset {at} is not base64")
                }
                Self::MisplacedPadding { at } => {
                    write!(f, "padding '=' at offset {at} is not at the end")
                }
            }
        }
    }

    impl Error for DecodeError {}

    /// Refuses any input whose length is not a whole number of four-character
    /// quanta. Padded base64 never has a trailing partial group, so a length
    /// that is not a multiple of four is malformed by construction.
    fn check_base64_quantum(len: usize) -> Result<(), DecodeError> {
        if !len.is_multiple_of(4) {
            Err(DecodeError::BadLength { len, at: len })
        } else {
            Ok(())
        }
    }

    /// Counts the `=` characters terminating `input` and rejects more than the
    /// two a valid final quantum can carry.
    ///
    /// Returns the padding width. The count is a suffix length, so it is never
    /// greater than `input.len()`; the subtraction that locates the first `=`
    /// therefore cannot go below zero.
    fn inspect_padding(input: &[u8]) -> Result<usize, DecodeError> {
        let padding = input.iter().rev().take_while(|&&byte| byte == b'=').count();
        if padding > 2 {
            Err(DecodeError::MisplacedPadding {
                at: input.len().saturating_sub(padding),
            })
        } else {
            Ok(padding)
        }
    }

    /// Rejects any `=` before the trailing padding run.
    ///
    /// A `=` inside the body would make the number of encoded characters
    /// ambiguous, which is exactly the class of malformed input
    /// INV-ENCODING-STRICT-DECODE refuses rather than silently skipping.
    fn check_interior_padding(body: &[u8]) -> Result<(), DecodeError> {
        if let Some(offset) = body.iter().position(|&byte| byte == b'=') {
            Err(DecodeError::MisplacedPadding { at: offset })
        } else {
            Ok(())
        }
    }

    /// The low eight bits of `value`, as a byte.
    ///
    /// `value` is the decoder's bit accumulator immediately after a six-bit
    /// sextet was shifted in. Its meaningful bits are always the low fourteen
    /// (eight residual bits plus six new ones), so every higher bit is a
    /// leftover from a quantum that was already emitted. Narrowing to the low
    /// octet is therefore exact, and naming it here keeps that narrowing a
    /// documented property rather than a silent `as` cast.
    fn low_byte(value: u32) -> u8 {
        value.to_le_bytes()[0]
    }

    /// Folds one body character into the accumulator and emits a byte for
    /// every eight bits that are complete.
    ///
    /// `filled` holds the number of pending bits and is always below eight on
    /// entry and on return, so both the increment (at most 13 bits pending)
    /// and the decrement (at least 8 bits pending in that branch) stay inside
    /// `u32` and cannot underflow.
    fn decode_body_chunk(
        byte: u8,
        index: usize,
        accumulator: &mut u32,
        filled: &mut u32,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let value = sextet(byte).ok_or(DecodeError::NotInAlphabet { at: index, byte })?;
        *accumulator = (*accumulator << 6) | u32::from(value);
        *filled = filled.saturating_add(6);
        if *filled >= 8 {
            *filled = filled.saturating_sub(8);
            out.push(low_byte(*accumulator >> *filled));
        }
        Ok(())
    }

    /// Decodes every character of a padding-free body.
    ///
    /// After the last character, any bits still pending must be zero: nonzero
    /// trailing bits mean the encoding carried data the decoder would have to
    /// discard, which is a malformed quantum rather than a shorter message.
    fn decode_body_bytes(body: &[u8], out: &mut Vec<u8>) -> Result<(), DecodeError> {
        let mut accumulator = 0u32;
        let mut filled = 0u32;
        for (index, &byte) in body.iter().enumerate() {
            decode_body_chunk(byte, index, &mut accumulator, &mut filled, out)?;
        }
        // `filled` is below eight here, and the mask is `1 << filled` with a
        // minimum of one bit, so clearing that bit and subtracting one cannot
        // underflow.
        if filled > 0 && (accumulator & ((1u32 << filled).saturating_sub(1))) != 0 {
            Err(DecodeError::MisplacedPadding { at: body.len() })
        } else {
            Ok(())
        }
    }

    /// Decodes padded standard base64.
    ///
    /// Accepts an empty input as the empty message, then requires the length to
    /// be a quantum, the padding to be well placed, and every body character to
    /// be in the alphabet. Bytes that follow a well-formed prefix are never
    /// ignored: the first malformed character is the error.
    pub fn decode(input: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
        let input = input.as_ref();
        if input.is_empty() {
            return Ok(Vec::new());
        }
        check_base64_quantum(input.len())?;
        let padding = inspect_padding(input)?;
        // `padding` is a count of trailing bytes, so it is at most `len`.
        let body = &input[..input.len().saturating_sub(padding)];
        check_interior_padding(body)?;

        // Three bytes out per four characters in. `check_base64_quantum` has
        // already proved the length is a multiple of four, so the quotient is
        // exact; the divisor is the literal 4 and cannot be zero, and a slice
        // length is bounded by `isize::MAX`, so the product cannot saturate.
        // The zero fallback is unreachable and only ever costs the reservation.
        let capacity = input.len().checked_div(4).unwrap_or(0).saturating_mul(3);
        let mut out = Vec::with_capacity(capacity);
        decode_body_bytes(body, &mut out)?;
        Ok(out)
    }

    /// Maps one base64 character to its six-bit value.
    ///
    /// Returns `None` for anything outside the standard alphabet, including the
    /// URL-safe `-`/`_` pair, so a string encoded with the alternate alphabet
    /// is rejected rather than decoded into different bytes.
    fn sextet(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte.saturating_sub(b'A')),
            b'a'..=b'z' => Some(byte.saturating_sub(b'a').saturating_add(26)),
            b'0'..=b'9' => Some(byte.saturating_sub(b'0').saturating_add(52)),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
}

// ── percent ─────────────────────────────────────────────────────────────────

/// RFC 3986 percent-encoding.
pub mod percent {
    use std::error::Error;
    use std::fmt;

    /// Encodes every byte that is not an RFC 3986 unreserved character, which
    /// is the component-safe set: reserved delimiters such as `/` and `?` are
    /// escaped, so the result is safe to place in a single URL component.
    #[must_use]
    pub fn encode_component(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for &byte in text.as_bytes() {
            if is_unreserved(byte) {
                out.push(char::from(byte));
            } else {
                out.push('%');
                out.push(upper_nibble(byte >> 4));
                out.push(upper_nibble(byte & 0x0f));
            }
        }
        out
    }

    /// Whether `byte` may appear literally in an encoded component.
    ///
    /// The unreserved set is the RFC 3986 one: ASCII letters, digits, `-`,
    /// `_`, `.` and `~`. Every other byte, high-bit ones included, is escaped,
    /// so multi-byte UTF-8 text is encoded one byte at a time.
    fn is_unreserved(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
    }

    /// The uppercase hex digit for `value`, which must be a nibble (`0..=15`).
    ///
    /// Callers pass the two halves of a byte, so the table lookup is in range
    /// by construction; RFC 3986 requires uppercase digits, which is why the
    /// table is not the crate's lowercase `hex` encoder.
    fn upper_nibble(value: u8) -> char {
        char::from(b"0123456789ABCDEF"[usize::from(value)])
    }

    /// Why a percent-encoded string could not be decoded.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum DecodeError {
        /// A `%` was not followed by two hex digits.
        TruncatedEscape {
            /// Zero-based offset of the offending `%`.
            at: usize,
        },
        /// A `%` was followed by something other than hex digits.
        NotHexDigit {
            /// Zero-based offset of the offending `%`.
            at: usize,
        },
        /// The decoded bytes are not valid UTF-8.
        NotUtf8 {
            /// Offset where invalid UTF-8 byte sequence began.
            at: usize,
        },
    }

    impl fmt::Display for DecodeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {
                Self::TruncatedEscape { at } => {
                    write!(f, "escape at offset {at} needs two hex digits")
                }
                Self::NotHexDigit { at } => {
                    write!(f, "escape at offset {at} is not hex")
                }
                Self::NotUtf8 { at } => {
                    write!(
                        f,
                        "decoded bytes are not valid UTF-8 starting at offset {at}"
                    )
                }
            }
        }
    }

    impl Error for DecodeError {}

    /// Decodes the two hex characters of one `%` escape.
    ///
    /// `escape` is the two bytes that followed the `%` and `pos` is the offset
    /// of that `%`, so the underlying hex decoder's own offsets are rebased
    /// into the caller's coordinate space. Both offsets lie inside the same
    /// input, so their sum is far below `usize::MAX` and cannot saturate.
    fn decode_escape_pair(escape: &[u8], pos: usize) -> Result<u8, DecodeError> {
        let decoded = crate::hex::decode(escape).map_err(|hex_err| match hex_err {
            crate::hex::DecodeError::NotHexDigit { at: hex_at, .. } => DecodeError::NotHexDigit {
                at: pos.saturating_add(hex_at),
            },
            crate::hex::DecodeError::OddLength { at: hex_at, .. } => DecodeError::NotHexDigit {
                at: pos.saturating_add(hex_at),
            },
        })?;
        Ok(decoded[0])
    }

    /// Consumes one input position, copying a literal byte or decoding an
    /// escape, and advances `i` past what it consumed.
    ///
    /// `i` indexes inside `bytes` and grows by at most three per call, so the
    /// increment cannot overflow; the range lookup is what turns a `%` with
    /// fewer than two characters left into `TruncatedEscape` instead of a
    /// panic.
    fn decode_step(bytes: &[u8], i: &mut usize, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        if bytes[*i] != b'%' {
            out.push(bytes[*i]);
            *i = i.saturating_add(1);
            Ok(())
        } else {
            let escape = bytes
                .get(i.saturating_add(1)..i.saturating_add(3))
                .ok_or(DecodeError::TruncatedEscape { at: *i })?;
            let byte_val = decode_escape_pair(escape, *i)?;
            out.push(byte_val);
            *i = i.saturating_add(3);
            Ok(())
        }
    }

    /// Decodes percent escapes back into text.
    ///
    /// Literal bytes pass through unchanged, escapes are case-insensitive, and
    /// the result must be valid UTF-8: a sequence of escapes that decodes to an
    /// invalid byte string is reported as [`DecodeError::NotUtf8`] with the
    /// offset where the invalid sequence began, never as replacement
    /// characters. The loop is bounded because each step advances the cursor by
    /// at least one byte.
    pub fn decode(text: &str) -> Result<String, DecodeError> {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut cursor = 0;
        while cursor < bytes.len() {
            decode_step(bytes, &mut cursor, &mut out)?;
        }
        String::from_utf8(out).map_err(|utf8_err| DecodeError::NotUtf8 {
            at: utf8_err.utf8_error().valid_up_to(),
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Tests exercising a fallible decode return `Result`: the refusal reports
    // its own `Debug` on failure, which is the same report `.unwrap` would have
    // panicked with, without an `unwrap` in the tree.
    /// RFC 4648 section 10 test vectors, verbatim.
    #[test]
    fn base64_matches_the_rfc_4648_vectors() -> Result<(), base64::DecodeError> {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(base64::encode(plain), encoded, "encoding {plain:?}");
            assert_eq!(
                base64::decode(encoded)?,
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn base64_roundtrips_every_byte_value() -> Result<(), base64::DecodeError> {
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = base64::encode(&all_bytes);
        let decoded = base64::decode(&encoded)?;
        assert_eq!(decoded, all_bytes);
        Ok(())
    }

    #[test]
    fn base64_refuses_a_length_that_is_not_a_quantum() {
        assert_eq!(
            base64::decode("abc"),
            Err(base64::DecodeError::BadLength { len: 3, at: 3 })
        );
        assert_eq!(
            base64::decode("a"),
            Err(base64::DecodeError::BadLength { len: 1, at: 1 })
        );
    }

    #[test]
    fn base64_refuses_an_alien_character() {
        assert_eq!(
            base64::decode("Zm9v!g=="),
            Err(base64::DecodeError::NotInAlphabet { at: 4, byte: b'!' })
        );
    }

    #[test]
    fn base64_refuses_interior_padding() {
        assert_eq!(
            base64::decode("Zm=v"),
            Err(base64::DecodeError::MisplacedPadding { at: 2 })
        );
    }

    #[test]
    fn base64_rejects_non_zero_padding_bits() {
        assert_eq!(
            base64::decode("Zh=="),
            Err(base64::DecodeError::MisplacedPadding { at: 2 })
        );
    }

    #[test]
    fn percent_escapes_everything_reserved() {
        assert_eq!(
            percent::encode_component("hello world?name=foo/bar"),
            "hello%20world%3Fname%3Dfoo%2Fbar"
        );
    }

    #[test]
    fn percent_roundtrips_multibyte_text() -> Result<(), percent::DecodeError> {
        let text = "こんにちは 世界";
        let encoded = percent::encode_component(text);
        assert_eq!(percent::decode(&encoded)?, text);
        Ok(())
    }

    #[test]
    fn percent_decode_accepts_lowercase_escapes() -> Result<(), percent::DecodeError> {
        assert_eq!(percent::decode("hello%20world")?, "hello world");
        Ok(())
    }

    #[test]
    fn percent_refuses_a_truncated_escape() {
        assert_eq!(
            percent::decode("hello%2"),
            Err(percent::DecodeError::TruncatedEscape { at: 5 })
        );
        assert_eq!(
            percent::decode("hello%"),
            Err(percent::DecodeError::TruncatedEscape { at: 5 })
        );
    }

    #[test]
    fn percent_refuses_a_non_hex_escape() {
        assert_eq!(
            percent::decode("hello%2z"),
            Err(percent::DecodeError::NotHexDigit { at: 6 })
        );
    }

    #[test]
    fn percent_refuses_escapes_that_decode_to_invalid_utf8() {
        assert_eq!(
            percent::decode("%FF%FE"),
            Err(percent::DecodeError::NotUtf8 { at: 0 })
        );
    }
}
