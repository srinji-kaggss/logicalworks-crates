//! Durable effect identity: the exact key a settlement is a statement about.
//!
//! The ECS ledger already settles an entry and already refuses a contradicting
//! settlement (`crate::ecs` keeps one settlement record per entry per
//! generation for exactly that reason). What it does not carry is *which
//! attempt* the evidence is about. An attempt count is not an identity: two
//! deliveries that both arrive as "attempt 3" for the same entry are
//! indistinguishable to the ledger, so a repeat of an old settlement and a
//! statement about a new one look identical at the moment the ledger decides
//! whether to accept them.
//!
//! This module supplies the missing half. [`EffectKey`] binds the seven fields
//! a settlement must name, and the ledger's evidence becomes a statement about
//! a key rather than a bare fact. INV-EFFECT-KEY-EXACT: two keys are equal if
//! and only if they name the same logical attempt against the same environment
//! generation, so a stale settlement is refusable by comparison rather than by
//! trusting a counter.
//!
//! # What is reused
//!
//! The 256-bit content digests are [`lgwks_std::hash::Digest`], the estate's
//! sole content-identity hash (INV-HASH-DETERMINISTIC). This module does not
//! define a second one. [`FlowRevision`] and [`ActionDigest`] are newtypes over
//! it, not replacements: they have different domains — one binds the validated
//! flow document, the other binds operation, target, preconditions,
//! postcondition and exact input — and a newtype each is what stops a call site
//! from passing one where the other is required.
//!
//! # Wire form
//!
//! The keys here are the Rust side of `effect-key-v1`. Ids render as 32
//! lowercase hex characters encoding 128 unsigned bits, big-endian; the
//! all-zero id is not a valid identifier and is refused on parse. Digests
//! render as 64 lowercase hex characters under an algorithm tag. See
//! [`DigestAlgorithm`] for why this crate accepts exactly one of the two tags
//! the schema permits.
//!
//! [`ActionDigest`]: crate::effect::ActionDigest
//! [`DigestAlgorithm`]: crate::effect::DigestAlgorithm
//! [`EffectKey`]: crate::effect::EffectKey
//! [`FlowRevision`]: crate::effect::FlowRevision

use core::fmt;
use core::num::{NonZeroU64, NonZeroU128};

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::wire::{AlignedVec, WireError};

/// Why an [`Id128`] could not be parsed.
///
/// `#[non_exhaustive]`: a later revision can add a rejection reason without
/// breaking a caller that matches the three below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdError {
    /// The input was not exactly 32 characters.
    ///
    /// Length is checked in bytes and checked first, so a short or long input
    /// reports its own length rather than a character offset rebased onto a
    /// string that was never the right shape.
    WrongLength {
        /// Observed length in bytes.
        len: usize,
    },
    /// A byte at this offset was outside `[0-9a-f]`.
    ///
    /// Uppercase hex is rejected rather than folded. These ids are
    /// identity-bearing wire data, and the schema that defines them admits
    /// `^[0-9a-f]{32}$`; accepting `A` here would mean the crate writes a
    /// canonical form it also refuses to read back from certain producers.
    NotHex {
        /// Byte offset of the offending character.
        at: usize,
    },
    /// The input was the all-zero id.
    ///
    /// Zero is the schema's explicit exclusion. It is also the value a
    /// zero-initialised or truncated buffer produces, so refusing it turns a
    /// whole class of uninitialised-identity bugs into a parse error.
    Zero,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WrongLength { len } => {
                write!(f, "identifier must be 32 hex characters, got {len}")
            }
            Self::NotHex { at } => {
                write!(f, "identifier has a non-lowercase-hex byte at offset {at}")
            }
            Self::Zero => f.write_str("the all-zero identifier is not a valid identifier"),
        }
    }
}

impl std::error::Error for IdError {}

/// Why a fresh identifier could not be minted from the operating system.
///
/// Two arms, and neither has a fallback. [`lgwks_std::random`] enforces
/// INV-RANDOM-ONE-SOURCE, so a clock, a process id, a counter and a userspace
/// PRNG are each explicitly not alternatives — an identifier derived from one of
/// those is the collision this crate's identity model exists to make
/// impossible. A caller that cannot reach entropy must fail.
///
/// `#[non_exhaustive]` for the same reason [`IdError`] is: a later revision can
/// add a rejection reason without breaking a caller that matches these two.
#[cfg(feature = "ephemeral")]
#[derive(Debug)]
#[non_exhaustive]
pub enum MintError {
    /// The operating system's entropy source could not be read.
    ///
    /// Carried rather than flattened to a string, because `lgwks_std`'s
    /// `EntropyError` names the backend that failed as a matchable value.
    Entropy(lgwks_std::random::EntropyError),
    /// The entropy source returned 128 zero bits.
    ///
    /// A probability of 2^-128 per call, and an error rather than a retry
    /// because zero is both the id the wire schema excludes and the value a
    /// zeroed or truncated buffer produces. A retry would silently paper over a
    /// source that is returning zeros; this names it.
    Zero,
}

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

