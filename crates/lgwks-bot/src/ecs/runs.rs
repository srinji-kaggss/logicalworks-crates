//! Runs into the world: the `admitted` and `settled` events (ADR 0006 R3).
//!
//! A [`RunFeed`] is the one record of every run a `Host` or a `Supervisor`
//! admitted and settled. Both write through the journal before anything reads
//! the fact, so the world learns a run happened from the same append-only
//! record a restart replays, never from a side channel:
//!
//! | Fact | Rungs appended under the run's key |
//! |---|---|
//! | admitted | `IntentAdmitted` |
//! | settled | `DispatchPrepared`, `OutcomeObserved`, `Verified` (the outcome) |
//! | handled by the world | `Verified` (the handled mark) |
//!
//! Five rungs per run, reserved whole at admission, so a run the journal
//! admitted can always be settled and handled: the journal's own ceiling
//! ([`MAX_JOURNAL_EVENTS`](crate::journal::MAX_JOURNAL_EVENTS)) bounds a feed
//! at a fifth of it, and an admission past that is refused rather than left
//! unsettleable.
//!
//! The world's bound system ([`RunFeed::bind`]) fires once per settled run
//! at the next tick, and the handled mark it appends afterwards is what a
//! restart reads: a feed reopened over the same journal queues exactly the runs
//! that settled and were never handled, so a kill between the two is fired
//! again (at least once) and a handled run is never fired twice.
//!
//! The feed names no driver (ADR 0006 decision 4): it is a journal and a queue
//! behind one lock, which any executor's tick reads.

use core::fmt;
use core::num::NonZeroU128;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use lgwks_std::hash::blake3;

use crate::broker::DispatchError;
use crate::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, EnvironmentId,
    FlowRevision, Id128, RunId,
};
use crate::error::BotError;
use crate::journal::{
    EffectEvent, EffectEvidence, EffectJournal, JournalError, Verification, VerificationResult,
};

use super::bot::EcsBot;

/// Rungs one run writes over its whole life: one admitted, three settled, one
/// handled. Reserved whole at admission.
const RUN_RUNGS: u64 = 5;

/// The most settled runs one tick fires, so a burst of settlements cannot hold
/// a tick for longer than a bounded batch; the rest fire on the next tick.
const RUNS_PER_TICK: usize = 1_024;

/// The high half of every run id a feed assigns: a domain tag, so a feed's
/// sequence-numbered ids never collide with a minted 128-bit run id in practice
/// and never with the all-zero id at all.
const FEED_TAG: u128 = 0x6c67_776b_7275_6e73;

/// A 128-bit constant as an identifier. The values below are literal and
/// non-zero, so the `None` arm is unreachable; it answers with the smallest id
/// rather than spending a panic on a proof the compiler cannot see.
const fn id(raw: u128) -> Id128 {
    match NonZeroU128::new(raw) {
        Some(value) => Id128::from_nonzero(value),
        None => Id128::from_nonzero(NonZeroU128::MIN),
    }
}

/// The one action every run-lifecycle key names.
const RUN_ACTION: ActionId = ActionId::new(id(0x6c67_776b_2e72_756e_732e_6c69_6665_6379));
/// The environment every run-lifecycle key names: the feed's journal is the
/// environment, so the constant is what lets a restart rebuild the same keys.
const RUN_ENVIRONMENT: EnvironmentId =
    EnvironmentId::new(id(0x6c67_776b_2e72_756e_732e_666e_6564_0001));
/// The predicate the settled rung's verification names; its version is the
/// outcome code.
const OUTCOME_PREDICATE: Id128 = id(0x6c67_776b_2e72_756e_732e_6f75_7463_6f6d);
/// The predicate the world's handled mark names.
const HANDLED_PREDICATE: Id128 = id(0x6c67_776b_2e72_756e_732e_6861_6e64_6c64);

/// How a run ended, as the world reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RunOutcome {
    /// The run's own work finished and returned success.
    Succeeded,
    /// The run's own work finished and reported a failure.
    Failed,
    /// The run stopped because its owner was cancelled.
    Cancelled,
    /// The run was admitted and then refused before its body ran.
    Refused,
    /// The run's body blocked on authority it did not hold.
    Blocked,
    /// The run's deadline elapsed.
    TimedOut,
    /// The run's body panicked.
    Panicked,
    /// The run was dropped before it finished.
    Aborted,
}

impl RunOutcome {
    /// Every outcome, in code order.
    const ALL: [Self; 8] = [
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::Refused,
        Self::Blocked,
        Self::TimedOut,
        Self::Panicked,
        Self::Aborted,
    ];

