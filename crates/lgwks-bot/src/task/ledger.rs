//! The durable per-run control ledger: what a repair has already consumed.
//!
//! # Why this is a second store rather than more step records
//!
//! A step record answers "what did this step return?" and is keyed by
//! `(run, step key)`; it is written once and replayed verbatim, because a
//! completed step's value is a fact, not a state. The three facts this ledger
//! holds are the opposite of that. The root **spend** and **attempt budget** are
//! cumulative counters that a repair must *not* reset, the repair **epoch** is
//! advanced by every applied ticket, and an **applied ticket** must be refused a
//! second time rather than replayed. All four are read-modify-write across a
//! resume, which is not what a step record is for and cannot be expressed by
//! replaying one. So they get their own file, its own chain, and its own record.
//!
//! The frame grammar ([`journal::frame`]) and the storage-owner thread
//! ([`journal::owner`]) are the ones the step store already uses, exactly as
//! INV-BOT-51 requires: one answer in this crate to "what is a frame" and to
//! "which thread reaches the disk", not one per store.
//!
//! # The budget is a root budget, and a repair spends it
//!
//! `attempts` and `spend` are the *root* counters. They are charged on every run
//! attempt and are never reduced by a repair, a replay or a fresh resume — so a
//! run that has spent its budget stays spent even after an authorized repair,
//! which is T13's "authorized repair is distinct and root budgets remain
//! charged". A run whose budget is already spent refuses another attempt with a
//! typed [`LeaseRefusal::BudgetSpent`] rather than resetting, so a permanent
//! refusal plus repeated `NotApplied` reaches a finite typed refusal instead of
//! unbounded retries.
//!
//! # One ordered step decides and writes
//!
//! Whether a repair is valid (is the epoch current, was this ticket already
//! applied, is the budget spent) and the act that makes it true (charge the
//! budget, advance the epoch, record the ticket) are decided and performed in one
//! step on the ledger's own thread. A caller that read the state, decided, and
//! charged later could decide against bytes another repair has already moved; the
//! single ordered step is what makes the check and the write un-overtakable, the
//! same property the step store's length fence rests on.

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::wire::{WireError, from_bytes, to_bytes};

use crate::effect::RunId;
use crate::journal::frame::{self, HEAD_BYTES};
use crate::journal::owner::{self, Stage, StorageOwner, SubmitError};

use super::store::{StoreError, StoreLimitKind};

/// The magic at the head of every ledger file.
///
/// Distinct from the step store's magic so a ledger is never opened as a step
/// store or the reverse: the two record different things and reading one as the
/// other would hand a caller records it did not write. The trailing byte is this
/// format's version.
const LEDGER_MAGIC: &[u8; 17] = b"lgwks-runledger\x00\x01";

/// The most archived bytes one ledger entry may occupy.
///
/// A ledger entry is a counter update or an epoch/ticket note — small by
/// construction — so this ceiling exists only to bound a hostile or runaway
/// value before any allocation, and is far below the step store's own record
/// ceiling.
pub const MAX_LEDGER_RECORD_BYTES: usize = 16 * 1024;

/// The most ledger entries one run's chain may hold.
///
/// A run's ledger is a handful of budget charges and repair notes. Sixty-four
/// thousand is generous enough that no honest run reaches it and small enough
/// that a runaway loop is refused rather than filling the disk.
pub const MAX_LEDGER_RECORDS_PER_RUN: u64 = 65_536;

/// The chain digest a ledger starts from, hashed through the same framing as any
/// entry so the first head is a real chain step rather than a constant every
/// file shares.
fn genesis_head() -> Digest {
    let mut hasher = Hasher::new();
    hasher.write_framed(b"lgwks-runledger/genesis");
    hasher.finalize()
}

