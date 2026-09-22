//! The four verbs on a `bevy_ecs` substrate.
//!
//! [`Bot`](crate::Bot) executes a spec by polling every source on every tick and
//! evaluating every condition on the freshly polled value. That is correct and
//! it re-derives an answer it already had: a source that did not move is
//! evaluated anyway. This module keeps the same four verbs and the same
//! `(condition, action)` tuples, and moves the *execution* onto an ECS where the
//! condition is bevy's own change detection.
//!
//! # The mapping
//!
//! | Verb | On this substrate |
//! |---|---|
//! | `GrantSet` | the `Grants` resource: one authority per world |
//! | `Observe` | a `Chains` entry, polled by the `observe_fold` system |
//! | `Evaluate::check` | `Changed<Revision>` on the source entity |
//! | `Execute::execute_action` | the `fire_plan` system, whose effects run between schedule steps |
//! | `Bot::tick()` | [`EcsBot::tick_async`], one `Schedule::run` between two awaited phases |
//!
//! # One execution path, and who drives it
//!
//! A tick is four phases, and only the middle one is a synchronous schedule
//! step:
//!
//! 1. **observe** — every source is polled, in bounded waves, awaited on the
//!    caller's executor.
//! 2. **decide** — one `Schedule::run` folds those observations into the world
//!    (committing each value and bumping `Revision` where it moved) and records
//!    the effect program for this tick.
//! 3. **act** — the recorded effects run in declaration order, awaited on the
//!    caller's executor.
//! 4. **report** — the count and the first failure are written back.
//!
//! [`EcsBot::tick_async`] is the whole of it, and it is the way to run a bot
//! from async code: the verb futures are awaited *by* the caller's runtime, so
//! a source that waits on a timer, a socket, or a sibling task has a driver
//! making progress underneath it. [`EcsBot::tick`] is a synchronous adapter
//! over the same four phases for verbs that need no reactor; it is refused,
//! not hung, when an async runtime is already driving the calling thread.
//!
//! # One semantic delta, stated rather than discovered
//!
//! [`Bot::tick`](crate::Bot::tick) evaluates a condition on **every** tick.
//! `EcsBot::tick` evaluates it only on a tick where the observed value
//! **moved**. For a condition that stays true ("status is 500" while the
//! endpoint stays down) the first fires on every tick and the second fires
//! once, on the transition. That is the point of the substrate, and it is a
//! behaviour change rather than a re-implementation: a bot that must act on a
//! *held* state belongs on `Bot`, and a bot that acts on a *transition* belongs
//! here.
//!
//! # Work outlives the change filter
//!
//! `Changed<Revision>` says a chain is *eligible*; it is not a record of what
//! still has to happen. Committing the revision and then failing halfway marks
//! the source handled, so the entries the failure skipped are never reached on
//! an unchanged source, and a failure in the first chain stops every later
//! chain as well, because the loop that stops is outside the per-chain one.
//!
//! Eligible work therefore lives in its own structure — the ledger — keyed by
//! `(chain, entry)` and by the revision that made it due, and `fire` walks
//! *that* rather than the change set. The filter is consulted only to open a
//! transition; a chain with work outstanding is walked on every tick until the
//! work is settled, whether or not its source moves. One transition per chain
//! at a time: a source that moves again while its work is outstanding joins the
//! transition in progress, because the observed value is the only one the
//! substrate keeps (`Box<dyn Any>` is compared, not cloned) and a second
//! transition would replay effects the first already acknowledged.
//!
//! An entry is never skipped to reach a later one. The walk resumes at the
//! first entry that is not resolved and stops at the first it cannot settle, so
//! an effect that already happened is never replayed to get past a successor —
//! the failure mode where retrying a refused second entry duplicates the first
//! entry's merge. An entry that *may* have happened
//! ([`BotError::EffectIndeterminate`]) is held outright: it is attempted again
//! only when the caller supplies evidence, through `EcsBot::resolve_effect`.
//!
//! A tick is clean only when nothing is left holding a transition. When
//! something is, `EcsBot::tick` reports it — the entry, what is holding it, how
//! many entries are outstanding — so a clean subsequent tick cannot be read as
//! "the transition was handled". An error a source or an action produced keeps
//! precedence over that report, because it carries the typed variant a retry
//! classifier reads; the abandonment it caused is named by `EcsBot::pending`,
//! which lists every entry that is not finished, given-up entries included, with
//! the reason and the source revision. A tick that gives up on work is never
//! clean, and an entry it gave up on is never silently dropped.
//!
//! # A restart continues the record rather than the process
//!
//! [`EcsBot::tick`] is not a boundary the run has to survive: the journal is.
//! Every dispatch appends `IntentAdmitted` and `DispatchPrepared` *before* the
//! effect exists to hand over, so a process that dies mid-attempt leaves a
//! record of an attempt with no outcome behind it — and
//! [`EcsBot::into_journal`] is how a host carries that record into the next
//! process. Assembly folds in every attempt the record names, and the three
//! decisions it takes are deliberately different from one another:
//!
//! - **Outcome unknown** — nothing established whether the bytes arrived, so
//!   the action is *held*. The walk stops at it rather than beginning another
//!   attempt, the tick reports
//!   [`BotError::PendingTransition`](crate::BotError::PendingTransition), and
//!   the key a caller settles by is on the report.
//! - **Outcome landed** — the effect is done for the generation its key names,
//!   so the walk retires the entry rather than dispatching it again. That
//!   retirement is scoped to the generation: a source that moves opens the next
//!   one, and new work, which is the only thing that may legitimately re-run an
//!   acknowledged action.
//! - **Outcome missing or negative** — an admitted intent that was never handed
//!   over, or a delivery that was established not to have happened. Neither
//!   leaves anything held, so the entry is eligible again — under a *new*
//!   attempt identity, because the journal's ladder allows one walk per key and
//!   the attempt the record already holds cannot be minted twice.
//!
//! # Limits
//!
//! A panic that unwinds out of an action's future unwinds out of `fire` and out
//! of `tick` with it. The walk holds the chain's transition while it runs, so
//! that unwind loses the transition for the chain in flight — the entries it had
//! already resolved with it. Nothing in this module catches an unwind, and the
//! in-flight record it writes before an attempt (`EntryState::Unrecorded`) is
//! therefore durable for a tick that returns, not for a thread that panics. A
//! caller that catches an unwind around `tick` must treat that chain's work as
//! unknown rather than as handled.
//!
//! # What is measured, and what is deliberately not done
//!
//! Three findings from the measurement behind this module (`bevy_ecs` 0.19.1,
//! `default-features = false, features = ["std"]`), each with a control:
//!
//! - **`NonSend` is the only route for a verb.** `Component: Send + Sync +
//!   'static` is unconditional in this version and there is no `non_send`
//!   feature to relax it, so a `Box<dyn Any>` (which the verb erasure produces
//!   and which is not `Send`) cannot be a component. The observers, the
//!   entries and the observed values all live in `NonSend` resources, reachable
//!   only from an exclusive system. The crate's non-`Send` contract is
//!   preserved rather than tightened.
//! - **`Changed<T>` is visible to a second exclusive system in the same tick,
//!   and it is precise.** Against an input that holds still the change filter
//!   matched `[2, 0, 2, 0, 2]` (the ticks where the value actually moved),
//!   which is the whole reason a `Revision` marker exists rather than a
//!   "this system ran" flag.
//! - **Effects do not go through `Commands`.** `auto_insert_apply_deferred`
//!   defaults to `true` but inserts the sync point only where some system is
//!   ordered *after* the writer; a `Commands`-writing system with no successor
//!   has its queue dropped under a manually stepped `Schedule::run`, measured
//!   as 0 effects where 3 were expected. Adding a bare `ApplyDeferred` is not a
//!   fix either: unordered relative to the writer it runs first and defers the
//!   effect by a tick, giving 2 of 3. Both are silent. Here the effect is a
//!   direct resource write from an exclusive system, ordered by `.chain()`.
//!
//! # One path
//!
//! This is how a bot executes, not a candidate the caller may decline. There is
//! no `ecs` feature: `Bot` *is* this, and the project's build and test gate
//! compiles and tests it like any other code. A default-off flag would have left
//! it unexercised and unowned.
//!
//! # Build-time validation
//!
//! `EcsObserveBuilder::build` runs `Schedule::initialize()` with
//! `ambiguity_detection: LogLevel::Error`, so a schedule whose systems cannot be
//! totally ordered is a refusal at build rather than a silent misordering at
//! tick. An exclusive system cannot return an error, so a failure at *tick* time
//! is recorded in the `TickError` resource and surfaced by
//! `EcsBot::tick`: the same error ordering `Bot::tick` documents, where the
//! first error in declaration order is returned.

use std::any::Any;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU128};

// `self` is load-bearing: the `Component` and `Resource` derives expand to
// `bevy_ecs::…` paths, so the crate name has to be in scope at the use site even
// though every edge in this crate is written through the `lgwks_deps` facade.
use lgwks_deps::bevy_ecs::{
    self,
    prelude::{Changed, Component, Entity, Resource, World},
    schedule::{
        IntoScheduleConfigs, LogLevel, Schedule, ScheduleBuildSettings, SingleThreadedExecutor,
    },
};
use lgwks_std::hash::{Digest, Hasher};

use super::broker::{Authority, Broker, DispatchError, prepare_dispatch};
use super::cap::{Deficit, Demand, Shortage};
use super::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, FlowRevision,
    Id128,
};
use super::error::{BotError, DispatchCertainty, Escaped, RetryClass};
use super::gate::GrantSet;
use super::journal::{AttemptStatus, EffectEvent, EffectJournal, JournalError, recover};
use super::spec::{ChainEntry, Erased, ObserveAny, Witness, typed_entry};
use super::verb::{Evaluate, Execute, Observe};

// ── The effect path: identity, fencing, and the write-ahead record ─────────

/// The domain separator for an [`ActionId`] derived from a bot's structure.
///
/// A separator rather than a bare concatenation, because the fields hashed into
/// an identity are variable-length strings and a run of them with no framing is
/// a value two different bots can collide on.
const ACTION_ID_DOMAIN: &[u8] = b"lgwks.bot.action-id.v1";

/// The domain separator for an [`ActionDigest`] derived from a bound generation.
const ACTION_DIGEST_DOMAIN: &[u8] = b"lgwks.bot.action-digest.v1";

/// Hash a sequence of byte fields into the estate's content-identity digest.
///
/// The fields are fed in order with no length prefix, which is sound only
/// because every caller's fields are fixed-width or domain-separated; see the
/// two derivation functions below.
fn hash_parts(parts: &[&[u8]]) -> Digest {
    let mut hasher = Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize()
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
    Id128::from_nonzero(NonZeroU128::new(u128::from_be_bytes(wide)).unwrap_or(NonZeroU128::MIN))
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
fn derive_action_id(bot: &str, chain: usize, entry: usize, domain: &str) -> ActionId {
    ActionId::new(id_from_digest(&hash_parts(&[
        ACTION_ID_DOMAIN,
        bot.as_bytes(),
        &chain.to_le_bytes(),
        &entry.to_le_bytes(),
        domain.as_bytes(),
    ])))
}

/// The exact generation of bound input one entry was dispatched from.
///
/// The ECS substrate has no payload bytes to hash: a transition's binding is an
/// erased `Box<dyn Any>` and nothing here asks it to be `Hash`. What it does
/// have is the *revision* — the monotonic marker a source moves, which is
/// exactly when a transition re-binds — so the generation is what this digest
/// binds. Two attempts within one revision share a digest, which is what makes
/// a retry a retry; a new revision gets a new digest, which is what makes a
/// delayed settlement refusable rather than merged.
fn derive_action_digest(
    flow: FlowRevision,
    chain: usize,
    entry: usize,
    revision: u64,
) -> ActionDigest {
    ActionDigest::new(hash_parts(&[
        ACTION_DIGEST_DOMAIN,
        flow.digest().as_bytes(),
        &chain.to_le_bytes(),
        &entry.to_le_bytes(),
        &revision.to_le_bytes(),
    ]))
}

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
    identity: EffectIdentity,
    /// The environment generations a dispatch is fenced against.
    broker: Broker,
    /// Where a dispatch is written down before it leaves the process.
    journal: Box<dyn EffectJournal>,
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
    #[must_use]
    pub fn new(identity: EffectIdentity, broker: Broker, journal: Box<dyn EffectJournal>) -> Self {
        Self {
            identity,
            broker,
            journal,
        }
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

/// The effect path as the kernel holds it, with the recovered state folded in.
///
/// Private, and deliberately not the public [`EffectScope`]: the scope is what
/// a host declares, and this is what the kernel does with it — including the
/// one piece of state the host cannot supply, which is what the journal already
/// says about attempts this bot has not made yet.
struct Effects {
    /// What the host declared.
    scope: EffectScope,
    /// Attempts the journal records as dispatched with no outcome, and which
    /// nothing has settled since.
    ///
    /// Seeded once, at assembly, from [`crate::journal::recover`]. Held as keys
    /// rather than as a count because settling one has to name it, and an
    /// attempt that is only a number in a total cannot be named.
    unsettled: Vec<EffectKey>,
    /// Attempts whose effect is recorded as landed, by the key that landed.
    ///
    /// Written when a recovered attempt is settled `Applied`, and seeded at
    /// assembly from the attempts [`crate::journal::recover`] reads back as
    /// applied or verified, and read by the walk before it dispatches: the
    /// record says the effect landed, so the entry it was about is done *for
    /// the generation the key names*. A source that moves opens a new
    /// generation, and new work, which is the one thing that may legitimately
    /// re-run an acknowledged action.
    ///
    /// Held as whole keys rather than as `(action, revision)` pairs because a
    /// key carries the generation as a digest, and a recovered key has no
    /// integer revision to pair with it — the digest is one-way. Comparing the
    /// digest the run would mint for its current revision against the digests
    /// it holds answers the same question, and it answers it for an
    /// acknowledgement this process never made.
    applied: Vec<EffectKey>,
    /// The latest attempt the journal records for each action it names.
    ///
    /// Seeded once, at assembly, from [`crate::journal::recover`], and read
    /// when the attempt after it is minted. A restart is not a fresh run: the
    /// journal's ladder allows one walk of `IntentAdmitted → DispatchPrepared →
    /// OutcomeObserved → Verified` per key, so minting an attempt the journal
    /// already records reproduces the key it was recorded under and the append
    /// is refused as out of order. Continuing a run therefore means continuing
    /// its attempt sequence, and the number to continue from is in the record.
    attempted: Vec<(ActionId, AttemptId)>,
}

impl Effects {
    /// A scope with nothing recovered yet.
    const fn new(scope: EffectScope) -> Self {
        Self {
            scope,
            unsettled: Vec::new(),
            applied: Vec::new(),
            attempted: Vec::new(),
        }
    }

    /// The host's three run-level facts.
    const fn identity(&self) -> EffectIdentity {
        self.scope.identity()
    }

    /// The environment broker this run's dispatches are fenced against.
    const fn broker(&self) -> &Broker {
        self.scope.broker()
    }

    /// Give the scope's journal back.
    fn into_journal(self) -> Box<dyn EffectJournal> {
        self.scope.into_journal()
    }

    /// Obtain the warrant for `key` and record the attempt as prepared.
    ///
    /// One method rather than two calls at the dispatch site, because the two
    /// borrows it needs — the broker to authorize and the journal to append —
    /// are two fields of one scope, and a call site that reached through
    /// `&self` for one and `&mut self` for the other would have to restructure
    /// itself to prove they do not overlap. The split is stated once, here.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Broker`] when the environment refuses, and
    /// [`DispatchError::Journal`] when the append is refused.
    fn prepare(&mut self, key: EffectKey) -> Result<Authority, DispatchError> {
        let scope = &mut self.scope;
        let (authority, _ack) =
            prepare_dispatch(&scope.broker, &mut *scope.journal, key)?.into_parts();
        Ok(authority)
    }

    /// The generation the broker currently holds for this run's environment.
    ///
    /// `None` when the broker has never been told about the environment. That
    /// is a declared state and not an accident: a broker that owns no
    /// environment mints no authority, which is the refusal
    /// [`prepare_dispatch`] reports as [`BrokerError::UnknownEnvironment`].
    ///
    /// [`BrokerError::UnknownEnvironment`]: crate::broker::BrokerError::UnknownEnvironment
    fn epoch(&self) -> Option<EnvironmentEpoch> {
        self.scope.broker().epoch(self.identity().environment())
    }

    /// The key for one attempt of one entry, or `None` when the broker does not
    /// own the environment this run acts on.
    fn key(
        &self,
        action: ActionId,
        chain: usize,
        entry: usize,
        revision: u64,
        attempt: AttemptId,
    ) -> Option<EffectKey> {
        Some(self.identity().key(
            action,
            attempt,
            derive_action_digest(self.identity().flow(), chain, entry, revision),
            self.epoch()?,
        ))
    }

    /// Append one fact, fencing on the tail this process believes is committed.
    ///
    /// Read-then-append rather than a retained tail: a retained tail is state
    /// this process would have to keep in step with every other writer, and the
    /// journal's own compare-and-append is what refuses a stale belief. A
    /// refusal here is a real one — another controller appended — and it is
    /// reported rather than retried, because the caller's view of the run is
    /// stale.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses.
    fn append(&mut self, event: &EffectEvent) -> Result<(), JournalError> {
        let tail = self.scope.journal().tail();
        self.scope.journal_mut().compare_and_append(tail, event)?;
        Ok(())
    }

    /// Whether an attempt on `action` is recorded as dispatched with no
    /// outcome, and nothing has settled it.
    ///
    /// This is the question a dispatch asks before it resends anything, and the
    /// answer that stops it. An unknown outcome is not permission to try again:
    /// nothing established that the bytes did not arrive.
    fn blocks(&self, action: ActionId) -> bool {
        self.unsettled.iter().any(|key| key.action() == action)
    }

    /// The unsettled key for `action`, when there is one.
    fn unsettled_for(&self, action: ActionId) -> Option<EffectKey> {
        self.unsettled
            .iter()
            .find(|key| key.action() == action)
            .copied()
    }

    /// Whether `action` was acknowledged applied for the generation `revision`
    /// of `(chain, entry)`.
    ///
    /// The comparison is between the digest the run would mint for its current
    /// revision and the digest a held key carries, rather than between two
    /// integers, because the held key may have come from a process that is
    /// gone: the digest is what the record has, so the digest is what the
    /// question is asked in.
    fn applied_in(&self, action: ActionId, chain: usize, entry: usize, revision: u64) -> bool {
        let digest = derive_action_digest(self.identity().flow(), chain, entry, revision);
        self.applied
            .iter()
            .any(|key| key.action() == action && key.digest() == digest)
    }

    /// The attempt the journal already records for `action`, when it names one.
    ///
    /// The highest such attempt rather than the last seen, although the fold
    /// that seeds this reaches them in ascending order: an ordering assumption
    /// that a later record shape could break is not worth the two lines it
    /// saves, and the answer being wrong is a refused append rather than a
    /// loud failure.
    fn recorded_attempt(&self, action: ActionId) -> Option<AttemptId> {
        self.attempted
            .iter()
            .find(|entry| entry.0 == action)
            .map(|entry| entry.1)
    }

    /// Note that the journal already records `attempt` for `action`.
    fn note_journal_attempt(&mut self, action: ActionId, attempt: AttemptId) {
        match self.attempted.iter_mut().find(|entry| entry.0 == action) {
            Some(slot) => {
                if attempt.get() > slot.1.get() {
                    slot.1 = attempt;
                }
            }
            None => self.attempted.push((action, attempt)),
        }
    }

    /// Settle one recovered attempt, appending the outcome that settles it.
    ///
    /// The append lands first: the journal is the record, and a bot that
    /// dropped the key from its own list and then crashed before writing the
    /// outcome would come back with the same unknown it just cleared.
    ///
    /// # Errors
    ///
    /// Whatever the journal refuses.
    fn settle_recovered(
        &mut self,
        key: EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), JournalError> {
        self.append(&EffectEvent::OutcomeObserved { key, evidence })?;
        self.unsettled.retain(|held| *held != key);
        if evidence == EffectEvidence::Applied {
            self.applied.push(key);
        }
        Ok(())
    }
}

impl fmt::Debug for Effects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Effects")
            .field("identity", &self.identity())
            .field("journal", &self.scope.journal().durability())
            .field("unsettled", &self.unsettled.len())
            .field("applied", &self.applied.len())
            .field("attempted", &self.attempted.len())
            .finish()
    }
}

// ── Components: identity and the change marker are separate ────────────────

/// A source's identity in the world.
///
/// Written once at spawn and never again. `docs/bot-on-ecs.md` §4 is the reason
/// this is not fused with the observed value: a component that carries both is
/// marked changed by every poll, and `Changed<T>` degenerates to "always true".
#[derive(Component, Debug, Clone)]
pub(crate) struct SourceId {
    /// The chain's index in declaration order; the key into `Chains` and
    /// `Observed`.
    pub(crate) chain: usize,
    /// The observer's own `domain_id()`, for diagnostics.
    domain: String,
}

impl SourceId {
    /// The observed source's domain identifier (e.g. `"net::endpoint"`).
    pub(crate) fn domain(&self) -> &str {
        &self.domain
    }
}

/// The change marker, bumped only when the observed value actually differs.
///
/// This is the component a condition filters on. It is one component for every
/// domain rather than one per domain, because the *value* is heterogeneous and
/// only the fact of its movement is shared, which keeps the archetype count
/// bounded, as `docs/bot-on-ecs.md` §4 requires.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Revision(u64);

// ── Resources ──────────────────────────────────────────────────────────────

/// The capability proof, as a world resource: one authority per world, no
/// ambient singleton.
#[derive(Resource, Debug)]
struct Grants(GrantSet);

/// Effects fired on the most recent tick.
#[derive(Resource, Debug, Default)]
struct Fired(usize);

/// The first tick-time failure, if any.
///
/// An exclusive system returns `()` and cannot propagate, so the error is
/// parked here and `EcsBot::tick` takes it. This is not a workaround for a
/// missing feature: it is the shape that keeps the failure ordered, because the
/// systems stop at the first error and the driver reports that one.
#[derive(Resource, Debug, Default)]
struct TickError(Option<BotError>);

/// The source entities in chain order.
#[derive(Resource, Debug, Default)]
struct Order(Vec<Entity>);

/// The chains whose source moved on this tick, one flag per chain.
///
/// A world resource rather than a local, because it is per-tick scratch and the
/// point of scratch is that it is allocated once. Built as `vec![false; count]`
/// inside the decision system, this was a fresh zeroed allocation every tick —
/// one of a set of them, added together, that made the schedule's own bookkeeping
/// the larger half of a tick's cost.
#[derive(Default)]
struct Moving(Vec<bool>);

/// The retry policy in force: one authority per world, like `Grants`.
#[derive(Resource, Debug)]
struct Policy(RetryPolicy);

// ── Non-send state: the verbs and the values ───────────────────────────────

/// One observation chain, holding the same erased halves a
/// [`Chain`](crate::Chain) does.
struct EcsChain {
    /// The observer. `Box<dyn ObserveAny>`'s `poll_any` is not `Send`, which is
    /// why this cannot live in a component.
    source: Box<dyn ObserveAny>,
    /// Equality for this observer's output, captured from the `PartialEq` bound
    /// on `EcsBuilder::observe`. Change detection needs to tell "the same
    /// value again" from "a new value", and the erased `Box<dyn Any>` cannot
    /// answer that on its own.
    same: fn(&dyn Any, &dyn Any) -> bool,
    /// The `(condition, action)` tuples, in declaration order.
    entries: Vec<ChainEntry>,
    /// What type this chain's source produces, taken where `S::Output` was
    /// still a type parameter and checked at every rendezvous.
    ///
    /// The chain is the *claim*: it is what the condition and the action were
    /// built against. The value that arrives each tick carries its own witness,
    /// and the rendezvous compares them. Neither half can be checked alone —
    /// both are erased — so the pair is the proof.
    witness: Witness,
}

/// Every chain, in declaration order.
#[derive(Default)]
struct Chains(Vec<EcsChain>);

