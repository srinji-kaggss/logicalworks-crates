//! Action and input identities: the framed digests a dispatch is keyed by, and the effect scope it runs under.

// imports
use super::ACTION_DIGEST_DOMAIN;
use super::ACTION_ID_DOMAIN;
use super::ActionDigest;
use super::ActionId;
use super::Broker;
use super::Digest;
use super::EffectIdentity;
use super::EffectJournal;
use super::EffectKey;
use super::EventKind;
use super::FlowRevision;
use super::Hasher;
use super::Id128;
use super::JournalError;
#[cfg(feature = "ephemeral")]
use super::MemoryJournal;
#[cfg(feature = "ephemeral")]
use super::MintError;
use super::NonZeroU128;
use super::fmt;
use crate::journal::frame::SaturatingFrom;

/// The refusal for a journal that already holds a *different* outcome for `key`
/// than the one being settled.
///
/// One constructor for both places that compare a recorded outcome with the
/// evidence in hand, so they cannot name different orderings for one conflict.
pub(super) fn changed_outcome(key: EffectKey) -> JournalError {
    JournalError::OutOfOrder {
        key: Box::new(key),
        expected: Some(EventKind::Verified),
        attempted: EventKind::OutcomeObserved,
    }
}

/// Hash a sequence of byte fields into the estate's content-identity digest.
///
/// Each field is length-prefixed with its `u64` little-endian width before the
/// bytes themselves, so a variable-length name cannot slide into the next field
/// and produce the same digest for a different parse. A domain separator
/// identifies the format; it does not frame the fields inside it. Indices are
/// always `u64::to_le_bytes` rather than `usize`, so a 32-bit and a 64-bit
/// target derive the same identity for the same bot (issue #101).
pub(super) fn hash_parts(parts: &[&[u8]]) -> Digest {
    let mut hasher = Hasher::new();
    for part in parts {
        // Length-framed: a stream over several variable-length parts must
        // depend on the parts, not only on their concatenation (issue #118).
        hasher.write_framed(part);
    }
    hasher.finalize()
}

/// A `usize` index as a portable `u64`.
///
/// `usize::to_le_bytes` is 4 bytes on a 32-bit target and 8 on a 64-bit one,
/// so the same bot derived different identities on each. Every index that
/// enters a digest goes through here.
fn portable_index(index: usize) -> u64 {
    u64::saturating_from(index)
}

/// The fallback identity for a binding that cannot name itself.
pub(super) fn derive_input_stamp(stamp: u64) -> [u8; 16] {
    let digest = hash_parts(&[b"lgwks.bot.input-stamp.v1", &stamp.to_le_bytes()]);
    let mut wide = [0_u8; 16];
    wide.copy_from_slice(&digest.as_bytes()[..16]);
    wide
}

/// Fold a digest into a non-zero 128-bit identifier.
///
/// The all-zero identifier is not a valid one (see [`IdError`]), and a hash has
/// no such exclusion, so a derivation that landed on zero would produce an
/// identity the parser would refuse to read back. The zero case is mapped to
/// [`NonZeroU128::MIN`] rather than refused, because a derivation that can fail
/// is a derivation every caller has to handle, and the collision it introduces
/// is one in 2^128.
///
/// [`IdError`]: crate::effect::IdError
fn id_from_digest(digest: &Digest) -> Id128 {
    let bytes = digest.as_bytes();
    let mut wide = [0_u8; 16];
    for (slot, byte) in wide.iter_mut().zip(bytes.iter()) {
        *slot = *byte;
    }
    match NonZeroU128::new(u128::from_be_bytes(wide)) {
        Some(identity) => Id128::from_nonzero(identity),
        None => Id128::from_nonzero(NonZeroU128::MIN),
    }
}

/// The logical intent of one entry of one chain.
///
/// Derived rather than declared, because the substrate is the only thing that
/// knows a chain's position, and position is what a settlement must survive an
/// edit elsewhere in the document to. The derivation reads the bot's name, the
/// entry's position, and the action's own domain identifier, so two entries
/// with the same domain in the same chain are still two intents.
///
/// `ActionId` is not a content digest, and this is not one either: [`ActionDigest`]
/// is what binds the input, and this binds *which action* it is. Deriving it
/// from the action's own `domain_id` is the closest the erased `ExecuteAny`
/// comes to naming what it does.
pub(super) fn derive_action_id(bot: &str, chain: usize, entry: usize, domain: &str) -> ActionId {
    ActionId::new(id_from_digest(&hash_parts(&[
        ACTION_ID_DOMAIN,
        bot.as_bytes(),
        &portable_index(chain).to_le_bytes(),
        &portable_index(entry).to_le_bytes(),
        domain.as_bytes(),
    ])))
}