/// One durable ledger entry: the run it belongs to and the control state it
/// leaves behind.
///
/// The counters are cumulative and absolute — the writer reads the current state,
/// computes the new one, and stores the result — so replaying the whole chain
/// reproduces exactly the counters and the epoch a reader would see live. That is
/// what makes the ledger idempotent under a crash that lost the acknowledgment but
/// kept the bytes: the entry is the fact, and reading it back is the truth.
#[derive(
    Debug, Clone, lgwks_std::wire::Archive, lgwks_std::wire::Serialize, lgwks_std::wire::Deserialize,
)]
#[rkyv(
    attr(non_exhaustive),
    crate = lgwks_std::wire::rkyv,
    compare(PartialEq),
    derive(Debug)
)]
struct Entry {
    /// The run this entry belongs to.
    #[rkyv(attr(doc = "The run this entry belongs to."))]
    run: RunId,
    /// The tenant that owns the run; the only tenant allowed to read it.
    #[rkyv(attr(doc = "The tenant that owns the run."))]
    tenant: String,
    /// The root attempts this run has been charged, in total.
    #[rkyv(attr(doc = "The root attempts this run has been charged, in total."))]
    attempts: u64,
    /// The root spend this run has been charged, in total.
    #[rkyv(attr(doc = "The root spend this run has been charged, in total."))]
    spend: u64,
    /// The repair epoch this entry leaves the run at.
    #[rkyv(attr(doc = "The repair epoch this entry leaves the run at."))]
    epoch: u64,
    /// The identity of the repair ticket this entry applied, when it applied one.
    #[rkyv(attr(doc = "The repair ticket identity this entry applied, if any."))]
    applied: Option<Vec<u8>>,
}

impl Entry {
    /// This entry's chain head over the head before it.
    ///
    /// Framed exactly as the step store frames its record, so a ledger frame and
    /// a step frame cannot hash the same byte string: the run, the tenant and
    /// the applied ticket are each length-framed, and the counters follow.
    fn head_from(&self, previous: &Digest) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(previous.as_bytes());
        hasher.write_framed(self.run.id().to_hex().as_bytes());
        hasher.write_framed(self.tenant.as_bytes());
        hasher.write_framed(&self.attempts.to_le_bytes());
        hasher.write_framed(&self.spend.to_le_bytes());
        hasher.write_framed(&self.epoch.to_le_bytes());
        hasher.write_framed(match self.applied {
            Some(ref ticket) => ticket.as_slice(),
            None => &[],
        });
        hasher.finalize()
    }
}

/// The run-keyed index one ledger holds, folded in the storage owner's ordered
/// step alongside each write.
#[derive(Debug)]
struct Index {
    /// Per run, its live control state.
    runs: HashMap<RunId, Control>,
    /// The file length every indexed entry accounts for.
    committed: u64,
    /// The chain head the next entry follows.
    tail: Digest,
}

/// One run's live control state, as replay and live updates agree on it.
///
/// The four facts a repair has to be decided against, read as a value rather than
/// through four accessors: a caller checking a budget wants the whole state at
/// once, and a half-read state is exactly the disagreement this ledger exists to
/// prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    /// The tenant that owns the run.
    tenant: String,
    /// The root attempts charged so far, never reset by a repair or resume.
    attempts: u64,
    /// The root spend charged so far, never reset by a repair or resume.
    spend: u64,
    /// The current repair epoch. Zero is the epoch of a run no repair has
    /// touched, which is the epoch every ticket minted before any repair names.
    epoch: u64,
    /// The identities of repair tickets already applied to this run, so the same
    /// ticket delivered twice is recognized and refused the second time.
    applied: Vec<Vec<u8>>,
}

impl Control {
    /// The control state for a run this ledger has never seen, owned by
    /// `tenant`.
    fn fresh(tenant: &str) -> Self {
        Self {
            tenant: tenant.to_owned(),
            attempts: 0,
            spend: 0,
            epoch: 0,
            applied: Vec::new(),
        }
    }

    /// The tenant that owns the run.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// The root attempts charged so far, never reset by a repair or a resume.
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }

    /// The root spend charged so far, never reset by a repair or a resume.
    #[must_use]
    pub const fn spend(&self) -> u64 {
        self.spend
    }

    /// The current repair epoch. Zero for a run no repair has touched.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many repair tickets have been applied to this run.
    #[must_use]
    pub fn applied(&self) -> usize {
        self.applied.len()
    }
}