/// The observed value per chain, from the most recent successful poll.
///
/// Paired to [`Chains`] by index, and by index only: a vector position carries
/// no type. Nothing here proves that `Observed[i]` belongs to `Chains[i]`, which
/// is why each value keeps its [`Witness`] and the fold compares it against the
/// chain's before committing anything.
#[derive(Default)]
struct Observed(Vec<Option<Erased>>);

// ── Staging between the awaited phases and the schedule steps ──────────────
//
// The verb futures are non-`Send` and may need an executor, so they are awaited
// by the driver *outside* the schedule. What crosses into the schedule is
// therefore data, in declaration order, and the two resources below are that
// handover. Both are overwritten wholesale at the point they are consumed, so
// a tick that is dropped between phases leaves nothing behind for the next one.

/// The results of the observation phase, staged for `observe_fold`.
///
/// In chain order, one entry per chain, exactly as `poll_sources` collected
/// them: the fold commits them positionally, so a result that arrived early
/// cannot be committed to the wrong chain.
/// An `Ok(None)` is not "no observation": it is the source's answer that the
/// value it just produced is **equal** to the baseline it was handed, so the
/// substrate keeps the value it already holds. Only `Ok(Some(_))` carries a new
/// payload, and only those are boxed.
#[derive(Default)]
struct Polled(Vec<Result<Option<Erased>, BotError>>);

/// Which chains moved on this tick, one flag per chain.
///
/// Per-tick scratch, like [`Moving`], and a resource for the same reason: it is
/// rebuilt on every tick of every bot and a rebuilt buffer is an allocation.
///
/// Named `Moved` and not `Changed` because `Changed` in this module is bevy's
/// query filter, and shadowing it would silently retarget every
/// `Changed<Revision>` in the file at this struct.
#[derive(Default)]
struct Moved(Vec<bool>);

/// The digest each source last reported when the tick actually polled it, one
/// slot per chain.
///
/// This is the whole of what the substrate remembers about a quiet source. It
/// is `Copy`, it is 16 bytes, and it is the answer to the only question change
/// detection ever asks — "is this the same as what I hold?" — which means a
/// chain whose source offers a fingerprint needs nothing else stored to be
/// watched. No value, no box, no `dyn`, no drop.
///
/// A slot is `None` for a source that does not implement
/// [`Observe::fingerprint`](crate::verb::Observe::fingerprint), for one whose
/// last poll failed, and for one that has never been polled. All three mean the
/// same thing to the tick: it has no cheap answer and must poll.
#[derive(Default)]
struct Fingerprints(Vec<Option<u128>>);

/// Which chains this tick must actually poll, one flag per chain.
///
/// The third piece of per-tick scratch, and a resource for the same reason as
/// the other two: the observation phase reads it twice — once to decide which
/// futures to build, once to pair the results back — and a decision recomputed
/// between those two points could differ from the one the futures were built
/// from.
#[derive(Default)]
struct Polling(Vec<bool>);

/// What the decision phase decided for one entry it reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The condition held, so the entry is attempted.
    ///
    /// It does not say *which* attempt this will be. The number belongs to the
    /// ledger, which is the only thing that can keep it monotonic across a
    /// settlement, and it is handed back by [`Ledger::begin`]. Deriving it here
    /// from [`EntryState`] gave a settled entry the number one all over again,
    /// so two attempts at one address could answer to the same name.
    Attempt,
    /// The condition did not hold, so the entry is recorded as skipped and the
    /// walk moves past it. Recorded rather than dropped: an entry whose
    /// condition is false owes nothing, and saying so is what lets a chain
    /// settle and stop being walked.
    Skip,
}

/// One entry the decision phase reached.
struct Step {
    /// Index into [`Chains`].
    chain: usize,
    /// Index into that chain's `entries`, in declaration order.
    entry: usize,
    /// What the walk decided for it.
    decision: Decision,
}

/// The effect program the decision phase recorded, plus the condition failures
/// it met on the way.
///
/// A failure is carried here rather than parked in [`TickError`] because it
/// happened at a *position* in the walk rather than at the end of it: the
/// [`Step`]s alongside it are exactly the effects the walk reached before that
/// position, and they still run. Whichever failure comes first in walk order is
/// the one the tick reports, which is the ordering the synchronous loop had when
/// it evaluated a condition and ran an action in the same iteration.
///
/// One failure per chain rather than one for the tick, because a condition that
/// cannot be evaluated stops its own chain: the chains after it are still
/// walked, and a failing action earlier in a chain is a more specific report than
/// a failing condition later in the same one. That is also why the failures are
/// indexed rather than ordered — the `chain` field of each [`Step`] is the order
/// they are reported in.
#[derive(Default)]
struct Plan {
    /// The decisions to apply, in declaration order: one chain's entries in
    /// walk order, then the next chain's.
    steps: Vec<Step>,
    /// The condition failure each chain's walk met, indexed by chain. A chain
    /// whose walk met none has `None`, and an action failure recorded by the
    /// driver displaces the condition failure of the chain it happened in.
    failures: Vec<Option<BotError>>,
}

/// Upper bound on sources polled simultaneously by one tick. A source poll may
/// occupy one `spawn_blocking` thread, so this caps the blocking-thread fan-out
/// regardless of how many chains a spec declares. Chains beyond the cap are
/// polled in additional waves.
const MAX_IN_FLIGHT_POLLS: usize = 32;

/// Compare two erased outputs as `S::Output`.
///
/// A downcast that fails is reported as *different* rather than equal: treating
/// an unreadable value as unchanged would silently suppress an effect, and the
/// failure mode this whole substrate is built to avoid is an effect that does
/// not happen with nothing to show for it.
fn same_output<S>(left: &dyn Any, right: &dyn Any) -> bool
where
    S: Observe + 'static,
    S::Output: PartialEq + 'static,
{
    match (
        left.downcast_ref::<S::Output>(),
        right.downcast_ref::<S::Output>(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// The parked error, if the tick has already failed.
fn parked(world: &World) -> bool {
    world
        .get_resource::<TickError>()
        .is_some_and(|error| error.0.is_some())
}

// ── The work ledger: eligible work, separate from change detection ─────────

/// One `(chain, entry)` slot of a chain.
///
/// An address, not an identity, and the distinction is the whole of what
/// [`PendingWork`] exists to carry. A slot is reused by every generation over
/// its chain and by every attempt within one, so two `WorkId`s comparing equal
/// says only that the *address* is the same — never that the work standing
/// there is. It names a place in a report; it does not settle one.
///
/// It is deliberately not constructible outside this crate. That is not what
/// makes settlement safe — a [`PendingWork`] is what does — but a caller that
/// could mint one could name work it never observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkId {
    /// The chain's index, in declaration order.
    chain: usize,
    /// The entry's index within its chain.
    entry: usize,
}

impl WorkId {
    /// The chain's index, in declaration order.
    #[must_use]
    pub const fn chain(self) -> usize {
        self.chain
    }

    /// The entry's index within its chain.
    #[must_use]
    pub const fn entry(self) -> usize {
        self.entry
    }
}

/// Why an entry was given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AbandonReason {
    /// The attempt budget for this entry is spent. The effect definitely did
    /// not happen on any of the attempts made; nothing further is attempted
    /// without evidence, which `EcsBot::resolve_effect` supplies.
    AttemptsExhausted {
        /// How many attempts were made.
        attempts: u32,
    },
    /// The failure is one no retry can fix: a refused capability, a type
    /// mismatch behind an erased verb, a malformed action input. Retrying would
    /// spend an attempt to learn the same answer.
    Terminal,
}

/// What is holding one entry of a transition back.
///
/// The `Running` state the ledger tracks is called [`Self::Unrecorded`] here:
/// at rest — which is the only time a caller can observe it — "an attempt is
/// in flight" and "an attempt began and no outcome was ever recorded" are the
/// same fact, and the second is the one that decides what happens next.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransitionHold {
    /// Never attempted. An entry ahead of it is unresolved, and the chain's
    /// declared order is part of what it means.
    NotStarted,
    /// An attempt began and no outcome was recorded — the tick that started it
    /// ended without one. Held exactly as an unknown outcome is: the effect may
    /// be live, so it is never attempted again without evidence.
    Unrecorded,
    /// An attempt on this entry's action was recorded as dispatched earlier in
    /// this run, and no outcome was ever recorded for it.
    ///
    /// Distinct from [`Self::Unrecorded`], which is an attempt *this* process
    /// began and whose outcome the same process never wrote, and from
    /// [`Self::OutcomeUnknown`], which is an attempt it ran and watched become
    /// indeterminate. This one was found, not made: the bot was assembled
    /// against a journal that already said the bytes may be live, and the
    /// substrate will not begin another attempt on that action until a caller
    /// settles it. The key to settle it with is
    /// [`PendingWork::key`].
    UnsettledByRecovery {
        /// The action whose earlier attempt never settled.
        action: ActionId,
    },
    /// The effect definitely did not happen, and the attempt budget remains.
    Failed {
        /// Attempts made so far, including the failed one.
        attempts: u32,
        /// The budget this entry is allowed.
        budget: u32,
        /// The failure the attempt produced.
        cause: String,
    },
    /// The effect may or may not have happened. Never attempted again without
    /// evidence.
    OutcomeUnknown {
        /// Attempts made so far, including the indeterminate one.
        attempts: u32,
        /// Why the outcome could not be settled.
        cause: String,
    },
    /// Given up on, and reported rather than dropped.
    Abandoned {
        /// Why it was given up on.
        reason: AbandonReason,
        /// The failure that ended it.
        cause: String,
    },
}

