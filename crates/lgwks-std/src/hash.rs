//! `hash` owns content-addressable hashing and enforces INV-HASH-DETERMINISTIC:
//! the same input bytes always produce the same digest, and the digest is the
//! BLAKE3 algorithm — the sole content-identity hash in this crate.

/// A 32-byte BLAKE3 digest.
///
/// Equality is constant-time to prevent timing side-channels.
#[derive(Clone, Copy, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl std::hash::Hash for Digest {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq for Digest {
    fn eq(&self, other: &Self) -> bool {
        let mut acc = 0u8;
        for (left, right) in self.0.iter().zip(other.0.iter()) {
            acc |= left ^ right;
        }
        acc == 0
    }
}

impl Eq for Digest {}

impl Digest {
    /// The raw 32-byte digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex encoding of the digest (64 characters).
    pub fn to_hex(&self) -> String {
        crate::hex::encode(self.0)
    }

    /// Parse a 64-character hex string into a digest.
    pub fn from_hex(s: &str) -> Result<Self, DigestParseError> {
        if s.len() != 64 {
            return Err(DigestParseError::WrongLength { len: s.len() });
        }
        let bytes = crate::hex::decode(s).map_err(DigestParseError::Hex)?;
        let arr: [u8; 32] = bytes.try_into().expect("64 hex chars = 32 bytes");
        Ok(Self(arr))
    }
}

/// Error from parsing a hex string into a [`Digest`].
#[derive(Debug, Clone)]
pub enum DigestParseError {
    /// Input was not exactly 64 hex characters (32 bytes).
    WrongLength {
        /// Actual length of the input.
        len: usize,
    },
    /// Input contained invalid hex.
    Hex(crate::hex::DecodeError),
}

impl core::fmt::Display for DigestParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WrongLength { len } => {
                write!(f, "digest hex must be 64 characters, got {len}")
            }
            Self::Hex(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DigestParseError {}

impl core::fmt::Debug for Digest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl core::fmt::Display for Digest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Hash `data` with BLAKE3 and return the 32-byte digest.
pub fn blake3(data: &[u8]) -> Digest {
    Digest(*blake3::hash(data).as_bytes())
}

/// Hash `data` with keyed BLAKE3 under `key` and return the 32-byte digest.
///
/// This is the estate's message-authentication primitive: whoever holds `key`
/// can recompute the tag, whoever does not cannot forge one. Use it where a
/// checksum is not enough because the writer is adversarial (audit chains,
/// sealed receipts). The key must come from outside the sealed artifact
/// (environment, keyring); a key stored beside the tags proves nothing.
pub fn keyed(key: &[u8; 32], data: &[u8]) -> Digest {
    Digest(*blake3::keyed_hash(key, data).as_bytes())
}

/// Incremental hasher for streaming data.
pub struct Hasher(blake3::Hasher);

impl Hasher {
    /// Create a new incremental hasher.
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    /// Feed bytes into the hasher.
    pub fn update(&mut self, data: &[u8]) -> &mut Self {
        self.0.update(data);
        self
    }

    /// Finalize and return the digest.
    pub fn finalize(&self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_matches_blake3_spec() {
        let d = blake3(b"");
        assert_eq!(
            d.to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn deterministic_across_calls() {
        let a = blake3(b"hello world");
        let b = blake3(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn different_input_different_digest() {
        assert_ne!(blake3(b"a"), blake3(b"b"));
    }

    #[test]
    fn incremental_matches_oneshot() {
        let oneshot = blake3(b"hello world");
        let mut h = Hasher::new();
        h.update(b"hello ");
        h.update(b"world");
        assert_eq!(h.finalize(), oneshot);
    }

    #[test]
    fn hex_roundtrip() {
        let d = blake3(b"test");
        let hex = d.to_hex();
        let parsed = Digest::from_hex(&hex).unwrap();
        assert_eq!(d, parsed);
    }

    #[test]
    fn display_is_hex() {
        let d = blake3(b"");
        assert_eq!(format!("{d}"), d.to_hex());
    }

    #[test]
    fn from_hex_rejects_wrong_length() {
        assert!(Digest::from_hex("abcd").is_err());
    }

    #[test]
    fn keyed_differs_from_unkeyed() {
        let key = [0x42u8; 32];
        assert_ne!(keyed(&key, b"hello world"), blake3(b"hello world"));
    }

    #[test]
    fn keyed_is_key_sensitive() {
        assert_ne!(keyed(&[0x01u8; 32], b"data"), keyed(&[0x02u8; 32], b"data"));
    }

    #[test]
    fn keyed_deterministic_across_calls() {
        let key = [0x07u8; 32];
        assert_eq!(keyed(&key, b"receipt"), keyed(&key, b"receipt"));
    }

    #[test]
    fn keyed_matches_blake3_keyed_hash() {
        let key = [0xABu8; 32];
        let expected = *blake3::keyed_hash(&key, b"vector").as_bytes();
        assert_eq!(keyed(&key, b"vector").as_bytes(), &expected);
    }
}
