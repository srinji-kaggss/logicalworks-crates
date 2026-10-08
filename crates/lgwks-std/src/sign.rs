//! `sign` owns detached ed25519 signatures for sealed records (feature `sign`).
//!
//! A seal binds a record's bytes to a tenant's key without sharing a secret:
//! anyone holding the public key verifies offline, and the signer never
//! reveals the seed. The signed message is domain-separated — the caller's
//! `tag` followed by the BLAKE3 digest of the record — so a seal over one
//! record kind never verifies as a seal over another. Key generation reads
//! [`crate::random`], the crate's one entropy source; there is no second one.
//!
//! The construction matches the consumer byte for byte: `tag ||
//! BLAKE3(record)`, signed with ed25519 and checked with `verify_strict`
//! (non-canonical scalars refused). A record sealed elsewhere with the same
//! tag construction verifies here, and a seal made here verifies there.

use std::error::Error;
use std::fmt;

use ed25519_dalek::Signer as _;

use crate::random::EntropyError;

/// A signing key: the 32-byte seed and the expanded scalar derived from it.
///
/// Opaque: the seed never leaves the type except through a signature. Debug is
/// redacted for the same reason — a log line must not become a key backup.
/// Zeroized on drop by the backend.
#[derive(Clone)]
#[non_exhaustive]
pub struct SecretKey(ed25519_dalek::SigningKey);

impl SecretKey {
    /// Rebuild a key from the 32-byte seed it was generated from.
    ///
    /// Infallible: every 32-byte string is a valid seed, and clamping makes it
    /// a scalar. A seed file that fails to parse is a hex or length refusal at
    /// the caller's file reading, not here.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// The public half of this key, for verifiers that never see the seed.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }
}

impl fmt::Debug for SecretKey {
    /// Redacted: the seed is the key, so even the key's length is all a
    /// debug line may say.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretKey(..)")
    }
}

/// A verification key: the 32 public bytes anyone may hold.
///
/// Comparable and copyable: public bytes are not secret, so ordering and
/// equality over them open no timing channel worth closing. Debug renders
/// the hex, the form an operator stores and compares.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PublicKey(ed25519_dalek::VerifyingKey);

impl fmt::Debug for PublicKey {
    /// The 64 hex characters of the key: the backend's own rendering is an
    /// implementation detail of the edge, and a consumer asking to print
    /// this type wants the value it files beside the record.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicKey(")?;
        crate::hex::write_lowercase(&self.0.to_bytes(), formatter)?;
        formatter.write_str(")")
    }
}

impl PublicKey {
    /// Parse the 32-byte public key.
    ///
    /// Delegated to the backend: whatever it accepts parses, whatever it
    /// refuses is [`KeyError::InvalidPublicKey`]. This module adds no second
    /// opinion about which encodings are points.
    ///
    /// # Errors
    ///
    /// [`KeyError::InvalidPublicKey`] when the bytes decode to no curve
    /// point.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, KeyError> {
        match ed25519_dalek::VerifyingKey::from_bytes(&bytes) {
            Ok(key) => Ok(Self(key)),
            Err(_) => Err(KeyError::InvalidPublicKey),
        }
    }

    /// The 32-byte public key, for storage beside the sealed record.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

/// A detached signature: 64 bytes that prove nothing on their own and
/// everything beside the record, the tag and the public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Signature(ed25519_dalek::Signature);

impl Signature {
    /// Rebuild a signature from its 64 bytes.
    ///
    /// Infallible: any 64 bytes parse, and malleability is refused at
    /// verification ([`verify_strict`]), not at parsing — a stored seal is
    /// data until it is checked.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(ed25519_dalek::Signature::from_bytes(&bytes))
    }

    /// The 64-byte signature, for storage beside the sealed record.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 64] {
        self.0.to_bytes()
    }
}

/// Why a public key was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyError {
    /// The 32 bytes are not a canonical encoding of a curve point.
    InvalidPublicKey,
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidPublicKey => {
                formatter.write_str("the public key bytes are not a valid ed25519 point")
            }
        }
    }
}