impl fmt::Display for TransitionHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self`, so each pattern's type is the enum's own
        // type rather than a reference to it, and the causes are bound by
        // `ref`. They are rendered through `Escaped`, which is what keeps a
        // foreign payload from forging a log record; the pass is idempotent
        // (see `error::Escaped`), so a cause that arrived already escaped is
        // rendered exactly once.
        match *self {
            Self::NotStarted => f.write_str("not attempted: an entry ahead of it is unresolved"),
            Self::Unrecorded => {
                f.write_str("an attempt began and no outcome was recorded: the effect may be live")
            }
            Self::UnsettledByRecovery { action } => write!(
                f,
                "may have happened: an earlier attempt on action {action} was dispatched \
                 and nothing recorded its outcome, so nothing is sent again until it is \
                 settled"
            ),
            Self::Failed {
                attempts,
                budget,
                ref cause,
            } => write!(
                f,
                "did not happen: attempt {attempts} of {budget} failed with {}",
                Escaped(cause)
            ),
            Self::OutcomeUnknown {
                attempts,
                ref cause,
            } => write!(
                f,
                "may have happened: attempt {attempts} was indeterminate ({})",
                Escaped(cause)
            ),
            Self::Abandoned { reason, ref cause } => match reason {
                AbandonReason::AttemptsExhausted { attempts } => write!(
                    f,
                    "given up on after {attempts} attempts: {}",
                    Escaped(cause)
                ),
                AbandonReason::Terminal => {
                    write!(f, "given up on, no retry can fix: {}", Escaped(cause))
                }
            },
        }
    }
}

/// One entry of a transition that is not finished, and the attempt it is about.
///
/// This is the report a caller reads, and [`Self::key`] is what a settlement is
/// made against. The two are one value because they are two halves of one fact:
/// the address ([`WorkId`]) tells a caller *where* to look in its own books and
/// the key tells the substrate *which* attempt, over *which* observed input, the
/// evidence is about.
///
/// The key is why settlement takes this value's key rather than the slot. A
/// chain retries within one generation, so `NotApplied` for the first attempt
/// makes the entry eligible again and a second attempt runs against the same
/// address over the same binding. A report about the first attempt that is
/// *delivered twice* — which settlement is explicitly designed to tolerate,
/// because a caller that never saw its first delivery acknowledged has to be
/// able to repeat it — would otherwise land on the second attempt and make an
/// attempt whose effect may be live eligible to run a third time. Naming the
/// attempt is what separates "send that again" from "that was about work that is
/// over".
///
/// [`Self::key`] is `None` for an entry that has never been attempted. There is
/// then nothing for evidence to settle, and the absence is the same absence
/// rather than a zero attempt that would have to be special-cased at every
/// settlement site.
///
/// The fields are private and there is no public constructor, so a caller can
/// only ever settle work it read from [`EcsBot::pending`]. That is a seal, not
/// a formality: it makes "settle an attempt I invented" unrepresentable rather
/// than merely refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWork {
    /// Which entry this is.
    id: WorkId,
    /// The attempt this entry is on, and the four facts that name it.
    ///
    /// `None` when the entry has not been attempted, which is the only state
    /// that has no attempt identity to hand out.
    ///
    /// Boxed because a key is a fixed-width identity — seven fields, 128 bytes
    /// — and this type is embedded in [`BotError::PendingTransition`], which is
    /// returned by value from every tick that leaves work unfinished. Held
    /// inline, one key would put that error over the size at which the
    /// `result_large_err` lint fires for every caller in the workspace; the
    /// indirection costs one allocation on a path that already produces a
    /// report, and buys back the error's size for all of them.
    ///
    /// [`BotError::PendingTransition`]: crate::BotError::PendingTransition
    key: Option<Box<EffectKey>>,
    /// What is holding it.
    hold: TransitionHold,
}

impl PendingWork {
    /// Which entry this is.
    ///
    /// The address in the bot's own declarations, not an identity: a slot is
    /// reused by every generation over its chain, so two reports comparing
    /// equal here say only that the *place* is the same. [`Self::key`] is what
    /// names the work.
    #[must_use]
    pub const fn id(&self) -> WorkId {
        self.id
    }

    /// The attempt this entry is on, when there has been one.
    ///
    /// This is the value [`EcsBot::resolve_effect`] takes. It carries the run,
    /// the action, the attempt, the flow, the binding, the environment and the
    /// generation, and all seven are compared before anything is read or
    /// written — which is what makes the delivery safe to repeat.
    ///
    /// `None` means nothing has been attempted on this entry yet, so there is
    /// no attempt for evidence to be about.
    #[must_use]
    pub fn key(&self) -> Option<EffectKey> {
        self.key.as_deref().copied()
    }

    /// What is holding it back.
    #[must_use]
    pub const fn hold(&self) -> &TransitionHold {
        &self.hold
    }
}

impl fmt::Display for PendingWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Written in two passes rather than through an intermediate `String`:
        // this renders on every tick that leaves work unfinished, and a
        // diagnostic that allocates twice to say which entry is stuck is a cost
        // paid on the path that is already the unhappy one.
        write!(f, "chain {} entry {} (", self.id.chain(), self.id.entry())?;
        match self.key.as_deref() {
            Some(key) => write!(
                f,
                "attempt {}, binding {}",
                key.attempt().get(),
                key.digest()
            )?,
            None => f.write_str("no attempt yet")?,
        }
        write!(f, "): {}", self.hold)
    }
}

/// What a caller knows about an effect the substrate could not settle.
///
/// Two arms, because there are two facts a caller can establish, and the
/// decision that matters — attempt it again or not — follows from which one
/// they are. "Unknown and staying unknown" is not evidence and has no arm: an
/// entry in that state stays reported, which is the honest outcome.
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
#[rkyv(attr(non_exhaustive), crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
#[non_exhaustive]
pub enum EffectEvidence {
    /// The effect happened. The entry is acknowledged and never attempted
    /// again.
    Applied,
    /// The effect did not happen. This is the evidence a retry requires: the
    /// entry becomes eligible for an attempt again, with a fresh budget,
    /// because a new fact is not a repeat of the attempt that failed.
    NotApplied,
}

/// Renders the fact the caller established, not the enum arm, because these
/// appear in refusal messages a person reads to decide whether their own
/// observation was wrong: `applied` and `not applied` are what they were asked
/// for, and `Applied`/`NotApplied` would make them translate.
///
/// Lives here rather than in `error.rs` for the same reason
/// [`PendingWork`]'s does — the type owns its rendering, so a later variant
/// cannot be interpolated anywhere unhandled.
impl fmt::Display for EffectEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Matched through a dereference rather than through the reference: the
        // type is `Copy`, and `clippy::pattern_type_mismatch` is forbidden in
        // this workspace.
        f.write_str(match *self {
            Self::Applied => "applied",
            Self::NotApplied => "not applied",
        })
    }
}

/// How many times an entry whose effect definitely did not happen is attempted.
///
/// A policy rather than a constant, because the right number is a property of
/// the action: a merge that transiently refuses wants several attempts, a
/// process launch wants one. There is no measured basis for a default, so
/// [`Self::DEFAULT`] is a declared guess and is named as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per entry, the first included. Non-zero: an entry that may
    /// never be attempted is not a policy, it is pre-abandoned work.
    max_attempts: NonZeroU32,
}

impl RetryPolicy {
    /// One attempt per entry: the first definite failure abandons it.
    pub const ONE_ATTEMPT: Self = Self {
        max_attempts: NonZeroU32::MIN,
    };

    /// The declared default: three attempts per entry, counting the first.
    pub const DEFAULT: Self = Self {
        // Total rather than panicking: a `const` has no error channel and
        // `unwrap` is refused crate-wide. The `None` arm is unreachable, since
        // three is not zero.
        max_attempts: match NonZeroU32::new(3) {
            Some(three) => three,
            None => NonZeroU32::MIN,
        },
    };

    /// A policy allowing `max_attempts` attempts per entry.
    #[must_use]
    pub const fn new(max_attempts: NonZeroU32) -> Self {
        Self { max_attempts }
    }

    /// Attempts allowed per entry.
    #[must_use]
    pub const fn max_attempts(self) -> u32 {
        self.max_attempts.get()
    }
}

/// The state of one entry within a transition.
#[derive(Debug, Clone)]
enum EntryState {
    /// Never attempted. An entry ahead of it may be holding it back.
    NotStarted,
    /// An attempt is under way, or was begun and its outcome never recorded.
    /// This is the issue's `Running`, named for the fact that survives the
    /// call.
    Unrecorded,
    /// The action ran and returned.
    Succeeded,
    /// The condition was false when it was evaluated against the value.
    Skipped,
    /// The effect definitely did not happen ([`BotError::DomainError`]), and
    /// attempts remain in the budget.
    DefinitelyFailed {
        /// Attempts made so far.
        attempts: u32,
        /// The failure.
        cause: String,
    },
    /// The effect may or may not have happened
    /// ([`BotError::EffectIndeterminate`]).
    OutcomeUnknown {
        /// Attempts made so far.
        attempts: u32,
        /// Why the outcome could not be settled.
        cause: String,
    },
    /// Given up on, and reported.
    Abandoned {
        /// Why.
        reason: AbandonReason,
        /// The failure that ended it.
        cause: String,
    },
}

impl EntryState {
    /// Whether the entry still needs something: an attempt, evidence, or the
    /// entry ahead of it to be resolved.
    fn is_open(&self) -> bool {
        matches!(
            self,
            Self::NotStarted
                | Self::Unrecorded
                | Self::DefinitelyFailed { .. }
                | Self::OutcomeUnknown { .. }
        )
    }

    /// Whether the entry was given up on: terminal for this transition, and
    /// reported rather than dropped.
    fn is_abandoned(&self) -> bool {
        matches!(self, Self::Abandoned { .. })
    }

    /// How this entry reads to a caller, `None` when it is resolved and has
    /// nothing to report.
    fn hold(&self, budget: u32) -> Option<TransitionHold> {
        match *self {
            Self::Succeeded | Self::Skipped => None,
            Self::NotStarted => Some(TransitionHold::NotStarted),
            Self::Unrecorded => Some(TransitionHold::Unrecorded),
            Self::DefinitelyFailed {
                attempts,
                ref cause,
            } => Some(TransitionHold::Failed {
                attempts,
                budget,
                cause: cause.clone(),
            }),
            Self::OutcomeUnknown {
                attempts,
                ref cause,
            } => Some(TransitionHold::OutcomeUnknown {
                attempts,
                cause: cause.clone(),
            }),
            Self::Abandoned { reason, ref cause } => Some(TransitionHold::Abandoned {
                reason,
                cause: cause.clone(),
            }),
        }
    }
}

/// What the ledger knows about the attempts made on one entry of one
/// generation.
///
/// Kept beside [`EntryState`] rather than inside it, because the state is
/// overwritten by every settlement and every failure and this is not: an entry
/// settled `NotApplied` and attempted again is a *later* attempt at the same
/// address, and the ordinal is what says so.
#[derive(Debug, Clone, Copy, Default)]
struct AttemptRecord {
    /// The latest attempt begun on this entry in this generation, or `None`
    /// while none has been.
    ///
    /// Monotonic by construction: only [`Ledger::begin_attempt`] moves it, and
    /// nothing resets it while the transition stands. It is the entry's attempt
    /// identity, and it is what a [`PendingWork`] hands out — an identity
    /// rather than a count, so the number a caller is handed and the number a
    /// settlement compares against are the same value rather than two
    /// derivations of one.
    begun: Option<AttemptId>,
    /// The attempt the latest settlement was about, and what it said.
    ///
    /// The ordinal is stored rather than assumed to be the latest, because a
    /// settlement is what *permits* the next attempt: recording it against
    /// whichever attempt happened to be current when a repeat arrived is how a
    /// report about one attempt comes to authorise another.
    settled: Option<SettledAttempt>,
}

/// The attempt one settlement was about, and the evidence it carried.
#[derive(Debug, Clone, Copy)]
struct SettledAttempt {
    /// The attempt the evidence was submitted for.
    attempt: AttemptId,
    /// What the evidence said.
    evidence: EffectEvidence,
}

/// The work of one source transition: one state per entry of its chain.
///
/// Deliberately neither `Clone` nor a derived `Debug`: it owns the observed
/// payload it is bound to, and that payload is erased. Copying a transition
/// would quietly copy the payload with it — the very copy the sources are not
/// required to support — and printing one would demand `Debug` of a type the
/// source never promised it for. The manual [`fmt::Debug`] below renders the
/// binding as its presence, which is the part a reader of a log wants.
struct Transition {
    /// The revision that opened it.
    revision: u64,
    /// One state per entry of the chain, in declaration order.
    entries: Vec<EntryState>,
    /// What the ledger knows about the attempts made on each entry, for the
    /// generation [`Self::revision`] names, indexed as [`Self::entries`] is.
    ///
    /// Two facts live here, and each answers a question [`EntryState`] cannot.
    /// The settlement is what makes evidence idempotent and a contradiction
    /// refusable: `Applied` and an attempt that returned `Ok` both land on
    /// [`EntryState::Succeeded`], and `NotApplied` and an entry that has not
    /// been reached both land on [`EntryState::NotStarted`], so without it a
    /// caller whose first delivery was ambiguous cannot safely repeat it. The
    /// attempt ordinal is what makes the entry's attempts *numerable*: a
    /// settled entry walks back to [`EntryState::NotStarted`], which carries no
    /// count, so a number read off the state alone would restart at one and two
    /// different attempts would answer to the same name.
    ///
    /// Per generation: both are reset when a transition is opened, because a
    /// settlement is a statement about one attempt within one generation and
    /// neither fact carries into the next.
    attempts: Vec<AttemptRecord>,
    /// The observed payload this transition is bound to.
    ///
    /// The binding is the whole point of the field. One transition is one
    /// generation of work over a chain, and every entry of it — the conditions
    /// that are evaluated and the actions that are run — must see the value the
    /// generation was opened under. Reading the newest observation instead
    /// splits a transition across two inputs: an entry that was acknowledged
    /// against the old value is retried against the new one, and the run ends
    /// up reporting an effect of the new value that was actually produced from
    /// the old.
    ///
    /// It is *moved* here out of the observation slot rather than copied from
    /// it, which is why nothing in this module asks a source's `Output` to be
    /// `Clone`. The observation slot is empty for exactly as long as a live
    /// transition owns the value, and [`observe_fold`] reads the binding back
    /// when it needs to know what the chain last acted on.
    ///
    /// `None` only for a transition opened while nothing had been observed —
    /// there is then no payload to bind, and the entries are held rather than
    /// evaluated against a value nobody read.
    value: Option<Erased>,
}

impl fmt::Debug for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transition")
            .field("revision", &self.revision)
            .field("entries", &self.entries)
            .field("attempts", &self.attempts)
            .field("bound", &self.value.is_some())
            .finish()
    }
}

impl Transition {
    /// The latest attempt begun on `entry`, or `None` when this transition has
    /// no such entry or none has been begun.
    ///
    /// Read through here rather than off [`EntryState`] so that the identity a
    /// caller is handed and the identity settlement checks are the same one: a
    /// settled entry walks back to `NotStarted`, which carries no attempt, and
    /// an identity derived from the state would restart at the first one.
    fn attempt_of(&self, entry: usize) -> Option<AttemptId> {
        self.attempts.get(entry).and_then(|record| record.begun)
    }

    /// A transition with every entry outstanding, bound to `value`.
    fn opened(revision: u64, entries: usize, value: Option<Erased>) -> Self {
        Self {
            revision,
            entries: vec![EntryState::NotStarted; entries],
            attempts: vec![AttemptRecord::default(); entries],
            value,
        }
    }

    /// Continue into a new revision, from a transition with nothing open.
    ///
    /// Every entry is outstanding again for the new value, *except* the ones
    /// given up on: those are terminal until evidence revives them, and
    /// carrying their record forward is what keeps a lost effect reported
    /// instead of silently dropped the moment the source moves.
    fn resumed(revision: u64, previous: &Self, value: Option<Erased>) -> Self {
        Self {
            revision,
            entries: previous
                .entries
                .iter()
                .map(|state| match *state {
                    EntryState::Abandoned { reason, ref cause } => EntryState::Abandoned {
                        reason,
                        cause: cause.clone(),
                    },
                    _ => EntryState::NotStarted,
                })
                .collect(),
            // Attempts as well as settlements: both are statements about one
            // attempt within one generation, and this is a new generation.
            attempts: vec![AttemptRecord::default(); previous.entries.len()],
            value,
        }
    }

    /// Whether any entry is still unresolved.
    fn has_open(&self) -> bool {
        self.entries.iter().any(EntryState::is_open)
    }

    /// Whether the ledger must keep this transition: it has work to do, or a
    /// fact to report.
    fn is_retained(&self) -> bool {
        self.entries
            .iter()
            .any(|state| state.is_open() || state.is_abandoned())
    }
}

/// What a settlement did to the ledger.
///
/// Six answers, because a caller's next move differs for each: [`Decided`] and
/// [`Duplicate`] are both success, and the other four are four distinct
/// refusals that must not be collapsed into one. Only [`NoSuchWork`] means
/// "there is no held effect for this action"; a caller told that about an action
/// it has a `PendingWork` for is being told its evidence is stale or
/// contradictory, which is a different repair.
///
/// Each refusal carries the address it was about, worked out where the ledger
/// already had it. A refusal that made the caller look the entry up again would
/// have to answer what to do when the lookup fails, and there is no honest
/// answer: the ledger just knew, and a second query is a chance for the two
/// answers to differ.
///
/// [`Decided`]: Settled::Decided
/// [`Duplicate`]: Settled::Duplicate
/// [`NoSuchWork`]: Settled::NoSuchWork
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// The key's generation's entry was outstanding, and this evidence is now
    /// its outcome.
    Decided,
    /// The key's generation was already given exactly this evidence for this
    /// entry. A caller that could not tell whether its first delivery landed
    /// repeats it and must not be punished for the repetition, nor may the
    /// repeat move the entry a second time.
    Duplicate,
    /// The key names a binding that is not the one in the slot now: the work
    /// the caller is reporting on was superseded, and the live transition is
    /// about a different attempt over a different observed input.
    Superseded {
        /// The entry the action is declared on.
        id: WorkId,
        /// The binding the slot holds now.
        current: ActionDigest,
    },
    /// The named attempt is not the one outstanding on that entry: a later
    /// attempt has been begun since the caller read the work.
    ///
    /// Distinct from [`Self::Superseded`], which is about the generation, and
    /// the distinction is the point of carrying an attempt at all. The
    /// generation can be the one the caller named and the attempt still be
    /// gone — that is the retry-inside-one-generation case — and accepting the
    /// report there would apply it to an attempt the caller never observed.
    StaleAttempt {
        /// The entry the action is declared on.
        id: WorkId,
        /// The attempt the evidence was about.
        reported: AttemptId,
        /// The attempt the entry is on now.
        outstanding: AttemptId,
    },
    /// The named generation's entry is settled, and this evidence says the
    /// opposite of what settled it.
    Contradicted {
        /// The entry the action is declared on.
        id: WorkId,
        /// The evidence already recorded for this generation.
        settled: EffectEvidence,
    },
    /// The key has no work to settle: either the action is not declared here at
    /// all, or it is declared at an entry whose outcome is already decided.
    ///
    /// The address is absent only in the first case, which is why it is an
    /// `Option` rather than a fabricated slot: a caller told "no such work, at
    /// chain 0 entry 0" would be reading a place the key never named.
    NoSuchWork {
        /// The entry the action is declared on, when it is declared at all.
        id: Option<WorkId>,
    },
}

/// The eligible work of the bot, keyed by chain.
///
/// One transition per chain, and never one per revision: an entry's work cannot
/// be re-derived from the change filter, and two transitions for one chain
/// would replay an acknowledged effect on the older one to reach an entry the
/// newer one skipped. Bounded by construction — at most one state per entry of
/// the spec, so the ledger cannot grow with the number of ticks.
///
/// A non-send resource rather than a plain one, because a transition owns the
/// observed payload it is bound to and `Box<dyn Any>` is neither `Send` nor
/// `Sync`. The binding travels *inside* the transition deliberately: a second
/// structure holding the payloads would have to be kept in step with this one
/// through every `take`, `put`, `begin`, `skip` and `fail`, and the first path
/// that forgot would silently re-bind a live transition to the wrong value.
#[derive(Debug)]
struct Ledger {
    /// One live transition per chain, in declaration order.
    transitions: Vec<Option<Transition>>,
    /// The action each entry of each chain runs, in declaration order.
    ///
    /// Derived once, at assembly, from the declarations — the bot's name, the
    /// chain's index, the entry's index and the action's own domain — rather
    /// than re-derived per attempt. Two derivations of one identity are two
    /// identities the moment they disagree, and an action identity that drifted
    /// between the key a dispatch was recorded under and the key a settlement
    /// names is a settlement that never lands.
    actions: Vec<Vec<ActionId>>,
    /// The effect path: the identity, the fence and the journal, plus what
    /// recovery found in them.
    effects: Effects,
}

impl Ledger {
    /// A ledger with one idle slot per chain, no work recorded, and the
    /// declared action identities indexed as the chains are.
    fn new(actions: Vec<Vec<ActionId>>, effects: Effects) -> Self {
        Self {
            transitions: (0..actions.len()).map(|_| None).collect(),
            actions,
            effects,
        }
    }

    /// The action one entry runs, when the ledger has that entry.
    fn action_of(&self, id: WorkId) -> Option<ActionId> {
        self.actions.get(id.chain())?.get(id.entry()).copied()
    }

    /// Give the journal back, leaving the ledger unusable.
    ///
    /// The undo of the move `assemble` made, and the only way a record outlives
    /// the bot that wrote it.
    fn into_journal(self) -> Box<dyn EffectJournal> {
        self.effects.into_journal()
    }

    /// The generation a chain's live transition was opened at, when it has one.
    ///
    /// What a caller needs to ask whether an acknowledgement covers the work in
    /// front of it: an `Applied` verdict retires its action for one generation
    /// and no other, so the comparison is between the acknowledgement's
    /// generation and this one.
    fn generation(&self, chain: usize) -> Option<u64> {
        self.transitions
            .get(chain)
            .and_then(Option::as_ref)
            .map(|transition| transition.revision)
    }

    /// The entry an action is declared on, when the ledger has one.
    ///
    /// The action is what a key carries and an address is what the ledger is
    /// indexed by, so this is the translation settlement runs first. It is a
    /// search rather than a map because the declaration is bounded by the spec
    /// — a handful of entries, scanned at most once per settlement and once per
    /// recovery — and a second index kept in step with `actions` is a second
    /// place for the two to disagree.
    fn locate(&self, action: ActionId) -> Option<WorkId> {
        for (chain, entries) in self.actions.iter().enumerate() {
            if let Some(entry) = entries.iter().position(|declared| *declared == action) {
                return Some(WorkId { chain, entry });
            }
        }
        None
    }

    /// Take a chain's transition out, leaving its slot empty. The walk needs it
    /// owned: the ledger is mutated while the chains, the observed values and
    /// the granted authority are borrowed from the same world.
    fn take(&mut self, chain: usize) -> Option<Transition> {
        self.transitions.get_mut(chain).and_then(Option::take)
    }

    /// Put a chain's transition back, `None` when nothing is retained.
    fn put(&mut self, chain: usize, transition: Option<Transition>) {
        if let Some(slot) = self.transitions.get_mut(chain) {
            *slot = transition;
        }
    }

    /// The entry a [`WorkId`] names, when the ledger holds it.
    fn entry_mut(&mut self, id: WorkId) -> Option<&mut EntryState> {
        self.transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
            .and_then(|transition| transition.entries.get_mut(id.entry()))
    }

    /// Record that an attempt on this entry is about to be made, *before* it is
    /// awaited.
    ///
    /// This is the write-ahead half of the ledger, and it is why the driver
    /// writes to the world between awaits at all: a tick dropped while the effect
    /// is in flight leaves this record behind, and "an attempt began and no
    /// outcome was recorded" is held exactly as an unknown outcome is. Without
    /// it, a dropped tick would leave a `NotStarted` entry behind and the next
    /// tick would replay an effect that may already be live.
    ///
    /// `Err` when the entry is not in a state an attempt can start from, when
    /// the action identity is unknown, or when the journal or the broker
    /// refused — which means the world moved behind the schedule's back.
    ///
    /// The two writes happen here, in this order, and neither can be moved
    /// later. `IntentAdmitted` is the decision to act, and `DispatchPrepared` is
    /// the warrant: both are committed *before* the caller builds the future that
    /// reaches the external system. A crash anywhere after this returns leaves a
    /// journal that says "this attempt was prepared and nothing recorded what
    /// became of it", which recovery reads as an unknown rather than as a
    /// never-sent — the exact distinction a duplicate merge turns on.
    ///
    /// The warrant is handed back rather than consumed here because the handoff
    /// is the caller's: it runs after the payload the attempt acts on is in
    /// hand, and it presents the warrant at [`Broker::revalidate`] on the way in.
    fn begin_attempt(&mut self, id: WorkId) -> Result<(EffectKey, Authority), BotError> {
        let no_such_work = || BotError::NoSuchWork { work: id };
        let action = self.action_of(id).ok_or_else(no_such_work)?;
        // Refused before anything is written, and refused for the reason
        // recovery exists: an attempt on this action is recorded as dispatched
        // with no outcome, so the bytes may already be live. Beginning another
        // one is the blind resend, and the guard is here rather than at the top
        // of the walk so it cannot be bypassed by a second caller.
        if self.effects.blocks(action) {
            return Err(BotError::EffectUnsettled { action });
        }
        let transition = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
            .ok_or_else(no_such_work)?;
        let state = transition
            .entries
            .get_mut(id.entry())
            .ok_or_else(no_such_work)?;
        if !matches!(
            *state,
            EntryState::NotStarted | EntryState::DefinitelyFailed { .. }
        ) {
            return Err(no_such_work());
        }
        let record = transition
            .attempts
            .get_mut(id.entry())
            .ok_or_else(no_such_work)?;
        // The attempt identity is minted here and nowhere else, which is what
        // makes it monotonic within the generation: `FIRST` for an entry that
        // has never been attempted, otherwise the successor of the last one. An
        // exhausted chain of successes is refused rather than wrapped, because
        // an identity that came round again would name two attempts.
        //
        // "Never been attempted" means *this run has not attempted it*, and the
        // two are not the same question after a restart. A transition is opened
        // fresh, so its record starts empty however far the journal got, and
        // minting attempt one again would name the attempt the record already
        // holds — which the journal refuses, and which is the refuse-forever
        // dead end a settled `NotApplied` would otherwise lead into. The
        // journal's own latest attempt for the action is therefore the floor,
        // and the successor of that floor is what this mints.
        let attempt = match record.begun {
            Some(previous) => previous
                .checked_next()
                .ok_or(BotError::EffectUnsettled { action })?,
            None => match self.effects.recorded_attempt(action) {
                Some(recorded) => recorded
                    .checked_next()
                    .ok_or(BotError::EffectUnsettled { action })?,
                None => AttemptId::FIRST,
            },
        };
        record.begun = Some(attempt);
        *state = EntryState::Unrecorded;
        let revision = transition.revision;
        let key = self
            .effects
            .key(action, id.chain(), id.entry(), revision, attempt)
            .ok_or(BotError::EffectUnsettled { action })?;
        self.effects
            .append(&EffectEvent::IntentAdmitted { key })
            .map_err(|cause| BotError::EffectRefused {
                cause: DispatchError::Journal(cause),
            })?;
        let authority = self
            .effects
            .prepare(key)
            .map_err(|cause| BotError::EffectRefused { cause })?;
        Ok((key, authority))
    }

    /// Record that the entry's condition did not hold, so it owes nothing.
    fn skip(&mut self, id: WorkId) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(
            *state,
            EntryState::NotStarted | EntryState::DefinitelyFailed { .. }
        ) {
            return false;
        }
        *state = EntryState::Skipped;
        true
    }

    /// Record that the attempt returned without error.
    fn succeed(&mut self, id: WorkId) -> bool {
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        if !matches!(*state, EntryState::Unrecorded) {
            return false;
        }
        *state = EntryState::Succeeded;
        true
    }

    /// Record what a failed attempt means for the entry it was made on.
    ///
    /// The attempt arrives as the identity the key carries rather than as a
    /// count, because the count [`EntryState::DefinitelyFailed`] renders is a
    /// `u32` and the identity is not. The conversion is saturating rather than
    /// wrapping: an attempt ordinal past `u32::MAX` would take the maximum,
    /// which reports an entry as further along than it is, and that is the
    /// direction a retry budget can absorb.
    fn fail(&mut self, id: WorkId, error: &BotError, attempt: AttemptId, budget: u32) -> bool {
        let attempts = u32::try_from(attempt.get()).unwrap_or(u32::MAX);
        let Some(state) = self.entry_mut(id) else {
            return false;
        };
        // Only an attempt in flight can fail: an outcome recorded for an entry
        // that was never begun is not this tick's to write.
        if !matches!(*state, EntryState::Unrecorded) {
            return false;
        }
        *state = failure_state(error, attempts, budget);
        true
    }

    /// Settle an attempt whose effect may or may not have happened.
    ///
    /// The caller hands over the [`EffectKey`] it read from
    /// [`PendingWork::key`], and nothing is read or written until that key's own
    /// facts match the ledger's: the run and the flow it acts under, the action
    /// that locates the entry, the binding the entry currently holds, and the
    /// attempt the entry is on.
    ///
    /// This is not a formality. A chain has one transition at a time and a slot
    /// is reused by every generation over it, so an action that matches says
    /// only that the *address* is the same. A report computed against one
    /// generation's binding and delivered after the slot was re-opened under the
    /// next would be accepted against work the caller has never seen: `Applied`
    /// would acknowledge an effect produced from an input that is gone, and
    /// `NotApplied` — worse, because it authorises a retry — would make an
    /// unknown attempt eligible to run against the new one.
    ///
    /// The binding is the digest, and it carries both halves of that check at
    /// once: it is derived from the generation's revision *and* from the entry
    /// it belongs to, so a key for one entry cannot be spent on another and a
    /// key for one generation cannot be spent on the next. Comparing it is what
    /// makes supersession a fact about the observed input rather than about a
    /// counter that a restarted process would begin again at one.
    ///
    /// The attempt closes the same hole one level down, where the binding does
    /// not help. An entry is retried *within* a generation: settling it
    /// `NotApplied` makes it eligible, and the next tick begins the next attempt
    /// over the same binding. A repeat of the earlier report — which must be
    /// tolerated, because a caller that never saw its first delivery
    /// acknowledged has to be able to send it again — is indistinguishable from
    /// a fresh one unless the attempt is named, and accepting it would reset an
    /// in-flight attempt to eligible.
    ///
    /// Returns a decision rather than a bool, because the answers lead a caller
    /// to different next actions and several of them are not errors.
    fn settle(&mut self, key: &EffectKey, evidence: EffectEvidence) -> Settled {
        let Some(id) = self.locate(key.action()) else {
            return Settled::NoSuchWork { id: None };
        };
        // The run and the flow are the identity this ledger speaks for. A key
        // from another run is not this ledger's to settle, and answering it with
        // any of the finer refusals would suggest the evidence nearly applied.
        let identity = self.effects.identity();
        if key.run() != identity.run() || key.flow() != identity.flow() {
            return Settled::NoSuchWork { id: None };
        }
        let Some(transition) = self
            .transitions
            .get_mut(id.chain())
            .and_then(Option::as_mut)
        else {
            return Settled::NoSuchWork { id: Some(id) };
        };
        // The binding first, before the attempt is even looked up: a key that
        // names the right action inside a generation that is gone is exactly the
        // case this exists to refuse.
        let live =
            derive_action_digest(identity.flow(), id.chain(), id.entry(), transition.revision);
        if key.digest() != live {
            return Settled::Superseded { id, current: live };
        }
        let Some(record) = transition.attempts.get(id.entry()).copied() else {
            return Settled::NoSuchWork { id: Some(id) };
        };
        let settleable = matches!(
            transition.entries.get(id.entry()),
            Some(
                EntryState::Unrecorded
                    | EntryState::OutcomeUnknown { .. }
                    | EntryState::Abandoned { .. }
            )
        );
        if !settleable {
            // Decided: an entry that ran, one whose condition was false, one
            // that has not been attempted yet, and one with a live budget all
            // have an answer, and evidence supplied after the fact cannot
            // contradict an attempt whose outcome was recorded.
            //
            // But an answer this *generation* was already given is not a
            // contradiction, it is the same answer twice, and a caller whose
            // first delivery it never saw an outcome for has to be able to
            // repeat it. Only the record distinguishes that from a fresh claim
            // about an entry that was never settled.
            // The record answers for the attempt it was made about and no
            // other. An entry can be decided while the record is about an
            // earlier attempt — an entry settled and then failed again is on
            // its second attempt with the first one's record still beside it —
            // and reading that record as an answer about *this* attempt is how
            // a report about attempt 2 comes back as a duplicate of attempt 1.
            return match record.settled {
                Some(previous)
                    if previous.attempt == key.attempt() && previous.evidence == evidence =>
                {
                    Settled::Duplicate
                }
                Some(previous) if previous.attempt == key.attempt() => Settled::Contradicted {
                    id,
                    settled: previous.evidence,
                },
                Some(_) | None => Settled::NoSuchWork { id: Some(id) },
            };
        }
        // Then the attempt, and this is the half that the slot and the
        // generation together do not cover. Within one generation an entry is
        // retried: settling it `NotApplied` makes it eligible and `begin` moves
        // it to the next attempt at the same address under the same revision. A
        // repeat of the earlier report — which has to be tolerated, because a
        // caller that never saw its first delivery acknowledged must be able to
        // send it again — would otherwise read as a statement about the attempt
        // now outstanding, and `NotApplied` would make an attempt whose effect
        // may be live eligible to run a third time.
        //
        // Placed after the decided case rather than before it, because a
        // decided entry is where the repeat is *supposed* to land: it is only
        // the live entry whose attempt has to match.
        // An entry with nothing begun cannot be settleable, so the `None` arm
        // is unreachable; it is answered with the same refusal as an entry the
        // ledger does not hold rather than being asserted away.
        let Some(outstanding) = record.begun else {
            return Settled::NoSuchWork { id: Some(id) };
        };
        if key.attempt() != outstanding {
            return Settled::StaleAttempt {
                id,
                reported: key.attempt(),
                outstanding,
            };
        }
        let next = match evidence {
            EffectEvidence::Applied => EntryState::Succeeded,
            EffectEvidence::NotApplied => EntryState::NotStarted,
        };
        if let Some(state) = transition.entries.get_mut(id.entry()) {
            *state = next;
        }
        if let Some(slot) = transition.attempts.get_mut(id.entry()) {
            // Recorded against the attempt the evidence was *about*, never
            // against whichever is current: this record is what decides
            // whether a later report is a repeat, and a record that drifted
            // onto the next attempt would make two different attempts look
            // like one.
            slot.settled = Some(SettledAttempt {
                attempt: outstanding,
                evidence,
            });
        }
        Settled::Decided
    }

    /// The payload the chain's live transition is bound to, if it has one.
    ///
    /// The binding is on loan from the observation slot, which is empty while
    /// the transition holds it, so this is the only way to read back what the
    /// chain last acted on. Two callers need that and they need it for the same
    /// reason: the change filter, which cannot compare a new poll against a
    /// slot that has nothing in it, and the executor, which must run an entry
    /// against the value its transition was opened under rather than against
    /// whatever has arrived since.
    fn bound(&self, chain: usize) -> Option<&Erased> {
        self.transitions
            .get(chain)
            .and_then(Option::as_ref)
            .and_then(|transition| transition.value.as_ref())
    }

    /// The key for the attempt an entry is currently on, when it has one.
    ///
    /// `None` for an entry never attempted, and for a chain or entry the ledger
    /// does not hold. Both are the same answer to the caller — there is no
    /// attempt identity to hand out — and one absence rather than two keeps the
    /// settlement sites from having to tell them apart.
    fn key_of(&self, chain: usize, entry: usize, transition: &Transition) -> Option<EffectKey> {
        let id = WorkId { chain, entry };
        self.effects.key(
            self.action_of(id)?,
            chain,
            entry,
            transition.revision,
            transition.attempt_of(entry)?,
        )
    }

    /// The first entry that is unresolved, in `(chain, entry)` order.
    ///
    /// "Unresolved" is open *or* abandoned, and the second half is load-bearing.
    /// An abandoned entry asks nothing of the substrate — it will not be
    /// attempted again and no evidence is owed by the tick — so a scan that
    /// looked only for open entries walked straight past it and reported a clean
    /// tick. But abandoned work is not handled work: [`Self::pending`] names it,
    /// and a caller that read `Ok` as "the chain is done" was reading the
    /// opposite of what the ledger held. It is also a prerequisite that is *not*
    /// satisfied, so the entries behind it are unresolved through it.
    ///
    /// Taking the first in declaration order is deliberate: the abandonment is
    /// the entry that explains every successor blocked behind it, so naming it
    /// is naming the reason.
    fn first_unresolved(&self, budget: u32) -> Option<PendingWork> {
        for (chain, transition) in self.transitions.iter().enumerate() {
            let Some(transition) = transition.as_ref() else {
                continue;
            };
            for (entry, state) in transition.entries.iter().enumerate() {
                if !(state.is_open() || state.is_abandoned()) {
                    continue;
                }
                let id = WorkId { chain, entry };
                // A recovered attempt nothing has settled outranks the entry's
                // own state in the report, because it is the reason the entry
                // cannot move. Reported first, and reported *instead*: two
                // reports for one entry would read as two problems.
                if let Some(key) = self
                    .action_of(id)
                    .and_then(|action| self.effects.unsettled_for(action))
                {
                    let action = key.action();
                    return Some(PendingWork {
                        id,
                        key: Some(Box::new(key)),
                        hold: TransitionHold::UnsettledByRecovery { action },
                    });
                }
                // Unreachable in practice: an open or abandoned state always
                // renders a hold.
                let Some(hold) = state.hold(budget) else {
                    continue;
                };
                return Some(PendingWork {
                    id,
                    key: self.key_of(chain, entry, transition).map(Box::new),
                    hold,
                });
            }
        }
        None
    }

    /// Every entry that is not finished, in `(chain, entry)` order: open, or
    /// given up on and still reported.
    fn pending(&self, budget: u32) -> Vec<PendingWork> {
        let mut pending = Vec::new();
        for (chain, transition) in self.transitions.iter().enumerate() {
            let Some(transition) = transition.as_ref() else {
                continue;
            };
            for (entry, state) in transition.entries.iter().enumerate() {
                let id = WorkId { chain, entry };
                if let Some(key) = self
                    .action_of(id)
                    .and_then(|action| self.effects.unsettled_for(action))
                {
                    let action = key.action();
                    pending.push(PendingWork {
                        id,
                        key: Some(Box::new(key)),
                        hold: TransitionHold::UnsettledByRecovery { action },
                    });
                    continue;
                }
                if let Some(hold) = state.hold(budget) {
                    pending.push(PendingWork {
                        id,
                        key: self.key_of(chain, entry, transition).map(Box::new),
                        hold,
                    });
                }
            }
        }
        pending
    }

    /// How many entries are still unresolved, across every chain: open, or
    /// abandoned and so still blocking whatever is behind them.
    ///
    /// Counted the same way [`Self::first_unresolved`] scans, because the two
    /// are rendered together in [`BotError::PendingTransition`]: a count that
    /// omitted abandoned entries would report a non-zero entry alongside an
    /// "0 outstanding", which reads as the one thing the pair is there to rule
    /// out — a chain that is somehow both stuck and finished.
    fn unresolved_count(&self) -> usize {
        self.transitions
            .iter()
            .flatten()
            .flat_map(|transition| transition.entries.iter())
            .filter(|state| state.is_open() || state.is_abandoned())
            .count()
    }
}

/// The revision the chain's source last moved to, or zero if it never has.
fn revision_of(world: &World, chain: usize) -> u64 {
    world
        .resource::<Order>()
        .0
        .get(chain)
        .and_then(|entity| world.get::<Revision>(*entity))
        .map_or(0, |revision| revision.0)
}

/// Move the newest observation for `chain` out of its slot.
///
/// Moved, not copied. A source's `Output` carries no `Clone` bound and this is
/// the reason it does not need one: the value is not wanted in two places at
/// once. Once a transition is bound to it, the transition is what speaks for
/// it, and the slot being empty is not a loss — it is the record that the value
/// is out on loan, which [`observe_fold`] reads back through the binding.
fn take_observed(world: &mut World, chain: usize) -> Option<Erased> {
    let mut observed = world.non_send_mut::<Observed>();
    observed.0.get_mut(chain).and_then(Option::take)
}

/// Whether the newest observation for `chain` should be admitted over the
/// payload a retained transition is bound to.
///
/// The comparison is the chain's own, the same one the change filter uses, so
/// "moved" means one thing in this substrate rather than two. It stands in for
/// a `Changed<Revision>` test rather than supplementing one: a movement that
/// arrives while the transition is open does bump the revision, but the change
/// filter is consumed on the tick it is seen, so by the time the transition
/// drains there is nothing left to consult — the observation itself is the only
/// surviving record that the source moved at all.
fn admits(world: &World, chain: usize, bound: Option<&Erased>) -> bool {
    let seen = world.non_send::<Observed>();
    let Some(next) = seen.0.get(chain).and_then(|slot| slot.as_ref()) else {
        // Nothing to admit. An empty slot beside a transition that has nothing
        // open is not a movement, and reading it as one would reopen the chain
        // on every tick and run its entries forever.
        return false;
    };
    let Some(bound) = bound else {
        // Bound to nothing, and now there is something: that is a movement.
        return true;
    };
    let chains = world.non_send::<Chains>();
    chains
        .0
        .get(chain)
        .is_some_and(|chain| !(chain.same)(bound.as_any(), next.as_any()))
}

/// The transition a chain should be walking this tick, if any.
///
/// Three ways one exists: work is outstanding (walked whether or not the source
/// moved — that is the whole point), the transition is kept only for an
/// abandonment record and the newest observation is not admitted over it, or a
/// transition is opened or resumed under an observation that is.
///
/// Opening and resuming are where the payload is bound, and both *take* it out
/// of the observation slot. That is the mechanism that keeps a transition on
/// one input: the entries it evaluates and runs read the binding, so a value
/// that arrives mid-transition cannot reach them, however many ticks the
/// transition takes to drain. The admission itself is then deferred rather than
/// dropped — the newer observation is still sitting in the slot, and the tick
/// after the transition stops being retained is the tick it becomes work.
fn resume(
    world: &mut World,
    chain: usize,
    moving: bool,
    held: Option<Transition>,
) -> Option<Transition> {
    match held {
        // Outstanding work keeps the payload it was opened under. A newer
        // observation is not admitted here, and deliberately: the entries
        // still running were acknowledged against this one.
        Some(transition) if transition.has_open() => Some(transition),
        // Nothing open: the transition is only a name for the work of a
        // generation that has finished. It gives way to the newest observation
        // when that observation has actually moved away from its binding, and
        // is otherwise kept exactly as it stands — abandonment record and all.
        Some(transition) if !admits(world, chain, transition.value.as_ref()) => Some(transition),
        Some(transition) => Some(Transition::resumed(
            revision_of(world, chain),
            &transition,
            take_observed(world, chain),
        )),
        None if moving => {
            let entries = world
                .non_send::<Chains>()
                .0
                .get(chain)
                .map_or(0, |chain| chain.entries.len());
            Some(Transition::opened(
                revision_of(world, chain),
                entries,
                take_observed(world, chain),
            ))
        }
        None => None,
    }
}

/// What a failed attempt means for the entry it was made on.
///
/// The three cases are the three things a caller has to be able to tell apart
/// afterwards: a retry is a retry, a retry may be a duplicate, and a retry is
/// pointless because the failure is not something another attempt can change. The
/// budget is consulted only for the first of them.
///
/// The classification is [`BotError::retry_class`] and nothing else. The error's
/// *variant* is not the question: `DomainError` is where a permanent refusal, a
/// connection that never opened, and a wiring defect all used to land, and they
/// want three different answers. Reading the rendered `cause` to tell them apart
/// is worse still — it makes a retry policy depend on the wording of a message.
/// The producer states what the failure establishes about the effect, the
/// certainty carries it here, and this function is a total map over that.
fn failure_state(error: &BotError, attempts: u32, budget: u32) -> EntryState {
    let cause = error.to_string();
    match error.retry_class() {
        // The effect definitely did not happen: a retry is a retry, and the
        // budget bounds it.
        RetryClass::Safe if attempts < budget => EntryState::DefinitelyFailed { attempts, cause },
        RetryClass::Safe => EntryState::Abandoned {
            reason: AbandonReason::AttemptsExhausted { attempts },
            cause,
        },
        // It may have happened. Never attempted again without evidence, whatever
        // the budget says.
        RetryClass::RequiresEvidence => EntryState::OutcomeUnknown { attempts, cause },
        // No retry fixes a refused capability, a parse refusal, or a type
        // mismatch — and the budget is untouched, so the disposition reports
        // what actually happened rather than "we ran out of attempts".
        RetryClass::Never => EntryState::Abandoned {
            reason: AbandonReason::Terminal,
            cause,
        },
    }
}

// ── Systems ────────────────────────────────────────────────────────────────

/// Observe, fold half: commit the polled values, then bump `Revision` for the
/// sources that moved.
///
/// Exclusive because the verbs are non-`Send`. Every source has already been
/// polled and awaited by the driver by the time this runs, so all that is left
/// is the commit — and a poll that failed is reported *before* any `Revision`
/// is written, which is `Bot::tick`'s documented error ordering: the first
/// error in declaration order, with no source left half-updated.
///
/// That the polls themselves happen outside is what makes a timer- or
/// socket-backed source work: this system is a synchronous step, and a source
/// awaiting a reactor inside it would have no reactor making progress.
fn observe_fold(world: &mut World) {
    if parked(world) {
        return;
    }

    let mut polled = std::mem::take(&mut world.non_send_mut::<Polled>().0);

    // Two passes over the results, and neither allocates. The first inspects
    // every result and commits nothing, which is what makes a failed tick leave
    // the world exactly as it was; the second applies the movements the first
    // cleared.
    //
    // The passes are separate for the same reason `collect::<Result<Vec<_>,_>>`
    // was here before: the first error in declaration order is the one `tick`
    // reports, and finding it may not have written a `Revision` or replaced an
    // observation on the way. Building the values into a fresh `Vec` to do that
    // — which is what the collect did — is an allocation per tick bought for
    // nothing, since the values already sit in a buffer that outlives the call.

    // Pass one: the first error, in declaration order. The failing slot is
    // replaced with a benign `Ok(None)` and the error moved out, because
    // `BotError` is the caller's evidence and is deliberately not `Clone` —
    // copying it to report it would let a caller settle an effect against a
    // duplicate of the failure rather than the failure itself.
    let mut first_error = None;
    for result in polled.iter_mut() {
        if result.is_err() {
            first_error = std::mem::replace(result, Ok(None)).err();
            break;
        }
    }
    if let Some(error) = first_error {
        world.resource_mut::<TickError>().0 = Some(error);
        put_polled(world, polled);
        return;
    }

    // The rendezvous. The results arrived in chain order because `poll_sources`
    // appended them that way, and the pairing from here on is by index — a
    // vector position, which carries no type. The witness is what proves the
    // two halves still agree, and it is checked *before* anything is committed
    // or compared, so a mis-pairing cannot reach a downcast.
    //
    // A miss is a defect in this crate — a chain whose index no longer names the
    // source it was built with — not a domain failure and not something a retry
    // can change. It is reported as `TypeMismatch`, which is classified terminal,
    // so it does not spend a budget; and it commits nothing, so the world is left
    // exactly as the previous tick left it.
    //
    // Only a value that moved is inspected. A source that answered "equal"
    // allocated nothing and has no witness to offer — and needs none: to answer
    // that, it had to downcast the baseline to its own output type, which is a
    // stricter proof of the pairing than comparing two names.
    {
        let chains = world.non_send::<Chains>();
        let mismatch = polled.iter().enumerate().find_map(|(index, result)| {
            let next = result.as_ref().ok()?.as_ref()?;
            let chain = chains.0.get(index)?;
            (!chain.witness.agrees_with(next.witness)).then_some(BotError::TypeMismatch {
                site: "observe_fold rendezvous",
                chain: Some(index),
                expected: chain.witness.name(),
                observed: next.witness.name(),
            })
        });
        if let Some(error) = mismatch {
            world.resource_mut::<TickError>().0 = Some(error);
            put_polled(world, polled);
            return;
        }
    }

    // Pass two: commit. A slot that takes a value has moved; a slot left empty
    // has not, and the value the substrate already held for that chain stays
    // exactly where it is.
    //
    // Holding the older value is not a compromise, it is the rule the substrate
    // already follows one step later: `admits` refuses to let a newer
    // observation displace the payload a retained transition is bound to when
    // the two compare equal, and `resume` keeps that binding. A chain whose
    // values compare equal is a chain whose payload has not changed, and the
    // observation the entries were selected against is the one they still run
    // against.
    //
    // The consequence to know about: `PartialEq` that is narrower than identity
    // — a type that compares only a status field and ignores a timestamp — will
    // now hold the *earlier* of two equal values rather than the later one. That
    // is the same value the action was already going to receive through the
    // binding, so nothing an effect observes changes; but a caller reading the
    // observation back after an equal-valued tick gets the older instance. A
    // type whose equality is not its identity should not be a chain's output
    // type, and this is the point where that shows up.
    let mut changed = std::mem::take(&mut world.non_send_mut::<Moved>().0);
    changed.clear();
    changed.resize(polled.len(), false);
    {
        let mut observed = world.non_send_mut::<Observed>();
        for (index, result) in polled.iter_mut().enumerate() {
            let moved = result
                .as_mut()
                .ok()
                .and_then(Option::take)
                .is_some_and(|value| {
                    if let Some(slot) = observed.0.get_mut(index) {
                        *slot = Some(value);
                        return true;
                    }
                    false
                });
            if let Some(flag) = changed.get_mut(index) {
                *flag = moved;
            }
        }
    }
    put_polled(world, polled);

    // Walked by index rather than over a cloned `Order`: the entity is one
    // `copy` away and the clone was a heap allocation per tick to avoid it.
    let count = changed.len();
    for index in 0..count {
        if !changed.get(index).copied().unwrap_or(false) {
            continue;
        }
        let Some(entity) = world.resource::<Order>().0.get(index).copied() else {
            continue;
        };
        if let Some(mut revision) = world.get_mut::<Revision>(entity) {
            revision.0 = revision.0.wrapping_add(1);
        }
    }
    world.non_send_mut::<Moved>().0 = changed;
}

/// Put the staging buffer back, capacity and all.
///
/// The counterpart to the `mem::take` at the top of [`observe_fold`]. Handing
/// the same allocation back is the point: a staging area rebuilt per tick is an
/// allocation per tick, and the observation phase is the one phase guaranteed to
/// run on every tick of every bot.
fn put_polled(world: &mut World, polled: Vec<Result<Option<Erased>, BotError>>) {
    world.non_send_mut::<Polled>().0 = polled;
}

/// Fire, decide half: walk the eligible work of every chain, in declaration
/// order, and record the effects this tick should run.
///
/// The capability check is not repeated when the effect runs: `run_any` mints a
/// fresh `Auth` from the retained `GrantSet`, which is where the proof belongs.
///
/// The load-bearing word is *retained*. `assemble` clones the set into the world
/// as `Grants(grants.clone())` and nothing revokes it, so this substrate offers
/// exactly the snapshot boundary `Bot` documents and not a live lease: changing
/// or dropping the caller's `GrantSet` after build cannot narrow a bot that is
/// already running. An earlier version of this comment claimed the opposite.
///
/// Conditions are evaluated here, in the schedule, and the effects they select
/// are run by the driver afterwards. That is a reordering of *when* each verb
/// runs, not of which effects happen or in what order: a condition takes only
/// the observed value (`Evaluate::check` has no other input) and so cannot
/// observe an action's effect, which makes the selected list a pure function of
/// the observations. The plan is therefore the exact effect program the
/// synchronous loop would have walked.
///
/// What is walked is the *ledger*, not the change set. `Changed<Revision>`
/// decides only which chains *open* a transition; a chain whose ledger entry is
/// outstanding is walked whether or not it moved, which is what keeps an
/// unattempted effect from being lost when its source holds still. A failure
/// stops its own chain rather than every chain after it, so the chains behind a
/// failing one still record their work.
fn fire_plan(world: &mut World) {
    // The plan's buffers are moved out and put back rather than refilled. This
    // used to clear the plan and then assign it a freshly collected `Vec` for
    // each field — so the `clear` bought nothing and every tick paid two heap
    // allocations per chain to rebuild buffers it was about to overwrite. The
    // buffers are scratch: allocated once, cleared (which keeps capacity), and
    // reused until the chain count actually changes.
    let (mut steps, mut failures) = take_plan_buffers(world);
    steps.clear();
    failures.clear();

    if parked(world) {
        put_plan_buffers(world, steps, failures);
        return;
    }

    let count = world.non_send::<Chains>().0.len();

    // A flag per chain rather than a search of the moved set: the walk below is
    // in declaration order, and membership has to be answerable in constant time
    // for that order to be the one that decides what runs.
    //
    // Taken out of the world because the query below borrows the world
    // immutably; `clear` + `resize` keeps the allocation, where the
    // `vec![false; count]` this replaces made a new zeroed one every tick. The
    // intermediate `Vec<usize>` of moved chains is gone with it: filling the
    // flags directly from the query is one pass instead of two and allocates
    // nothing.
    let mut moving = std::mem::take(&mut world.non_send_mut::<Moving>().0);
    moving.clear();
    moving.resize(count, false);
    {
        let mut query = world.query_filtered::<&SourceId, Changed<Revision>>();
        for id in query.iter(world) {
            if let Some(flag) = moving.get_mut(id.chain) {
                *flag = true;
            }
        }
    }

    failures.resize_with(count, || None);

    for index in 0..count {
        let held = world.non_send_mut::<Ledger>().take(index);
        let Some(mut transition) = resume(
            world,
            index,
            moving.get(index).copied().unwrap_or(false),
            held,
        ) else {
            continue;
        };

        // The borrow of the world for the walk is scoped: `Ledger::take` above
        // and `Ledger::put` below each need it mutably, while the walk needs the
        // chains and the transition's binding immutably. Nothing is written to
        // the transition here — every state that depends on an attempt is
        // written by the driver, which is the only place that knows the outcome.
        {
            let chains = world.non_send::<Chains>();
            if let (Some(chain), Some(value), Some(failure)) = (
                chains.0.get(index),
                transition.value.as_ref(),
                failures.get_mut(index),
            ) {
                plan_chain(chain, &transition, value, index, &mut steps, failure);
            }
            // No chain, or a transition bound to nothing: the work is kept,
            // not discarded. A transition is opened only for a source that was
            // polled, so a chain with no value bound to it is a world that was
            // mutated behind the schedule's back.
        }

        // ── The handover that keeps the last-seen value alive ─────────────
        //
        // A transition holds its payload out on loan: `Transition::opened`
        // *takes* it out of `Observed`, and while the transition is retained
        // that binding is where the chain's newest value lives. When the
        // transition is finished and dropped instead, the binding goes with
        // it — and unless it is handed back, the chain is left with no
        // baseline at all. The next tick would then compare a fresh
        // observation against nothing, read every source as moved, re-open
        // every chain and fire every entry again, forever.
        //
        // That is not a slowdown, it is the substrate running work it had
        // already done: the fairness gate in `bench/` reports it as the bot
        // firing 896,000 effects where the hand-rolled baseline fires 17,920
        // for identical input. The value goes back only when the slot is
        // empty, because a slot the observation phase filled this tick holds a
        // newer value than the one being handed back.
        let retained = transition.is_retained();
        let returned = if retained {
            None
        } else {
            std::mem::take(&mut transition.value)
        };
        world
            .non_send_mut::<Ledger>()
            .put(index, if retained { Some(transition) } else { None });
        if let Some(returned) = returned {
            let mut observed = world.non_send_mut::<Observed>();
            if let Some(slot) = observed.0.get_mut(index).filter(|slot| slot.is_none()) {
                *slot = Some(returned);
            }
        }
    }

    world.non_send_mut::<Moving>().0 = moving;
    put_plan_buffers(world, steps, failures);
}

/// Take the plan's two buffers, leaving empty ones behind.
///
/// They are taken rather than borrowed so the decision walk can push into them
/// while the world is borrowed immutably for the chains and the transition
/// bindings. Empty `Vec`s are left in their place, so a tick that dies before
/// putting them back stages nothing.
fn take_plan_buffers(world: &mut World) -> (Vec<Step>, Vec<Option<BotError>>) {
    let mut plan = world.non_send_mut::<Plan>();
    (
        std::mem::take(&mut plan.steps),
        std::mem::take(&mut plan.failures),
    )
}

/// Put the plan's buffers back, capacity and all.
///
/// The counterpart to [`take_plan_buffers`]. Handing the same allocations back
/// is the whole point: a plan rebuilt from empty every tick allocates once per
/// chain for each of its two fields, which is the cost this pair exists to
/// remove.
fn put_plan_buffers(world: &mut World, steps: Vec<Step>, failures: Vec<Option<BotError>>) {
    let mut plan = world.non_send_mut::<Plan>();
    plan.steps = steps;
    plan.failures = failures;
}

/// Record the decisions one chain's walk reaches, in entry order.
///
/// Only the decisions a walk can make without attempting anything are taken
/// here: an entry the walk is finished with, and an entry it reached whose
/// condition did not hold. Everything that depends on an attempt — marking the
/// entry in flight, and what its outcome means — is left to the driver, which is
/// the only place that knows it.
///
/// A skipped entry is recorded as a [`Decision::Skip`] rather than written to the
/// ledger here so that the condition behind it is evaluated against the value the
/// attempt will actually see. An entry the walk never reaches because an earlier
/// action failed keeps the state that makes it reachable, instead of being marked
/// decided by a condition evaluated ahead of the failure that stopped the walk
/// before it.
fn plan_chain(
    chain: &EcsChain,
    transition: &Transition,
    value: &Erased,
    index: usize,
    steps: &mut Vec<Step>,
    failure: &mut Option<BotError>,
) {
    for (entry_index, entry) in chain.entries.iter().enumerate() {
        let Some(state) = transition.entries.get(entry_index) else {
            continue;
        };
        match *state {
            // Decided, and the chain continues past it: an entry that ran, and
            // one whose condition was false. Both are facts about the entry that
            // leave the entry behind it with nothing standing in its way.
            EntryState::Succeeded | EntryState::Skipped => continue,
            // Given up on, and a *barrier* to everything behind it. This is the
            // line between declaration order and success dependency, and it is
            // the one this walk used to get wrong: an entry in a chain is a
            // prerequisite of the next, so an abandonment is the strongest
            // possible statement that the entry behind it must not run — the
            // draft was never reserved, so there is nothing to send. Continuing
            // past it completed a command whose prerequisite had been given up
            // on.
            //
            // The successors are not lost or forgotten by stopping here. They
            // stay `NotStarted`, so [`Ledger::pending`] keeps reporting them and
            // the tick that finds them names the abandonment in front of them.
            // The way past it is evidence: `NotApplied` revives the entry, and
            // the successors with it.
            EntryState::Abandoned { .. } => break,
            // Held: the effect may be live and only evidence settles that. A
            // later entry is not run ahead of it.
            EntryState::Unrecorded | EntryState::OutcomeUnknown { .. } => break,
            EntryState::NotStarted | EntryState::DefinitelyFailed { .. } => {}
        }

        // The condition reads the value as it stands *now*, which is why an
        // outstanding transition is walked on a tick where the source held
        // still: the value it has not been evaluated against is the one from the
        // movement that joined the transition.
        match entry.condition.check_any(value) {
            Ok(true) => {}
            Ok(false) => {
                steps.push(Step {
                    chain: index,
                    entry: entry_index,
                    decision: Decision::Skip,
                });
                continue;
            }
            Err(error) => {
                // A condition that cannot be evaluated has decided nothing about
                // the effect: the entry stays where it is, this chain stops, and
                // the tick reports the error. Nothing is abandoned, because
                // nothing was attempted. Recorded at most once per chain, since
                // the walk stops here.
                *failure = Some(error);
                break;
            }
        }

        steps.push(Step {
            chain: index,
            entry: entry_index,
            decision: Decision::Attempt,
        });
    }
}

/// Build the schedule this substrate runs, with the two settings that make its
/// order a property of the graph rather than of thread arrival.
fn schedule() -> Schedule {
    let mut schedule = Schedule::default();
    // Explicit, not inherited: the multithreaded executor admits any system
    // whose access set is disjoint from the running set, so which of two
    // non-conflicting systems runs first is not fixed.
    schedule.set_executor(SingleThreadedExecutor::default());
    schedule.set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Error,
        ..Default::default()
    });
    // `.chain()` is the declared order: commit the observations, then decide
    // the effects.
    schedule.add_systems((observe_fold, fire_plan).chain());
    schedule
}

/// Initialize and validate a schedule, turning a build failure into a
/// [`BotError`]. Factored out so the refusal is testable without constructing a
/// bot whose schedule happens to be ambiguous.
fn validate(schedule: &mut Schedule, world: &mut World) -> Result<(), BotError> {
    schedule
        .initialize(world)
        .map(|_| ())
        .map_err(|error| BotError::DomainError {
            domain: "ecs::schedule".into(),
            certainty: DispatchCertainty::Refused,
            // `ScheduleBuildError` carries an inherent `to_string(&self, graph,
            // world)` that shadows `Display::to_string`; the inherent one is the
            // one that resolves system names against the graph.
            cause: error.to_string(schedule.graph(), world),
        })
}