/// A 128-bit identifier that is never zero.
///
/// The non-zero invariant is carried by [`NonZeroU128`] rather than checked at
/// each use, so "is this id real?" has one answer and the compiler enforces it.
/// Equality is the ordinary integer comparison: unlike
/// [`Digest`], these are public identifiers rather than secret-derived values,
/// and a constant-time comparison would buy nothing.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct Id128(NonZeroU128);

impl Id128 {
    /// Wrap a value already known to be non-zero.
    #[must_use]
    pub const fn from_nonzero(value: NonZeroU128) -> Self {
        Self(value)
    }

    /// Parse 32 lowercase hex characters, big-endian, rejecting the all-zero id.
    ///
    /// # Errors
    ///
    /// [`IdError::WrongLength`], [`IdError::NotHex`] or [`IdError::Zero`].
    pub fn from_hex(text: &str) -> Result<Self, IdError> {
        if text.len() != 32 {
            return Err(IdError::WrongLength { len: text.len() });
        }
        if let Some(at) = text
            .bytes()
            .position(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(IdError::NotHex { at });
        }
        // Unreachable in practice: the two checks above leave exactly 32 hex
        // characters, which is exactly 128 bits, so the value always fits a
        // `u128`. The arm exists because `from_str_radix` has an error channel
        // and this crate has no `unwrap` to spend on a proof it cannot state to
        // the compiler.
        let raw = u128::from_str_radix(text, 16).map_err(|_| IdError::WrongLength { len: 32 })?;
        match NonZeroU128::new(raw) {
            Some(value) => Ok(Self(value)),
            None => Err(IdError::Zero),
        }
    }

    /// The value as a non-zero integer.
    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }

    /// Lowercase hex, 32 characters, big-endian.
    #[must_use]
    pub fn to_hex(self) -> String {
        lgwks_std::hex::encode(self.0.get().to_be_bytes())
    }

    /// Mint a fresh identifier from the operating system's entropy source.
    ///
    /// Big-endian, 128 bits, from [`lgwks_std::random`] — the estate's one
    /// CSPRNG backend. Distinctness is the entire property being bought: two
    /// runs of the same flow have to be distinguishable, and a value derived
    /// from a clock, a process id or a counter is exactly the kind that is not.
    ///
    /// # Errors
    ///
    /// [`MintError::Entropy`] when the source could not be read, and
    /// [`MintError::Zero`] when it returned 128 zero bits.
    ///
    /// `pub(crate)` because a caller outside this crate has no reason to hold a
    /// bare [`Id128`]: an identifier is meaningless without its role, and the
    /// two roles that are minted expose their own.
    #[cfg(feature = "ephemeral")]
    pub(crate) fn mint() -> Result<Self, MintError> {
        let raw = lgwks_std::random::bytes::<16>().map_err(MintError::Entropy)?;
        match NonZeroU128::new(u128::from_be_bytes(raw)) {
            Some(value) => Ok(Self(value)),
            None => Err(MintError::Zero),
        }
    }
}