/// The repair ticket a charge is carrying, as the ledger sees it.
///
/// An identity and the epoch it was minted at. The identity is what makes a
/// duplicate recognizable and the epoch is what makes a stale ticket stale; both
/// are the ticket's facts, carried rather than re-derived here, because the
/// ledger cannot know what a caller's ticket "means" — only whether it has seen
/// it before.
///
/// Crate-private, because it is the ledger's own vocabulary: a caller holds a
/// [`RepairTicket`](super::RepairTicket) and never a stamp, and a stamp a caller
/// could rewrite would be a way to forge "the same ticket" or "a fresh epoch"
/// without going through the ticket's content-derived identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TicketStamp {
    /// The identity of the ticket, used only for duplicate recognition.
    pub(crate) identity: Vec<u8>,
    /// The repair epoch the ticket was minted at.
    pub(crate) epoch: u64,
}

/// Why a lease could not be taken. Every refusal charges nothing and mints no
/// epoch, so a refused repair leaves the run's authority and budget exactly where
/// they were.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaseRefusal {
    /// The run belongs to a tenant other than the one asking.
    ForeignTenant {
        /// The tenant that owns the run.
        owner: String,
        /// The tenant that asked.
        asked: String,
    },
    /// The repair epoch on the ticket is not the run's current epoch, so the
    /// ticket was minted against a different repair state. An older ticket is
    /// stale — replaying it would re-apply an earlier authority; a newer one was
    /// minted against a state this run is not in. Both are refused by the same
    /// check, so "stale" names the disagreement rather than the direction.
    StaleEpoch {
        /// The epoch the run is at.
        current: u64,
        /// The epoch the ticket names.
        offered: u64,
    },
    /// The same repair ticket has already been applied to this run. Delivering a
    /// ticket twice must apply its effect once.
    AlreadyApplied,
    /// The root budget this run carries is already spent; a repair does not
    /// refill it.
    BudgetSpent {
        /// The attempts that would have been charged.
        attempts: u64,
        /// The largest attempt count admitted.
        max_attempts: u64,
    },
}

impl std::error::Error for LeaseRefusal {}

impl fmt::Display for LeaseRefusal {
    /// The refusal, naming both sides of the disagreement.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ForeignTenant {
                ref owner,
                ref asked,
            } => write!(
                formatter,
                "run belongs to tenant {owner:?}, not {asked:?}; refusing to read its repair ledger"
            ),
            Self::StaleEpoch { current, offered } => write!(
                formatter,
                "repair epoch {offered} is not this run's current epoch {current}; \
                 refusing a stale repair ticket"
            ),
            Self::AlreadyApplied => formatter.write_str(
                "this repair ticket was already applied to this run; refusing to apply it twice",
            ),
            Self::BudgetSpent {
                attempts,
                max_attempts,
            } => write!(
                formatter,
                "root budget spent: {attempts} attempts against a ceiling of {max_attempts}; \
                 an authorized repair does not refill it"
            ),
        }
    }
}

/// One durable, file-backed set of per-run repair controls.
///
/// Cloning shares the file and the index through an `Arc`, so a host's handles
/// see one ledger rather than one per clone — the same rule the step store's
/// admission budget follows, and for the same reason: two writers over one chain
/// would fork it.
#[derive(Clone)]
pub struct RunLedger {
    /// Where the ledger lives and the index every clone shares.
    inner: Arc<Inner>,
}

/// The state one ledger owns, shared by every clone of its handle.
struct Inner {
    /// The thread that holds the file and performs every ordered step. Its
    /// answer is the control state a charge leaves behind, so a caller reads the
    /// result of its own decision rather than a second lookup that could race
    /// another charge.
    owner: StorageOwner<Arc<Mutex<Index>>, Control>,
    /// Where the file is, for diagnostics.
    path: PathBuf,
    /// The index every clone reads and the owner folds into.
    index: Arc<Mutex<Index>>,
}

