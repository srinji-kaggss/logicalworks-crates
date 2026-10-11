//! The effect scope a bot dispatches through, and its construction errors.

use std::fmt;

// `self` is load-bearing: the `Component` and `Resource` derives expand to
// `bevy_ecs::…` paths, so the crate name has to be in scope at the use site even
// though every edge in this crate is written through the `lgwks_deps` facade.

#[cfg(doc)]
use super::bot::EcsBot;
#[cfg(doc)]
use super::builder::EcsBuilder;
use crate::broker::Broker;
use crate::effect::EffectIdentity;
#[cfg(feature = "ephemeral")]
use crate::effect::MintError;
use crate::journal::EffectJournal;
#[cfg(feature = "ephemeral")]
use crate::journal::MemoryJournal;

// The one declared clock every deadline in this crate names (INV-BOT-30). Only
// its wall watchdog is read below — a source that stopped answering is not
// waiting for time, so a logical counter a caller can advance is the wrong
// authority for it — but it is reached through the crate's clock rather than
// through a free-floating `Instant`, which is what makes the elapsed time that
// cancelled a poll traceable to a named authority.

/// The effect identity and the two adapters a dispatch is recorded through.
///
/// Handed to [`EcsBuilder::with_effects`], and required: there is no default
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
    pub(super) identity: EffectIdentity,
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
    /// wrote it. See [`EcsBot::into_journal`].
    #[must_use]
    pub fn into_journal(self) -> Box<dyn EffectJournal> {
        self.journal
    }
}
