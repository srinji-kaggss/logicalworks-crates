//! The environment broker: registering environments, fencing the generations
//! they go through, and handing out authority for one exact attempt.
//!
//! [`crate::journal`] records what happened. This decides what may happen at
//! all. A durable record of a dispatch is worth little if the dispatch was
//! aimed at an environment that had already been replaced: the record would be
//! accurate and the effect would still have landed somewhere nobody expected.
//!
//! # Fencing is a comparison, not a flag
//!
//! An [`EffectKey`] names an environment and the generation it was prepared
//! against. Authority is minted only when that generation is the one the broker
//! currently holds, so a command prepared before a replacement cannot execute
//! after it. Two failure directions are distinguished rather than collapsed:
//!
//! - [`BrokerError::Superseded`] when the key names an *older* generation. This
//!   is the real case, a command that was correct and is now stale.
//! - [`BrokerError::NeverIssued`] when the key names a *newer* one. The broker
//!   never minted it, so the key was not produced by this broker at all. Folded
//!   into one "generation mismatch" error these would print identically while
//!   calling for opposite investigations.
//!
//! Minting is one instant of that comparison, and one instant is not a
//! guarantee. [`Broker::revalidate`] runs the identical check again at the
//! handoff, because a replacement landing between the mint and the handoff would
//! otherwise leave a legitimately minted warrant dispatchable.
//!
//! # What the broker owns
//!
//! The environment identity itself comes from the host, because the host is what
//! has the entropy and what actually created the process or container. What the
//! broker owns is the environment's lifetime and its generation: once registered
//! an environment is this broker's to replace and to close, and a closed
//! environment mints no further authority. That is the resource contract this
//! module declares, and it is why registration is explicit rather than a
//! counter the broker invents.
//!
//! [`Broker::authorize`]: crate::broker::Broker::authorize
//! [`Broker::revalidate`]: crate::broker::Broker::revalidate
//! [`BrokerError::NeverIssued`]: crate::broker::BrokerError::NeverIssued
//! [`BrokerError::Superseded`]: crate::broker::BrokerError::Superseded
//! [`DurableAck`]: crate::journal::DurableAck
//! [`EffectJournal`]: crate::journal::EffectJournal
//! [`EffectKey`]: crate::effect::EffectKey

use core::fmt;
use core::num::NonZeroU64;
use std::collections::HashMap;

use crate::effect::{EffectKey, EnvironmentEpoch, EnvironmentId};
use crate::journal::{DurableAck, EffectEvent, EffectJournal, JournalError};

/// What the broker knows about one environment.
#[derive(Debug, Clone, Copy)]
struct Environment {
    /// The generation currently authorized to receive commands.
    epoch: EnvironmentEpoch,
    /// Whether this broker still owns the environment. A closed environment
    /// mints no further authority, which is the cleanup half of the resource
    /// contract.
    open: bool,
}