impl RunLedger {
    /// Open (or create) the ledger at `path`.
    ///
    /// The file is created with a header if it does not exist and replayed if it
    /// does, exactly as the step store opens its own file. A torn final entry is
    /// dropped (it was never anyone's answer) and every earlier entry survives;
    /// a committed entry whose head does not follow is refused rather than
    /// trimmed, because those bytes were acknowledged.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the device refuses, when `path` is not a ledger, or
    /// when a committed entry does not follow from the ones before it.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(StoreError::storage)?;
        if !existed || file.metadata().map_err(StoreError::storage)?.len() == 0 {
            file.write_all(LEDGER_MAGIC)
                .and_then(|()| file.sync_all())
                .map_err(StoreError::storage)?;
        }
        let index = replay(&mut file)?;
        if file.metadata().map_err(StoreError::storage)?.len() != index.committed {
            file.set_len(index.committed).map_err(StoreError::storage)?;
            file.sync_all().map_err(StoreError::storage)?;
        }
        let index = Arc::new(Mutex::new(index));
        let owner =
            StorageOwner::spawn(file, Arc::clone(&index), false).map_err(StoreError::storage)?;
        Ok(Self {
            inner: Arc::new(Inner { owner, path, index }),
        })
    }

    /// Open a ledger under `dir`, named by the tenant that owns it, so two
    /// tenants pointed at one directory never share a file.
    ///
    /// # Errors
    ///
    /// As [`RunLedger::open`].
    pub fn open_in(dir: &Path, tenant: &str) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).map_err(StoreError::storage)?;
        Self::open(dir.join(format!("{tenant}.runledger")))
    }

    /// Where this ledger lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// The control state `run` currently holds, or `None` when this ledger has
    /// never seen it.
    ///
    /// A run the ledger has *never seen* is `None`, not a zeroed control state:
    /// the difference between "no budget charged" and "no run here" is the
    /// difference between a first attempt and somebody else's run, and a caller
    /// must be able to tell them apart before it charges anything.
    #[must_use]
    pub fn control(&self, run: RunId) -> Option<Control> {
        owner::lock(&self.inner.index).runs.get(&run).cloned()
    }

    /// Perform one ordered step on `run`: decide the ticket against the current
    /// state and, if it is admissible, charge the budget and apply it.
    ///
    /// `ticket` is `Some` for a repair and `None` for a plain resume. A repair
    /// must carry the epoch it was minted at the run's *current* epoch and an
    /// identity this run has not seen; an accepted repair advances the epoch and
    /// records its identity, which is what makes the same ticket a second time
    /// both stale and a duplicate.
    ///
    /// `spend` is charged against the root budget whether or not a ticket was
    /// carried: a repair is an attempt, and an authorized repair does not refund
    /// the run for the attempts that led to it.
    ///
    /// # Errors
    ///
    /// [`CommitError::Refused`] for every logical refusal — the four
    /// [`LeaseRefusal`] arms — charging nothing, and [`CommitError::Store`] when
    /// the ledger's device refuses the write.
    ///
    /// The future is `Send`: this is awaited on the host's own path, which the
    /// engine may drive on a multi-threaded runtime whatever the task body is, so
    /// the answer travels the owner's concrete awaiting future rather than the
    /// non-`Send` box a step body would want.
    pub(crate) async fn charge(
        &self,
        tenant: &str,
        run: RunId,
        ticket: Option<&TicketStamp>,
        spend: u64,
        max_attempts: u64,
        max_spend: u64,
    ) -> Result<Control, CommitError> {
        let tenant = tenant.to_owned();
        let stamp = ticket.cloned();
        self.inner
            .owner
            .enqueue_awaiting(move |file, shared| {
                charge_on_owner(
                    file,
                    shared,
                    &tenant,
                    run,
                    stamp.as_ref(),
                    spend,
                    max_attempts,
                    max_spend,
                )
            })
            .await
            .map_err(classify)
    }
}

/// Tell a logical refusal from a device one, on the way out of the owner.
///
/// The owner's one reply channel is an `io::Error`, so a decision made on its
/// thread travels as one — but a refusal that reached the caller as a string would
/// be exactly the flattening this crate's typed refusals exist to prevent: a
/// caller could not tell "this ticket was already applied" from "the device
/// refused". So the refusal is carried as the error itself (it implements
/// [`std::error::Error`]) and recovered here by downcast, and only a genuinely
/// unrecognised error becomes a [`StoreError`].
fn classify(cause: SubmitError) -> CommitError {
    let is_ours = matches!(
        cause,
        SubmitError::Device(ref error)
            if error
                .get_ref()
                .and_then(<dyn std::error::Error + Send + Sync>::downcast_ref::<LeaseRefusal>)
                .is_some()
    );
    if !is_ours {
        return CommitError::Store(StoreError::from(cause));
    }
    match cause {
        SubmitError::Device(error) => match error.into_inner() {
            Some(inner) => match inner.downcast::<LeaseRefusal>() {
                Ok(refusal) => CommitError::Refused(*refusal),
                Err(other) => CommitError::Store(StoreError::storage(std::io::Error::other(other))),
            },
            None => CommitError::Store(StoreError::storage(std::io::Error::other(
                "the ledger owner refused a charge without naming why",
            ))),
        },
        other => CommitError::Store(StoreError::from(other)),
    }
}

