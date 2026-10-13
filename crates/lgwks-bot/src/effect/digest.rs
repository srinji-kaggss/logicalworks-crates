//! The content-digest roles a key binds: the wire algorithm tag, why a tagged digest is refused, and the FlowRevision and ActionDigest newtypes over the estate's one content hash.

use core::fmt;

use lgwks_std::hash::Digest;

/// The algorithm tag a digest carries on the wire.
///
/// The schema admits two. This crate accepts one.
///
/// [`Blake3_256`](Self::Blake3_256) is what it computes and the only tag it can
/// verify: a settlement is accepted because the receiver recomputed the digest
/// from the bytes it holds, and a tag naming an algorithm the receiver cannot
/// recompute is an unverifiable claim dressed as identity. The estate has
/// exactly one content-identity hash, `lgwks_std::hash`, which is BLAKE3.
///
/// [`Sha256`](Self::Sha256) is parsed and then refused, rather than rejected as
/// malformed: `sha256` is a well-formed tag under the schema and a fixture
/// produced elsewhere may legitimately carry one, so the error a caller sees
/// should say the algorithm is unsupported rather than that their JSON was
/// wrong. Nothing in this workspace produces one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DigestAlgorithm {
    /// BLAKE3-256, the estate's sole content-identity hash.
    Blake3_256,
    /// SHA-256: well-formed on the wire, unsupported here.
    Sha256,
}

impl DigestAlgorithm {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blake3_256 => "blake3_256",
            Self::Sha256 => "sha256",
        }
    }

    /// The fixed-width byte this algorithm is tagged with inside a hash chain.
    ///
    /// Parse the wire spelling. Exact: no case folding, no aliases.
    #[must_use]
    pub fn from_wire(text: &str) -> Option<Self> {
        match text {
            "blake3_256" => Some(Self::Blake3_256),
            "sha256" => Some(Self::Sha256),
            _ => None,
        }
    }
}

impl fmt::Display for DigestAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a tagged digest was refused.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DigestError {
    /// The algorithm tag was not one the schema defines.
    UnknownAlgorithm,
    /// The tag named an algorithm this crate cannot compute or verify.
    UnsupportedAlgorithm {
        /// The tag as it appeared.
        algorithm: DigestAlgorithm,
    },
    /// The hex body was malformed, with the underlying reason.
    Malformed(lgwks_std::hash::DigestParseError),
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnknownAlgorithm => f.write_str("digest algorithm is not one the schema defines"),
            Self::UnsupportedAlgorithm { algorithm } => write!(
                f,
                "digest algorithm {algorithm} is not computable or verifiable here; \
                 this crate accepts blake3_256"
            ),
            Self::Malformed(ref cause) => write!(f, "{cause}"),
        }
    }
}

impl std::error::Error for DigestError {}

/// Declare a newtype over the estate's content digest for one digest role.
macro_rules! digest_role {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(
            lgwks_std::wire::Archive,
            lgwks_std::wire::Serialize,
            lgwks_std::wire::Deserialize,
        )]
        #[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
        pub struct $name(Digest);

        impl $name {
            /// Wrap an already-computed digest.
            #[must_use]
            pub const fn new(digest: Digest) -> Self {
                Self(digest)
            }

            /// The underlying digest.
            #[must_use]
            pub const fn digest(self) -> Digest {
                self.0
            }

            /// The algorithm this digest is always carried under.
            #[must_use]
            pub const fn algorithm() -> DigestAlgorithm {
                DigestAlgorithm::Blake3_256
            }

            /// Parse the tagged wire form.
            ///
            /// # Errors
            ///
            /// [`DigestError`]. A `sha256` tag is
            /// [`DigestError::UnsupportedAlgorithm`].
            pub fn from_tagged(algorithm: &str, hex: &str) -> Result<Self, DigestError> {
                let tag =
                    DigestAlgorithm::from_wire(algorithm).ok_or(DigestError::UnknownAlgorithm)?;
                if tag != DigestAlgorithm::Blake3_256 {
                    return Err(DigestError::UnsupportedAlgorithm { algorithm: tag });
                }
                Ok(Self(Digest::from_hex(hex).map_err(DigestError::Malformed)?))
            }

            /// The lowercase hex body, without the tag.
            #[must_use]
            pub fn to_hex(self) -> String {
                self.0.to_hex()
            }

            /// What this digest binds, for a refusal message.
            #[must_use]
            pub const fn role() -> &'static str {
                $what
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

digest_role! {
    /// Binds the validated flow document a command came from.
    ///
    /// Content identity, and it does not replace intent identity: the same flow
    /// revision appears on every attempt of every action it contains, which is
    /// why it is one field of [`EffectKey`](super::EffectKey) rather than the
    /// whole of it.
    FlowRevision,
    "flow revision"
}

digest_role! {
    /// Binds operation, target, preconditions, postcondition and exact input.
    ///
    /// Deliberately excludes the run, action and attempt ids. Those are outside
    /// this digest but inside [`EffectKey`](super::EffectKey), so that the same logical action
    /// attempted twice shares a digest and still has two distinct keys.
    ActionDigest,
    "action digest"
}