/// What the broker can refuse.
#[derive(Debug)]
#[non_exhaustive]
pub enum BrokerError {
    /// The environment was already registered with this broker.
    ///
    /// Refused rather than treated as idempotent, because the two cases a
    /// silent re-register would merge are "the host is setting this up" and
    /// "the host lost track of what it already set up", and only the first
    /// should reset nothing.
    AlreadyRegistered {
        /// The environment that was already known.
        id: EnvironmentId,
    },
    /// The broker has never heard of the environment.
    UnknownEnvironment {
        /// The environment named by the key.
        id: EnvironmentId,
    },
    /// The environment has been closed.
    Closed {
        /// The environment that was closed.
        id: EnvironmentId,
    },
    /// The command names a generation that has been replaced.
    ///
    /// A command prepared against an older generation is refused here rather
    /// than dispatched, which is the whole point of fencing.
    Superseded {
        /// The environment the command was prepared for.
        environment: EnvironmentId,
        /// The generation the command names.
        presented: EnvironmentEpoch,
        /// The generation the broker currently holds.
        current: EnvironmentEpoch,
    },
    /// The command names a generation this broker never issued.
    ///
    /// Distinct from [`Self::Superseded`] on purpose: a stale command means one
    /// investigation and a generation from nowhere means another, and one error
    /// for both would print the same sentence for each.
    NeverIssued {
        /// The environment the command was prepared for.
        environment: EnvironmentId,
        /// The generation the command names.
        presented: EnvironmentEpoch,
        /// The generation the broker currently holds.
        current: EnvironmentEpoch,
    },
    /// The environment has no generations left.
    ///
    /// Reachable only after 2^64 replacements. Named rather than wrapped,
    /// because a wrapped generation would re-authorize commands the broker
    /// already fenced.
    Exhausted {
        /// The environment whose generation space is spent.
        id: EnvironmentId,
    },
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self` so each pattern's type is the enum's own type
        // rather than a reference to it. `clippy::pattern_type_mismatch` is
        // forbidden in this workspace.
        match *self {
            Self::AlreadyRegistered { id } => {
                write!(f, "environment {id} is already registered with this broker")
            }
            Self::UnknownEnvironment { id } => {
                write!(f, "this broker has no environment {id}")
            }
            Self::Closed { id } => {
                write!(f, "environment {id} is closed and mints no authority")
            }
            Self::Superseded {
                environment,
                presented,
                current,
            } => write!(
                f,
                "a command for {environment} at generation {presented} names a \
                 generation that has been replaced; the broker now holds {current}"
            ),
            Self::NeverIssued {
                environment,
                presented,
                current,
            } => write!(
                f,
                "a command for {environment} names generation {presented}, which \
                 this broker never issued; it holds {current}"
            ),
            Self::Exhausted { id } => {
                write!(f, "environment {id} has no generations left")
            }
        }
    }
}

impl std::error::Error for BrokerError {}

/// Authority to hand one exact attempt to the environment it was prepared for.
///
/// Minted only by [`Broker::authorize`], and deliberately not [`Clone`]. The
/// seal is that the fields are private and there is no public constructor, so a
/// warrant cannot be assembled from an environment id and a generation a caller
/// chose.
///
/// Not being `Clone` stops a warrant being copied; it does not stop one being
/// used twice through a shared reference, and it is not what makes a double
/// dispatch impossible. The journal's ordering ladder is what does that. This
/// type stops the warrant from being *scattered*.
#[derive(Debug)]
pub struct Authority {
    /// The environment the attempt may be handed to.
    environment: EnvironmentId,
    /// The generation it was authorized against.
    epoch: EnvironmentEpoch,
}

impl Authority {
    /// Seal a warrant. Crate-private on purpose: this is the seal.
    /// [`Broker::authorize`] is the only caller, so a warrant can never name a
    /// generation the broker did not check.
    pub(crate) const fn new(environment: EnvironmentId, epoch: EnvironmentEpoch) -> Self {
        Self { environment, epoch }
    }

    /// The environment the attempt may be handed to.
    #[must_use]
    pub const fn environment(&self) -> EnvironmentId {
        self.environment
    }

    /// The generation it was authorized against.
    #[must_use]
    pub const fn epoch(&self) -> EnvironmentEpoch {
        self.epoch
    }
}

impl fmt::Display for Authority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "authority for {} at generation {}",
            self.environment, self.epoch
        )
    }
}

/// The environments a run may act on, and the generation each is at.
///
/// One broker per run. It holds no native handle itself: the adopter that
/// actually owns the process, socket or input seat lives behind whatever
/// [`EffectJournal`] adapter and dispatch path the host supplies. What this owns
/// is the fence, because fencing is a comparison and a comparison needs one
/// place that holds the current generation.
#[derive(Debug, Default)]
pub struct Broker {
    /// The known environments, by identity.
    environments: HashMap<EnvironmentId, Environment>,
}

impl Broker {
    /// A broker that owns no environment.
    #[must_use]
    pub fn new() -> Self {
        Self {
            environments: HashMap::new(),
        }
    }

    /// Take ownership of an environment at its first generation.
    ///
    /// The identity comes from the host, which is what created the process or
    /// container and therefore holds the entropy; the broker owns the
    /// environment's lifetime from here.
    ///
    /// # Errors
    ///
    /// [`BrokerError::AlreadyRegistered`] when this broker already owns `id`.
    pub fn register(&mut self, id: EnvironmentId) -> Result<EnvironmentEpoch, BrokerError> {
        if self.environments.contains_key(&id) {
            return Err(BrokerError::AlreadyRegistered { id });
        }
        // The first generation is one, not zero: the counter is a `NonZeroU64`
        // precisely so that a zeroed or truncated field cannot be read as a
        // generation the broker issued.
        let epoch = EnvironmentEpoch::new(NonZeroU64::MIN);
        self.environments
            .insert(id, Environment { epoch, open: true });
        Ok(epoch)
    }

    /// Replace the environment, invalidating every command prepared against the
    /// generation it was at.
    ///
    /// This is what a restart of the underlying process looks like from the
    /// broker's side: same identity, new generation, and every warrant already
    /// handed out is now stale.
    ///
    /// # Errors
    ///
    /// [`BrokerError::UnknownEnvironment`], [`BrokerError::Closed`] or
    /// [`BrokerError::Exhausted`].
    pub fn replace(&mut self, id: EnvironmentId) -> Result<EnvironmentEpoch, BrokerError> {
        let environment = self
            .environments
            .get_mut(&id)
            .ok_or(BrokerError::UnknownEnvironment { id })?;
        if !environment.open {
            return Err(BrokerError::Closed { id });
        }
        let next = environment
            .epoch
            .checked_next()
            .ok_or(BrokerError::Exhausted { id })?;
        environment.epoch = next;
        Ok(next)
    }

    /// Release the environment. Idempotent, because a cleanup path that has to
    /// know whether it already ran is a cleanup path that will sometimes not run.
    ///
    /// # Errors
    ///
    /// [`BrokerError::UnknownEnvironment`] when this broker never owned `id`.
    pub fn close(&mut self, id: EnvironmentId) -> Result<(), BrokerError> {
        let environment = self
            .environments
            .get_mut(&id)
            .ok_or(BrokerError::UnknownEnvironment { id })?;
        environment.open = false;
        Ok(())
    }

    /// The generation this broker currently holds for `id`.
    #[must_use]
    pub fn epoch(&self, id: EnvironmentId) -> Option<EnvironmentEpoch> {
        self.environments
            .get(&id)
            .map(|environment| environment.epoch)
    }

    /// Whether this broker still owns `id` and will mint authority for it.
    #[must_use]
    pub fn is_open(&self, id: EnvironmentId) -> bool {
        self.environments
            .get(&id)
            .is_some_and(|environment| environment.open)
    }

    /// The generation check, run both when a warrant is minted and when it is
    /// presented again.
    ///
    /// One function because the two callers must agree exactly. A second copy
    /// of this comparison that drifted would mean a warrant the broker would
    /// refuse to mint and would accept at the boundary, which is the failure
    /// fencing exists to prevent.
    fn check_generation(
        &self,
        id: EnvironmentId,
        presented: EnvironmentEpoch,
    ) -> Result<EnvironmentEpoch, BrokerError> {
        let environment = self
            .environments
            .get(&id)
            .ok_or(BrokerError::UnknownEnvironment { id })?;
        if !environment.open {
            return Err(BrokerError::Closed { id });
        }
        let current = environment.epoch;
        if presented.get() > current.get() {
            return Err(BrokerError::NeverIssued {
                environment: id,
                presented,
                current,
            });
        }
        if presented != current {
            return Err(BrokerError::Superseded {
                environment: id,
                presented,
                current,
            });
        }
        Ok(current)
    }

    /// Mint authority for `key`, if and only if the generation it names is the
    /// one this broker currently holds.
    ///
    /// This is a check at one instant. It is not sufficient on its own: see
    /// [`Broker::revalidate`] for why the handoff path has to ask again.
    ///
    /// # Errors
    ///
    /// [`BrokerError::UnknownEnvironment`], [`BrokerError::Closed`],
    /// [`BrokerError::Superseded`] or [`BrokerError::NeverIssued`].
    pub fn authorize(&self, key: EffectKey) -> Result<Authority, BrokerError> {
        let id = key.environment();
        let current = self.check_generation(id, key.epoch())?;
        Ok(Authority::new(id, current))
    }

    /// Re-check a warrant that was minted earlier, immediately before the
    /// handoff it authorizes.
    ///
    /// Authorizing and handing over are two instants, and a replacement landing
    /// between them would leave a warrant that was minted legitimately and is
    /// now stale. RQ-006 says replacement invalidates *all* old commands, and a
    /// check that only runs at mint time does not deliver that. So the handoff
    /// path presents its warrant here, and a warrant whose generation has been
    /// replaced is refused even though nothing was wrong when it was minted.
    ///
    /// # Errors
    ///
    /// [`BrokerError::UnknownEnvironment`], [`BrokerError::Closed`],
    /// [`BrokerError::Superseded`] or [`BrokerError::NeverIssued`].
    pub fn revalidate(&self, authority: &Authority) -> Result<(), BrokerError> {
        self.check_generation(authority.environment(), authority.epoch())?;
        Ok(())
    }
}

/// What went wrong on the one path that may reach an external system.
///
/// One enum rather than a tuple of two, so a caller cannot match the broker
/// refusal and forget the journal one.
#[derive(Debug)]
#[non_exhaustive]
pub enum DispatchError {
    /// The environment refused to authorize the attempt.
    Broker(BrokerError),
    /// The journal refused to record it.
    Journal(JournalError),
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Broker(ref cause) => write!(f, "dispatch refused by the broker: {cause}"),
            Self::Journal(ref cause) => write!(f, "dispatch refused by the journal: {cause}"),
        }
    }
}

impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Broker(ref cause) => Some(cause),
            Self::Journal(ref cause) => Some(cause),
        }
    }
}

impl From<BrokerError> for DispatchError {
    fn from(cause: BrokerError) -> Self {
        Self::Broker(cause)
    }
}

impl From<JournalError> for DispatchError {
    fn from(cause: JournalError) -> Self {
        Self::Journal(cause)
    }
}

/// An attempt that has been authorized and durably recorded, and may now be
/// handed to its environment.
///
/// Holding one of these is the only way to reach the handoff, which is how the
/// required order stops being a convention: authority is checked first, the
/// `DispatchPrepared` append lands second, and only then does anything exist to
/// hand over.
///
/// The warrant it carries was checked when it was minted, which is an instant
/// and not a guarantee. The handoff path must present it to
/// [`Broker::revalidate`] immediately before handing over, because a replacement
/// landing in between would otherwise leave this preparation dispatchable.
#[derive(Debug)]
pub struct Prepared {
    /// Authority for the environment generation the attempt was prepared
    /// against.
    authority: Authority,
    /// The committed journal position for the `DispatchPrepared` append.
    ack: DurableAck,
}

impl Prepared {
    /// Authority for the handoff.
    #[must_use]
    pub const fn authority(&self) -> &Authority {
        &self.authority
    }

    /// The committed position of the `DispatchPrepared` append.
    #[must_use]
    pub const fn ack(&self) -> DurableAck {
        self.ack
    }

    /// Spend the preparation, taking the authority and the acknowledgment.
    #[must_use]
    pub fn into_parts(self) -> (Authority, DurableAck) {
        (self.authority, self.ack)
    }
}

/// Authorize `key` against the current environment generation, then append
/// `DispatchPrepared` for it.
///
/// The order is the durable half of RQ-007: authority is obtained and the
/// attempt recorded *before* any bytes exist to hand over, so a crash after this
/// returns leaves a committed `DispatchPrepared` with no outcome, which recovery
/// reads back as an unknown rather than as a never-sent.
///
/// A refusal from either half leaves the journal unchanged. In particular a
/// superseded key never reaches the append, so a fenced command cannot become a
/// recorded attempt.
///
/// # Errors
///
/// [`DispatchError::Broker`] when the environment refuses, and
/// [`DispatchError::Journal`] when the append is refused. The append is also
/// where a second `DispatchPrepared` for one attempt is refused, so a retry that
/// reuses an `AttemptId` fails here rather than dispatching twice.
pub fn prepare_dispatch(
    broker: &Broker,
    journal: &mut impl EffectJournal,
    key: EffectKey,
) -> Result<Prepared, DispatchError> {
    let authority = broker.authorize(key)?;
    let tail = journal.tail();
    let ack = journal.compare_and_append(tail, &EffectEvent::DispatchPrepared { key })?;
    Ok(Prepared { authority, ack })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{ActionDigest, ActionId, AttemptId, EnvironmentId, FlowRevision, RunId};
    use crate::journal::{EffectEvidence, MemoryJournal};
    use std::collections::HashMap;

    const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
    const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    const OTHER_ENV: &str = "5152535455565758595a5b5c5d5e5f60";
    const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

    /// The crate refuses a panicking path anywhere, tests included, so a test
    /// that needs a parsed value returns `Result` and propagates.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A key for the shared run and action, on the named environment and
    /// generation.
    fn key_on(env: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
        Ok(EffectKey::new(
            RunId::from_hex(RUN)?,
            ActionId::from_hex(ACTION)?,
            AttemptId::from_decimal("1")?,
            FlowRevision::from_tagged("blake3_256", FLOW_HEX)?,
            ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?,
            EnvironmentId::from_hex(env)?,
            EnvironmentEpoch::from_decimal(epoch)?,
        ))
    }

    /// The environment id most of these tests use.
    fn env() -> Result<EnvironmentId, Box<dyn std::error::Error>> {
        Ok(EnvironmentId::from_hex(ENV)?)
    }

    /// A key on the shared environment at the named generation.
    fn key(epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
        key_on(ENV, epoch)
    }

    /// Walk an attempt to the point where a dispatch may be prepared.
    fn admitted(
        journal: &mut MemoryJournal,
        key: EffectKey,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tail = journal.tail();
        journal.compare_and_append(tail, &EffectEvent::IntentAdmitted { key })?;
        Ok(())
    }

    #[test]
    fn register_takes_the_first_generation() -> TestResult {
        let mut broker = Broker::new();
        let epoch = broker.register(env()?)?;
        assert_eq!(broker.epoch(env()?), Some(epoch));
        assert!(broker.is_open(env()?));
        Ok(())
    }

    #[test]
    fn a_second_register_of_one_environment_is_refused() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let refused = broker.register(env()?);
        assert!(
            matches!(refused, Err(BrokerError::AlreadyRegistered { .. })),
            "a double register must not silently reset anything"
        );
        assert_eq!(broker.epoch(env()?), Some(first));
        Ok(())
    }

    #[test]
    fn a_current_generation_authorizes() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let authority = broker.authorize(key(&first.get().to_string())?)?;
        assert_eq!(authority.environment(), env()?);
        assert_eq!(authority.epoch(), first);
        Ok(())
    }

    #[test]
    fn an_unknown_environment_is_refused() -> TestResult {
        let broker = Broker::new();
        let refused = broker.authorize(key("1")?);
        assert!(matches!(
            refused,
            Err(BrokerError::UnknownEnvironment { .. })
        ));
        Ok(())
    }

    #[test]
    fn replacement_supersedes_a_command_prepared_before_it() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let stale = key(&first.get().to_string())?;
        assert!(broker.authorize(stale).is_ok());

        let second = broker.replace(env()?)?;
        assert_ne!(second, first);

        match broker.authorize(stale) {
            Err(BrokerError::Superseded {
                environment,
                presented,
                current,
            }) => {
                assert_eq!(environment, env()?);
                assert_eq!(presented, first);
                assert_eq!(current, second);
            }
            Err(other) => return Err(format!("expected a superseded refusal, got {other}").into()),
            Ok(authority) => {
                return Err(format!(
                    "a command from a replaced generation must not be authorized, got {authority}"
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn a_generation_the_broker_never_issued_is_not_reported_as_stale() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let ahead = EnvironmentEpoch::new(NonZeroU64::new(first.get() + 1).ok_or("increment")?);
        let forged = key(&ahead.get().to_string())?;

        match broker.authorize(forged) {
            Err(BrokerError::NeverIssued { current, .. }) => assert_eq!(current, first),
            Err(other) => {
                return Err(format!("expected a never-issued refusal, got {other}").into());
            }
            Ok(authority) => {
                return Err(format!(
                    "a generation the broker never issued must not authorize, got {authority}"
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn a_warrant_minted_before_a_replacement_is_refused_at_the_boundary() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let warrant = broker.authorize(key(&first.get().to_string())?)?;
        assert!(broker.revalidate(&warrant).is_ok());

        // The warrant was minted legitimately. A replacement between the mint
        // and the handoff is what makes it stale, and the boundary check is the
        // only thing that catches it.
        let second = broker.replace(env()?)?;

        match broker.revalidate(&warrant) {
            Err(BrokerError::Superseded {
                presented, current, ..
            }) => {
                assert_eq!(presented, first);
                assert_eq!(current, second);
            }
            Err(other) => return Err(format!("expected a superseded refusal, got {other}").into()),
            Ok(()) => {
                return Err("a warrant from before the replacement must not revalidate".into());
            }
        }
        Ok(())
    }

    #[test]
    fn a_warrant_is_refused_at_the_boundary_once_the_environment_closes() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let warrant = broker.authorize(key(&first.get().to_string())?)?;
        broker.close(env()?)?;
        assert!(matches!(
            broker.revalidate(&warrant),
            Err(BrokerError::Closed { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_closed_environment_mints_nothing() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let live = key(&first.get().to_string())?;
        broker.close(env()?)?;

        assert!(!broker.is_open(env()?));
        assert!(matches!(
            broker.authorize(live),
            Err(BrokerError::Closed { .. })
        ));
        assert!(matches!(
            broker.replace(env()?),
            Err(BrokerError::Closed { .. })
        ));
        Ok(())
    }

    #[test]
    fn closing_twice_is_allowed() -> TestResult {
        let mut broker = Broker::new();
        broker.register(env()?)?;
        broker.close(env()?)?;
        broker.close(env()?)?;
        Ok(())
    }

    #[test]
    fn closing_an_unknown_environment_is_refused() -> TestResult {
        let mut broker = Broker::new();
        assert!(matches!(
            broker.close(env()?),
            Err(BrokerError::UnknownEnvironment { .. })
        ));
        Ok(())
    }

    #[test]
    fn an_exhausted_generation_space_is_named_rather_than_wrapped() -> TestResult {
        let id = env()?;
        let last = EnvironmentEpoch::new(NonZeroU64::MAX);
        let mut broker = Broker {
            environments: HashMap::from([(
                id,
                Environment {
                    epoch: last,
                    open: true,
                },
            )]),
        };
        assert!(matches!(
            broker.replace(id),
            Err(BrokerError::Exhausted { .. })
        ));
        assert_eq!(broker.epoch(id), Some(last));
        Ok(())
    }

    #[test]
    fn one_broker_does_not_authorize_another_environments_key() -> TestResult {
        let mut broker = Broker::new();
        broker.register(env()?)?;
        let other = key_on(OTHER_ENV, "1")?;
        assert!(matches!(
            broker.authorize(other),
            Err(BrokerError::UnknownEnvironment { .. })
        ));
        Ok(())
    }

    #[test]
    fn prepare_dispatch_records_the_attempt_it_authorized() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let key = key(&first.get().to_string())?;
        let mut journal = MemoryJournal::new();
        admitted(&mut journal, key)?;

        let prepared = prepare_dispatch(&broker, &mut journal, key)?;
        assert_eq!(prepared.authority().epoch(), first);
        assert_eq!(prepared.ack().position(), journal.tail());
        assert_eq!(journal.committed().len(), 2);
        Ok(())
    }

    #[test]
    fn a_superseded_key_never_reaches_the_journal() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let stale = key(&first.get().to_string())?;
        let mut journal = MemoryJournal::new();
        admitted(&mut journal, stale)?;
        let before = journal.tail();

        broker.replace(env()?)?;

        let refused = prepare_dispatch(&broker, &mut journal, stale);
        assert!(
            matches!(
                refused,
                Err(DispatchError::Broker(BrokerError::Superseded { .. }))
            ),
            "a fenced command must be refused before it can be recorded"
        );
        assert_eq!(journal.tail(), before);
        assert_eq!(journal.committed().len(), 1);
        Ok(())
    }

    #[test]
    fn a_second_preparation_of_one_attempt_is_refused_by_the_ladder() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let key = key(&first.get().to_string())?;
        let mut journal = MemoryJournal::new();
        admitted(&mut journal, key)?;

        let first_prepare = prepare_dispatch(&broker, &mut journal, key)?;
        let spent = first_prepare.into_parts();

        let refused = prepare_dispatch(&broker, &mut journal, key);
        assert!(
            matches!(
                refused,
                Err(DispatchError::Journal(JournalError::OutOfOrder { .. }))
            ),
            "the environment still authorizes, and the journal is what refuses the resend"
        );
        assert_eq!(spent.1.position(), journal.tail());
        assert_eq!(journal.committed().len(), 2);
        Ok(())
    }

    #[test]
    fn an_unadmitted_attempt_is_refused_even_with_authority() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let key = key(&first.get().to_string())?;
        let mut journal = MemoryJournal::new();

        // The environment would authorize this attempt: the generation is
        // current. The order still refuses it, because nothing was admitted.
        assert!(broker.authorize(key).is_ok());
        let refused = prepare_dispatch(&broker, &mut journal, key);
        assert!(matches!(
            refused,
            Err(DispatchError::Journal(JournalError::OutOfOrder { .. }))
        ));
        assert!(journal.committed().is_empty());
        Ok(())
    }

    #[test]
    fn a_stale_controller_cannot_append_after_recovery() -> TestResult {
        let mut broker = Broker::new();
        let first = broker.register(env()?)?;
        let key = key(&first.get().to_string())?;
        let mut journal = MemoryJournal::new();
        admitted(&mut journal, key)?;

        // A second controller reads the tail before the first appends.
        let stale_tail = journal.tail();
        prepare_dispatch(&broker, &mut journal, key)?;

        let late = journal.compare_and_append(
            stale_tail,
            &EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            },
        );
        assert!(
            matches!(late, Err(JournalError::TailMismatch { .. })),
            "the tail check is what stops the second controller writing over the first"
        );
        Ok(())
    }
}