// ── The bot ────────────────────────────────────────────────────────────────

/// A bot executing on a `bevy_ecs` world.
///
/// Stepped by hand: one tick is one [`EcsBot::tick_async`], which is one
/// `Schedule::run` between the awaited observe and act phases. No framework owns
/// a loop, and no `App::run` is ever called.
pub struct EcsBot {
    /// The name the spec declared.
    name: String,
    /// The world the schedule runs against.
    world: World,
    /// The validated schedule.
    schedule: Schedule,
}

impl EcsBot {
    /// Start building a named bot on this substrate.
    #[must_use]
    pub fn builder(name: impl Into<String>) -> EcsBuilder {
        EcsBuilder {
            name: name.into(),
            chains: Vec::new(),
            policy: RetryPolicy::DEFAULT,
            effects: None,
        }
    }

    /// Take the journal back out of the bot.
    ///
    /// `None` when the ledger is not in the world, which is the state after a
    /// previous call already took it. What a host calls on its way out, so the
    /// record outlives the process that wrote it — and what a restart test
    /// calls to hand the same journal to a second bot.
    #[must_use]
    pub fn into_journal(mut self) -> Option<Box<dyn EffectJournal>> {
        self.world
            .remove_non_send::<Ledger>()
            .map(Ledger::into_journal)
    }

