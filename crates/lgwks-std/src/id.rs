//! `id` owns UUID version 4 generation and canonical text parsing. Generated
//! values carry version 4 and the RFC variant bits; parsing and raw-byte
//! construction accept any 128-bit UUID value without imposing a version.

use std::error::Error;
use std::fmt;

use crate::hex::DecodeError;
use crate::random::{self, EntropyError};

/// A 128-bit RFC 4122 identifier, stored as raw bytes in network order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// Generates a version 4 UUID from OS entropy.
    pub fn new_v4() -> Result<Self, EntropyError> {
        Self::new_v4_with(random::bytes)
    }

    /// Obtains the bytes and applies the version and variant masks.
    fn new_v4_with<E>(entropy: impl FnOnce() -> Result<[u8; 16], E>) -> Result<Self, E> {
        let mut raw = entropy()?;
        raw[6] = (raw[6] & 0x0f) | 0x40;
        raw[8] = (raw[8] & 0x3f) | 0x80;
        Ok(Self(raw))
    }

    /// The identifier's raw bytes in network order.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The RFC 4122 version nibble, or `None` for a value that carries no
    /// recognisable variant.
    #[must_use]
    pub fn version(&self) -> Option<u8> {
        if self.0[8] & 0xc0 == 0x80 {
            Some(self.0[6] >> 4)
        } else {
            None
        }
    }

    /// Parses the canonical hyphenated 8-4-4-4-12 lowercase or uppercase form.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let bytes = text.as_bytes();
        check_uuid_length(bytes.len())?;
        let mut raw = [0u8; 16];
        parse_uuid_groups(bytes, &mut raw)?;
        Ok(Self(raw))
    }
}

impl From<[u8; 16]> for Uuid {
    /// Rehydrates an identifier from raw bytes without imposing version bits.
    ///
    /// The raw bytes are the identifier's network-order form, so this is a
    /// widening of the value rather than a parse: use it only for a value that
    /// was generated elsewhere, and use [`Uuid::new_v4`] for one this process
    /// originates.
    fn from(raw: [u8; 16]) -> Self {
        Self(raw)
    }
}

/// Refuses any text that is not exactly the 36 bytes of the canonical form.
///
/// Both `len` and `at` carry the observed length: there is no meaningful
/// interior offset for a length violation, and reporting the length twice keeps
/// the error's two fields self-describing for a machine consumer.
fn check_uuid_length(len: usize) -> Result<(), ParseError> {
    if len != 36 {
        Err(ParseError::WrongLength { len, at: len })
    } else {
        Ok(())
    }
}

/// Requires the group separator at `cursor`.
///
/// `bytes.get` is used deliberately: a truncated input reports
/// [`ParseError::MissingHyphen`] at the exact offset instead of indexing past
/// the end.
fn check_hyphen(bytes: &[u8], cursor: usize) -> Result<(), ParseError> {
    if bytes.get(cursor) != Some(&b'-') {
        Err(ParseError::MissingHyphen { at: cursor })
    } else {
        Ok(())
    }
}

/// Decodes one hyphen-delimited hex group into `out`.
///
/// `bytes` must hold at least `cursor + width` bytes: the caller walks the
/// fixed 8-4-4-4-12 layout of an input that `check_uuid_length` already proved
/// to be 36 bytes, so the slice is in range. `out` is the destination for this
/// group's bytes and must be exactly `width / 2` bytes wide. The error retains
/// both the group's start and the offending source character offset.
fn parse_uuid_group(
    bytes: &[u8],
    cursor: usize,
    width: usize,
    out: &mut [u8],
) -> Result<(), ParseError> {
    let group = &bytes[cursor..cursor.saturating_add(width)];
    crate::hex::decode_into(group, out).map_err(|err| match err {
        DecodeError::NotHexDigit { at, .. } | DecodeError::OddLength { at, .. } => {
            ParseError::NotHexDigit {
                group_at: cursor,
                at: cursor.saturating_add(at),
            }
        }
        DecodeError::OutputLength { expected, actual } => {
            ParseError::DecodeOutputLength { expected, actual }
        }
    })?;
    Ok(())
}

/// Decodes the five canonical groups of the hyphenated form into `raw`.
///
/// `GROUPS` pairs each group's hex-character width with the number of bytes it
/// decodes to (8-4-4-4-12 characters become 4-2-2-2-6 bytes), so no width is
/// ever divided at run time. A hyphen is required before every group after the
/// first. The cursors stay inside a 36-byte input and the offsets stay inside
/// the 16-byte output, so none of the accumulations can saturate.
fn parse_uuid_groups(bytes: &[u8], raw: &mut [u8; 16]) -> Result<(), ParseError> {
    const GROUPS: [(usize, usize); 5] = [(8, 4), (4, 2), (4, 2), (4, 2), (12, 6)];
    let mut out_offset = 0usize;
    let mut cursor = 0usize;
    for (group_index, &(width, byte_count)) in GROUPS.iter().enumerate() {
        if group_index > 0 {
            check_hyphen(bytes, cursor)?;
            cursor = cursor.saturating_add(1);
        }
        parse_uuid_group(
            bytes,
            cursor,
            width,
            &mut raw[out_offset..out_offset.saturating_add(byte_count)],
        )?;
        out_offset = out_offset.saturating_add(byte_count);
        cursor = cursor.saturating_add(width);
    }
    Ok(())
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::hex::write_lowercase(&self.0[..4], f)?;
        f.write_str("-")?;
        crate::hex::write_lowercase(&self.0[4..6], f)?;
        f.write_str("-")?;
        crate::hex::write_lowercase(&self.0[6..8], f)?;
        f.write_str("-")?;
        crate::hex::write_lowercase(&self.0[8..10], f)?;
        f.write_str("-")?;
        crate::hex::write_lowercase(&self.0[10..], f)?;
        Ok(())
    }
}

