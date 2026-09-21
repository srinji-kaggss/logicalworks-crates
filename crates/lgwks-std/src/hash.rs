//! `hash` owns content-addressable hashing and enforces INV-HASH-DETERMINISTIC:
//! the same input bytes always produce the same digest, and the digest is the
//! BLAKE3 algorithm, the sole content-identity hash in this crate.

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
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex encoding of the digest (64 characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        crate::hex::encode(self.0)
    }

    /// Parse a 64-character hex string into a digest.
    ///
    /// Upper and lower case are both accepted. Length is validated first, so a
    /// short or long input reports [`DigestParseError::WrongLength`] with the
    /// observed length rather than a hex offset rebased onto a string that was
    /// never the right shape. The decoded bytes are then moved into the fixed
    /// 32-byte array without a panic path: the conversion is fallible in the
    /// type system, and although the length check above makes it infallible in
    /// practice, a failure maps back to the same `WrongLength` rather than
    /// aborting the process.
    pub fn from_hex(text: &str) -> Result<Self, DigestParseError> {
        if text.len() != 64 {
            return Err(DigestParseError::WrongLength { len: text.len() });
        }
        let bytes = crate::hex::decode(text).map_err(DigestParseError::Hex)?;
        let raw: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| DigestParseError::WrongLength { len: bytes.len() })?;
        Ok(Self(raw))
    }
}

/// Error from parsing a hex string into a [`Digest`].
///
/// Variants are stable and machine-readable; `#[non_exhaustive]` lets a later
/// revision add a rejection reason without breaking callers that match on the
/// current two.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
        match *self {
            Self::WrongLength { len } => {
                write!(f, "digest hex must be 64 characters, got {len}")
            }
            Self::Hex(ref err) => write!(f, "{err}"),
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
#[must_use]
pub fn blake3(data: &[u8]) -> Digest {
    Digest(*blake3::hash(data).as_bytes())
}

/// Hash `data` with keyed BLAKE3 under `key` and return the 32-byte digest.
///
/// This is the message-authentication primitive: whoever holds `key`
/// can recompute the tag, whoever does not cannot forge one. Use it where a
/// checksum is not enough because the writer is adversarial (audit chains,
/// sealed receipts). The key must come from outside the sealed artifact
/// (environment, keyring); a key stored beside the tags proves nothing.
#[must_use]
pub fn keyed(key: &[u8; 32], data: &[u8]) -> Digest {
    Digest(*blake3::keyed_hash(key, data).as_bytes())
}

/// Incremental hasher for streaming data.
pub struct Hasher(blake3::Hasher);

impl Hasher {
    /// Create a new incremental hasher.
    #[must_use]
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    /// Feed bytes into the hasher.
    ///
    /// Returns `&mut Self` so calls chain; the digest is unchanged by how the
    /// input was split across calls, which the test below pins.
    pub fn update(&mut self, data: &[u8]) -> &mut Self {
        self.0.update(data);
        self
    }

    /// Finalize and return the digest.
    ///
    /// Borrows rather than consumes, so a caller can keep feeding the same
    /// hasher; the digest is a snapshot of the bytes written so far.
    #[must_use]
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
        let digest = blake3(b"");
        assert_eq!(
            digest.to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn deterministic_across_calls() {
        let first = blake3(b"hello world");
        let second = blake3(b"hello world");
        assert_eq!(first, second);
    }

    #[test]
    fn different_input_different_digest() {
        assert_ne!(blake3(b"a"), blake3(b"b"));
    }

    #[test]
    fn incremental_matches_oneshot() {
        let oneshot = blake3(b"hello world");
        let mut hasher = Hasher::new();
        hasher.update(b"hello ");
        hasher.update(b"world");
        assert_eq!(hasher.finalize(), oneshot);
    }

    #[test]
    fn hex_roundtrip() -> Result<(), DigestParseError> {
        let digest = blake3(b"test");
        let hex = digest.to_hex();
        let parsed = Digest::from_hex(&hex)?;
        assert_eq!(digest, parsed);
        Ok(())
    }

    #[test]
    fn display_is_hex() {
        let digest = blake3(b"");
        assert_eq!(format!("{digest}"), digest.to_hex());
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