    /// The name the spec declared.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Effects fired on the most recent tick.
    #[must_use]
    pub fn fired(&self) -> usize {
        self.world.resource::<Fired>().0
    }

    /// The world, for tests and instrumentation. Read-only: mutating it behind
    /// the schedule's back would break the ordering the build validated.
    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The `Revision` of every source, in chain order.
    #[must_use]
    pub fn revisions(&self) -> Vec<u64> {
        let order = self.world.resource::<Order>().0.clone();
        order
            .iter()
            .filter_map(|entity| self.world.get::<Revision>(*entity))
            .map(|revision| revision.0)
            .collect()
    }

    /// The observed sources' domain identifiers, in chain order.
    ///
    /// Replaces the old `chains()` accessor: what a caller wanted from it was
    /// "what is this bot watching", and that is identity, not the erased
    /// observer objects.
    #[must_use]
    pub fn source_domains(&self) -> Vec<String> {
        let order = self.world.resource::<Order>().0.clone();
        order
            .iter()
            .filter_map(|entity| self.world.get::<SourceId>(*entity))
            .map(|id| id.domain().to_owned())
            .collect()
    }

    /// Run one tick, awaiting the verb futures on the caller's executor.
    ///
    /// Returns the number of actions that fired. The bot is `mut` because a
    /// tick advances its world.
    ///
    /// # What this is for
    ///
    /// This is the tick to call from async code, and the reason is that the
    /// verb futures are awaited **by the caller's runtime** rather than by a
    /// thread parked inside a synchronous system. A source that waits on
    /// `rt::time`, on a socket, on a channel fed by a sibling task, or on
    /// anything else that needs a driver resolves while this future is pending,
    /// because the runtime that owns it is the one driving.
    ///
    /// # Phases
    ///
    /// 1. every source is polled in bounded waves of `MAX_IN_FLIGHT_POLLS`,
    ///    concurrently, in declaration order;
    /// 2. one `Schedule::run` commits those observations, detects which values
    ///    moved, and records the effect program (`observe_fold`, `fire_plan`),
    ///    walking the ledger's outstanding work rather than the change set;
    /// 3. the recorded effects run one at a time, in declaration order, and each
    ///    entry is written into the ledger as it goes;
    /// 4. the count, the first failure, and any entry still held are reported.
    ///
    /// The middle two phases are what keep this deterministic: the schedule is
    /// a total order validated at build, the observations are folded
    /// positionally, and the effect program is fixed before any effect runs.
    ///
    /// # Failure
    ///
    /// A poll that failed fires nothing (no `Revision` was written), while an
    /// action that failed leaves the effects before it live and returns the first
    /// error. A condition that fails stops its own chain where it failed, so the
    /// effects that chain's walk had already reached still run — the plan carries
    /// the decisions and the chains after it are still walked, and the tick
    /// reports the first failure in declaration order. `Err` therefore means "this
    /// run did not finish", never "nothing happened".
    ///
    /// The failure keeps precedence over the pending report when a tick has both,
    /// because it carries the typed variant a retry classifier reads: an
    /// [`BotError::EffectIndeterminate`] written into a string would stop being
    /// the distinction the whole error type exists to draw. So the tick that gives
    /// up on an entry reports the error that caused it, and the abandonment —
    /// never a silent drop — is reported by [`Self::pending`], which names every
    /// abandoned entry, its reason, and its source revision for as long as it
    /// stands.
    ///
    /// # Cancellation
    ///
    /// Dropping this future before it resolves cancels the tick at the point it
    /// was dropped. Between phases 2 and 3 that loses the effects phase 2
    /// selected, and the next tick does not re-fire them: the revisions were
    /// committed in phase 2, so the sources no longer read as moved. This is the
    /// same "unattempted work is lost, not queued" rule the failure path
    /// documents, and it is stated rather than discovered because it is the price
    /// of a tick that is a future.
    ///
    /// Dropping it *during* phase 3 is the case the ledger exists for. An entry
    /// whose attempt was in flight was written to the ledger before the effect
    /// was awaited, so the next tick finds it held as
    /// [`TransitionHold::Unrecorded`] — "an attempt began and no outcome was
    /// recorded" — and does not replay it. An entry whose turn had not come is
    /// left `NotStarted`, so it is walked again. Which of the two an entry is
    /// depends only on whether the effect was awaited, which is the same fact the
    /// caller is missing; the difference is that the substrate no longer has to
    /// guess.
    ///
    /// # Errors
    ///
    /// The first domain failure of the tick, in the order described above, or
    /// [`BotError::PendingTransition`] when nothing failed and work is still held.
    pub async fn tick_async(&mut self) -> Result<usize, BotError> {
        self.begin_tick();

        // The staging buffer is moved out, refilled and put back, so the
        // observation phase reuses one allocation across every tick rather than
        // building a fresh vector per tick. It is taken rather than borrowed
        // because `poll_sources` borrows the world immutably across its awaits
        // and a mutable borrow of a resource cannot be held across them.
        let mut polled = std::mem::take(&mut self.world.non_send_mut::<Polled>().0);
        let mut prints = std::mem::take(&mut self.world.non_send_mut::<Fingerprints>().0);
        let mut polling = std::mem::take(&mut self.world.non_send_mut::<Polling>().0);
        polled.clear();
        self.poll_sources(&mut polled, &mut prints, &mut polling)
            .await;
        self.world.non_send_mut::<Polled>().0 = polled;
        self.world.non_send_mut::<Fingerprints>().0 = prints;
        self.world.non_send_mut::<Polling>().0 = polling;

        self.schedule.run(&mut self.world);

        // Taken rather than borrowed: the effects are awaited below, and a
        // borrow of the plan would outlive the schedule that wrote it. An empty
        // plan is left behind, so a tick dropped here stages nothing.
        let plan = {
            let mut guard = self.world.non_send_mut::<Plan>();
            std::mem::take(&mut *guard)
        };
        let Plan {
            mut steps,
            mut failures,
        } = plan;
        let budget = self.world.resource::<Policy>().0.max_attempts();
        let (fired, failure) = self.run_steps(&steps, &mut failures, budget).await;

        // Handed back with their capacity, not dropped. The plan is taken out of
        // the world to run because the driver needs it mutably while it awaits,
        // and a buffer that is taken out and then dropped is a buffer the next
        // tick has to allocate again — which is exactly what made a tick cost a
        // heap allocation per chain before it ran a single effect.
        steps.clear();
        failures.clear();
        put_plan_buffers(&mut self.world, steps, failures);

        self.world.resource_mut::<Fired>().0 = fired;
        if let Some(error) = failure {
            self.world.resource_mut::<TickError>().0 = Some(error);
        }

        if let Some(error) = self.world.resource_mut::<TickError>().0.take() {
            return Err(error);
        }

        // A tick that reported no failure is not necessarily a tick with nothing
        // left. An entry the substrate cannot settle on its own — an attempt in
        // flight whose outcome was never recorded, an effect that may be live, or
        // one the budget gave up on — is held, and a held entry is reported
        // rather than passed over in silence: a caller that read a clean `Ok` as
        // "the transition was handled" would be reading something that is not
        // true. That includes an abandonment, which is the case this used to
        // miss: it asks the tick for nothing, so it looks finished from inside
        // the walk, but it is work nobody resolved.
        let ledger = self.world.non_send::<Ledger>();
        match ledger.first_unresolved(budget) {
            Some(work) => Err(BotError::PendingTransition {
                work,
                outstanding: ledger.unresolved_count(),
            }),
            None => Ok(self.world.resource::<Fired>().0),
        }
    }

    /// Run one tick on this thread, without an async runtime.
    ///
    /// # Scope
    ///
    /// This is the adapter for **runtime-independent futures**: polls and
    /// actions that complete without a timer or an I/O driver. Immediate
    /// futures, `lgwks_std::task::spawn_blocking` work, and a channel fed by
    /// another thread all belong here; it drives the same four phases as
    /// [`EcsBot::tick_async`] with `lgwks_std::task::block_on`, which parks
    /// this thread until they resolve.
    ///
    /// A verb that needs this crate's timer (`rt::time`) or a driver cannot run
    /// here, because there is no reactor for it to register with: on this
    /// adapter `rt::time::sleep` reaches the runtime-context panic its own
    /// documentation names. Await [`EcsBot::tick_async`] from inside a runtime
    /// instead.
    ///
    /// # The refusal
    ///
    /// [`BotError::TickInsideRuntime`] when an async runtime is already driving
    /// the calling thread. Parking a thread that owns a runtime's driver is not
    /// a slow tick, it is a deadlock — the timer never fires, the socket never
    /// reports ready, and the tick never returns — so the one thing this
    /// adapter must not do is what its name suggests. The check is made here
    /// rather than left to the caller because the calling mode is not knowable
    /// from the verb API.
    ///
    /// This refuses even on a runtime with several worker threads, where
    /// parking one worker may happen to work. Which thread the caller was
    /// handed is not knowable from here, and a deadlock that appears only on
    /// the current-thread runtime is the failure this exists to prevent rather
    /// than to mask.
    ///
    /// # Errors
    ///
    /// [`BotError::TickInsideRuntime`] as above, or the first domain failure of
    /// the tick.
    pub fn tick(&mut self) -> Result<usize, BotError> {
        #[cfg(feature = "rt")]
        if lgwks_deps::tokio::runtime::Handle::try_current().is_ok() {
            return Err(BotError::TickInsideRuntime);
        }
        lgwks_std::task::block_on(self.tick_async())
    }

    /// Reset the per-tick scratch, so a tick starts from the same state however
    /// the previous one ended.
    ///
    /// The staged observations and the effect program are not cleared here:
    /// both are overwritten wholesale by the phase that produces them, which is
    /// what makes a *dropped* tick safe as well as a completed one.
    fn begin_tick(&mut self) {
        self.world.resource_mut::<Fired>().0 = 0;
        self.world.resource_mut::<TickError>().0 = None;
    }

    /// Poll every source, `MAX_IN_FLIGHT_POLLS` at a time, awaiting each wave
    /// on the caller's executor, appending to `polled`.
    ///
    /// Bounded waves, joined concurrently: a source poll may occupy one
    /// `spawn_blocking` thread, so polling them one at a time would make a tick
    /// as slow as the sum of its sources rather than as slow as its slowest.
    /// The wave cap is what keeps that from becoming unbounded blocking-thread
    /// fan-out. Determinism is unaffected: the results are collected in
    /// declaration order whatever order they resolve in.
    ///
    /// # Why the output is a parameter
    ///
    /// `polled` is the caller's buffer, already cleared and already holding the
    /// capacity of the previous tick's results. Returning a fresh `Vec` instead
    /// would make the observation phase allocate its staging area every tick,
    /// which is one of the costs this substrate is measured on. The caller owns
    /// the buffer because it also owns the resource the buffer lives in; this
    /// function only borrows the world.
    ///
    /// # What each source is handed
    ///
    /// Each source is given the payload the substrate currently holds for its
    /// chain — the newest committed observation, or the binding of a transition
    /// that owns it — so the source itself can answer "did I move?" against a
    /// concrete value, before anything is boxed. A source that returned
    /// `Ok(None)` did not move, and nothing is allocated for it.
    /// # The lazy seam
    ///
    /// A chain is polled only when its source says there is something to poll
    /// for. Where a source offers a [`fingerprint`](crate::verb::Observe::fingerprint)
    /// equal to the one on record, the tick records the chain as unchanged and
    /// **never calls `poll`** — so no value is produced, nothing is boxed, and
    /// no future is built.
    ///
    /// The order matters and is the whole point: the decision is taken *before*
    /// the future exists, not inside it. A check inside the future would still
    /// have allocated the box that carries the future, which on a quiet tick is
    /// the only thing there was to allocate. So the skip is a `filter` on the
    /// iterator feeding `join_all_boxed`, and a skipped chain contributes no
    /// allocation of any kind.
    ///
    /// `fingerprint` is therefore called twice for a chain that is polled: once
    /// to decide, once to pair the results back to their chains. That is sound
    /// because the method is required to be pure with respect to the value and
    /// cheap — see its contract. A skipped chain is asked once.
    async fn poll_sources(
        &self,
        polled: &mut Vec<Result<Option<Erased>, BotError>>,
        prints: &mut [Option<u128>],
        polling: &mut Vec<bool>,
    ) {
        let chains = self.world.non_send::<Chains>();
        let grants = self.world.resource::<Grants>();
        let seen = self.world.non_send::<Observed>();
        let ledger = self.world.non_send::<Ledger>();

        let count = chains.0.len();
        // Every chain starts out "unchanged". A chain that is polled overwrites
        // its slot; one that is skipped keeps it, which is precisely the answer
        // its source gave. Sizing here rather than pushing means a skipped
        // chain costs one write and nothing else — no value, no box, no future.
        polled.clear();
        polled.resize_with(count, || Ok(None));

        // The decision, taken once and before any future exists. `polling` is
        // scratch like the rest: it is read by two passes that must agree, so it
        // is computed once rather than asked twice.
        //
        // The digest is read here and **kept**, rather than read again after the
        // poll. Reading it twice was the seam's whole overhead on a workload
        // where nothing is ever skipped: `churn-64x1` moves every source every
        // tick, so no chain is ever quiet and every chain paid a second virtual
        // call for an answer it had already been given.
        //
        // Keeping the earlier digest is also the safe direction. It describes
        // the source at or before the moment the value was taken, so a source
        // that moved in between leaves a digest that no longer matches, and the
        // next tick polls again instead of skipping. The error is one redundant
        // poll, never a missed movement.
        polling.clear();
        polling.resize(count, true);
        for (index, chain) in chains.0.iter().enumerate() {
            let now = chain.source.fingerprint();
            let quiet = matches!(
                (now, prints.get(index).copied().flatten()),
                (Some(now), Some(then)) if now == then
            );
            if let Some(slot) = prints.get_mut(index) {
                *slot = now;
            }
            if let Some(flag) = polling.get_mut(index) {
                *flag = !quiet;
            }
        }

        for (wave_index, wave) in chains.0.chunks(MAX_IN_FLIGHT_POLLS).enumerate() {
            let base = wave_index.saturating_mul(MAX_IN_FLIGHT_POLLS);
            let index_of = |offset: usize| base.saturating_add(offset);
            let wanted = |offset: usize| polling.get(index_of(offset)).copied().unwrap_or(true);

            let batch = lgwks_std::task::join_all_boxed(
                wave.iter()
                    .enumerate()
                    .filter(|&(offset, _)| wanted(offset))
                    .map(|(offset, chain)| {
                        // `chunks` gives no index, so the chain's position is
                        // the wave's start plus the offset within it. This is
                        // the same index `Observed` and `Ledger` are keyed by,
                        // which is what makes the baseline below the right one
                        // to hand over.
                        let index = index_of(offset);
                        let baseline = seen
                            .0
                            .get(index)
                            .and_then(|slot| slot.as_ref())
                            .or_else(|| ledger.bound(index));
                        chain.source.poll_any(&grants.0, baseline)
                    }),
            );
            let results = batch.await;

            for ((offset, _), result) in wave
                .iter()
                .enumerate()
                .filter(|&(offset, _)| wanted(offset))
                .zip(results)
            {
                let index = index_of(offset);
                // A poll that failed clears the digest, which is what makes the
                // next tick ask again rather than skip the chain and swallow the
                // failure. A source that errors therefore reports it every tick,
                // exactly as it did before fingerprints existed.
                if result.is_err()
                    && let Some(slot) = prints.get_mut(index)
                {
                    *slot = None;
                }
                if let Some(slot) = polled.get_mut(index) {
                    *slot = result;
                }
            }
        }
    }

    /// Run the effects the decision phase selected, in the order it selected
    /// them, awaiting each on the caller's executor.
    ///
    /// Returns the number that fired and the first failure in declaration order,
    /// or `None` when nothing failed.
    ///
    /// Every chain is run on its own, because a failure stops its own chain rather
    /// than every chain after it: the chains behind a failing one still run and
    /// still record their work. Iterating chains rather than steps is also what
    /// keeps a chain that recorded *no* step from being skipped: a condition the
    /// walk could not evaluate stops that chain before it records anything, and
    /// its failure is still the tick's to report. The failure of the first chain
    /// in declaration order that has one is that report, which is the ordering the
    /// synchronous walk had.
    ///
    /// States are written to the ledger as the walk reaches them — an attempt in
    /// flight *before* its effect is awaited, and its outcome after — so a tick
    /// dropped in the middle holds what it had begun instead of leaving a
    /// `NotStarted` entry for the next tick to replay.
    async fn run_steps(
        &mut self,
        steps: &[Step],
        failures: &mut [Option<BotError>],
        budget: u32,
    ) -> (usize, Option<BotError>) {
        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        let mut start = 0;
        for chain in 0..failures.len() {
            // `steps` is in `(chain, entry)` walk order, so the steps of `chain`
            // are the run at `start` — and a chain that recorded none contributes
            // the empty run, because its first step is a later chain's.
            let end = steps[start..]
                .iter()
                .position(|step| step.chain != chain)
                .map_or(steps.len(), |offset| start.saturating_add(offset));
            let run = match steps.get(start) {
                Some(step) if step.chain == chain => &steps[start..end],
                _ => &[],
            };

            // Taken, so the condition failure belongs to the chain it was recorded
            // for and cannot be reported for a chain that never reached it.
            let condition_failure = failures.get_mut(chain).and_then(Option::take);
            let (chain_fired, chain_failure) = self.run_chain(run, condition_failure, budget).await;
            fired = fired.saturating_add(chain_fired);
            // Chain order, so "the first error in declaration order" is the one a
            // caller gets, exactly as the synchronous walk had it — except that
            // the chains after it still ran.
            if failure.is_none() {
                failure = chain_failure;
            }
            start = end;
        }
        (fired, failure)
    }

    /// Apply one chain's walk, in entry order, awaiting each attempt.
    ///
    /// A [`Decision::Skip`] is applied without an attempt. A
    /// [`Decision::Attempt`] is written to the ledger as in flight, awaited, and
    /// then recorded. The run stops at the first failure, leaving the entries
    /// behind it exactly as they were, so an acknowledged effect is never
    /// replayed to reach a successor.
    ///
    /// The report falls back to the condition failure the decision phase recorded
    /// for this chain when no attempt of its own failed: a condition the walk
    /// could not evaluate is a failure of the chain it is in, and the decisions
    /// before it are exactly the effects the walk had already cleared.
    async fn run_chain(
        &mut self,
        steps: &[Step],
        condition_failure: Option<BotError>,
        budget: u32,
    ) -> (usize, Option<BotError>) {
        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        for step in steps {
            let work = WorkId {
                chain: step.chain,
                entry: step.entry,
            };
            if matches!(step.decision, Decision::Skip) {
                self.world.non_send_mut::<Ledger>().skip(work);
                continue;
            }

            // Two questions the ledger answers before anything is sent, and
            // they lead to different places. An action an acknowledgement has
            // retired for this generation is *done*, so the walk steps over it
            // exactly as it steps over a false condition: the effect landed, and
            // the generation is the one it landed in. An action an unsettled
            // recovered attempt stands against is *held*, so the walk stops:
            // nothing established that the bytes did not arrive, and the report
            // a caller needs is the one `first_unresolved` renders from the same
            // fact.
            if let Some(action) = self.world.non_send::<Ledger>().action_of(work) {
                let ledger = self.world.non_send::<Ledger>();
                if ledger.generation(step.chain).is_some_and(|revision| {
                    ledger
                        .effects
                        .applied_in(action, step.chain, step.entry, revision)
                }) {
                    // Retired, not merely stepped over. An entry whose effect
                    // landed owes nothing for this generation, and leaving it
                    // outstanding would report finished work as pending for as
                    // long as the generation lasts.
                    self.world.non_send_mut::<Ledger>().skip(work);
                    continue;
                }
                if ledger.effects.blocks(action) {
                    break;
                }
            }

            // Written *before* the attempt, not after: a tick dropped while the
            // effect is in flight leaves this behind, and a record that says
            // "dispatched, outcome unknown" is what makes the next tick hold the
            // effect instead of replaying it. Both the intent and the prepared
            // dispatch are committed here, so a crash after this line recovers
            // as an unknown rather than as a never-sent. A panic unwinding out
            // of the action is a limit this substrate does not close, and it is
            // stated in the module documentation.
            //
            // The key comes back *from* the ledger rather than being derived
            // beside it. The attempt a caller is handed and the attempt
            // settlement checks have to be one identity, and two derivations of
            // one identity are two identities the moment they disagree.
            let (key, authority) = match self.world.non_send_mut::<Ledger>().begin_attempt(work) {
                Ok(pair) => pair,
                Err(error) => {
                    // The entry is not in a state an attempt can start from, or
                    // a guard refused it. Either way nothing left the process,
                    // and the run stops here rather than guessing what the entry
                    // now means.
                    failure = Some(error);
                    break;
                }
            };

            // The generation check, at the handoff rather than at the mint.
            // Authorization and handing over are two instants, and a replacement
            // landing between them leaves a warrant that was minted legitimately
            // and is now stale; a check that only ran at mint time would let it
            // through. Nothing is awaited before this, so the warrant is
            // presented as close to the handoff as the substrate can put it.
            if let Err(cause) = self
                .world
                .non_send::<Ledger>()
                .effects
                .broker()
                .revalidate(&authority)
            {
                let error = BotError::EffectRefused {
                    cause: DispatchError::Broker(cause),
                };
                self.world
                    .non_send_mut::<Ledger>()
                    .fail(work, &error, key.attempt(), budget);
                failure = Some(error);
                break;
            }

            // Scoped, so the world is borrowed immutably only across the await
            // and the ledger is reachable mutably on either side of it.
            //
            // The value is the transition's binding, not the newest observation.
            // That is the whole of the invariant: this entry was *selected*
            // against the payload its transition was opened under, so it has to
            // be *run* against that same payload. Reading the observation slot
            // here is how a chain comes to refuse against one input and then
            // retry against another, combining two command inputs into one
            // transition's effects. The slot is empty for as long as the binding
            // is out on loan, so there is nothing to reach for by accident.
            let outcome = {
                let chains = self.world.non_send::<Chains>();
                let grants = self.world.resource::<Grants>();
                let bound = self.world.non_send::<Ledger>().bound(step.chain);
                match (chains.0.get(step.chain), bound) {
                    (Some(chain), Some(value)) => match chain.entries.get(step.entry) {
                        Some(entry) => Some(entry.action.run_any(&grants.0, value).await),
                        None => None,
                    },
                    // No chain, or a transition bound to nothing: the entry
                    // stays held by the record written above rather than being
                    // guessed at.
                    _ => None,
                }
            };

            let Some(outcome) = outcome else {
                break;
            };
            // The outcome is written down for the two cases where it is a fact,
            // and deliberately not for the third. `Applied` and `NotApplied` are
            // answers; an indeterminate effect is the *absence* of one, and
            // appending `NotApplied` for it would record a fact the substrate
            // does not have — which is exactly how a recovery path comes to
            // believe a merge did not land.
            let observed = match outcome {
                Ok(_) => Some(EffectEvidence::Applied),
                Err(ref error) => match error.dispatch_certainty() {
                    DispatchCertainty::Refused | DispatchCertainty::NotDelivered => {
                        Some(EffectEvidence::NotApplied)
                    }
                    DispatchCertainty::Unsettled => None,
                },
            };
            if let Some(evidence) = observed
                && let Err(cause) = self
                    .world
                    .non_send_mut::<Ledger>()
                    .effects
                    .append(&EffectEvent::OutcomeObserved { key, evidence })
            {
                let error = BotError::EffectRefused {
                    cause: DispatchError::Journal(cause),
                };
                self.world
                    .non_send_mut::<Ledger>()
                    .fail(work, &error, key.attempt(), budget);
                failure = Some(error);
                break;
            }
            match outcome {
                Ok(_) => {
                    if self.world.non_send_mut::<Ledger>().succeed(work) {
                        fired = fired.saturating_add(1);
                    }
                }
                Err(error) => {
                    self.world
                        .non_send_mut::<Ledger>()
                        .fail(work, &error, key.attempt(), budget);
                    failure = Some(error);
                    break;
                }
            }
        }
        // An action failure happens *earlier* in walk order than a condition
        // failure that follows it, so it is the more specific report.
        (fired, failure.or(condition_failure))
    }