    /// The code the settled rung's verification carries as its version.
    const fn code(self) -> u64 {
        match self {
            Self::Succeeded => 1,
            Self::Failed => 2,
            Self::Cancelled => 3,
            Self::Refused => 4,
            Self::Blocked => 5,
            Self::TimedOut => 6,
            Self::Panicked => 7,
            Self::Aborted => 8,
        }
    }

    /// The outcome a recorded code names, or `None` for a code this build does
    /// not know, which a replay treats as a foreign record.
    fn from_code(code: u64) -> Option<Self> {
        Self::ALL.into_iter().find(|outcome| outcome.code() == code)
    }

    /// Whether the run's work happened at all: a refused or blocked run ran no
    /// body, which is the one fact `OutcomeObserved` records.
    const fn evidence(self) -> EffectEvidence {
        match self {
            Self::Refused | Self::Blocked => EffectEvidence::NotApplied,
            Self::Succeeded
            | Self::Failed
            | Self::Cancelled
            | Self::TimedOut
            | Self::Panicked
            | Self::Aborted => EffectEvidence::Applied,
        }
    }
}

/// One settled run, as the world's bound system receives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SettledRun {
    /// The run the feed assigned at admission.
    run: RunId,
    /// How it ended.
    outcome: RunOutcome,
}

impl SettledRun {
    /// The run the feed assigned at admission.
    #[must_use]
    pub const fn run(&self) -> RunId {
        self.run
    }

    /// How it ended.
    #[must_use]
    pub const fn outcome(&self) -> RunOutcome {
        self.outcome
    }
}

/// An admitted run, owed one settlement.
///
/// Not `Clone`: [`RunFeed::settle`] consumes it, so one admission settles once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunTicket {
    /// The run the feed assigned.
    run: RunId,
}

/// Why the feed refused a write.
#[derive(Debug)]
pub(crate) enum RunFeedError {
    /// The journal refused the append; nothing past the refused rung landed.
    Journal(JournalError),
    /// The feed's run sequence is spent: `u64::MAX` runs were admitted.
    RunsExhausted,
}

impl fmt::Display for RunFeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Journal(ref cause) => write!(f, "the run feed's journal refused: {cause}"),
            Self::RunsExhausted => f.write_str("the run feed has admitted u64::MAX runs"),
        }
    }
}

impl std::error::Error for RunFeedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Journal(ref cause) => Some(cause),
            Self::RunsExhausted => None,
        }
    }
}

impl From<JournalError> for RunFeedError {
    fn from(cause: JournalError) -> Self {
        Self::Journal(cause)
    }
}

/// The counts a feed has recorded, read in one lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RunCounts {
    /// Runs whose `admitted` event is in the journal.
    pub admitted: u64,
    /// Runs whose `settled` event is in the journal.
    pub settled: u64,
    /// Settled runs the world has fired and marked handled.
    pub handled: u64,
    /// Settled runs waiting for the world.
    pub due: usize,
}

/// The shared record of every run admitted and settled into one world.
///
/// Cloning is a handle: every clone writes the same journal and feeds the same
/// world. A host, a supervisor and the bot that binds it share one.
#[derive(Clone)]
pub struct RunFeed {
    /// The journal, the counts and the due queue.
    shared: Arc<Mutex<FeedState>>,
}

/// What a feed's handles share.
struct FeedState {
    /// The append-only record every fact goes through.
    journal: Box<dyn EffectJournal + Send>,
    /// The sequence the next admitted run takes.
    next: u64,
    /// The counts [`RunFeed::counts`] reports.
    counts: RunCounts,
    /// Settled runs the world has not fired, in settlement order. Bounded by
    /// the journal: each holds five reserved rungs.
    due: VecDeque<SettledRun>,
}

/// One run's facts, folded from a replay.
#[derive(Default)]
struct Replayed {
    /// The settlement, once its outcome rung is read.
    outcome: Option<RunOutcome>,
    /// Whether the world's handled mark was read.
    handled: bool,
}

