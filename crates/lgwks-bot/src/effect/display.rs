//! How the effect identities and their refusals print, and the conversions
//! between the refusals.

use core::fmt;

#[cfg(feature = "ephemeral")]
use super::MintError;
use super::{CounterError, DigestError, EffectKey, EffectKeyError, Id128, IdError};

impl std::error::Error for IdError {}

#[cfg(feature = "ephemeral")]
impl fmt::Display for MintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self` so each pattern's type is the enum's own
        // rather than a reference to it, and the non-`Copy` payload is bound by
        // `ref`. `clippy::pattern_type_mismatch` is forbidden in this workspace
        // and this is the form it asks for, as in `JournalError`.
        match *self {
            Self::Entropy(ref cause) => write!(f, "could not mint an identifier: {cause}"),
            Self::Zero => f.write_str("the entropy source returned the all-zero identifier"),
        }
    }
}

#[cfg(feature = "ephemeral")]
impl std::error::Error for MintError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Entropy(ref cause) => Some(cause),
            Self::Zero => None,
        }
    }
}

impl fmt::Display for Id128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Display for CounterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            Self::NotDecimal => "counter must be a minimal base-10 unsigned integer",
            Self::Zero => "counter must be at least 1",
            Self::Overflow => "counter exceeds u64::MAX",
        })
    }
}

impl std::error::Error for CounterError {}

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

impl fmt::Display for EffectKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Id(cause) => write!(f, "{cause}"),
            Self::Counter(cause) => write!(f, "{cause}"),
            Self::Digest(ref cause) => write!(f, "{cause}"),
        }
    }
}

impl std::error::Error for EffectKeyError {}

impl From<IdError> for EffectKeyError {
    fn from(cause: IdError) -> Self {
        Self::Id(cause)
    }
}

impl From<CounterError> for EffectKeyError {
    fn from(cause: CounterError) -> Self {
        Self::Counter(cause)
    }
}

impl From<DigestError> for EffectKeyError {
    fn from(cause: DigestError) -> Self {
        Self::Digest(cause)
    }
}

impl fmt::Display for EffectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "run {} action {} attempt {} flow {} digest {} env {} epoch {}",
            self.run,
            self.action,
            self.attempt,
            self.flow,
            self.digest,
            self.environment,
            self.epoch
        )
    }
}
