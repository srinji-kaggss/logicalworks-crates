//! `encoding` owns base64 and percent-encoding, enforcing
//! INV-ENCODING-STRICT-DECODE: both decoders refuse malformed input with a
//! typed error rather than skipping bytes, so a corrupted value cannot
//! round-trip into something plausible.

// ── base64 ──────────────────────────────────────────────────────────────────

/// RFC 4648 standard base64 with `+`, `/`, and `=` padding.
pub mod base64 {
    use std::error::Error;
    use std::fmt;

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn encode_first_pair(b0: u8, b1: u8, out: &mut String) {
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
    }

    fn encode_third_char(b1: u8, b2: u8, has_b1: bool, out: &mut String) {
        if has_b1 {
            out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
    }

    fn encode_fourth_char(b2: u8, has_b2: bool, out: &mut String) {
        if has_b2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }

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
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            encode_chunk_chars(chunk, &mut out);
        }
        out
    }

    /// Why a base64 string could not be decoded.
    #[derive(Debug, Clone, PartialEq, Eq)]
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
            match self {
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

    fn check_base64_quantum(len: usize) -> Result<(), DecodeError> {
        if !len.is_multiple_of(4) {
            Err(DecodeError::BadLength { len, at: len })
        } else {
            Ok(())
        }
    }

    fn inspect_padding(input: &[u8]) -> Result<usize, DecodeError> {
        let padding = input.iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 {
            Err(DecodeError::MisplacedPadding {
                at: input.len() - padding,
            })
        } else {
            Ok(padding)
        }
    }

    fn check_interior_padding(body: &[u8]) -> Result<(), DecodeError> {
        if let Some(offset) = body.iter().position(|&b| b == b'=') {
            Err(DecodeError::MisplacedPadding { at: offset })
        } else {
            Ok(())
        }
    }

    fn decode_body_chunk(
        byte: u8,
        index: usize,
        accumulator: &mut u32,
        filled: &mut u32,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let value = sextet(byte).ok_or(DecodeError::NotInAlphabet { at: index, byte })?;
        *accumulator = (*accumulator << 6) | u32::from(value);
        *filled += 6;
        if *filled >= 8 {
            *filled -= 8;
            out.push((*accumulator >> *filled) as u8);
        }
        Ok(())
    }

    fn decode_body_bytes(body: &[u8], out: &mut Vec<u8>) -> Result<(), DecodeError> {
        let mut accumulator = 0u32;
        let mut filled = 0u32;
        for (index, &byte) in body.iter().enumerate() {
            decode_body_chunk(byte, index, &mut accumulator, &mut filled, out)?;
        }
        if filled > 0 && (accumulator & ((1 << filled) - 1)) != 0 {
            Err(DecodeError::MisplacedPadding { at: body.len() })
        } else {
            Ok(())
        }
    }

    /// Decodes padded standard base64.
    pub fn decode(input: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
        let input = input.as_ref();
        if input.is_empty() {
            return Ok(Vec::new());
        }
        check_base64_quantum(input.len())?;
        let padding = inspect_padding(input)?;
        let body = &input[..input.len() - padding];
        check_interior_padding(body)?;

        let mut out = Vec::with_capacity(input.len() / 4 * 3);
        decode_body_bytes(body, &mut out)?;
        Ok(out)
    }

    fn sextet(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
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
    pub fn encode_component(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for &byte in text.as_bytes() {
            if is_unreserved(byte) {
                out.push(byte as char);
            } else {
                out.push('%');
                out.push(upper_nibble(byte >> 4));
                out.push(upper_nibble(byte & 0x0f));
            }
        }
        out
    }

    fn is_unreserved(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
    }

    fn upper_nibble(value: u8) -> char {
        b"0123456789ABCDEF"[value as usize] as char
    }

    /// Why a percent-encoded string could not be decoded.
    #[derive(Debug, Clone, PartialEq, Eq)]
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
            match self {
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

    fn decode_escape_pair(escape: &[u8], pos: usize) -> Result<u8, DecodeError> {
        let decoded = crate::hex::decode(escape).map_err(|hex_err| match hex_err {
            crate::hex::DecodeError::NotHexDigit { at: hex_at, .. } => {
                DecodeError::NotHexDigit { at: pos + hex_at }
            }
            crate::hex::DecodeError::OddLength { at: hex_at, .. } => {
                DecodeError::NotHexDigit { at: pos + hex_at }
            }
        })?;
        Ok(decoded[0])
    }

    fn decode_step(bytes: &[u8], i: &mut usize, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        if bytes[*i] != b'%' {
            out.push(bytes[*i]);
            *i += 1;
            Ok(())
        } else {
            let escape = bytes
                .get(*i + 1..*i + 3)
                .ok_or(DecodeError::TruncatedEscape { at: *i })?;
            let byte_val = decode_escape_pair(escape, *i)?;
            out.push(byte_val);
            *i += 3;
            Ok(())
        }
    }

    /// Decodes percent escapes back into text.
    pub fn decode(text: &str) -> Result<String, DecodeError> {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            decode_step(bytes, &mut i, &mut out)?;
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

    /// RFC 4648 section 10 test vectors, verbatim.
    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
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
                base64::decode(encoded).unwrap(),
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
    }

    #[test]
    fn base64_roundtrips_every_byte_value() {
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = base64::encode(&all_bytes);
        let decoded = base64::decode(&encoded).expect("valid base64");
        assert_eq!(decoded, all_bytes);
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
    fn percent_roundtrips_multibyte_text() {
        let text = "こんにちは 世界";
        let encoded = percent::encode_component(text);
        assert_eq!(percent::decode(&encoded).unwrap(), text);
    }

    #[test]
    fn percent_decode_accepts_lowercase_escapes() {
        assert_eq!(percent::decode("hello%20world").unwrap(), "hello world");
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