/// The one ordered step: decide the charge against the ledger's own state, write
/// the entry, fold it into the index, and answer the state it left behind.
///
/// The decision and the write are here together rather than in two functions so
/// no caller can decide against one state and write against another: the whole
/// run holds the index lock, and a second charge cannot enter until this one has
/// written and answered. That is what makes "this ticket was not applied twice"
/// and "the epoch did not move twice" facts about bytes rather than about the
/// order two threads happened to run in.
///
/// # Errors
///
/// Whatever the device or the ledger's own check reports, carried as the
/// device's own error so the owner's one reply channel serves this store the way
/// it serves the step store. A logical refusal is decided before any byte moves,
/// so a refused charge leaves the file byte-identical.
///
/// The step performs its own `sync_all` and fold under the index lock before it
/// returns, so it answers [`Stage::Committed`]: it owes the batch's flush nothing,
/// because the one ordered step that makes "applied once" a fact about bytes is
/// the same step that flushed it. A later member of a shared batch must decide
/// against the fold this charge left, which is why the fold cannot be deferred to
/// a batch settle the way a step record's can.
#[expect(
    clippy::too_many_arguments,
    reason = "the ledger's state is the file and the index, and the charge is the tenant, \
              the run, the optional ticket, the spend and the two ceilings — each is a \
              distinct fact the ordered step needs and none of them is derivable from another"
)]
fn charge_on_owner(
    file: &mut File,
    shared: &Arc<Mutex<Index>>,
    tenant: &str,
    run: RunId,
    ticket: Option<&TicketStamp>,
    spend: u64,
    max_attempts: u64,
    max_spend: u64,
) -> std::io::Result<Stage<Control, Arc<Mutex<Index>>>> {
    let mut index = owner::lock(shared);
    let (entry, next) = decide_under(&index, tenant, run, ticket, spend, max_attempts, max_spend)
        .map_err(std::io::Error::other)?;
    write_entry(file, &mut index, &entry)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(Stage::Committed(next))
}