impl fmt::Display for Id128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Declare a newtype over [`Id128`] for one role in the effect key.
///
/// Three of the seven key fields are 128-bit ids with identical representation
/// and entirely different meanings. One macro rather than three written-out
/// copies, so the three cannot drift: a role that gains a parse rule or loses
/// its `Display` gains or loses it everywhere, in one edit.
macro_rules! id_role {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(
            lgwks_std::wire::Archive,
            lgwks_std::wire::Serialize,
            lgwks_std::wire::Deserialize,
        )]
        #[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
        pub struct $name(Id128);

        impl $name {
            /// Wrap an identifier already known to be non-zero.
            #[must_use]
            pub const fn new(id: Id128) -> Self {
                Self(id)
            }

            /// Parse the 32-character lowercase hex form.
            ///
            /// # Errors
            ///
            /// As [`Id128::from_hex`].
            pub fn from_hex(text: &str) -> Result<Self, IdError> {
                Ok(Self(Id128::from_hex(text)?))
            }

            /// The underlying identifier.
            #[must_use]
            pub const fn id(self) -> Id128 {
                self.0
            }

        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

id_role! {
    /// Identifies one run: a host-generated value persisted before the first
    /// admission, so that two runs of the same flow are distinguishable.
    RunId
}

id_role! {
    /// Identifies one logical intent.
    ///
    /// Distinct from a content digest on purpose: identical content does not
    /// imply identical intent, so two actions with the same [`ActionDigest`] are
    /// still two actions and must not collapse into one settlement.
    ActionId
}

id_role! {
    /// Identifies the broker-created environment.
    ///
    /// Not a PID and not a display name. Both of those are reused by the
    /// operating system, so a settlement naming one can be delivered to a
    /// different process than the one it was about.
    EnvironmentId
}

/// A fresh run identity, minted from the operating system's entropy source.
///
/// Public, and on [`RunId`] rather than on the shared `id_role` macro,
/// because the three roles do not share this: a run id is the one a host
/// generates and persists before its first admission, and an [`ActionId`] is
/// already derived from a bot's structure by `crate::ecs::derive_action_id`.
/// A minting method on that role would be a second way to make one value, so
/// the macro carries none and each role states its own.
///
/// # Errors
///
/// [`MintError::Entropy`] when the source could not be read, and
/// [`MintError::Zero`] when it returned 128 zero bits.
#[cfg(feature = "ephemeral")]
impl RunId {
    /// Mint a run identity no other run can share.
    ///
    /// # Errors
    ///
    /// [`MintError::Entropy`] when the operating system's entropy source could
    /// not be read, and [`MintError::Zero`] when it returned 128 zero bits.
    pub fn mint() -> Result<Self, MintError> {
        Ok(Self(Id128::mint()?))
    }
}

/// A fresh environment identity, minted from the operating system's entropy.
///
/// The host names the environment a run acts on; the broker is what creates
/// it. A name that is not the process's own pid or display name is what stops a
/// settlement from being delivered to a different process than the one it was
/// about, and entropy is the only source for a name with that property.
///
/// # Errors
///
/// [`MintError::Entropy`] when the source could not be read, and
/// [`MintError::Zero`] when it returned 128 zero bits.
#[cfg(feature = "ephemeral")]
impl EnvironmentId {
    /// Mint an environment identity no other environment can share.
    ///
    /// # Errors
    ///
    /// [`MintError::Entropy`] when the operating system's entropy source could
    /// not be read, and [`MintError::Zero`] when it returned 128 zero bits.
    pub fn mint() -> Result<Self, MintError> {
        Ok(Self(Id128::mint()?))
    }
}

/// Why an [`AttemptId`] or [`EnvironmentEpoch`] could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CounterError {
    /// The decimal spelling was empty or contained a non-digit.
    NotDecimal,
    /// The value was zero, which the schema excludes.
    Zero,
    /// The value exceeded `u64::MAX`.
    Overflow,
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

/// Declare a newtype over [`NonZeroU64`] for one monotonic counter.
///
/// Shared for the same reason [`id_role`] is: [`AttemptId`] and
/// [`EnvironmentEpoch`] have the same representation, the same non-zero
/// invariant, and the same refusal to wrap, and they are not interchangeable.
macro_rules! counter_role {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(
            lgwks_std::wire::Archive,
            lgwks_std::wire::Serialize,
            lgwks_std::wire::Deserialize,
        )]
        #[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
        pub struct $name(NonZeroU64);

        impl $name {
            /// The first value in the sequence.
            pub const FIRST: Self = Self(NonZeroU64::MIN);

            /// Wrap a value already known to be non-zero.
            #[must_use]
            pub const fn new(value: NonZeroU64) -> Self {
                Self(value)
            }

            /// Parse the minimal base-10 spelling, rejecting zero.
            ///
            /// # Errors
            ///
            /// [`CounterError::NotDecimal`], [`CounterError::Zero`] or
            /// [`CounterError::Overflow`].
            pub fn from_decimal(text: &str) -> Result<Self, CounterError> {
                if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(CounterError::NotDecimal);
                }
                let raw: u64 = text.parse().map_err(|_| CounterError::Overflow)?;
                match NonZeroU64::new(raw) {
                    Some(value) => Ok(Self(value)),
                    None => Err(CounterError::Zero),
                }
            }

            /// The value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }

            /// The next value, or `None` at `u64::MAX`.
            ///
            /// `None` rather than a wrap. A wrapped counter restarts at
            /// [`Self::FIRST`], which is a value this type has already handed
            /// out for this sequence; a settlement arriving under it would
            /// compare equal to one from the beginning of the run, and the
            /// ledger would refuse a genuine settlement as a duplicate. Running
            /// out is recoverable; silently reusing an identity is not.
            #[must_use]
            pub const fn checked_next(self) -> Option<Self> {
                match self.0.checked_add(1) {
                    Some(next) => Some(Self(next)),
                    None => None,
                }
            }

            /// Whether the sequence is exhausted.
            #[must_use]
            pub const fn is_exhausted(self) -> bool {
                self.0.get() == u64::MAX
            }

            /// What this counter counts, for a refusal message.
            #[must_use]
            pub const fn role() -> &'static str {
                $what
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0.get())
            }
        }
    };
}

counter_role! {
    /// Which attempt at one [`ActionId`] an effect belongs to.
    ///
    /// Monotonic within an action and never reused. An attempt number is not an
    /// authorization credential: it identifies which try this is, and authority
    /// is checked separately, every time.
    AttemptId,
    "attempt"
}

counter_role! {
    /// The fencing generation of an [`EnvironmentId`].
    ///
    /// A replacement environment gets a higher epoch, and every command naming
    /// a lower one is invalid. This is what stops a command prepared before a
    /// restart from reaching the replacement.
    EnvironmentEpoch,
    "environment epoch"
}

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
    /// why it is one field of [`EffectKey`] rather than the whole of it.
    FlowRevision,
    "flow revision"
}

digest_role! {
    /// Binds operation, target, preconditions, postcondition and exact input.
    ///
    /// Deliberately excludes the run, action and attempt ids. Those are outside
    /// this digest but inside [`EffectKey`], so that the same logical action
    /// attempted twice shares a digest and still has two distinct keys.
    ActionDigest,
    "action digest"
}