// The backend's parse error carries no `std::error::Error` implementation
// under the `alloc`-only stack this module builds on, so there is no upstream
// cause to chain and `source` is `None` rather than a re-wrapped copy.
impl Error for KeyError {}

/// Why a seal did not verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VerifyError {
    /// The signature does not match this record, tag and key — wrong key,
    /// wrong record, wrong tag, corrupted bytes, or a malleated scalar.
    Mismatch,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Mismatch => formatter.write_str(
                "the seal does not match the record under this key: wrong key, wrong record, wrong tag, or damaged bytes",
            ),
        }
    }
}

// `verify_strict` reports only match or mismatch, so there is no upstream
// error object to chain; `source` is `None` rather than an invented cause.
impl Error for VerifyError {}

/// A fresh signing key from OS entropy.
///
/// # Errors
///
/// [`EntropyError`] when the OS refused entropy; there is no fallback source,
/// so the caller fails rather than signing under a weak key.
///
/// ```rust
/// let secret = lgwks_std::sign::generate()?;
/// let public = secret.public_key();
/// let seal = lgwks_std::sign::sign(&secret, b"example/v1", b"{\"run\": 1}");
/// lgwks_std::sign::verify_strict(&public, b"example/v1", b"{\"run\": 1}", &seal)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn generate() -> Result<SecretKey, EntropyError> {
    let seed = crate::random::bytes::<32>()?;
    Ok(SecretKey::from_seed(seed))
}

/// The domain-separated message `tag || BLAKE3(record)`.
///
/// Built here once so signing and verification cannot disagree about the
/// construction: the tag names the record kind (a seal over one kind never
/// verifies as another), and hashing the record first bounds the signed bytes
/// at 32 plus the tag no matter how large the record is.
fn message(tag: &[u8], record: &[u8]) -> Vec<u8> {
    let digest = crate::hash::blake3(record);
    let mut signed = Vec::with_capacity(tag.len().saturating_add(32));
    signed.extend_from_slice(tag);
    signed.extend_from_slice(digest.as_bytes());
    signed
}

/// Seal `record` under `secret` for the domain `tag`.
///
/// Deterministic: the same key, tag and record always produce the same 64
/// bytes, so a seal is reproducible and two seals of one record compare
/// equal.
#[must_use]
pub fn sign(secret: &SecretKey, tag: &[u8], record: &[u8]) -> Signature {
    Signature(secret.0.sign(&message(tag, record)))
}

/// Check `signature` over `record` for the domain `tag` under `public`.
///
/// Strict: a non-canonical scalar (the malleability twin that a lax verifier
/// accepts) is refused here, so there is exactly one valid seal per
/// key-tag-record rather than two.
///
/// # Errors
///
/// [`VerifyError::Mismatch`] when the seal does not match.
pub fn verify_strict(
    public: &PublicKey,
    tag: &[u8],
    record: &[u8],
    signature: &Signature,
) -> Result<(), VerifyError> {
    match public.0.verify_strict(&message(tag, record), &signature.0) {
        Ok(()) => Ok(()),
        Err(_) => Err(VerifyError::Mismatch),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A live key straight from OS entropy signs and verifies: the generation
    /// path, not a reconstructed seed, is what a tenant's first use runs.
    #[test]
    fn a_generated_key_seals_and_opens() -> Result<(), Box<dyn Error>> {
        let secret = generate()?;
        let public = secret.public_key();
        let seal = sign(&secret, b"test/v1", b"live record");
        verify_strict(&public, b"test/v1", b"live record", &seal)?;
        assert_eq!(
            seal,
            sign(&secret, b"test/v1", b"live record"),
            "signing is deterministic: one key, tag and record give one seal"
        );
        Ok(())
    }

    /// The debug rendering carries no key material: a redacted line is the
    /// whole of what a log may hold.
    #[test]
    fn the_secret_key_debug_is_redacted() -> Result<(), Box<dyn Error>> {
        let secret = generate()?;
        let rendered = format!("{:?}", secret.public_key());
        assert!(
            !rendered.is_empty(),
            "the public key renders for operator display"
        );
        assert_eq!(
            format!("{secret:?}"),
            "SecretKey(..)",
            "the secret key renders as nothing but its own name"
        );
        Ok(())
    }
}