/// The decision, over the ledger's own state: what charging `run` would leave, and
/// the entry that would say so.
///
/// Returns the entry *and* the state beside it rather than only the state, so the
/// write cannot be skipped by a caller that already knows the answer.
fn decide_under(
    index: &Index,
    tenant: &str,
    run: RunId,
    ticket: Option<&TicketStamp>,
    spend: u64,
    max_attempts: u64,
    max_spend: u64,
) -> Result<(Entry, Control), LeaseRefusal> {
    let mut next = index
        .runs
        .get(&run)
        .cloned()
        .unwrap_or_else(|| Control::fresh(tenant));

    // A run that already belongs to someone else is refused before any counter
    // moves. This is the same cross-tenant rule the step store enforces, and for
    // the same reason: charging another tenant's run under this one's name is how
    // a shared ledger becomes a cross-tenant write.
    if next.tenant != tenant {
        let refusal = Err(LeaseRefusal::ForeignTenant {
            owner: next.tenant,
            asked: tenant.to_owned(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_under: returning an error to the caller");
        return refusal;
    }

    let next_attempts = next.attempts.saturating_add(1);
    let next_spend = next.spend.saturating_add(spend);
    if next_attempts > max_attempts || next_spend > max_spend {
        let refusal = Err(LeaseRefusal::BudgetSpent {
            attempts: next_attempts,
            max_attempts,
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_under: returning an error to the caller");
        return refusal;
    }

    let mut applied = None;
    if let Some(stamp) = ticket {
        // A ticket minted at an epoch other than the run's current one was minted
        // against a repair state this run is not in. Applying it would replay an
        // authority the run has already moved past, so it is refused; a newer
        // ticket is refused by the same check, so "stale" names the disagreement
        // rather than the direction.
        if stamp.epoch != next.epoch {
            let refusal = Err(LeaseRefusal::StaleEpoch {
                current: next.epoch,
                offered: stamp.epoch,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_under: returning an error to the caller");
            return refusal;
        }
        // The same ticket delivered twice is one repair applied once. The identity
        // is checked against the applied set on this run, so a different ticket
        // for the same run is unaffected.
        if next.applied.contains(&stamp.identity) {
            let refusal = Err(LeaseRefusal::AlreadyApplied);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_under: returning an error to the caller");
            return refusal;
        }
        // Applying a valid repair advances the epoch, which is what makes the
        // *next* delivery of this same ticket stale as well as duplicate: two
        // independent checks, either of which would catch a replay.
        next.epoch = next.epoch.saturating_add(1);
        next.applied.push(stamp.identity.clone());
        applied = Some(stamp.identity.clone());
    }
    next.attempts = next_attempts;
    next.spend = next_spend;

    let entry = Entry {
        run,
        tenant: tenant.to_owned(),
        attempts: next.attempts,
        spend: next.spend,
        epoch: next.epoch,
        applied,
    };
    Ok((entry, next))
}

/// The write and the fold, in one ordered step, under the index the owner holds.
fn write_entry(file: &mut File, index: &mut Index, entry: &Entry) -> Result<(), StoreError> {
    let previous = index.tail;
    let (framed, head) = frame(entry, &previous)?;
    let staged = u64::try_from(framed.len()).unwrap_or(u64::MAX);
    let next = index
        .committed
        .checked_add(staged)
        .ok_or(StoreError::Limit {
            kind: StoreLimitKind::StoreBytes,
            requested: u64::MAX,
            limit: super::MAX_STORE_BYTES,
        })?;
    // The length fence, before a byte moves, under the ledger's thread where no
    // other charge can overtake it: the length this handle indexed must be the
    // length on the disk.
    let on_disk = file.metadata().map_err(StoreError::storage)?.len();
    if on_disk != index.committed {
        let refusal = Err(StoreError::Corrupt { at: 0 });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "write_entry: returning an error to the caller");
        return refusal;
    }
    file.write_all(&framed).map_err(StoreError::storage)?;
    file.sync_all().map_err(StoreError::storage)?;
    index.committed = next;
    index.tail = head;
    fold(index, entry);
    Ok(())
}

/// Fold one entry's control state into `index`, creating the run's slot if this
/// is the first entry for it.
///
/// One function because the live write and the replay must agree exactly: a state
/// reached by committing an entry and the same state reached by reading it back
/// have to be the same value, or a ledger's answer would depend on whether the
/// reader crashed. The three counters are assigned rather than accumulated
/// because an entry is the *absolute* state, and the applied-ticket list is
/// extended rather than replaced for the same reason: a ticket applied by an
/// earlier entry is still applied.
fn fold(index: &mut Index, entry: &Entry) {
    let run = index.runs.entry(entry.run).or_insert_with(|| Control {
        tenant: entry.tenant.clone(),
        attempts: 0,
        spend: 0,
        epoch: 0,
        applied: Vec::new(),
    });
    run.tenant.clone_from(&entry.tenant);
    run.attempts = entry.attempts;
    run.spend = entry.spend;
    run.epoch = entry.epoch;
    if let Some(ref ticket) = entry.applied
        && !run.applied.contains(ticket)
    {
        run.applied.push(ticket.clone());
    }
}

/// The frame for one ledger entry: length, payload, head, through the shared
/// grammar.
fn frame(entry: &Entry, previous: &Digest) -> Result<(Vec<u8>, Digest), StoreError> {
    frame::frame_record(
        entry,
        previous,
        MAX_LEDGER_RECORD_BYTES,
        |entry| {
            to_bytes::<WireError>(entry)
                .map(|bytes| bytes.as_ref().to_vec())
                .map_err(|cause| StoreError::Encoding { cause })
        },
        |entry, previous, _| entry.head_from(previous),
        |len| StoreError::Limit {
            kind: StoreLimitKind::RecordBytes,
            requested: u64::try_from(len).unwrap_or(u64::MAX),
            limit: u64::try_from(MAX_LEDGER_RECORD_BYTES).unwrap_or(u64::MAX),
        },
    )
}

/// One complete, decoded frame read from the ledger.
struct Framed {
    /// The entry the frame's payload decoded to.
    entry: Entry,
    /// The chain head the frame recorded for itself.
    head: [u8; HEAD_BYTES],
    /// The payload length the frame declared.
    declared: usize,
}

/// Read the frame at ordinal `at`, or `None` where the file stops holding a
/// whole frame.
///
/// A partial length prefix, or a payload or head cut short, is an append that
/// never finished: it was never anyone's answer, so the scan stops and the
/// caller trims to `committed`. A complete prefix that names a frame this
/// ledger never writes, or a payload that does not decode, cannot be an
/// interrupted append and is refused.
fn next_frame(file: &mut File, at: u64) -> Result<Option<Framed>, StoreError> {
    let corrupt = || StoreError::Corrupt { at };
    let Some(raw) = frame::read_raw(file, MAX_LEDGER_RECORD_BYTES, StoreError::storage, corrupt)?
    else {
        return Ok(None);
    };
    let entry = from_bytes::<Entry, WireError>(&raw.payload).map_err(|error| {
        lgwks_std::trace::debug!(?error, at, "next_frame: the payload did not decode");
        StoreError::Corrupt { at }
    })?;
    Ok(Some(Framed {
        entry,
        declared: raw.payload.len(),
        head: raw.head,
    }))
}

/// Read every committed entry, returning the index it implies.
fn replay(file: &mut File) -> Result<Index, StoreError> {
    let total = file.metadata().map_err(StoreError::storage)?.len();
    file.seek(SeekFrom::Start(0)).map_err(StoreError::storage)?;
    let mut header = [0u8; LEDGER_MAGIC.len()];
    if !read_full(file, &mut header)? || header != *LEDGER_MAGIC {
        let refusal = Err(StoreError::NotAStore);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "replay: returning an error to the caller");
        return refusal;
    }
    let mut index = Index {
        runs: HashMap::new(),
        committed: u64::try_from(LEDGER_MAGIC.len()).unwrap_or(u64::MAX),
        tail: genesis_head(),
    };
    let mut previous = genesis_head();
    let mut at = 0u64;
    while let Some(Framed {
        entry,
        head,
        declared,
    }) = next_frame(file, at)?
    {
        if entry.head_from(&previous) != Digest::from_bytes(head) {
            let refusal = Err(StoreError::Corrupt { at });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "replay: returning an error to the caller");
            return refusal;
        }
        let frame_len = frame::framed_len(declared);
        if index
            .committed
            .checked_add(frame_len)
            .is_none_or(|end| end > total)
        {
            break;
        }
        index.committed = index.committed.saturating_add(frame_len);
        index.tail = entry.head_from(&previous);
        previous = index.tail;
        at = at.saturating_add(1);
        if at > MAX_LEDGER_RECORDS_PER_RUN {
            let refusal = Err(StoreError::Limit {
                kind: StoreLimitKind::Records,
                requested: at,
                limit: MAX_LEDGER_RECORDS_PER_RUN,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "replay: returning an error to the caller");
            return refusal;
        }
        fold(&mut index, &entry);
    }
    Ok(index)
}

/// Read `buf` in full, reporting whether all of it arrived.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, StoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(cause) => {
                let refusal = Err(StoreError::storage(cause));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_full: returning an error to the caller");
                return refusal;
            }
        }
    }
    Ok(true)
}

/// Why charging a run failed: a device refusal, or a logical one.
#[derive(Debug)]
pub enum CommitError {
    /// The ledger's device refused the write. Nothing was charged.
    Store(StoreError),
    /// The charge was refused on its merits. Nothing was charged and no epoch was
    /// minted, so the run's authority and budget are exactly where they were.
    Refused(LeaseRefusal),
}

impl fmt::Display for CommitError {
    /// The refusal, whether it came from the device or from the decision.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Store(ref cause) => write!(formatter, "{cause}"),
            Self::Refused(ref cause) => write!(formatter, "{cause}"),
        }
    }
}

impl std::error::Error for CommitError {
    /// The device's own error, when the refusal came from the device.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Store(ref cause) => Some(cause),
            Self::Refused(_) => None,
        }
    }
}

impl fmt::Debug for RunLedger {
    /// The path and the indexed run count, never the counters.
    ///
    /// The values are not rendered: a report or a log line must not carry a run's
    /// budget into a reader that asked where the ledger is.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunLedger")
            .field("path", &self.inner.path)
            .field("runs", &owner::lock(&self.inner.index).runs.len())
            .finish()
    }
}