/// Why an [`EffectKey`] was refused.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EffectKeyError {
    /// A 128-bit id field was malformed.
    Id(IdError),
    /// A counter field was malformed.
    Counter(CounterError),
    /// A digest field was malformed or carried an unsupported tag.
    Digest(DigestError),
}

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

/// The exact identity a settlement is about.
///
/// Seven fields, because a settlement that omits any of them cannot be checked
/// against the attempt it claims to describe:
///
/// - `run` and `action` separate two runs, and two intents within a run.
/// - `attempt` separates the tries of one intent.
/// - `flow` ties the attempt to the document that produced it, so a settlement
///   from an older revision of the flow is refusable rather than merged.
/// - `digest` ties it to the exact bound input, so a settlement about the same
///   action with different arguments is not the same settlement.
/// - `environment` and `epoch` tie it to one generation of one broker-created
///   environment, so a command prepared before a restart cannot settle against
///   the process that replaced it.
///
/// A slot or entry index is instrumentation, not identity, and is deliberately
/// absent: it is the one field that changes when a chain is edited, and a
/// settlement must survive an edit to a different part of the chain.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    lgwks_std::wire::Archive,
    lgwks_std::wire::Serialize,
    lgwks_std::wire::Deserialize,
)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct EffectKey {
    /// Which run this effect belongs to.
    run: RunId,
    /// Which logical intent within the run.
    action: ActionId,
    /// Which try at that intent.
    attempt: AttemptId,
    /// The flow document the attempt was produced from.
    flow: FlowRevision,
    /// The exact bound input, preconditions and postcondition.
    digest: ActionDigest,
    /// The broker-created environment the attempt was handed to.
    environment: EnvironmentId,
    /// The fencing generation that environment was at.
    epoch: EnvironmentEpoch,
}

impl EffectKey {
    /// Assemble a key from its seven fields.
    #[must_use]
    pub const fn new(
        run: RunId,
        action: ActionId,
        attempt: AttemptId,
        flow: FlowRevision,
        digest: ActionDigest,
        environment: EnvironmentId,
        epoch: EnvironmentEpoch,
    ) -> Self {
        Self {
            run,
            action,
            attempt,
            flow,
            digest,
            environment,
            epoch,
        }
    }

    /// The run.
    #[must_use]
    pub const fn run(self) -> RunId {
        self.run
    }

    /// The logical intent within the run.
    #[must_use]
    pub const fn action(self) -> ActionId {
        self.action
    }

    /// The attempt within the intent.
    #[must_use]
    pub const fn attempt(self) -> AttemptId {
        self.attempt
    }

    /// The flow revision the attempt came from.
    #[must_use]
    pub const fn flow(self) -> FlowRevision {
        self.flow
    }

    /// The action digest, binding the exact input.
    #[must_use]
    pub const fn digest(self) -> ActionDigest {
        self.digest
    }

    /// The environment.
    #[must_use]
    pub const fn environment(self) -> EnvironmentId {
        self.environment
    }

    /// The environment's fencing generation.
    #[must_use]
    pub const fn epoch(self) -> EnvironmentEpoch {
        self.epoch
    }

    /// The same key with a different attempt.
    ///
    /// The one field a retry moves. Everything else is held fixed by
    /// construction, which is what makes "unchanged logical intent" a property
    /// of the type rather than a rule a caller has to remember: you cannot
    /// produce the next attempt with a different input, because the input
    /// digest is not a parameter.
    #[must_use]
    pub const fn with_attempt(self, attempt: AttemptId) -> Self {
        Self { attempt, ..self }
    }

    /// Whether `other` is the same attempt, ignoring its environment epoch.
    ///
    /// This is the comparison a restart asks: the command was prepared against
    /// environment `e` at epoch `n`, and the environment is now `e` at epoch
    /// `n + 1`. Everything matching and only the epoch moved means the
    /// environment was replaced under a command that was never handed over.
    #[must_use]
    pub fn same_attempt_earlier_epoch(self, other: Self) -> bool {
        self.run == other.run
            && self.action == other.action
            && self.attempt == other.attempt
            && self.flow == other.flow
            && self.digest == other.digest
            && self.environment == other.environment
            && self.epoch.get() < other.epoch.get()
    }