/// The exact admitted input one entry was dispatched from.
///
/// The ECS substrate has no payload bytes to hash: a transition's binding is an
/// erased `Box<dyn Any>` and nothing here asks it to be `Hash`. Content
/// equality is not event identity either — two distinct commands with the same
/// payload must stay distinguishable — so the digest binds the *admitted input
/// identity*: a monotonic stamp minted when the observation was admitted and
/// continued across a restart from the journal's tail, not the process-local
/// `Revision` counter, which reopens at 1 after every reconstruction (issue
/// #101).
///
/// Two attempts within one admitted input share a digest, which is what makes
/// a retry a retry. A new admission gets a new stamp even when the value is
/// equal or the revision counter has wrapped back to 1, which is what stops a
/// restarted bot from reading new work as already applied.
pub(super) fn derive_action_digest(
    flow: FlowRevision,
    chain: usize,
    entry: usize,
    input: &[u8; 16],
) -> ActionDigest {
    ActionDigest::new(hash_parts(&[
        ACTION_DIGEST_DOMAIN,
        flow.digest().as_bytes(),
        &portable_index(chain).to_le_bytes(),
        &portable_index(entry).to_le_bytes(),
        input,
    ]))
}

/// The effect identity and the two adapters a dispatch is recorded through.
///
/// Handed to [`EcsBuilder::with_effects`](super::builder::EcsBuilder::with_effects), and required: there is no default
/// and no way to build a bot without one. A default would be a default
/// *identity*, and an identity a caller did not choose is one they cannot
/// recover against — which is precisely the state that makes a restart resend a
/// merge.
///
/// The three pieces are separate arguments rather than one because they fail
/// differently. A journal with no durability refuses at the boundary; a broker
/// with the wrong environment refuses to authorize; an identity naming the
/// wrong run makes every recovered key foreign. Fusing them into one value
/// would let a caller construct a scope it cannot reason about piece by piece.
pub struct EffectScope {
    /// Which run this is, which environment it acts on, and which flow revision
    /// it came from.
    identity: EffectIdentity,
    /// The environment generations a dispatch is fenced against.
    pub(super) broker: Broker,
    /// Where a dispatch is written down before it leaves the process.
    pub(super) journal: Box<dyn EffectJournal>,
}

/// Why an ephemeral scope could not be built.
///
/// Two arms, because building one crosses two independent refusals: the
/// operating system's entropy source, and the broker's registration of the
/// environment the identity was minted against. Neither converts into the
/// other, and flattening them would name the wrong failure.
#[cfg(feature = "ephemeral")]
#[derive(Debug)]
#[non_exhaustive]
pub enum EphemeralError {
    /// A run or environment identifier could not be minted.
    Mint(MintError),
    /// The broker refused to register the minted environment.
    ///
    /// Unreachable through this constructor, which registers exactly one
    /// freshly minted id into an empty broker, and present because
    /// [`Broker::register`] has a refusal channel and this crate has no
    /// `unwrap` to spend on a proof it cannot state to the compiler.
    Broker(crate::broker::BrokerError),
}

#[cfg(feature = "ephemeral")]
impl fmt::Display for EphemeralError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `*self` and `ref`, for the reason `JournalError` gives: the patterns
        // then carry the enum's own type and `pattern_type_mismatch` is
        // satisfied without a dereference at each field.
        match *self {
            Self::Mint(ref cause) => write!(f, "could not mint an ephemeral identity: {cause}"),
            Self::Broker(ref cause) => {
                write!(f, "could not register the ephemeral environment: {cause}")
            }
        }
    }
}

#[cfg(feature = "ephemeral")]
impl std::error::Error for EphemeralError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Mint(ref cause) => Some(cause),
            Self::Broker(ref cause) => Some(cause),
        }
    }
}

#[cfg(feature = "ephemeral")]
impl From<MintError> for EphemeralError {
    fn from(cause: MintError) -> Self {
        Self::Mint(cause)
    }
}

/// Printed as the journal's promise and position rather than as the journal.
///
/// [`EffectJournal`] is an object-safe trait with no `Debug` bound, and adding
/// one to serve a `{:?}` would push the burden onto every implementation for no
/// gain. What a reader needs is which journal this is and how far it is
/// committed, and both of those the trait already answers.
impl fmt::Debug for EffectScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectScope")
            .field("identity", &self.identity)
            .field("broker", &self.broker)
            .field("durability", &self.journal.durability())
            .field("tail", &self.journal.tail())
            .finish()
    }
}