    /// Every entry that is not finished, in `(chain, entry)` order.
    ///
    /// Two kinds are reported and [`TransitionHold`] distinguishes them: an
    /// entry still being held — waiting for an attempt or for evidence — and
    /// one given up on, which stays listed so lost work has a name after the
    /// tick that lost it. An empty result means the last transition is fully
    /// handled.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingWork> {
        let budget = self.world.resource::<Policy>().0.max_attempts();
        self.world.non_send::<Ledger>().pending(budget)
    }

    /// Settle an entry whose effect may or may not have happened.
    ///
    /// A held entry is one the substrate cannot decide about on its own, and
    /// retrying it blind is how a bot duplicates a merge, a message, or a
    /// launch. Supply what you know — [`EffectEvidence::Applied`] acknowledges
    /// the effect, [`EffectEvidence::NotApplied`] makes the entry eligible for
    /// an attempt again — and the entry moves.
    ///
    /// An entry that was given up on accepts evidence too, which is how a
    /// caller revives work the attempt budget abandoned.
    ///
    /// The evidence is submitted against the [`PendingWork`] it is about,
    /// rather than against a slot: the value carries the address, the
    /// generation and the attempt, and all three are compared before anything
    /// is read or written. That is what makes the delivery safe to retry — a
    /// caller that never saw its first report acknowledged can send the same
    /// one again and have it answered idempotently, rather than applied a
    /// second time.
    ///
    /// # Errors
    ///
    /// [`BotError::NoSuchWork`] when the entry is not held: it ran, its
    /// condition was false, it has not been reached yet, or its chain holds no
    /// transition at all. Evidence cannot contradict an attempt whose outcome
    /// was recorded, and accepting it silently would let a caller believe an
    /// effect was acknowledged when nothing was.
    ///
    /// [`BotError::EvidenceSuperseded`] when the chain holds a transition whose
    /// revision is not the one the work was read from — the work the caller is
    /// reporting on was superseded, and nothing was changed. Re-read
    /// [`pending`](Self::pending) and report against the generation that is
    /// there now.
    ///
    /// [`BotError::EvidenceStaleAttempt`] when the generation still holds but
    /// the entry has moved on to a later attempt since. The report is about an
    /// attempt that is over, so there is nothing for it to decide; re-read
    /// [`pending`](Self::pending) to see the attempt that is outstanding.
    ///
    /// [`BotError::EvidenceContradicted`] when this generation's entry was
    /// already settled with the opposite evidence. Nothing was changed:
    /// [`Applied`](EffectEvidence::Applied) does not become
    /// [`NotApplied`](EffectEvidence::NotApplied), or the reverse, after the
    /// fact. Repeating the *same* evidence is not a contradiction and succeeds
    /// idempotently, so a caller that never saw its first delivery through can
    /// safely send it again.
    pub fn resolve_effect(
        &mut self,
        key: &EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), BotError> {
        // A recovered attempt is settled first, and the order is the whole of
        // deliverable F05's second half: a key the journal records as dispatched
        // with no outcome belongs to no live transition, so the ledger's own
        // settlement has nothing to match it against and would answer
        // `NoSuchWork`. It is the one case where "settle it" is the repair for a
        // refusal the substrate itself raised.
        let recovered = self
            .world
            .non_send::<Ledger>()
            .effects
            .unsettled_for(key.action())
            .is_some_and(|held| held == *key);
        if recovered {
            // The generation the acknowledgement covers is the key's own. An
            // `Applied` verdict retires the action for the generation that key
            // names and no other, so a source that moves afterwards gets new
            // work — which is what a new generation *is* — while the generation
            // the effect was produced from never runs it again. Reading the
            // generation off the key rather than off the entry's live
            // transition is what makes this reach the recovered case: a
            // recovered attempt belongs to no live transition, so the entry
            // holds no revision to take it from.
            return self
                .world
                .non_send_mut::<Ledger>()
                .effects
                .settle_recovered(*key, evidence)
                .map_err(|cause| BotError::EffectRefused {
                    cause: DispatchError::Journal(cause),
                });
        }
        match self.world.non_send_mut::<Ledger>().settle(key, evidence) {
            // Both are success, and deliberately one arm: to the caller, a
            // repeat that landed a second time is the same fact as one that
            // landed the first.
            Settled::Decided | Settled::Duplicate => Ok(()),
            Settled::Superseded { id, current } => Err(BotError::EvidenceSuperseded {
                work: id,
                named: key.digest(),
                current,
            }),
            Settled::StaleAttempt {
                id,
                reported,
                outstanding,
            } => Err(BotError::EvidenceStaleAttempt {
                work: id,
                reported,
                outstanding,
            }),
            Settled::Contradicted { id, settled } => Err(BotError::EvidenceContradicted {
                work: id,
                settled,
                submitted: evidence,
            }),
            Settled::NoSuchWork { id: Some(id) } => Err(BotError::NoSuchWork { work: id }),
            Settled::NoSuchWork { id: None } => Err(BotError::ActionNotDeclared {
                action: key.action(),
            }),
        }
    }
}

/// Builder for [`EcsBot`].
pub struct EcsBuilder {
    /// The bot name.
    name: String,
    /// Chains finished so far.
    chains: Vec<EcsChain>,
    /// The retry policy every entry will run under.
    policy: RetryPolicy,
    /// The identity, the fence and the journal every dispatch goes through.
    ///
    /// `None` until [`Self::with_effects`] supplies one, and `build` refuses
    /// while it is. There is no default: a default would be a default
    /// *identity*, and an identity a caller did not choose is one they cannot
    /// recover against — which is precisely the state that makes a restart
    /// resend a merge. A default-off variant of the same bot would be worse
    /// still, because nothing would exercise it.
    effects: Option<EffectScope>,
}

impl EcsBuilder {
    /// Set the retry budget for entries whose effect definitely did not happen.
    ///
    /// Additive and defaulted: a caller that never calls this gets
    /// [`RetryPolicy::DEFAULT`], which is what makes the budget a policy rather
    /// than a constant. Without it, "the work was given up on" would be a fact
    /// the caller could not influence, and a bot whose actions are idempotent
    /// would abandon work it could perfectly well retry.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Bind the identity, the environment fence and the journal every dispatch
    /// goes through.
    ///
    /// Required: [`build`](Self::build) refuses without it, reporting
    /// [`BotError::IncompleteSpec`] with `field: "effects"`. That is the same
    /// idiom the name check uses, and it is deliberate rather than a typestate —
    /// a bot whose dispatch path is optional is a bot with two dispatch paths,
    /// and the estate does not keep two implementations of one job.
    #[must_use]
    pub fn with_effects(mut self, effects: EffectScope) -> Self {
        self.effects = Some(effects);
        self
    }

    /// Bind a source to observe.
    ///
    /// The source is *not* erased here. It stays a concrete `S` inside the
    /// returned builder for as long as the chain is being declared, so `on` can
    /// type its condition and its action against `S::Output`; the erasure
    /// happens in [`EcsObserveBuilder::observe`] and
    /// [`EcsObserveBuilder::build`], which are the two points where the chain
    /// stops being a declaration and becomes a chain.
    #[must_use]
    pub fn observe<S>(self, source: S) -> EcsObserveBuilder<S>
    where
        S: Observe,
    {
        EcsObserveBuilder {
            name: self.name,
            prior: self.chains,
            source,
            entries: Vec::new(),
            policy: self.policy,
            effects: self.effects,
        }
    }

    /// Build with no observation chains: a bot that only serves direct
    /// `Query` and `Execute` calls.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError> {
        EcsBot::assemble(self.name, self.chains, grants, self.policy, self.effects)
    }
}

impl<S: Observe> EcsObserveBuilder<S> {
    /// Bind the effect path, as [`EcsBuilder::with_effects`] does.
    ///
    /// Offered here as well so the call can sit anywhere in the declaration
    /// chain — after the last `on` as naturally as before the first `observe` —
    /// rather than only at the one point the builder happens to hand it over.
    #[must_use]
    pub fn with_effects(mut self, effects: EffectScope) -> Self {
        self.effects = Some(effects);
        self
    }

    /// Add a `(condition, action)` tuple to this chain.
    ///
    /// Both halves are typed against the source this chain is observing: the
    /// condition reads `S::Output`, and the action takes exactly it. There is no
    /// third type parameter, and that is the point — the shape this replaced had
    /// one (`on<C, A, T>`) which was tied to nothing at all, so a chain whose
    /// condition read one type and whose action expected another compiled, built,
    /// and failed at tick time with a downcast miss. A wiring defect that
    /// compiles is a defect that ships.
    ///
    /// The tuple's erasure is shared with the `Bot` executor rather than
    /// re-implemented, so the `Auth` issue, the downcast and the type-mismatch
    /// error cannot drift between the two substrates.
    ///
    /// # The rule this relies on
    ///
    /// Equal types across the chain is sound only because every verb so far
    /// consumes its input and produces a caller-visible one: [`Evaluate::check`]
    /// returns a `bool`, a pure predicate with no derived output, so
    /// `Source::Output == Condition::Input == Action::Input` is already the real
    /// semantics rather than a simplification of it.
    ///
    /// **No stage may introduce a caller-selected type parameter disconnected
    /// from its input.** A stage that transforms the value must carry the
    /// transformation as an associated type — `Transform<I>::Output` — so the
    /// next stage's input is a consequence of the previous one's output. A free
    /// parameter here is how this defect happened once, and it will happen again
    /// the first time a verb takes a type the chain does not determine.
    #[must_use]
    pub fn on<C, A>(mut self, condition: C, action: A) -> Self
    where
        S::Output: 'static,
        C: Evaluate<S::Output> + 'static,
        A: Execute<Input = S::Output> + 'static,
        A::Output: 'static,
    {
        self.entries
            .push(typed_entry::<C, A, S::Output>(condition, action));
        self
    }

    /// Finish this chain and start another.
    ///
    /// The erasure boundary for the chain being closed: `S` stops being a type
    /// parameter here, and the witness is taken in the same breath, while
    /// `S::Output` can still be named.
    ///
    /// The `PartialEq` bound is the substrate's one extra requirement, and it is
    /// semantic rather than incidental: the condition on this substrate *is*
    /// change detection, and a value that cannot be compared cannot be detected
    /// as changed. [`Bot::builder`](crate::Bot::builder) carries no such bound
    /// because its condition is re-evaluated every tick and needs no equality.
    #[must_use]
    pub fn observe<U>(self, source: U) -> EcsObserveBuilder<U>
    where
        S: 'static,
        S::Output: PartialEq + 'static,
        U: Observe,
    {
        // Destructured rather than moved field by field: taking `prior` by
        // `mem::take` and then moving `source` out leaves `self` partially
        // moved, and the borrow checker will not let a method call finish the
        // chain in between.
        let Self {
            name,
            mut prior,
            source: previous,
            entries,
            policy,
            effects,
        } = self;
        prior.push(EcsChain {
            source: Box::new(previous),
            same: same_output::<S>,
            witness: Witness::of::<S::Output>(),
            entries,
        });
        EcsObserveBuilder {
            name,
            prior,
            source,
            entries: Vec::new(),
            policy,
            effects,
        }
    }

    /// Assemble the bot, admitting every capability and validating the schedule.
    ///
    /// The erasure boundary for the last chain, for the same reason as
    /// [`observe`](Self::observe), and the one call that turns a declaration into
    /// a running bot.
    pub fn build(self, grants: &GrantSet) -> Result<EcsBot, BotError>
    where
        S: 'static,
        S::Output: PartialEq + 'static,
    {
        let Self {
            name,
            mut prior,
            source,
            entries,
            policy,
            effects,
        } = self;
        prior.push(EcsChain {
            source: Box::new(source),
            same: same_output::<S>,
            witness: Witness::of::<S::Output>(),
            entries,
        });
        EcsBot::assemble(name, prior, grants, policy, effects)
    }
}

/// A chain being assembled, holding its source as the concrete type it is.
///
/// Generic over the source so that [`on`](Self::on) can type a condition and an
/// action against `S::Output`. The type parameter is the chain's one claim about
/// its own wiring, and it is checked by the compiler rather than by a downcast:
/// a `(condition, action)` tuple attached here has both halves typed against the
/// source, so a chain that cannot work does not build.
///
/// `S` is erased at [`observe`](Self::observe) and [`build`](Self::build), which
/// are the only two points where the chain is finished. There is no third state
/// between "declaring a chain" and "a chain": the builder cannot be stored,
/// serialized, or passed to anything that expects an `EcsChain`, because it is
/// not one until the erasure runs.
pub struct EcsObserveBuilder<S> {
    /// Carried from [`EcsBuilder`]; the chain being built does not consume it.
    name: String,
    /// Carried from [`EcsBuilder`] alongside `name`. Taken by the terminal
    /// `build`, which is the only place it is needed.
    effects: Option<EffectScope>,
    /// Chains finished by an earlier `observe` call, in order.
    prior: Vec<EcsChain>,
    /// The source being bound, still concrete.
    source: S,
    /// Tuples attached so far, each already erased for storage.
    entries: Vec<ChainEntry>,
    /// Carried from [`EcsBuilder`] alongside `name`.
    policy: RetryPolicy,
}

impl EcsBot {
    /// Validate and assemble. Shared by both terminal builder calls so
    /// admission cannot drift between them, the same rule
    /// [`spec::assemble`](crate::spec) follows for `Bot`.
    fn assemble(
        name: String,
        chains: Vec<EcsChain>,
        grants: &GrantSet,
        policy: RetryPolicy,
        effects: Option<EffectScope>,
    ) -> Result<Self, BotError> {
        if name.is_empty() {
            return Err(BotError::IncompleteSpec { field: "name" });
        }
        // Refused before the capability gate, because a bot that cannot record
        // a dispatch is not a bot that is missing a capability — it is one that
        // was never given a dispatch path, and the repair is different.
        let Some(effects) = effects else {
            return Err(BotError::IncompleteSpec { field: "effects" });
        };
        // What recovery found, folded in before the world exists. A key naming
        // an action this bot does not declare is a foreign journal — the
        // record belongs to a different bot or a different revision of this
        // one — and it is refused here rather than ignored, because ignoring it
        // means dispatching an action whose earlier attempt the journal says
        // may be live.
        let recovered = recover(
            effects
                .journal()
                .committed()
                .map_err(|cause| BotError::EffectRefused {
                    cause: DispatchError::Journal(cause),
                })?
                .iter(),
        );
        // The same admission gate `Bot::build` applies: a bot requiring a
        // capability it was not granted fails before it runs, not at tick.
        //
        // Every unmet requirement in the bot, from one pass, each attributed to
        // the domain that declared it. Admission that returned at the first one
        // made "what does this bot need" a loop: each answer revealed the next
        // question, so a bot short of four capabilities took four refusals to
        // diagnose and the fourth was the first time the caller had the whole
        // picture. The gate already computes the whole difference — it is the
        // required set minus the granted set — so it reports the whole of it,
        // and the caller has one list to act on rather than a sequence to
        // discover.
        let mut shortages: Vec<Shortage> = Vec::new();
        for chain in &chains {
            shortages.extend(grants.uncovered(
                chain.source.required_caps(),
                &Demand::new(chain.source.domain_id()),
            ));
            for entry in &chain.entries {
                shortages.extend(grants.uncovered(
                    entry.action.required_caps(),
                    &Demand::new(entry.action.domain_id()),
                ));
            }
        }
        if let Some(deficit) = Deficit::from_shortages(shortages) {
            return Err(BotError::CapabilityDenied { deficit });
        }

        let mut world = World::new();
        world.insert_resource(Grants(grants.clone()));
        world.insert_resource(Fired::default());
        world.insert_resource(TickError::default());
        world.insert_resource(Policy(policy));

        let mut order = Vec::with_capacity(chains.len());
        for (index, chain) in chains.iter().enumerate() {
            order.push(
                world
                    .spawn((
                        SourceId {
                            chain: index,
                            domain: chain.source.domain_id().to_owned(),
                        },
                        Revision::default(),
                    ))
                    .id(),
            );
        }
        world.insert_resource(Order(order));

        let count = chains.len();
        // The declared action identity of every entry, derived once here and
        // indexed exactly as the chains are. `derive_action_id` is the single
        // producer of an `ActionId` in this crate: a second derivation anywhere
        // else would be a second identity for the same entry.
        let actions: Vec<Vec<ActionId>> = chains
            .iter()
            .enumerate()
            .map(|(chain, held)| {
                held.entries
                    .iter()
                    .enumerate()
                    .map(|(entry, declared)| {
                        derive_action_id(&name, chain, entry, declared.action.domain_id())
                    })
                    .collect()
            })
            .collect();
        let mut ledger = Ledger::new(actions, Effects::new(effects));
        // Every attempt the journal names, folded in. Not only the uncertain
        // ones: an attempt whose outcome is recorded is a fact about this run
        // too, and the two things this loop derives from it are what let a
        // restarted run continue instead of stopping at the first entry the
        // previous process touched.
        //
        //   - the attempt the action must mint next, because the journal has
        //     already walked its ladder for the attempts below it and a
        //     repeated `IntentAdmitted` for one of them is an out-of-order
        //     append rather than a dispatch;
        //   - the attempts whose effect landed, which the walk retires rather
        //     than dispatching again.
        for attempt in recovered.attempts() {
            let key = attempt.key();
            let Some(id) = ledger.locate(key.action()) else {
                return Err(BotError::ActionNotDeclared {
                    action: key.action(),
                });
            };
            // The journal's key must name this run. A journal that holds keys
            // from another run is the foreign-journal case one step finer: the
            // action matches, and the run does not, and settling it here would
            // acknowledge an attempt in a run this bot is not.
            if key.run() != ledger.effects.identity().run() {
                return Err(BotError::ActionNotDeclared {
                    action: key.action(),
                });
            }
            let _ = id;
            ledger
                .effects
                .note_journal_attempt(key.action(), key.attempt());
            match attempt.status() {
                // A dispatch with no outcome. Nothing established that the
                // bytes did not arrive, so the action is held until a caller
                // settles it.
                AttemptStatus::OutcomeUnknown => ledger.effects.unsettled.push(key),
                // The effect landed, and the record says so.
                AttemptStatus::Applied | AttemptStatus::Verified => {
                    ledger.effects.applied.push(key);
                }
                // Nothing to hold. `Prepared` means the intent was admitted and
                // nothing was handed over, and `NotApplied` means exactly that
                // the bytes did not arrive; both leave the entry free to be
                // attempted again, under the next attempt identity.
                AttemptStatus::Prepared | AttemptStatus::NotApplied => {}
            }
        }
        world.insert_non_send(Chains(chains));
        // Not `vec![None; count]`: `Box<dyn Any>` is not `Clone`, so the
        // repeat-form macro cannot build this.
        world.insert_non_send(Observed((0..count).map(|_| None).collect()));
        // The eligible work of the bot, one idle slot per chain. Non-send,
        // because each transition owns the observed payload it is bound to.
        world.insert_non_send(ledger);
        // The two staging resources the awaited phases hand to the schedule.
        // Inserted here rather than at first use so every phase can name them
        // unconditionally: a phase that found one missing would have to decide
        // what that means, and there is no answer that is better than "this
        // cannot happen".
        world.insert_non_send(Polled::default());
        // The change flags and the plan's buffers are per-tick scratch, inserted
        // here so the first tick allocates them once and every later tick reuses
        // the same allocations. Nothing else writes them.
        world.insert_non_send(Moving::default());
        world.insert_non_send(Moved::default());
        // One slot per chain, so the first tick has somewhere to record the
        // digest each source reports. `None` throughout, which reads as "never
        // polled" and sends every chain down the ordinary poll path.
        world.insert_non_send(Fingerprints(vec![None; count]));
        world.insert_non_send(Polling(Vec::new()));
        world.insert_non_send(Plan::default());

        let mut schedule = schedule();
        validate(&mut schedule, &mut world)?;

        Ok(Self {
            name,
            world,
            schedule,
        })
    }
}

// ── `Debug` for the assembled bot and its two builders ─────────────────────
//
// Written as impls rather than derives because a derive line added above a type
// would renumber the citations the module's own docs make by line number, and
// because `EcsObserveBuilder<S>` and `EcsBuilder` hold `dyn` seams that no
// derive can cross. Each rendering reports the shape a caller can act on — the
// name, how many chains or entries are staged, the retry policy — and stops
// short of the engine's own state through `finish_non_exhaustive`, which marks
// the omission instead of hiding it.

impl core::fmt::Debug for EcsBot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EcsBot")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for EcsBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EcsBuilder")
            .field("name", &self.name)
            .field("chains", &self.chains.len())
            .field("policy", &self.policy)
            .finish()
    }
}