    /// The key as a byte string, for hashing into a chain.
    ///
    /// Encoded through [`lgwks_std::wire`], the estate's binary format, rather
    /// than by hand. The format is derived from the type, so a field added to
    /// this key is carried by the encoding instead of being silently absent
    /// from every chain position computed after it — the failure the
    /// hand-summed widths this replaced could catch only if the sum was
    /// remembered.
    ///
    /// Every field is a fixed-width integer or a 32-byte digest, so the archive
    /// holds no relative pointers and its layout does not depend on pointer
    /// width. Two keys are equal exactly when their bytes are, and the test
    /// below is what holds that.
    ///
    /// The digest algorithm is deliberately *not* written. It was a byte in the
    /// hand-rolled form, and it was the same byte in every key that form could
    /// produce: [`FlowRevision`] and [`ActionDigest`] each fix their algorithm
    /// as a constant of the role, so the tag distinguished nothing. What
    /// separates two algorithms is the schema version this module's doc names,
    /// and a second algorithm is a new version rather than a differently-tagged
    /// key.
    ///
    /// # Errors
    ///
    /// [`WireError`] if the archive cannot be allocated.
    pub fn to_bytes(self) -> Result<AlignedVec, WireError> {
        lgwks_std::wire::to_bytes::<WireError>(&self)
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

/// The host-supplied facts that make one run's effect keys reproducible.
///
/// Three of [`EffectKey`]'s seven fields are constants of a run rather than of
/// an attempt: which run this is, which environment it acts on, and which
/// revision of the flow document it was admitted under. Keeping them in one
/// value is what makes a key *reconstructible*: a controller that comes back
/// after a crash can derive the same key for the same attempt only if it holds
/// the same three facts, and a fact it has to remember separately is a fact it
/// will eventually remember differently.
///
/// All three are the host's to supply, and none of them is derived here. The
/// run identity is generated and persisted before the first admission, so two
/// runs of the same flow are distinguishable; the environment identity names
/// the process or container the host created, not a pid or a display name; and
/// the flow revision is the digest of the document that was validated. A
/// default for any of them would be a default identity, which is the one thing
/// this module exists to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectIdentity {
    /// The run every key built from this identity belongs to.
    run: RunId,
    /// The environment every key built from this identity acts on.
    environment: EnvironmentId,
    /// The flow revision every key built from this identity came from.
    flow: FlowRevision,
}

/// The flow revision an ephemeral run carries.
///
/// An ephemeral run has no flow document, so there is no revision to bind. This
/// is a constant rather than a minted digest because minting one would be a
/// false statement: a revision asserts the *content* of a flow, and inventing a
/// fresh value per run would claim the content differed when there is none to
/// differ. Domain-separated rather than 32 zero bytes so it cannot be confused
/// with the digest of an empty document.
#[cfg(feature = "ephemeral")]
const EPHEMERAL_FLOW: &[u8] = b"lgwks.effect.v1.ephemeral-flow";

impl EffectIdentity {
    /// Record the host's three run-level facts.
    #[must_use]
    pub const fn new(run: RunId, environment: EnvironmentId, flow: FlowRevision) -> Self {
        Self {
            run,
            environment,
            flow,
        }
    }

    /// An identity for a run that persists nothing and has no flow document.
    ///
    /// The run and environment ids are **minted**, not defaulted: two calls
    /// produce two distinguishable identities, which is the property a default
    /// would destroy and the one thing this module refuses. OS entropy is the
    /// only source for them, because a value derived from a clock or a counter
    /// is not distinguishable across the restarts it has to survive.
    ///
    /// The flow revision is the one field this cannot mint, and the doc on
    /// [`EPHEMERAL_FLOW`] says why. A caller that *has* a validated flow
    /// document has a [`FlowRevision`] and belongs on [`EffectIdentity::new`] —
    /// a run with a document is not an ephemeral run, and the environment
    /// registered against this identity is what makes that distinction
    /// enforceable rather than advisory.
    ///
    /// # Errors
    ///
    /// [`MintError::Entropy`] when the operating system's entropy source could
    /// not be read, and [`MintError::Zero`] when it returned 128 zero bits.
    ///
    /// `pub(crate)`: the only caller is [`EffectScope::ephemeral`], and
    /// ephemerality is a property of a *scope*, not of an identity. An identity
    /// is three facts with no lifetime, so an "ephemeral" one held on its own is
    /// half a construction — the caller who wants a fresh identity and a durable
    /// journal has [`RunId::mint`], [`EnvironmentId::mint`] and
    /// [`EffectIdentity::new`].
    ///
    /// [`EffectScope::ephemeral`]: crate::ecs::EffectScope::ephemeral
    #[cfg(feature = "ephemeral")]
    pub(crate) fn ephemeral() -> Result<Self, MintError> {
        Ok(Self {
            run: RunId::mint()?,
            environment: EnvironmentId::mint()?,
            flow: FlowRevision::new(lgwks_std::hash::blake3(EPHEMERAL_FLOW)),
        })
    }

    /// The run.
    #[must_use]
    pub const fn run(self) -> RunId {
        self.run
    }

    /// The environment.
    #[must_use]
    pub const fn environment(self) -> EnvironmentId {
        self.environment
    }

    /// The flow revision.
    #[must_use]
    pub const fn flow(self) -> FlowRevision {
        self.flow
    }

    /// The key for one attempt: this identity's three fields plus the four the
    /// attempt supplies.
    #[must_use]
    pub const fn key(
        self,
        action: ActionId,
        attempt: AttemptId,
        digest: ActionDigest,
        epoch: EnvironmentEpoch,
    ) -> EffectKey {
        EffectKey::new(
            self.run,
            action,
            attempt,
            self.flow,
            digest,
            self.environment,
            epoch,
        )
    }