/// Why a string is not a canonical UUID.
///
/// Variants are stable and machine-readable; callers match on them rather than
/// on the message text, and `#[non_exhaustive]` keeps a future rejection reason
/// an additive change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// The canonical form is exactly 36 characters.
    WrongLength {
        /// Length of the offending input, in bytes.
        len: usize,
        /// Offset where error occurred.
        at: usize,
    },
    /// A group separator was expected at this offset.
    MissingHyphen {
        /// Zero-based offset where a `-` was required.
        at: usize,
    },
    /// A group contained a character outside `[0-9a-fA-F]`.
    NotHexDigit {
        /// Zero-based offset where the malformed hex group begins.
        group_at: usize,
        /// Zero-based offset of the first invalid character in the input.
        at: usize,
    },
    /// The internal fixed-size destination did not match its hex group.
    DecodeOutputLength {
        /// Number of bytes represented by the group.
        expected: usize,
        /// Number of bytes in the fixed-size destination.
        actual: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WrongLength { len, at: _ } => {
                write!(f, "UUID text has length {len}; expected exactly 36 bytes")
            }
            Self::MissingHyphen { at } => {
                write!(f, "expected '-' at offset {at}")
            }
            Self::NotHexDigit { group_at, at } => {
                write!(
                    f,
                    "non-hex character at offset {at} in UUID group starting at offset {group_at}"
                )
            }
            Self::DecodeOutputLength { expected, actual } => write!(
                f,
                "UUID group decodes to {expected} bytes but its destination has {actual}"
            ),
        }
    }
}

impl Error for ParseError {}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // These tests return `Result` rather than unwrapping: an entropy or parse
    // refusal reports its own `Debug` on failure, which is the same report
    // `.unwrap` would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn generated_value_carries_version_four_and_the_rfc_variant()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        assert_eq!(id.version(), Some(4));
        assert_eq!(id.as_bytes()[6] >> 4, 4);
        assert_eq!(id.as_bytes()[8] & 0xc0, 0x80);
        Ok(())
    }

    #[test]
    fn successive_identifiers_differ() -> Result<(), Box<dyn std::error::Error>> {
        let first = Uuid::new_v4()?;
        let second = Uuid::new_v4()?;
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn display_is_hyphenated_lowercase_in_eight_four_four_four_twelve()
    -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        let text = id.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(&text[8..9], "-");
        assert_eq!(&text[13..14], "-");
        assert_eq!(&text[18..19], "-");
        assert_eq!(&text[23..24], "-");
        assert!(
            text.chars()
                .all(|char_value| char_value.is_ascii_hexdigit() || char_value == '-')
        );
        Ok(())
    }

    #[test]
    fn parse_reverses_display() -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        let parsed = Uuid::parse(&id.to_string())?;
        assert_eq!(id, parsed);
        Ok(())
    }

    #[test]
    fn parse_accepts_uppercase() -> Result<(), Box<dyn std::error::Error>> {
        let id = Uuid::new_v4()?;
        let upper = id.to_string().to_ascii_uppercase();
        let parsed = Uuid::parse(&upper)?;
        assert_eq!(id, parsed);
        Ok(())
    }

    #[test]
    fn parse_refuses_the_wrong_length() {
        assert_eq!(
            Uuid::parse(""),
            Err(ParseError::WrongLength { len: 0, at: 0 })
        );
        assert_eq!(
            Uuid::parse("not-a-uuid"),
            Err(ParseError::WrongLength { len: 10, at: 10 })
        );
    }

    #[test]
    fn parse_refuses_a_missing_hyphen() {
        assert_eq!(
            Uuid::parse("12345678x1234-1234-1234-123456789abc"),
            Err(ParseError::MissingHyphen { at: 8 })
        );
    }

    #[test]
    fn parse_refuses_a_non_hex_group() {
        assert_eq!(
            Uuid::parse("12345678-123z-1234-1234-123456789abc"),
            Err(ParseError::NotHexDigit {
                group_at: 9,
                at: 12
            })
        );
    }

    #[test]
    fn parse_preserves_arbitrary_uuid_version_and_variant_bits() -> Result<(), ParseError> {
        let value = Uuid::parse("00000000-0000-0000-0000-000000000000")?;
        assert_eq!(
            value.as_bytes(),
            &[0; 16],
            "parsing keeps every supplied bit"
        );
        assert_eq!(
            value.version(),
            None,
            "non-RFC variant has no reported version"
        );
        assert_eq!(
            value.to_string(),
            "00000000-0000-0000-0000-000000000000",
            "arbitrary UUID values round-trip through canonical text"
        );
        let version_one = Uuid::parse("00000000-0000-1000-8000-000000000000")?;
        assert_eq!(
            version_one.version(),
            Some(1),
            "parsing accepts non-v4 values with an RFC variant"
        );
        Ok(())
    }

    #[test]
    fn generation_refuses_entropy_failure_without_substitution() {
        let result = Uuid::new_v4_with(|| Err::<[u8; 16], _>("entropy unavailable"));
        assert_eq!(
            result,
            Err("entropy unavailable"),
            "entropy error propagates unchanged"
        );
    }
}