impl<S> core::fmt::Debug for EcsObserveBuilder<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EcsObserveBuilder")
            .field("name", &self.name)
            .field("prior", &self.prior.len())
            .field("entries", &self.entries.len())
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────
//
// These run under the ordinary workspace test run: there is no feature to turn
// on, because a substrate whose tests only run when someone remembers a flag is
// a substrate whose guarantees are claims.

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::cap::{Auth, Cap, Demand};
    use crate::effect::{EnvironmentId, RunId};
    use crate::journal::MemoryJournal;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A scripted source: yields the next value from `values` on each poll.
    ///
    /// Deliberately `Rc`-based and therefore `!Send`, which is the property the
    /// verbs actually have and the reason this whole module routes through
    /// `NonSend` resources.
    struct Script {
        values: Rc<RefCell<Vec<u16>>>,
        cursor: Rc<Cell<usize>>,
        caps: Vec<Cap>,
    }

    impl Script {
        fn new(values: Vec<u16>) -> Self {
            Self {
                values: Rc::new(RefCell::new(values)),
                cursor: Rc::new(Cell::new(0)),
                caps: vec![Cap::net()],
            }
        }
    }

    impl Observe for Script {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            let index = self.cursor.get();
            self.cursor.set(index.saturating_add(1));
            Ok(self.values.borrow().get(index).copied().unwrap_or(0))
        }

        fn domain_id(&self) -> &str {
            "test::script"
        }
    }

    /// A source that reports one value and then holds it for every later poll.
    ///
    /// The counterpart to [`Script`], which advances on every call. A chain over
    /// a scripted source moves on every tick, so it cannot tell "the chain ran
    /// once" from "the chain runs every tick" — both look like work. This one
    /// moves exactly once and then stands still, which is the shape that makes a
    /// chain re-running settled work visible as a count rather than as a
    /// coincidence.
    struct Holds {
        value: u16,
        caps: Vec<Cap>,
    }

    impl Holds {
        fn new(value: u16) -> Self {
            Self {
                value,
                caps: vec![Cap::net()],
            }
        }
    }

    impl Observe for Holds {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            Ok(self.value)
        }

        fn domain_id(&self) -> &str {
            "test::holds"
        }
    }

    /// A source that counts how often it is asked for a value, and can answer
    /// "has anything moved?" without being asked at all.
    ///
    /// The counter is the point. `Holds` proves *what* a settled chain does;
    /// this proves *how much it costs* — specifically that a quiet tick does
    /// not call `poll`, which is the difference between the tick paying for a
    /// value and the tick paying for a `u128`.
    struct Counted {
        value: Rc<Cell<u16>>,
        polls: Rc<Cell<usize>>,
        caps: Vec<Cap>,
    }

    impl Counted {
        fn new(value: u16, polls: Rc<Cell<usize>>) -> Self {
            Self {
                value: Rc::new(Cell::new(value)),
                polls,
                caps: vec![Cap::net()],
            }
        }
    }

    impl Observe for Counted {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            self.polls.set(self.polls.get().saturating_add(1));
            Ok(self.value.get())
        }

        fn fingerprint(&self) -> Option<u128> {
            Some(u128::from(self.value.get()))
        }

        fn domain_id(&self) -> &str {
            "test::counted"
        }
    }

    /// A source that fails once its script runs out.
    struct Exhausting {
        remaining: Rc<Cell<usize>>,
        caps: Vec<Cap>,
    }

    impl Observe for Exhausting {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            let left = self.remaining.get();
            self.remaining.set(left.saturating_sub(1));
            if left == 0 {
                return Err(BotError::DomainError {
                    domain: "test::exhausting".into(),
                    certainty: DispatchCertainty::NotDelivered,
                    cause: "script exhausted".into(),
                });
            }
            Ok(200)
        }

        fn domain_id(&self) -> &str {
            "test::exhausting"
        }
    }

    /// An action that counts how many times it ran.
    struct Count(Rc<Cell<usize>>);

    impl Execute for Count {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Ok(())
        }

        fn domain_id(&self) -> &str {
            "test::count"
        }
    }

    /// An action that records what it saw, so a test can assert on a
    /// *persistent* effect rather than on a call count.
    struct Record(Rc<RefCell<Vec<u16>>>);

    impl Execute for Record {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.borrow_mut().push(*call.1);
            Ok(())
        }

        fn domain_id(&self) -> &str {
            "test::record"
        }
    }

    /// An action that fails a fixed number of times and then records: the
    /// retry half of a chain, with a persistent effect to assert on.
    struct Flaky {
        remaining: Rc<Cell<usize>>,
        log: Rc<RefCell<Vec<u16>>>,
        kind: FlakyKind,
    }

    /// Which failure the action reports, because the two are not the same
    /// question and the substrate must treat them differently.
    #[derive(Clone, Copy)]
    enum FlakyKind {
        /// The effect definitely did not happen.
        Refused,
        /// The effect may or may not have happened.
        Indeterminate,
    }

    impl Execute for Flaky {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            let left = self.remaining.get();
            if left == 0 {
                self.log.borrow_mut().push(*call.1);
                return Ok(());
            }
            self.remaining.set(left.saturating_sub(1));
            Err(match self.kind {
                FlakyKind::Refused => BotError::DomainError {
                    domain: "test::flaky".into(),
                    certainty: DispatchCertainty::NotDelivered,
                    cause: "refused, and the effect did not happen".into(),
                },
                FlakyKind::Indeterminate => BotError::EffectIndeterminate {
                    domain: "test::flaky".into(),
                    cause: "the acknowledgment never arrived".into(),
                },
            })
        }

        fn domain_id(&self) -> &str {
            "test::flaky"
        }
    }

    /// The `(chain, entry)` identity of a pending report.
    fn identity(work: &PendingWork) -> (usize, usize) {
        (work.id().chain(), work.id().entry())
    }

    /// The identities of every pending report, in order.
    fn identities(bot: &EcsBot) -> Vec<(usize, usize)> {
        bot.pending().iter().map(identity).collect()
    }

    /// What is holding the first pending report, if anything is pending.
    fn first_hold(bot: &EcsBot) -> Option<TransitionHold> {
        bot.pending().first().map(|work| work.hold().clone())
    }

    /// An action that always refuses, counting the attempts it received.
    struct Refuses(Rc<Cell<usize>>);

    impl Execute for Refuses {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Err(BotError::DomainError {
                domain: "test::refuses".into(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "refused".into(),
            })
        }

        fn domain_id(&self) -> &str {
            "test::refuses"
        }
    }

    /// A condition that fails structurally rather than evaluating to `false`.
    ///
    /// Named apart from the refusing *action* above rather than sharing a name
    /// with it: the two refuse in different halves of a chain, one is a condition
    /// and one is an action, and a single name for both would make a call site
    /// that reads `.on(Refuses, ..)` ambiguous about which half it is asserting on.
    struct RefusesToEvaluate;

    impl Evaluate<u16> for RefusesToEvaluate {
        fn check(&self, observed: &u16) -> Result<bool, BotError> {
            Err(BotError::EvaluateError {
                cause: format!("this condition cannot decide about {observed}"),
            })
        }

        fn condition_id(&self) -> &str {
            "test::refuses"
        }
    }

    /// A source requiring exactly the capabilities it is handed, so a test can
    /// put more than one unmet requirement in front of admission. `Script`
    /// fixes its own list to `bot.net`, which is one requirement and therefore
    /// cannot distinguish a whole shortfall from its first element.
    struct Needs {
        caps: Vec<Cap>,
        value: u16,
    }

    impl Observe for Needs {
        type Output = u16;

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
            call.0.check(&self.caps)?;
            Ok(self.value)
        }

        fn domain_id(&self) -> &str {
            "test::needs"
        }
    }

    /// The action half of the same arrangement: `Count` requires nothing, so a
    /// test built on it can only ever put a source's requirements in front of
    /// the gate.
    struct NeedsCaps {
        caps: Vec<Cap>,
        domain: &'static str,
    }

    impl Execute for NeedsCaps {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &self.caps
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&self.caps)?;
            Ok(())
        }

        fn domain_id(&self) -> &str {
            self.domain
        }
    }

    fn net_grants() -> GrantSet {
        GrantSet::empty().grant(Cap::net())
    }

    /// The attempt a report is about, for the tests that settle one.
    ///
    /// A `Result` rather than a bare key because an entry that has never been
    /// attempted has no attempt identity, and a test that asked for one anyway
    /// should say so rather than be handed a fabricated one.
    fn held_key(work: &PendingWork) -> Result<EffectKey, Box<dyn std::error::Error>> {
        work.key().ok_or_else(|| {
            format!(
                "chain {} entry {} has no attempt",
                work.id().chain(),
                work.id().entry()
            )
            .into()
        })
    }

    /// The run every test dispatches under.
    const TEST_RUN: &str = "0102030405060708090a0b0c0d0e0f10";
    /// The environment every test dispatches against.
    const TEST_ENV: &str = "2122232425262728292a2b2c2d2e2f30";
    /// The flow revision every test dispatches from.
    const TEST_FLOW: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    /// A scope the tests dispatch under: one run, one environment at its first
    /// generation, and an in-memory journal.
    ///
    /// Returns a `Result` rather than panicking, because the crate forbids a
    /// panicking path anywhere — tests included — and a failure then names the
    /// cause rather than a line number inside a macro.
    ///
    /// The environment is registered because a broker that owns none mints no
    /// authority, so an unregistered one would make every dispatch refuse with
    /// `UnknownEnvironment` and no test would reach the path it is about.
    fn test_effects() -> Result<EffectScope, Box<dyn std::error::Error>> {
        let environment = EnvironmentId::from_hex(TEST_ENV)?;
        let mut broker = Broker::new();
        broker.register(environment)?;
        Ok(EffectScope::new(
            EffectIdentity::new(
                RunId::from_hex(TEST_RUN)?,
                environment,
                FlowRevision::from_tagged("blake3_256", TEST_FLOW)?,
            ),
            broker,
            Box::new(MemoryJournal::new()),
        ))
    }

    /// The sequence every test uses: two ticks of 200, two of 503, one of 200.
    /// It holds still for a tick at a time, which is what makes a change filter
    /// falsifiable: a sequence that moved on every tick would make "changed"
    /// and "ran" indistinguishable.
    const SCRIPT: [u16; 5] = [200, 200, 503, 503, 200];

    #[test]
    fn a_condition_fires_only_on_the_tick_its_source_moves() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("scripted")
            .observe(Script::new(SCRIPT.to_vec()))
            .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        let mut fired = Vec::new();
        let mut revisions = Vec::new();
        for _ in 0..5 {
            fired.push(bot.tick()?);
            revisions.push(bot.revisions().first().copied().unwrap_or_default());
        }

        // `Revision` moves on the ticks the value actually moved: the first
        // poll, the step to 503, and the step back to 200.
        assert_eq!(
            revisions,
            vec![1, 1, 2, 2, 3],
            "Revision must track value movement, not system execution"
        );
        // 503 is held for two ticks; the condition is true on both, and the
        // effect runs once, on the transition.
        assert_eq!(fired, vec![0, 0, 1, 0, 0]);
        assert_eq!(counter.get(), 1);
        Ok(())
    }

    /// A chain whose source settles must settle.
    ///
    /// This is the regression for a defect found by `bench/`'s fairness gate and
    /// by nothing else: the bot fired 896,000 effects where the hand-rolled
    /// baseline fired 17,920 for identical input, a 50x over-run that every
    /// test in this crate passed through.
    ///
    /// The cause was a lost baseline. A transition *takes* the observed value out
    /// of its slot, and while the transition is retained that binding is where
    /// the chain's newest value lives. When the transition finished and was
    /// dropped instead, the value went with it, leaving the chain with nothing to
    /// compare against. The next tick read "no baseline" as "the source moved",
    /// re-opened the chain and fired the entry again — and again, every tick
    /// after, forever.
    ///
    /// So the assertion is not "it fired" but "it fired **once**", over a run
    /// long enough that a per-tick re-fire is unmistakable. `Holds` is what makes
    /// it decidable: a source that advances every poll cannot tell a chain that
    /// settled from a chain that is still working.
    #[test]
    fn a_settled_chain_does_not_fire_again_on_every_later_tick() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("held")
            .observe(Holds::new(200))
            .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        // The first tick is the movement: the chain opens and the effect runs.
        assert_eq!(bot.tick()?, 1, "the movement opens the chain");

        // Twenty more ticks in which nothing whatever happens. A lost baseline
        // shows up here as `1` rather than `0` on the second tick and every one
        // after it.
        for tick in 0_u32..20 {
            assert_eq!(
                bot.tick()?,
                0,
                "tick {}: the source held still, so the chain had nothing to run",
                tick.saturating_add(2)
            );
        }

        assert_eq!(
            counter.get(),
            1,
            "the effect is owed once per movement, not once per tick"
        );
        // The revision is the substrate's own record of movement, and it is the
        // other half of the same claim: one movement, one bump.
        assert_eq!(
            bot.revisions().first().copied().unwrap_or_default(),
            1,
            "Revision counts movements, so a settled chain's is still 1"
        );
        Ok(())
    }

    /// A source that offers a digest is not polled while the digest holds.
    ///
    /// This is the lazy seam, and the assertion is on the *poll count* rather
    /// than on the effect count, because the effect count is already zero on a
    /// quiet tick either way. What changed is that the tick no longer builds,
    /// erases, boxes and drops a value in order to discover that nothing moved:
    /// the source answers the equality question from a `u128` it already holds,
    /// and `poll` — the only thing that produces a value — is never called.
    #[test]
    fn a_source_that_reports_a_digest_is_not_polled_while_it_holds_still() -> TestResult {
        let polls = Rc::new(Cell::new(0));
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("lazy")
            .observe(Counted::new(200, Rc::clone(&polls)))
            .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        assert_eq!(
            bot.tick()?,
            1,
            "the first tick has no digest to compare, so it polls"
        );
        assert_eq!(polls.get(), 1, "and polls exactly once");

        for tick in 0_u32..20 {
            assert_eq!(
                bot.tick()?,
                0,
                "tick {}: nothing moved",
                tick.saturating_add(2)
            );
        }
        assert_eq!(
            polls.get(),
            1,
            "twenty quiet ticks must not produce a single value"
        );
        assert_eq!(counter.get(), 1, "and must not fire the effect again");
        Ok(())
    }

    /// A digest that moves is a movement the tick must not miss.
    ///
    /// The failure mode a fingerprint can introduce is the silent one: a source
    /// that reports a stale digest makes the substrate skip a real change, and
    /// nothing downstream ever notices, because the whole point of the skip is
    /// that nothing downstream ran. So the seam is only as good as this test —
    /// the digest moves, and the chain must fire on the tick it moves and poll
    /// on every tick after it.
    #[test]
    fn a_digest_that_moves_is_polled_again_and_fires() -> TestResult {
        let polls = Rc::new(Cell::new(0));
        let counter = Rc::new(Cell::new(0));
        let source = Counted::new(200, Rc::clone(&polls));
        let value = Rc::clone(&source.value);
        let mut bot = EcsBot::builder("lazy-moving")
            .observe(source)
            .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        assert_eq!(bot.tick()?, 1, "the first value fires");
        assert_eq!(bot.tick()?, 0, "and holds");
        assert_eq!(polls.get(), 1, "so the second tick does not poll");

        value.set(503);
        assert_eq!(bot.tick()?, 1, "the movement is seen, not skipped");
        assert_eq!(polls.get(), 2, "and the tick paid for a value to see it");

        assert_eq!(bot.tick()?, 0, "and settles again");
        assert_eq!(polls.get(), 2, "without polling");
        assert_eq!(counter.get(), 2, "two movements, two effects");
        Ok(())
    }

    /// A source with no digest is polled on every tick, exactly as before.
    ///
    /// The default is `None`, so this is not a special case in the code — it is
    /// what every source written before the seam existed does. It is asserted
    /// because "the old path is unchanged" is a claim the seam makes, and the
    /// cost of the claim being false is every existing domain silently losing
    /// its observations.
    #[test]
    fn a_source_without_a_digest_is_polled_every_tick() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("unguarded")
            .observe(Holds::new(200))
            .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        assert_eq!(bot.tick()?, 1);
        for _ in 0..5 {
            assert_eq!(bot.tick()?, 0);
        }
        // `Holds` has no fingerprint, so every tick polls it — the effect still
        // fires once, because the *value* is what decides the chain, and the
        // value never moved.
        assert_eq!(
            counter.get(),
            1,
            "the value decides the chain, not the digest"
        );
        Ok(())
    }

    #[test]
    fn building_without_the_grant_is_refused() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        match EcsBot::builder("ungranted")
            .observe(Script::new(SCRIPT.to_vec()))
            .on(|value: &u16| *value >= 500, Count(counter))
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())
        {
            Ok(_) => Err("a source requiring `bot.net` built without it".into()),
            Err(error) => {
                assert!(
                    matches!(error, BotError::CapabilityDenied { .. }),
                    "expected CapabilityDenied, got {error:?}"
                );
                Ok(())
            }
        }
    }

    /// Admission as one answer rather than a loop.
    ///
    /// This bot is short of `bot.net` on its source and `bot.fs` on its action.
    /// Admission that returned at the first unmet requirement told the caller
    /// `bot.net`, and `bot.fs` only appeared on the next build — so a person
    /// repairing a bot discovered its requirements one refusal at a time. The
    /// gate already knows both; one refusal now carries both, and each says
    /// which domain declared it.
    #[test]
    fn admission_names_every_unmet_requirement_and_the_domain_that_declared_it() -> TestResult {
        match EcsBot::builder("short")
            .observe(Needs {
                caps: vec![Cap::net()],
                value: 1,
            })
            .on(
                |value: &u16| *value > 0,
                NeedsCaps {
                    caps: vec![Cap::fs()],
                    domain: "test::writes",
                },
            )
            .with_effects(test_effects()?)
            .build(&GrantSet::empty())
        {
            Ok(_) => Err("a bot requiring two ungranted capabilities built".into()),
            Err(BotError::CapabilityDenied { deficit }) => {
                let named: Vec<(&str, Option<&str>)> = deficit
                    .shortages()
                    .map(|shortage| {
                        (
                            shortage.required().as_str(),
                            shortage.demand().map(Demand::domain),
                        )
                    })
                    .collect();
                assert_eq!(
                    named,
                    vec![
                        (Cap::NET, Some("test::needs")),
                        (Cap::FS, Some("test::writes")),
                    ],
                    "one refusal must name both requirements and both domains: {deficit}"
                );
                // And the repair is one call, not one per refusal.
                assert!(
                    deficit
                        .to_grant_set()
                        .admit(&[Cap::net(), Cap::fs()])
                        .is_ok(),
                    "the deficit must derive the whole repair: {deficit}"
                );
                Ok(())
            }
            Err(other) => Err(format!("expected a capability denial, got {other:?}").into()),
        }
    }

    #[test]
    fn a_poll_error_is_returned_and_commits_nothing() -> TestResult {
        let counter = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("exhausting")
            .observe(Exhausting {
                remaining: Rc::new(Cell::new(1)),
                caps: vec![Cap::net()],
            })
            .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        assert_eq!(
            bot.tick()?,
            0,
            "the first poll answers 200 and fires nothing"
        );
        let after_success = bot.revisions();

        // The second poll fails. The tick reports it, and the observed value
        // from the first tick must survive: a failed tick leaves the world as
        // it was rather than half-updated.
        match bot.tick() {
            Ok(_) => return Err("a failing poll was reported as a successful tick".into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the observer's own error, got {error:?}"
            ),
        }
        assert_eq!(
            bot.revisions(),
            after_success,
            "a failed tick must not commit a new Revision"
        );
        assert_eq!(
            counter.get(),
            0,
            "a failed poll fires nothing: the action must not have run"
        );
        Ok(())
    }

    #[test]
    fn a_failure_mid_chain_leaves_the_untouched_entries_pending() -> TestResult {
        // The counterexample to "a tick is all-or-nothing", and the regression
        // for the repair. Three entries: the first records an effect, the
        // second is refused once, the third is never reached on the failing
        // tick.
        //
        // Before the repair this test asserted the *loss*: the revision was
        // committed before any action ran, so the unchanged second tick fired
        // nothing, the refused entry was never retried, and the third entry's
        // effect was absent forever with no record of it. That assertion
        // encoded the defect as correct and has been replaced.
        //
        // The script holds still at 200 so the second tick has an unchanged
        // source, which is what makes both halves falsifiable: a moving source
        // would fire again for reasons unrelated to the failure.
        let first = Rc::new(RefCell::new(Vec::new()));
        let second = Rc::new(RefCell::new(Vec::new()));
        let third = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("partial")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&first)))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&second),
                    kind: FlakyKind::Refused,
                },
            )
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&third)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refused action was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(
            *first.borrow(),
            vec![200],
            "the action before the failure ran and its effect is not rolled back"
        );
        assert!(
            second.borrow().is_empty(),
            "the refused action recorded nothing"
        );
        assert!(
            third.borrow().is_empty(),
            "the entry after the failure was not attempted on the failing tick"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 1), (0, 2)],
            "the refused entry and the entry behind it are both still outstanding"
        );
        assert!(
            matches!(
                first_hold(&bot),
                Some(TransitionHold::Failed {
                    attempts: 1,
                    budget: 3,
                    ..
                })
            ),
            "the refused entry is held as a definite failure with the attempt counted: {:?}",
            first_hold(&bot)
        );

        // The source has not moved. The retry and the untouched third entry run
        // anyway, because eligible work is retained rather than re-derived from
        // the change filter.
        assert_eq!(
            bot.tick()?,
            2,
            "the refused entry is retried and the entry behind it runs, on a still source"
        );
        assert_eq!(
            *first.borrow(),
            vec![200],
            "the acknowledged first effect is not replayed to reach the third entry"
        );
        assert_eq!(
            *second.borrow(),
            vec![200],
            "the retried action ran exactly once, on the second tick"
        );
        assert_eq!(
            *third.borrow(),
            vec![200],
            "the effect the old code lost ran once the entry ahead of it settled"
        );
        assert!(
            bot.pending().is_empty(),
            "a clean tick means nothing is left holding the transition: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn a_failure_before_the_first_effect_still_runs_it_once() -> TestResult {
        // The narrowest version of the defect: nothing has happened yet, the
        // first entry never stops failing, and the entry behind it must still
        // run — after the attempt budget is spent and the entry is abandoned,
        // which is reported rather than silent.
        let attempts = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("refusing")
            .observe(Script::new(vec![200, 200, 200, 200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&attempts)))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        for round in 1..=2 {
            match bot.tick() {
                Ok(fired) => return Err(format!("round {round} reported {fired} fired").into()),
                Err(error) => assert!(
                    matches!(error, BotError::DomainError { .. }),
                    "round {round}: expected the action's own error, got {error:?}"
                ),
            }
        }
        assert_eq!(attempts.get(), 2, "two ticks, two attempts");
        assert!(
            log.borrow().is_empty(),
            "the second entry is not run ahead of the entry holding it back"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 0), (0, 1)],
            "both entries are outstanding while the first keeps failing"
        );

        // Third attempt: the budget is spent and the entry is given up on
        // rather than retried forever. The tick reports the action's own typed
        // error, not a summary of it: a caller classifies a retry by matching
        // the variant, and rendering it into a string would throw that away.
        match bot.tick() {
            Ok(fired) => return Err(format!("a spent budget was reported as {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "the abandonment does not replace the error that caused it: {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 3, "exactly the declared budget was spent");

        // Giving up is reported, never silent: the entry is named with the
        // reason and the cause, and stays named.
        assert_eq!(
            first_hold(&bot),
            Some(TransitionHold::Abandoned {
                reason: AbandonReason::AttemptsExhausted { attempts: 3 },
                cause: "test::refuses: refused".into(),
            }),
            "the given-up-on entry is named after the tick that gave up on it"
        );

        // And the work it was holding back is not lost with it — but it is not
        // run either. The walk stops at the attempt that spent the budget, and
        // the abandonment it leaves is a barrier: the successor stays
        // `NotStarted` behind it and stays reported.
        //
        // These two assertions used to read `assert_eq!(bot.tick()?, 1, "the
        // unattempted work runs as soon as the entry ahead of it is decided")`
        // followed by `assert_eq!(*log.borrow(), vec![200])`, and later
        // `assert_eq!(bot.tick()?, 0, "the resolved transition fires nothing")`
        // while `pending()` was non-empty. Both are the F02 defect written down
        // as the intent — a successor executed past an abandoned prerequisite,
        // and a clean tick reported over work the ledger still names. They are
        // corrected rather than loosened: the tick is now expected to refuse
        // where it was expected to succeed.
        assert!(
            log.borrow().is_empty(),
            "the successor is not run ahead of the entry that was just decided"
        );
        match bot.tick() {
            Ok(fired) => {
                return Err(format!(
                    "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                    log.borrow()
                )
                .into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "expected the unresolved chain to be reported, got {error:?}"
            ),
        }
        assert!(
            log.borrow().is_empty(),
            "the unattempted work does not run past the entry that was given up on"
        );

        // Terminal, and the rest of the chain is blocked behind it, so the
        // transition stays reported: the abandonment first, because it is the
        // entry that explains the one behind it.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!(
                    "an abandoned entry was reported as {fired} fired while pending() still \
                     names it: {:?}",
                    bot.pending()
                )
                .into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "expected the unresolved chain to be reported, got {error:?}"
            ),
        }
        assert!(
            identities(&bot) == vec![(0, 0), (0, 1)],
            "the abandoned entry and the entry it blocks are what is left to report: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn one_attempt_is_a_policy_a_caller_can_declare() -> TestResult {
        // The budget is a policy, not a constant: with one attempt the same
        // chain abandons on the first refusal instead of the third.
        let attempts = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("impatient")
            .with_retry_policy(RetryPolicy::ONE_ATTEMPT)
            .observe(Script::new(vec![200, 200, 200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&attempts)))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => return Err(format!("a refusal was reported as {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 1, "the declared budget is one attempt");
        assert!(
            log.borrow().is_empty(),
            "the entry behind the abandoned one is not run in the tick that abandoned it"
        );

        // The next tick does not run what the abandoned entry was holding back,
        // and does not report a clean tick either. This assertion used to read
        // `assert_eq!(bot.tick()?, 1, "the next tick runs the work the abandoned
        // entry was holding back")`, which is the defect written down as the
        // intent: an abandonment is a prerequisite that is *not* satisfied, so
        // the send behind an unreserved draft must not run. It is corrected
        // rather than loosened — the tick is now expected to refuse, which is
        // strictly more than it was expected to do before.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!(
                    "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                    log.borrow()
                )
                .into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "expected the unresolved chain to be reported, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 1, "the abandoned entry is not retried");
        assert!(
            log.borrow().is_empty(),
            "the successor of an abandoned entry is not attempted while it stands abandoned"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 0), (0, 1)],
            "the abandonment and the entry it blocks are both still reported: {:?}",
            bot.pending()
        );

        // Evidence that the effect did not happen is the way past the barrier:
        // the entry becomes eligible again and its successor with it. The
        // action refuses once more, so this tick abandons it again — which is
        // the point of asserting it: evidence reopens the chain, it does not
        // promise the retry will succeed.
        let blocked = bot.pending().into_iter().next().ok_or("held")?;
        bot.resolve_effect(&held_key(&blocked)?, EffectEvidence::NotApplied)?;
        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refusal was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the retry's own error, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 2, "the revived entry attempted again");
        assert!(
            log.borrow().is_empty(),
            "and the successor is behind the barrier again, because the retry failed too"
        );
        Ok(())
    }

    #[test]
    fn a_failure_does_not_stop_a_later_chain() -> TestResult {
        // The second half of the defect: the old loop broke out of *every*
        // chain on the first error, so an independent chain behind a broken one
        // never ran, and never would.
        let attempts = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("two-chains")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&attempts)))
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refused action was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(
            *log.borrow(),
            vec![200],
            "the chain after the failure ran: a failure is contained to its own chain"
        );
        assert_eq!(
            identities(&bot),
            vec![(0, 0)],
            "only the refusing chain has work outstanding: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn an_indeterminate_effect_is_held_until_evidence_says_what_happened() -> TestResult {
        // The effect may have happened, so a retry is a possible duplicate. The
        // substrate refuses to guess: it holds the entry, reports it every
        // tick, and waits for evidence.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("indeterminate")
            .observe(Script::new(vec![200, 200, 200]))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&log),
                    kind: FlakyKind::Indeterminate,
                },
            )
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(
                    format!("an indeterminate effect was reported as {fired} fired").into(),
                );
            }
            Err(error) => assert!(
                matches!(error, BotError::EffectIndeterminate { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert!(
            log.borrow().is_empty(),
            "an indeterminate effect is not an effect: nothing was recorded"
        );

        // The source has not moved, and the entry is not re-attempted: a blind
        // retry is exactly the duplicate this state exists to prevent.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a held effect was reported as {fired} fired").into());
            }
            Err(error) => {
                let BotError::PendingTransition { work, outstanding } = error else {
                    return Err(format!("expected PendingTransition, got {error:?}").into());
                };
                assert_eq!(identity(&work), (0, 0), "the held entry is named");
                assert!(
                    matches!(
                        work.hold(),
                        TransitionHold::OutcomeUnknown { attempts: 1, .. }
                    ),
                    "the hold carries the attempt that could not be settled: {:?}",
                    work.hold()
                );
                assert_eq!(outstanding, 1, "one entry is still open");
                assert!(
                    bot.pending()
                        .iter()
                        .all(|work| !matches!(work.hold(), TransitionHold::Abandoned { .. })),
                    "nothing was given up on: {:?}",
                    bot.pending()
                );
            }
        }
        assert!(
            log.borrow().is_empty(),
            "a held effect is not attempted again without evidence"
        );

        // Evidence that it did not happen: the entry becomes eligible again and
        // the attempt finally lands. The revision the report carries is passed
        // back with the identity, which is what makes this delivery sound: the
        // evidence is about the generation the caller was shown.
        let held = bot
            .pending()
            .into_iter()
            .next()
            .ok_or("the held entry disappeared from the report")?;
        bot.resolve_effect(&held_key(&held)?, EffectEvidence::NotApplied)?;
        assert_eq!(bot.tick()?, 1, "the entry runs once evidence authorises it");
        assert_eq!(*log.borrow(), vec![200], "the effect happened, once");
        assert!(
            bot.pending().is_empty(),
            "and nothing is left holding the transition: {:?}",
            bot.pending()
        );
        Ok(())
    }

    #[test]
    fn evidence_that_the_effect_happened_records_it_without_replaying_it() -> TestResult {
        // The other half of the evidence question, and the one that saves the
        // duplicate: the caller knows the effect is live, so it is acknowledged
        // and never attempted again.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("acknowledged")
            .observe(Script::new(vec![200, 200, 200]))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&log),
                    kind: FlakyKind::Indeterminate,
                },
            )
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(
                    format!("an indeterminate effect was reported as {fired} fired").into(),
                );
            }
            Err(error) => assert!(
                matches!(error, BotError::EffectIndeterminate { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }

        let held = bot
            .pending()
            .into_iter()
            .next()
            .ok_or("the held entry is not reported")?;
        bot.resolve_effect(&held_key(&held)?, EffectEvidence::Applied)?;
        assert_eq!(
            bot.tick()?,
            0,
            "an acknowledged effect is never attempted again"
        );
        assert!(
            log.borrow().is_empty(),
            "the action that may already have run was not run a second time"
        );
        assert_eq!(bot.revisions(), vec![1], "and the transition is resolved");
        assert!(
            bot.pending().is_empty(),
            "nothing is left pending: {:?}",
            bot.pending()
        );

        // Evidence about an entry that is not held is refused rather than
        // silently accepted: a caller that thinks it acknowledged something
        // needs to find out that it did not. The generation is still named
        // here, and the refusal is still `NoSuchWork` rather than a generation
        // complaint — the tick above resolved the transition and dropped its
        // slot, so there is genuinely no held effect at this address. That
        // distinction is the whole reason the two new refusals exist.
        match bot.resolve_effect(&held_key(&held)?, EffectEvidence::Applied) {
            Ok(()) => Err("evidence was accepted for an entry that is not held".into()),
            Err(error) => {
                assert!(
                    matches!(error, BotError::NoSuchWork { work } if work == held.id()),
                    "expected NoSuchWork for {:?}, got {error:?}",
                    held.id()
                );
                Ok(())
            }
        }
    }

    #[test]
    fn a_source_that_moves_while_work_is_outstanding_neither_loses_nor_duplicates_it() -> TestResult
    {
        // Two movements while work is outstanding, and then a movement after it
        // resolves. An entry the transition is *holding* reads the payload the
        // transition was opened under — not the newest one — while an entry in a
        // new transition reads the newest; the effect that already happened is
        // not replayed; and the chain is not wedged afterwards.
        let first = Rc::new(RefCell::new(Vec::new()));
        let second = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("moving")
            .observe(Script::new(vec![200, 503, 200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&first)))
            .on(
                |value: &u16| *value >= 200,
                Flaky {
                    remaining: Rc::new(Cell::new(1)),
                    log: Rc::clone(&second),
                    kind: FlakyKind::Refused,
                },
            )
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => return Err(format!("a refusal was reported as {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(*first.borrow(), vec![200], "the first entry ran once");
        assert!(second.borrow().is_empty(), "the second entry refused");

        // 503 arrives while the second entry is still outstanding. The
        // acknowledged first entry is not replayed to reach it, and the retry
        // runs against the value the transition is *bound to*: 200, the value
        // the first entry already succeeded on.
        //
        // This is the invariant, and it used to read the other way — the retry
        // took the value as it stood, and this test asserted `vec![503]` under
        // the words "the outstanding entry reads the newest value, not the one
        // it failed on". That is F04 stated as intent: entry zero had produced
        // an effect of 200, entry one would then produce an effect of 503, and
        // the transition would have acted on two command inputs while reporting
        // itself as one unit of work. A transition is one input.
        assert_eq!(bot.tick()?, 1, "only the outstanding entry runs");
        assert_eq!(
            *first.borrow(),
            vec![200],
            "an effect the transition already acknowledged is not replayed for a later movement"
        );
        assert_eq!(
            *second.borrow(),
            vec![200],
            "the outstanding entry reads the payload its transition was opened under, \
             not the one that arrived since"
        );
        assert!(
            bot.pending().is_empty(),
            "the transition is fully resolved: {:?}",
            bot.pending()
        );

        // The source is back at 200 by the next tick, which is the value the
        // transition was opened under and acknowledged against. Nothing is
        // admitted, so nothing fires: the 503 the source passed through on the
        // way is superseded, and re-running both entries here would replay an
        // effect that is already acknowledged — the duplicate half of this
        // test's name.
        //
        // This assertion used to read `assert_eq!(bot.tick()?, 2, "a new
        // movement re-evaluates both entries")` with `vec![503, 200]` logged for
        // the second entry, and it was only true because the retry above had
        // already jumped to 503. Under the binding it is 200 that is current,
        // 200 that the chain has already handled, and 503 that nothing ever
        // acted on — and nothing claims it did.
        //
        // What a source that *stays* moved does is the other half of the
        // invariant, and `an_open_transition_is_bound_to_the_payload_it_was_opened_under`
        // is where it is pinned: the new value is admitted as soon as the
        // transition has nothing open, over every entry, against itself.
        assert_eq!(
            bot.tick()?,
            0,
            "a source that has returned to the value the transition was acknowledged \
             for is not new work"
        );
        assert_eq!(
            *first.borrow(),
            vec![200],
            "and the acknowledged effect is not replayed to make it look like one"
        );
        assert_eq!(*second.borrow(), vec![200], "nor is the second entry");
        // A movement that did not move is still nothing at all.
        assert_eq!(
            bot.tick()?,
            0,
            "a still source with no outstanding work fires nothing"
        );
        assert_eq!(
            bot.revisions(),
            vec![3],
            "three polls moved the value: 200, 503, 200"
        );
        Ok(())
    }

    #[test]
    fn an_attempt_whose_outcome_was_never_recorded_is_held_not_replayed() -> TestResult {
        // The state a tick leaves behind when an attempt was begun and no
        // outcome was recorded. The effect may be live, so the next tick holds
        // it — replaying it is the duplicate this state exists to prevent.
        //
        // Constructed directly rather than by killing a tick: the ledger is the
        // record of what happened, and this is what that record looks like when
        // a tick's walk found the entry unrecorded. Nothing in the substrate
        // produces the state and returns — the write sits immediately before the
        // attempt and is overwritten by the outcome — which is why the record is
        // exercised here rather than through an action.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("interrupted")
            .observe(Script::new(vec![200, 200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        assert_eq!(bot.tick()?, 1, "the first tick runs the action");
        {
            let revision = *bot.revisions().first().ok_or("the source has a revision")?;
            // The payload is part of the record: a transition is bound to the
            // observation it was opened under, and the value here is the one the
            // script has been answering with. `None` would be a transition
            // opened with nothing observed, which holds its entries rather than
            // evaluating them against a value nobody read — a real state, but
            // not this one.
            let mut transition = Transition::opened(revision, 1, Some(Erased::new(200_u16)));
            *transition
                .entries
                .first_mut()
                .ok_or("the chain declares no entries")? = EntryState::Unrecorded;
            // The attempt the unrecorded entry is on. `Unrecorded` is the state
            // a prepared dispatch leaves, so there is always an attempt to name
            // it by — and the record has to carry one, because the key a caller
            // settles against is derived from the attempt and an entry with no
            // attempt has no key for evidence to be about.
            *transition
                .attempts
                .first_mut()
                .ok_or("the chain declares no entries")? = AttemptRecord {
                begun: Some(AttemptId::FIRST),
                settled: None,
            };
            bot.world.non_send_mut::<Ledger>().put(0, Some(transition));
        }

        assert!(
            matches!(first_hold(&bot), Some(TransitionHold::Unrecorded)),
            "the interrupted attempt is reported as unrecorded: {:?}",
            first_hold(&bot)
        );
        match bot.tick() {
            Ok(fired) => return Err(format!("an unrecorded attempt fired {fired}").into()),
            Err(error) => {
                let BotError::PendingTransition { work, .. } = error else {
                    return Err(format!("expected PendingTransition, got {error:?}").into());
                };
                assert!(
                    matches!(work.hold(), TransitionHold::Unrecorded),
                    "the tick reports it as held, not as handled: {:?}",
                    work.hold()
                );
            }
        }
        assert_eq!(
            *log.borrow(),
            vec![200],
            "the action was not run a second time: the first attempt may have taken effect"
        );

        // Evidence that it did not take effect is what unlocks the retry, and
        // it is evidence about the generation the caller was shown.
        let held = bot
            .pending()
            .into_iter()
            .next()
            .ok_or("the held entry is not reported")?;
        bot.resolve_effect(&held_key(&held)?, EffectEvidence::NotApplied)?;
        assert_eq!(bot.tick()?, 1, "and then it runs");
        assert_eq!(*log.borrow(), vec![200, 200], "exactly once more");
        Ok(())
    }

    #[test]
    fn a_condition_that_cannot_be_evaluated_holds_the_chain_without_claiming_anything() -> TestResult
    {
        // A condition failure decides nothing about the effect. The entry has
        // not been attempted, nothing about it is claimed, and the entry behind
        // it is not run ahead of a question that has no answer — the failure
        // the old substrate turned into "this chain is done".
        //
        // The mismatched condition is built *behind* the builder now, because
        // the typed `on` refuses it: a `u16` source cannot carry a condition
        // that reads a `String`. That refusal is the front half of this fix, and
        // the back half is still needed, because erasure is not the only way to
        // reach a mis-paired chain — anything holding this world can insert an
        // entry. `typed_entry` is the erasure helper and keeps its free `T`, so
        // it can still express the defect; this test is the one place that does,
        // on purpose, to pin what happens when it is reached.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("mismatched")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;
        bot.world.non_send_mut::<Chains>().0[0].entries.insert(
            0,
            typed_entry::<_, _, String>(|value: &String| value.len() >= 3, Record(Rc::clone(&log))),
        );

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("an evaluate error was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::EvaluateError { .. }),
                "expected the condition's own error, got {error:?}"
            ),
        }
        assert!(log.borrow().is_empty(), "nothing was attempted");
        assert_eq!(
            identities(&bot),
            vec![(0, 0), (0, 1)],
            "the entry whose condition failed is still outstanding, and so is the entry \
             behind it: {:?}",
            bot.pending()
        );
        assert_eq!(
            first_hold(&bot),
            Some(TransitionHold::NotStarted),
            "a condition that cannot be evaluated is not an attempt, so nothing is abandoned"
        );

        // And it stays that way rather than decaying into an abandonment: the
        // report is the truth, not a stage on the way to losing the work.
        match bot.tick() {
            Ok(fired) => return Err(format!("the second tick reported {fired} fired").into()),
            Err(error) => assert!(
                matches!(error, BotError::EvaluateError { .. }),
                "the condition is evaluated again, not silently given up on: {error:?}"
            ),
        }
        assert!(log.borrow().is_empty(), "and still nothing was attempted");
        Ok(())
    }

    #[test]
    fn a_condition_failure_stops_the_walk_after_the_effects_it_cleared() -> TestResult {
        // The other half of "partial run", on the condition side, and a
        // characterisation rather than a new guarantee: this passes before and
        // after the decision phase was split out of the effect phase, and it is
        // here because that split is only sound if it does.
        //
        // Conditions are pure over the observed value, so the whole effect
        // program can be recorded before any of it runs. The equivalence to the
        // interleaved loop it replaced rests on exactly three things: the
        // entries before a failing condition still run, the entries after it
        // are not attempted, and the tick reports the condition's own error.
        let log = Rc::new(RefCell::new(Vec::new()));
        let after = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("condition-failure")
            .observe(Script::new(vec![200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .on(RefusesToEvaluate, Count(Rc::clone(&after)))
            .on(|value: &u16| *value >= 200, Count(Rc::clone(&after)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a refused condition was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::EvaluateError { .. }),
                "expected the condition's own error, got {error:?}"
            ),
        }
        assert_eq!(
            *log.borrow(),
            vec![200],
            "the effect the walk cleared before the failing condition ran"
        );
        assert_eq!(
            after.get(),
            0,
            "the entries after the failing condition were not attempted"
        );
        Ok(())
    }

    #[test]
    fn an_ambiguous_schedule_is_refused_at_build() -> TestResult {
        let mut world = World::new();

        // Two exclusive systems conflict with everything, including each other,
        // so their relative order is indeterminate.
        let mut ambiguous = Schedule::default();
        ambiguous.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..Default::default()
        });
        ambiguous.add_systems((observe_fold, fire_plan));

        match validate(&mut ambiguous, &mut world) {
            Ok(()) => return Err("an ambiguous schedule was accepted at build".into()),
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "a schedule-build failure must surface as a typed BotError, got {error:?}"
            ),
        }

        // Control: the same two systems, ordered, must validate. Without this
        // the refusal above could be any failure at all.
        let mut ordered = Schedule::default();
        ordered.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..Default::default()
        });
        ordered.add_systems((observe_fold, fire_plan).chain());
        validate(&mut ordered, &mut world)?;
        Ok(())
    }

    /// An action that declares `Input = u32` while counting its attempts.
    ///
    /// Paired below with a `u16` source: the pairing is the fault, and the
    /// count is how the test tells "one attempt" from "the whole budget".
    struct CountsU32(Rc<Cell<usize>>);

    impl Execute for CountsU32 {
        type Input = u32;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Ok(())
        }

        fn domain_id(&self) -> &str {
            "test::counts_u32"
        }
    }

    /// An action whose failure is permanent and happens before dispatch: the
    /// shape every "binding required" domain has.
    struct RefusesPermanently(Rc<Cell<usize>>);

    impl Execute for RefusesPermanently {
        type Input = u16;
        type Output = ();

        fn required_caps(&self) -> &[Cap] {
            &[]
        }

        async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
            call.0.check(&[])?;
            self.0.set(self.0.get().saturating_add(1));
            Err(BotError::DomainError {
                domain: "test::permanent".into(),
                certainty: DispatchCertainty::Refused,
                cause: "no binding is installed for this domain".into(),
            })
        }

        fn domain_id(&self) -> &str {
            "test::permanent"
        }
    }

    #[test]
    fn a_mispairing_behind_the_erasure_is_a_witness_miss() -> TestResult {
        // The pairing from `Chains` to `Observed` is by index, and a vector
        // position carries no type. This is that pairing gone wrong: the chain
        // declares a `u16` source, and the staged value is a `u32`.
        //
        // Written into the staging resource directly, because the shipped path
        // cannot produce this state: `poll_sources` boxes the output of the very
        // source it took the witness from, so a disagreement means the world
        // moved behind the schedule's back. That is precisely the state the
        // witness exists to refuse rather than to downcast, and it is why the
        // test has to construct it rather than trigger it.
        //
        // The assertions are the three things the witness is for. It fires
        // *before* the value is used, so nothing is committed and no condition
        // is asked. It names both types and the site. And it is a mismatch, not
        // a domain failure, so the disposition is terminal and no retry budget
        // is touched.
        let ran = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("mispairing")
            .observe(Script::new(vec![200]))
            .on(|value: &u16| *value >= 200, Refuses(Rc::clone(&ran)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        // `Ok(Some(_))`: a value that *moved*, which is the only shape that
        // carries a witness to check. A source answering `Ok(None)` has proven
        // its pairing by downcasting the baseline to its own output type, and
        // there is nothing left for the rendezvous to inspect.
        bot.world.non_send_mut::<Polled>().0 = vec![Ok(Some(Erased::new(300u32)))];
        bot.schedule.run(&mut bot.world);

        match bot.world.resource::<TickError>().0 {
            Some(BotError::TypeMismatch {
                site,
                chain,
                expected,
                observed,
            }) => {
                assert_eq!(site, "observe_fold rendezvous", "the site is greppable");
                assert_eq!(chain, Some(0), "the report names which chain disagreed");
                assert_eq!(expected, "u16", "the chain's claim");
                assert_eq!(observed, "u32", "what actually arrived");
            }
            ref other => {
                return Err(format!("expected a witness miss, got {other:?}").into());
            }
        }

        assert!(
            bot.world
                .non_send::<Observed>()
                .0
                .iter()
                .all(Option::is_none),
            "the fold commits nothing on a mismatch, so the previous tick's state survives"
        );
        assert_eq!(ran.get(), 0, "the action was never reached");
        Ok(())
    }

    #[test]
    fn a_mismatched_pairing_costs_one_attempt_not_the_retry_budget() -> TestResult {
        // The chain is wired for `u16` while the action declares `u32`. That is
        // a wiring defect behind the erasure, not a domain failure: no number
        // of retries reaches a different answer, so it costs one attempt and
        // lands as terminal rather than spending the whole budget and being
        // reported as "we ran out of budget" — which names the budget as the
        // reason when the reason is that the two halves disagree.
        //
        // The report names both types, which is what makes it a diagnosis
        // rather than a report that something is wrong somewhere. It names no
        // domain, because no domain was reached.
        //
        // The attempt count is read from the ledger rather than counted by the
        // action, because the action must never be reached: the downcast is
        // checked before the proof is issued, so a mismatch costs no side
        // effect. `ran` is that second half.
        // Built behind the builder for the same reason as the condition
        // mismatch above: `on` no longer admits `CountsU32` on a `u16` source,
        // which is the fix, and the downcast arm is still what reports the
        // mis-pairing that reaches the erasure boundary anyway.
        let ran = Rc::new(Cell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut bot = EcsBot::builder("mismatched")
            .observe(Script::new(vec![200, 200, 200, 200]))
            .on(|value: &u16| *value >= 200, Record(Rc::clone(&log)))
            .with_effects(test_effects()?)
            .build(&net_grants())?;
        bot.world.non_send_mut::<Chains>().0[0].entries[0] =
            typed_entry::<_, _, u16>(|value: &u16| *value >= 200, CountsU32(Rc::clone(&ran)));

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a wiring mismatch was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(
                    error,
                    BotError::TypeMismatch {
                        site: "spec::typed_entry",
                        expected: "u32",
                        observed: "u16",
                        ..
                    }
                ),
                "a wiring defect is a type mismatch naming both types, not a domain failure: {error:?}"
            ),
        }
        assert_eq!(
            ran.get(),
            0,
            "the downcast is checked before the action runs, so the mismatch was not a side effect"
        );
        assert!(
            matches!(
                first_hold(&bot),
                Some(TransitionHold::Abandoned {
                    reason: AbandonReason::Terminal,
                    ..
                })
            ),
            "one attempt, and the disposition is terminal rather than `AttemptsExhausted`: {:?}",
            first_hold(&bot)
        );

        // The second tick must not spend a second attempt on the same answer.
        // That is the observable difference the budget makes: `AttemptsExhausted`
        // is a fact about how many times the substrate was willing to ask, and
        // asking twice here buys nothing.
        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a retried wiring mismatch reported {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::PendingTransition { .. }),
                "the abandoned entry is the barrier the next tick reports, got {error:?}"
            ),
        }
        Ok(())
    }

    #[test]
    fn a_permanent_refusal_does_not_burn_the_retry_budget() -> TestResult {
        // A refusal that happens before dispatch and cannot be changed by
        // repeating it. Retrying spends attempts to learn the same answer, and
        // the report then blames the budget for a failure the budget was never
        // the reason for.
        let attempts = Rc::new(Cell::new(0));
        let mut bot = EcsBot::builder("permanent")
            .observe(Script::new(vec![200, 200, 200, 200]))
            .on(
                |value: &u16| *value >= 200,
                RefusesPermanently(Rc::clone(&attempts)),
            )
            .with_effects(test_effects()?)
            .build(&net_grants())?;

        match bot.tick() {
            Ok(fired) => {
                return Err(format!("a permanent refusal was reported as {fired} fired").into());
            }
            Err(error) => assert!(
                matches!(error, BotError::DomainError { .. }),
                "expected the action's own error, got {error:?}"
            ),
        }
        assert_eq!(attempts.get(), 1, "a permanent refusal is attempted once");
        assert!(
            matches!(
                first_hold(&bot),
                Some(TransitionHold::Abandoned {
                    reason: AbandonReason::Terminal,
                    ..
                })
            ),
            "the disposition is terminal, not `AttemptsExhausted`: {:?}",
            first_hold(&bot)
        );
        Ok(())
    }
}