    /// Re-render `key` under this identity's environment at `epoch`.
    ///
    /// What a restart asks: the command was prepared against this environment
    /// at an older epoch, and the environment is now at a newer one. Everything
    /// but the generation is held, because everything but the generation is
    /// what makes it the *same* attempt.
    #[must_use]
    pub const fn at_epoch(self, key: EffectKey, epoch: EnvironmentEpoch) -> EffectKey {
        EffectKey::new(
            key.run(),
            key.action(),
            key.attempt(),
            key.flow(),
            key.digest(),
            self.environment,
            epoch,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

    /// The crate refuses a panicking path anywhere, tests included, so a test
    /// that needs a parsed value returns `Result` and propagates. A failure
    /// then names the cause instead of a line number in a macro.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn nonzero(value: u64) -> Result<NonZeroU64, CounterError> {
        NonZeroU64::new(value).ok_or(CounterError::Zero)
    }

    /// The one `MintError` arm a test can reach, and only from in here: the
    /// enum is `#[non_exhaustive]` so a caller outside has to write a wildcard
    /// arm, which also means a caller outside cannot construct an arm to check
    /// its rendering. `Entropy` needs the operating system to fail, so it is
    /// covered by the type it carries rather than by a test.
    #[cfg(feature = "ephemeral")]
    #[test]
    fn the_all_zero_mint_error_names_the_value_it_refused() {
        let rendered = MintError::Zero.to_string();
        assert!(
            rendered.contains("all-zero"),
            "an operator reading this cannot tell what was refused: {rendered}"
        );
        assert!(
            std::error::Error::source(&MintError::Zero).is_none(),
            "a refusal with no cause must not claim one"
        );
    }

    fn key(attempt: u64, epoch: u64) -> Result<EffectKey, EffectKeyError> {
        Ok(EffectKey::new(
            RunId::from_hex(RUN)?,
            ActionId::from_hex(ACTION)?,
            AttemptId::new(nonzero(attempt)?),
            FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
            ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
            EnvironmentId::from_hex(ENV)?,
            EnvironmentEpoch::new(nonzero(epoch)?),
        ))
    }

    #[test]
    fn id_round_trips_through_its_hex_form() -> TestResult {
        let id = Id128::from_hex(RUN)?;
        assert_eq!(id.to_hex(), RUN);
        Ok(())
    }

    #[test]
    fn id_round_trips_a_value_with_the_high_bit_set() -> TestResult {
        // Big-endian means the first byte is the most significant; a value
        // whose top bit is set is where a sign-confusion bug would show.
        let text = "ffffffffffffffffffffffffffffffff";
        let id = Id128::from_hex(text)?;
        assert_eq!(id.to_hex(), text);
        assert_eq!(id.get().get(), u128::MAX);
        Ok(())
    }

    #[test]
    fn id_rejects_the_all_zero_value() {
        assert_eq!(
            Id128::from_hex("00000000000000000000000000000000"),
            Err(IdError::Zero)
        );
    }

    #[test]
    fn id_rejects_uppercase_rather_than_folding_it() -> TestResult {
        let upper = RUN.to_uppercase();
        // The offset is derived rather than hardcoded: the leading bytes of
        // this constant are digits, which are valid hex in either case, so the
        // first refusal lands wherever the first letter happens to be.
        match Id128::from_hex(&upper) {
            Err(IdError::NotHex { at }) => {
                assert!(
                    upper.as_bytes()[at].is_ascii_uppercase(),
                    "offset {at} does not name the uppercase byte"
                );
            }
            other => return Err(format!("expected a NotHex refusal, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn id_reports_the_observed_length_not_a_rebased_offset() {
        assert_eq!(
            Id128::from_hex("abcd"),
            Err(IdError::WrongLength { len: 4 })
        );
    }

    #[test]
    fn id_rejects_a_non_ascii_input_without_panicking() {
        // Exactly 32 bytes of UTF-8, so the length check passes and the byte
        // check is what has to catch it.
        let text = "é".repeat(16);
        assert_eq!(text.len(), 32);
        assert_eq!(Id128::from_hex(&text), Err(IdError::NotHex { at: 0 }));
    }

    #[test]
    fn id_accepts_a_nonzero_value_by_construction() {
        let id = Id128::from_nonzero(NonZeroU128::MIN);
        assert_eq!(id.get().get(), 1);
        assert_eq!(id.to_hex(), "00000000000000000000000000000001");
    }

    #[test]
    fn every_id_role_parses_the_same_wire_form() -> TestResult {
        let run = RunId::from_hex(RUN)?;
        let action = ActionId::from_hex(RUN)?;
        let environment = EnvironmentId::from_hex(RUN)?;
        // The same 32 bytes, three types. That is the point of the newtypes: a
        // call site that swaps them does not compile, which is not something a
        // runtime assertion here could establish.
        assert_eq!(run.id(), action.id());
        assert_eq!(action.id(), environment.id());
        assert_eq!(run.to_string(), RUN);
        Ok(())
    }

    #[test]
    fn attempt_rejects_zero_and_non_decimal() {
        assert_eq!(AttemptId::from_decimal("0"), Err(CounterError::Zero));
        assert_eq!(AttemptId::from_decimal(""), Err(CounterError::NotDecimal));
        assert_eq!(AttemptId::from_decimal("1a"), Err(CounterError::NotDecimal));
        assert_eq!(AttemptId::from_decimal("-1"), Err(CounterError::NotDecimal));
        assert_eq!(
            AttemptId::from_decimal("18446744073709551616"),
            Err(CounterError::Overflow)
        );
        assert_eq!(AttemptId::from_decimal("42").map(AttemptId::get), Ok(42));
    }

    #[test]
    fn attempt_refuses_to_wrap_rather_than_reusing_an_identity() {
        let last = AttemptId::new(NonZeroU64::MAX);
        assert!(last.is_exhausted());
        assert_eq!(last.checked_next(), None);
        assert_eq!(AttemptId::FIRST.get(), 1);
        assert_eq!(AttemptId::FIRST.checked_next().map(AttemptId::get), Some(2));
    }

    #[test]
    fn the_two_counter_roles_keep_their_own_names() {
        assert_eq!(AttemptId::role(), "attempt");
        assert_eq!(EnvironmentEpoch::role(), "environment epoch");
        assert_eq!(EnvironmentEpoch::FIRST.to_string(), "1");
    }

    #[test]
    fn a_sha256_tag_is_unsupported_rather_than_malformed() {
        assert!(matches!(
            ActionDigest::from_tagged("sha256", DIGEST_HEX),
            Err(DigestError::UnsupportedAlgorithm {
                algorithm: DigestAlgorithm::Sha256
            })
        ));
        assert!(matches!(
            ActionDigest::from_tagged("md5", DIGEST_HEX),
            Err(DigestError::UnknownAlgorithm)
        ));
    }

    #[test]
    fn a_malformed_digest_body_is_distinct_from_an_unsupported_tag() {
        assert!(matches!(
            ActionDigest::from_tagged("blake3_256", "nothex"),
            Err(DigestError::Malformed(
                lgwks_std::hash::DigestParseError::WrongLength { len: 6 }
            ))
        ));
    }

    #[test]
    fn the_two_digest_roles_carry_the_same_algorithm_tag() {
        assert_eq!(FlowRevision::algorithm(), DigestAlgorithm::Blake3_256);
        assert_eq!(ActionDigest::algorithm().as_str(), "blake3_256");
        assert_eq!(FlowRevision::role(), "flow revision");
        assert_eq!(ActionDigest::role(), "action digest");
    }

    #[test]
    fn a_key_round_trips_every_field() -> TestResult {
        let bound = key(3, 7)?;
        assert_eq!(bound.run().to_string(), RUN);
        assert_eq!(bound.action().to_string(), ACTION);
        assert_eq!(bound.attempt().get(), 3);
        assert_eq!(bound.flow().to_hex(), FLOW_HEX);
        assert_eq!(bound.digest().to_hex(), DIGEST_HEX);
        assert_eq!(bound.environment().to_string(), ENV);
        assert_eq!(bound.epoch().get(), 7);
        Ok(())
    }

    #[test]
    fn with_attempt_moves_only_the_attempt() -> TestResult {
        let base = key(1, 4)?;
        let next = base.with_attempt(AttemptId::new(nonzero(2)?));
        assert_ne!(base, next);
        assert_eq!(base.run(), next.run());
        assert_eq!(base.action(), next.action());
        assert_eq!(base.flow(), next.flow());
        assert_eq!(base.digest(), next.digest());
        assert_eq!(base.environment(), next.environment());
        assert_eq!(base.epoch(), next.epoch());
        assert_eq!(next.attempt().get(), 2);
        Ok(())
    }

    #[test]
    fn an_epoch_bump_under_one_command_is_recognised() -> TestResult {
        let prepared = key(1, 4)?;
        let after_restart = key(1, 5)?;
        assert!(prepared.same_attempt_earlier_epoch(after_restart));
        assert!(!after_restart.same_attempt_earlier_epoch(prepared));
        Ok(())
    }

    #[test]
    fn a_different_input_digest_is_a_different_key_and_not_an_epoch_move() -> TestResult {
        let base = key(1, 4)?;
        let moved = EffectKey::new(
            base.run(),
            base.action(),
            base.attempt(),
            base.flow(),
            ActionDigest::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e2f",
            )?,
            base.environment(),
            EnvironmentEpoch::new(nonzero(5)?),
        );
        assert_ne!(base.digest(), moved.digest());
        assert!(!base.same_attempt_earlier_epoch(moved));
        Ok(())
    }

    #[test]
    fn a_key_renders_every_field_for_a_refusal_message() -> TestResult {
        let rendered = key(2, 9)?.to_string();
        assert!(rendered.contains(RUN), "{rendered}");
        assert!(rendered.contains("attempt 2"), "{rendered}");
        assert!(rendered.contains("epoch 9"), "{rendered}");
        assert!(rendered.contains("env "), "{rendered}");
        Ok(())
    }
}

/// How an admitted observation is named for the purpose of a dispatch digest.
///
/// A content identity, not an event number: the same value of the same type
/// writes the same bytes (so a restart can retire work that already landed),
/// and a different value writes different ones (so a restart cannot read new
/// work as already applied). Two *events* with equal content must still be
/// distinguishable — content equality is not event identity — so a caller that
/// needs that puts the event id into the value and into this trait. See
/// [`EventId`].
///
/// Every integer writes `to_le_bytes` of a fixed width, so a 32-bit and a
/// 64-bit target derive the same identity (issue #101).
///
/// # The schema id is the durable name of the type (issue #118)
///
/// [`SCHEMA_ID`](Self::SCHEMA_ID) is what binds identity bytes to a type, and
/// it is a *declared* versioned string, never `std::any::type_name`. Rust
/// documents `type_name` as diagnostic: its format is unspecified and it may
/// change between compiler versions, so a module rename or a toolchain upgrade
/// would silently give every admitted event a new identity and resurrect
/// settled work as fresh. A declared id survives all of that; a *changed* id
/// is an explicit schema migration whose identities differ by construction —
/// recovered work under the old id is refused as superseded rather than
/// silently folded. `type_name` remains in use only where a human reads it
/// (diagnostics), never inside a digest.
pub trait InputIdentity {
    /// The versioned schema identity of `Self`, declared by the type's author.
    ///
    /// Convention: `lgwks.bot.schema.v1.<kind>`, lowercased, stable across
    /// builds and compilers. Bump the version when the identity *bytes* change
    /// meaning — that bump is the migration boundary, and identities across it
    /// are distinct by construction.
    const SCHEMA_ID: &'static [u8];

    /// Write this input's canonical identity bytes into `hasher`.
    ///
    /// Variable-length parts go through [`Hasher::write_framed`] so the
    /// stream's field boundaries are part of the digest.
    fn write_identity(&self, hasher: &mut Hasher);

    /// Whether these identity bytes name an *event*, rather than only content.
    ///
    /// The two readings of a returning identity are different questions and
    /// only one of them is right for a given type (issue #129).
    ///
    /// - `true` — the bytes name an event, so equal bytes mean the same event
    ///   has come back. That is a redelivery of work that already landed, and
    ///   it retires however many other episodes came between. [`EventId`] is
    ///   this: its identity carries the caller's event id.
    /// - `false` (the default) — the bytes name only content, so a state watch
    ///   returning to a previous value is a *new* episode of work rather than a
    ///   redelivery. It runs again. Only the value the last applied episode
    ///   carried retires, which is what keeps a restart from double-firing on
    ///   an unchanged source while a `0 → 1 → 0` watch still fires three times.
    ///
    /// Defaulting to `false` is deliberate: a type that has not claimed event
    /// identity gets the state-transition semantics the watch contract
    /// documents, and opting into `true` is a statement about the type.
    fn names_an_event(&self) -> bool {
        false
    }
}

/// Implement [`InputIdentity`] for a fixed-width integer via `to_le_bytes`.
macro_rules! input_identity_int {
    ($($t:ty => $name:literal),* $(,)?) => {$(
        impl InputIdentity for $t {
            const SCHEMA_ID: &'static [u8] =
                concat!("lgwks.bot.schema.v1.int-", $name).as_bytes();
            fn write_identity(&self, hasher: &mut Hasher) {
                hasher.update(&self.to_le_bytes());
            }
        }
    )*};
}

input_identity_int!(
    u8 => "u8",
    u16 => "u16",
    u32 => "u32",
    u64 => "u64",
    i8 => "i8",
    i16 => "i16",
    i32 => "i32",
    i64 => "i64",
);

impl InputIdentity for bool {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.bool";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.update(&[u8::from(*self)]);
    }
}

impl InputIdentity for String {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.string";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.write_framed(self.as_bytes());
    }
}

impl InputIdentity for &str {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.string";