impl RunFeed {
    /// Open a feed over `journal`, replaying what it already holds.
    ///
    /// Every run the journal shows admitted and settled is counted; a settled
    /// run with no handled mark is queued for the world, and the sequence
    /// continues past the highest run the journal names, so a restart never
    /// reissues a run id.
    ///
    /// # Errors
    ///
    /// The journal's own error when it cannot be read back.
    pub fn open(journal: Box<dyn EffectJournal + Send>) -> Result<Self, JournalError> {
        let committed = journal.committed()?;
        let mut runs: BTreeMap<RunId, Replayed> = BTreeMap::new();
        let mut order: Vec<RunId> = Vec::new();
        let mut counts = RunCounts::default();
        let mut next: u64 = 1;
        for event in &committed {
            let key = event.key();
            if key.action() != RUN_ACTION || key.environment() != RUN_ENVIRONMENT {
                continue;
            }
            let run = key.run();
            next = next.max(sequence_of(run).saturating_add(1));
            match *event {
                EffectEvent::IntentAdmitted { .. } => {
                    counts.admitted = counts.admitted.saturating_add(1);
                    runs.entry(run).or_default();
                }
                EffectEvent::Verified { verification, .. } => {
                    let held = runs.entry(run).or_default();
                    if verification.predicate() == OUTCOME_PREDICATE {
                        if let Some(outcome) =
                            RunOutcome::from_code(verification.predicate_version())
                            && held.outcome.is_none()
                        {
                            held.outcome = Some(outcome);
                            counts.settled = counts.settled.saturating_add(1);
                            order.push(run);
                        }
                    } else if verification.predicate() == HANDLED_PREDICATE && !held.handled {
                        held.handled = true;
                        counts.handled = counts.handled.saturating_add(1);
                    }
                }
                EffectEvent::DispatchPrepared { .. } | EffectEvent::OutcomeObserved { .. } => {}
            }
        }
        let due: VecDeque<SettledRun> = order
            .into_iter()
            .filter_map(|run| {
                let held = runs.get(&run)?;
                match (held.outcome, held.handled) {
                    (Some(outcome), false) => Some(SettledRun { run, outcome }),
                    _ => None,
                }
            })
            .collect();
        counts.due = due.len();
        Ok(Self {
            shared: Arc::new(Mutex::new(FeedState {
                journal,
                next,
                counts,
                due,
            })),
        })
    }

    /// Record a run as admitted, reserving the rungs it will need to settle
    /// and be handled.
    ///
    /// # Errors
    ///
    /// [`RunFeedError::Journal`] when the journal cannot hold the run's five
    /// rungs or refuses the append, and [`RunFeedError::RunsExhausted`] when the
    /// sequence is spent. Either way nothing was recorded.
    pub(crate) fn admit(&self) -> Result<RunTicket, RunFeedError> {
        let mut state = self.lock();
        state.journal.reserve_handoff_capacity(RUN_RUNGS)?;
        let sequence = state.next;
        let following = sequence.checked_add(1).ok_or(RunFeedError::RunsExhausted)?;
        let run = run_of(sequence);
        state.append(&EffectEvent::IntentAdmitted { key: key_of(run) })?;
        state.next = following;
        state.counts.admitted = state.counts.admitted.saturating_add(1);
        Ok(RunTicket { run })
    }

    /// Record an admitted run as settled, due to the world's next tick.
    ///
    /// # Errors
    ///
    /// [`RunFeedError::Journal`] when the append is refused. The three rungs
    /// are one batch, so a durable journal records all of them or none, and a
    /// replay reads a refused settlement as admitted and unsettled.
    pub(crate) fn settle(
        &self,
        ticket: RunTicket,
        outcome: RunOutcome,
    ) -> Result<(), RunFeedError> {
        let key = key_of(ticket.run);
        let mut state = self.lock();
        state.append_batch(&[
            EffectEvent::DispatchPrepared { key },
            EffectEvent::OutcomeObserved {
                key,
                evidence: outcome.evidence(),
            },
            EffectEvent::Verified {
                key,
                verification: mark(OUTCOME_PREDICATE, outcome.code(), ticket.run, outcome),
            },
        ])?;
        state.counts.settled = state.counts.settled.saturating_add(1);
        state.due.push_back(SettledRun {
            run: ticket.run,
            outcome,
        });
        state.counts.due = state.due.len();
        Ok(())
    }

    /// The counts this feed has recorded.
    #[must_use]
    pub fn counts(&self) -> RunCounts {
        self.lock().counts
    }

    /// Up to `limit` settled runs the world has not fired, oldest first,
    /// without taking them.
    fn due(&self, limit: usize) -> Vec<SettledRun> {
        self.lock().due.iter().take(limit).copied().collect()
    }

    /// Mark `runs`, the front of the due queue, handled in the journal as one
    /// batch, then drop them from the queue.
    ///
    /// The marks are appended first: a batch that failed leaves every run due,
    /// so each fires again rather than being lost (at least once).
    fn handled(&self, runs: &[SettledRun]) -> Result<(), RunFeedError> {
        let marks: Vec<EffectEvent> = runs
            .iter()
            .map(|run| EffectEvent::Verified {
                key: key_of(run.run),
                verification: mark(HANDLED_PREDICATE, 1, run.run, run.outcome),
            })
            .collect();
        let mut state = self.lock();
        state.append_batch(&marks)?;
        for run in runs {
            if state.due.front() == Some(run) {
                state.due.pop_front();
            }
            state.counts.handled = state.counts.handled.saturating_add(1);
        }
        state.counts.due = state.due.len();
        Ok(())
    }