impl EffectScope {
    /// Record the identity, the fence and the journal a bot dispatches under.
    ///
    /// A caller with no host identity to supply — a test, an example, or work
    /// whose effects stay in this process — wants `EffectScope::ephemeral()`
    /// (feature `ephemeral`), which mints the identity and pairs it with an
    /// in-memory journal. It is not a shorter spelling of this: the journal it
    /// supplies is one [`EffectJournal::admit_external_handoff`] refuses, so the
    /// two are different capabilities rather than two ways to do one.
    ///
    /// Spelled as a code span rather than a link because the method is behind
    /// that feature, and a `--no-default-features` build has no item to link to
    /// — which the rustdoc gate refuses rather than tolerating.
    #[must_use]
    pub fn new(identity: EffectIdentity, broker: Broker, journal: Box<dyn EffectJournal>) -> Self {
        Self {
            identity,
            broker,
            journal,
        }
    }

    /// A scope for a run whose effects never leave the process.
    ///
    /// The one constructor a caller reaches for when there is no host, no
    /// persisted history and no flow document: a test, an example, or work
    /// whose effects stay inside this process. It mints a fresh run and
    /// environment from OS entropy, registers that environment, and pairs them
    /// with an in-memory journal.
    ///
    /// **The scope is safe to hand to real code because the journal refuses to
    /// be the record behind an external handoff.** [`MemoryJournal`] reports
    /// [`DurabilityPromise::Ephemeral`], and
    /// [`EffectJournal::admit_external_handoff`] returns
    /// [`JournalError::PromiseUnmet`] for it — so a dispatch that would leave
    /// the process fails at the boundary rather than proceeding on a record
    /// that cannot survive the process it was written in. That refusal is what
    /// makes this a capability rather than a testing shortcut: it is the
    /// difference between "no journal" and "a journal that tells you it is not
    /// enough".
    ///
    /// The three pieces [`EffectScope::new`] takes separately are still three
    /// pieces — this supplies all three rather than fusing them, so a caller
    /// that outgrows the ephemeral case can read each one back out
    /// ([`identity`](Self::identity), [`broker`](Self::broker),
    /// [`into_journal`](Self::into_journal)) and move to a durable journal
    /// without rebuilding the identity.
    ///
    /// # Errors
    ///
    /// [`EphemeralError::Mint`] when the operating system's entropy source
    /// could not be read, and [`EphemeralError::Broker`] if the broker refused
    /// the minted environment.
    ///
    /// [`DurabilityPromise::Ephemeral`]: crate::journal::DurabilityPromise::Ephemeral
    /// [`EffectJournal::admit_external_handoff`]: crate::journal::EffectJournal::admit_external_handoff
    /// [`JournalError::PromiseUnmet`]: crate::journal::JournalError::PromiseUnmet
    #[cfg(feature = "ephemeral")]
    pub fn ephemeral() -> Result<Self, EphemeralError> {
        let identity = EffectIdentity::ephemeral()?;
        let mut broker = Broker::new();
        broker
            .register(identity.environment())
            .map_err(EphemeralError::Broker)?;
        Ok(Self::new(identity, broker, Box::new(MemoryJournal::new())))
    }

    /// The host's three run-level facts.
    #[must_use]
    pub const fn identity(&self) -> EffectIdentity {
        self.identity
    }

    /// The environment broker this scope fences against.
    #[must_use]
    pub const fn broker(&self) -> &Broker {
        &self.broker
    }

    /// The journal a dispatch is appended to.
    #[must_use]
    pub fn journal(&self) -> &dyn EffectJournal {
        &*self.journal
    }

    /// The journal, to append through.
    ///
    /// `&mut self` rather than interior mutability: [`EffectJournal`] fences
    /// writers within one process through its receiver, and a shared handle
    /// that could append from two places at once would hand that fence back.
    #[must_use]
    pub fn journal_mut(&mut self) -> &mut dyn EffectJournal {
        &mut *self.journal
    }

    /// Take the journal back.
    ///
    /// What a host calls on its way out, so the record outlives the bot that
    /// wrote it. See [`EcsBot::into_journal`](super::bot::EcsBot::into_journal).
    #[must_use]
    pub fn into_journal(self) -> Box<dyn EffectJournal> {
        self.journal
    }
}