    fn write_identity(&self, hasher: &mut Hasher) {
        hasher.write_framed(self.as_bytes());
    }
}

/// A value tagged with an event id, so two equal payloads stay two events.
///
/// The id is written first and is part of the identity: `EventId { id: 1,
/// value: 5 }` and `EventId { id: 2, value: 5 }` are different admitted
/// inputs even though the payloads compare equal. `PartialEq` includes the id
/// for the same reason — the change filter is content equality, and two
/// distinct events that compare equal would be collapsed into one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct EventId<T> {
    /// The caller's event identity. Distinct per event, including events that
    /// carry equal payloads.
    id: u64,
    /// The payload.
    value: T,
}

impl<T> EventId<T> {
    /// Tag `value` as event `id`.
    #[must_use]
    pub const fn new(id: u64, value: T) -> Self {
        Self { id, value }
    }

    /// The caller's event identity.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The payload.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }
}

impl<T: InputIdentity> InputIdentity for EventId<T> {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.event-id";

    fn write_identity(&self, hasher: &mut Hasher) {
        // The payload's own schema travels inside the stream: two wrappers
        // over different payload types stay distinct identities even when
        // their ids and payload bytes coincide, which the erased
        // `type_name`-based scheme could only tell apart unreliably.
        hasher.write_framed(T::SCHEMA_ID);
        hasher.update(&self.id.to_le_bytes());
        self.value.write_identity(hasher);
    }

    /// The id is written into the identity, so equal identity bytes really do
    /// mean the same event. A returning one is a redelivery and retires.
    fn names_an_event(&self) -> bool {
        true
    }
}