    /// The shared state, whatever a panicking holder left behind: every write
    /// here lands in the journal before the counts move, so the state a panic
    /// interrupts is the state a replay would rebuild.
    fn lock(&self) -> MutexGuard<'_, FeedState> {
        match self.shared.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl fmt::Debug for RunFeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunFeed")
            .field("counts", &self.counts())
            .finish_non_exhaustive()
    }
}

impl FeedState {
    /// Append one rung at the journal's own tail.
    fn append(&mut self, event: &EffectEvent) -> Result<(), JournalError> {
        let tail = self.journal.tail();
        self.journal.compare_and_append(tail, event).map(drop)
    }

    /// Append rungs as one batch: one sync on a journal that can make it one.
    fn append_batch(&mut self, events: &[EffectEvent]) -> Result<(), JournalError> {
        self.journal.append_batch(events).map(drop)
    }
}

/// The run id the feed assigns to sequence `sequence`.
fn run_of(sequence: u64) -> RunId {
    RunId::new(id((FEED_TAG << 64) | u128::from(sequence)))
}

/// The sequence a feed-assigned run id carries in its low eight bytes.
fn sequence_of(run: RunId) -> u64 {
    let bytes = run.id().get().get().to_be_bytes();
    let (_tag, low) = bytes.split_at(8);
    let mut sequence = [0_u8; 8];
    sequence.copy_from_slice(low);
    u64::from_be_bytes(sequence)
}

/// The one key every rung of `run` is written under.
fn key_of(run: RunId) -> EffectKey {
    EffectIdentity::new(
        run,
        RUN_ENVIRONMENT,
        FlowRevision::new(blake3(b"lgwks.runs.v1.flow")),
    )
    .key(
        RUN_ACTION,
        AttemptId::FIRST,
        ActionDigest::new(blake3(b"lgwks.runs.v1.lifecycle")),
        EnvironmentEpoch::FIRST,
    )
}

/// A verification rung over `run` and `outcome`.
fn mark(predicate: Id128, version: u64, run: RunId, outcome: RunOutcome) -> Verification {
    let mut observed = [0_u8; 24];
    let (head, tail) = observed.split_at_mut(16);
    head.copy_from_slice(&run.id().get().get().to_be_bytes());
    tail.copy_from_slice(&outcome.code().to_be_bytes());
    let result = if matches!(outcome, RunOutcome::Succeeded) {
        VerificationResult::Satisfied
    } else {
        VerificationResult::NotSatisfied
    };
    Verification::new(predicate, version, blake3(&observed), result)
}

/// The world's bound system and the feed it drains.
pub(super) struct RunHooks {
    /// The feed whose settled runs this world fires.
    feed: RunFeed,
    /// The system bound to `settled`.
    on_settled: Box<dyn FnMut(SettledRun)>,
}

impl RunFeed {
    /// Bind this feed to `world`: every settled run fires `on_settled` once, at
    /// the start of the world's next tick, and is then marked handled in the
    /// journal.
    ///
    /// Binding again replaces the world's previous feed and system. A feed
    /// reopened after a kill fires only the runs that settled and were never
    /// handled.
    pub fn bind(&self, world: &mut EcsBot, on_settled: impl FnMut(SettledRun) + 'static) {
        world.world.insert_non_send(RunHooks {
            feed: self.clone(),
            on_settled: Box::new(on_settled),
        });
    }
}

impl EcsBot {
    /// Fire the bound system for up to [`RUNS_PER_TICK`] settled runs, then
    /// mark them handled in one journal batch: one sync per tick, not per run.
    ///
    /// Taken out of the world while it fires, so the system can run without a
    /// borrow of the world, and put back however the batch ends. A kill between
    /// the firing and the batch refires this tick's runs after the restart:
    /// firing is at least once, and a run whose mark landed never fires again.
    pub(super) fn fire_settled(&mut self) -> Result<usize, BotError> {
        let Some(mut hooks) = self.world.remove_non_send::<RunHooks>() else {
            return Ok(0);
        };
        let due = hooks.feed.due(RUNS_PER_TICK);
        for &run in &due {
            (hooks.on_settled)(run);
        }
        let marked = if due.is_empty() {
            Ok(())
        } else {
            hooks.feed.handled(&due)
        };
        self.world.insert_non_send(hooks);
        match marked {
            Ok(()) => Ok(due.len()),
            Err(RunFeedError::Journal(cause)) => Err(BotError::EffectRefused {
                cause: DispatchError::Journal(cause),
            }),
            Err(RunFeedError::RunsExhausted) => Err(BotError::EffectRefused {
                cause: DispatchError::Journal(JournalError::Exhausted),
            }),
        }
    }
}
