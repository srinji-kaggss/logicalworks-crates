//! The estate-owned durable keyed store behind multi-tenant run records.
//!
//! [`TenantStore`] is what `logical_ci`'s `src/store.rs` needs without its
//! direct `rusqlite` edge (estate issue #317): tenant policies, run rows and
//! lane rows with per-tenant keyed reads, a dispatch row committed before each
//! lane starts, and a transactional close of runs whose coordinator died. The
//! estate's durable owner today is the append-only effect journal, bounded at
//! 100,000 events / 64 MiB with no keyed query, so it cannot hold an unbounded
//! run history with lookups by tenant and run-id prefix. This module is that
//! history; it is a second file store, not a second journal.
//!
//! # What is shared and what is this store's own
//!
//! The *frame grammar* is the estate's and is shared verbatim: `journal::frame`
//! holds the length prefix, the 32-byte head, the torn-tail classification
//! and the refusal of a frame no writer produces, and this store calls those
//! same functions. What cannot be reused is either existing store's *record*:
//! [`crate::journal::EffectEvent`] frames effect outcomes and `task::store`
//! frames per-step values under a run id, so a run row smuggled through
//! either would claim to be something it is not and chain against a sequence
//! that has nothing to do with runs. So this module states its own record and
//! reuses the frame grammar; the encoding is `lgwks_std::wire` in all three.
//! The chain head is over the record's own fields, so one file holds one
//! chain and the tenant check on reads is what keeps tenants separated.
//!
//! # Durability without SQLite
//!
//! `rusqlite` is not on the estate's approved register, so no SQLite edge is
//! taken here. The three properties `logical_ci` relies on are reproduced
//! without it:
//!
//! - **Write-ahead.** Every mutation is one framed record appended to
//!   `tenant.store` and `sync_all`'d before the call returns, so a commit
//!   survives power loss (`synchronous = FULL` in SQLite terms). A torn final
//!   frame is an interrupted append and is trimmed on the next open or refresh;
//!   every earlier frame is retained.
//! - **One writer at a time.** Coordinators on one host serialize through a
//!   `tenant.lock` file beside the store, held across exactly one
//!   refresh-apply-sync step. A holder that died mid-step leaves a stale lock,
//!   which the next acquirer steals once it outlives any live step — two
//!   minutes; a live step is one small write and one sync, so it never runs
//!   that long. Waiting for the lock past the busy timeout is a typed
//!   [`TenantStoreError::Busy`] refusal, which is the `busy_timeout`
//!   behaviour: a caller waits, then is told, and nothing is half-written.
//! - **Crash windows behave like the SQLite store.** A frame complete on disk
//!   whose writer died before acknowledging it is folded on the next refresh,
//!   and a retried mutation then meets the same refusal a retried SQLite
//!   statement meets (a second begin is refused, a second dispatch finds no
//!   pending row). An acknowledged commit was synced before it was
//!   acknowledged, so recovery never takes back an answer.
//!
//! # The record vocabulary
//!
//! Six record kinds, each applied under the lock in arrival order:
//! `InitPolicy`, `BeginRun` (a run row plus its pending lane rows, one frame —
//! the transactional begin), `LaneDispatched` (pending to dispatched, exactly
//! one row), `LaneEnded` (to ended with outcome, detail and tail, never
//! overwriting an ended row), `FinishRun` (verdict, sealed record and seal,
//! refused when already closed), and `AbandonRun` (one frame closing the run
//! row and every open lane row — the transactional close V2-04's recovery
//! needs). Timestamps are opaque strings the caller mints (RFC 3339 in
//! `logical_ci`); ordering by them matches that format lexicographically.
//!
//! # What this does not do
//!
//! Policy gates and hardness travel as opaque values (`gates_json`, a `u8`
//! level, `created_at`): decoding them against `logical_ci`'s `Family` ladder
//! is that crate's job, and this store stays decoupled from it. Cross-tenant
//! reads follow `logical_ci`'s own rule — runs are named by unguessable ids
//! and listings filter by tenant — rather than adding a check the consumer
//! never asked for. A second executor host shares this directory and the same
//! two files; the fencing across hosts is [`crate::dist_lease`]'s job, and
//! this store is the durable ground it stands on.
//!
//! [`TenantStore`]: crate::tenant_store::TenantStore
//! [`TenantStoreError::Busy`]: crate::tenant_store::TenantStoreError::Busy
//!
//! `tests/it/tenant_store.rs` runs the store against the real file and lock: a hundred
//! coordinators each write a policy and take a run from begin through its dispatches, ends and
//! finish, and a store dropped with a run open reopens to that run, whose abandonment closes the
//! run and every lane in one frame.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use lgwks_std::hash::Digest;

use crate::journal::frame::SaturatingFrom;
use crate::journal::owner::lock;

/// The byte width of the store file's header: the magic and the version.
const HEADER_LEN: usize = 16;

/// The file's constant prefix. The last byte is the format version, so every
/// version shares the first fifteen bytes and a file matching those is this
/// store's at some version, while a file not matching them was never one.
const STORE_MAGIC: &[u8; 16] = b"lgwks-tstore\x00\x00\x00\x00";

/// The index of the version byte in the header: the last one, leaving the
/// first fifteen free of version bytes.
const VERSION_BYTE: usize = 15;

/// The format version this build writes and reads.
const STORE_FORMAT: u8 = 1;

/// The most bytes one framed record may occupy, tail blob included.
///
/// A lane tail past this is refused rather than written: the store replays
/// every record into memory, so a value that does not fit here belongs in the
/// caller's own storage with a digest in the row, not in the row itself.
const MAX_FRAME_BYTES: usize = 256 * 1024;

/// The most runs a listing returns. Past it the caller pages with a longer
/// prefix or an older admitting bound; the store never hands back an
/// unbounded listing.
const MAX_LISTED: u32 = 200;

/// How long a coordinator waits for another coordinator's lock before it is
/// refused rather than left hanging. `logical_ci` waits sixty seconds for a
/// writer transaction; this is the same bound on the same fact.
const BUSY_WAIT: Duration = Duration::from_secs(60);

/// The longest a waiter goes between claim attempts when no release in this
/// process wakes it: how late a lock freed by another process is noticed.
/// Small enough to pick that one up promptly, large enough not to spin.
const LOCK_SLICE_MS: u128 = 5;

/// How old a lock file must be before the next acquirer treats its holder as
/// dead and steals it. A live holder keeps the lock for one small write and
/// one sync, so a lock older than this was orphaned by a crash, never by a
/// slow disk.
const STALE_LOCK_AFTER: Duration = Duration::from_secs(120);

/// The store file's name inside the state directory.
const STORE_FILE: &str = "tenant.store";

/// The lock file's name beside it.
const LOCK_FILE: &str = "tenant.lock";

/// The record kinds this file holds, as stored in [`Stored::kind`].
///
/// Named rather than numbered at the fold, because a number in three places
/// is three chances for one of them to mean a different record.
const KIND_INIT_POLICY: u8 = 1;
/// A run row plus its pending lane rows, committed as one frame.
const KIND_BEGIN_RUN: u8 = 2;
/// One lane from pending to dispatched.
const KIND_LANE_DISPATCHED: u8 = 3;
/// One lane to ended, with its outcome.
const KIND_LANE_ENDED: u8 = 4;
/// A run's verdict, sealed record and seal.
const KIND_FINISH_RUN: u8 = 5;
/// The transactional close of a run whose coordinator died.
const KIND_ABANDON_RUN: u8 = 6;

/// The verdict an abandoned run carries. Fixed, because a lane that was
/// dispatched but never recorded an end may or may not have run: its outcome
/// is unknown, never a pass and never a failure.
const VERDICT_ABANDONED: &str = "UNMEASURED";

/// The reason an abandoned run carries, naming the fact rather than the cause.
const REASON_ABANDONED: &str = "abandoned: its coordinator ended before the run finished";

/// The outcome a dispatched-but-unended lane carries after abandonment.
const OUTCOME_UNKNOWN: &str = "outcome-unknown";

/// The detail a dispatched-but-unended lane carries after abandonment.
const DETAIL_UNKNOWN: &str = "dispatched before its coordinator ended; whether it ran is unknown";

/// The outcome a never-dispatched lane carries after abandonment.
const OUTCOME_UNMEASURED: &str = "unmeasured";

/// The detail a never-dispatched lane carries after abandonment.
const DETAIL_NEVER_STARTED: &str = "never started: its coordinator ended first";

/// A lane that has been admitted but not yet dispatched.
const LANE_PENDING: &str = "pending";

/// A lane whose dispatch row is committed but whose process may not exist.
const LANE_DISPATCHED: &str = "dispatched";

/// A lane that reached a terminal outcome, by ending or by abandonment.
const LANE_ENDED: &str = "ended";

/// The estate's durable keyed store: tenant policies, run rows and lane rows.
///
/// Cloning shares the file and the index through an `Arc`, so handles see one
/// store rather than one per clone. Two separate opens of one directory are
/// two writers, and they serialize through the lock file rather than through
/// the handle: the authoritative answer is the one taken while holding it.
#[derive(Clone)]
pub struct TenantStore {
    /// The shared file, index and holder counter.
    inner: Arc<StoreInner>,
}

/// The one state every clone of a store handle shares.
struct StoreInner {
    /// The file the frames live in.
    file: Mutex<File>,
    /// The state directory, for the lock file beside the store.
    dir: PathBuf,
    /// The folded index: policies and runs in arrival order.
    index: Mutex<Index>,
    /// How long a lock acquisition waits before it is refused as busy.
    busy_wait: Duration,
    /// Whose lock file content names, disambiguated per store.
    holder_seq: AtomicU64,
}

/// The folded index, and the committed length it accounts for.
#[derive(Debug)]
struct Index {
    /// Per tenant: the immutable policy.
    policies: BTreeMap<String, StoredPolicy>,
    /// Per run id, in arrival order: the run and its lanes.
    runs: BTreeMap<String, RunEntry>,
    /// How many frames the index folds, so a refusal names the frame it met.
    frames: u64,
    /// The file length every folded frame accounts for.
    committed: u64,
    /// The chain head the next frame follows. Carried beside the index
    /// because it cannot be recomputed from it: a head is over the archived
    /// record, so recomputing one would re-archive bytes and risk hashing
    /// something other than what is on the disk.
    tail: Digest,
}

/// One tenant's policy as folded.
#[derive(Debug, Clone)]
struct StoredPolicy {
    /// The gate set, exactly as recorded.
    gates_json: String,
    /// The hardness level, exactly as recorded.
    hardness: u8,
    /// When the policy was recorded.
    created_at: String,
}

/// How a folded run closed, in the same shape [`RunClose`] reports.
#[derive(Debug, Clone)]
struct StoredClose {
    /// When the run finished.
    finished_at: String,
    /// The verdict the run closed with.
    verdict: String,
    /// Why the run closed the way it did.
    reason: Option<String>,
    /// The sealed record, if the run sealed one.
    record: Option<String>,
    /// The seal, if the run sealed one.
    seal: Option<String>,
}

impl StoredClose {
    /// A finished run's close: its verdict, sealed record and seal.
    fn finished(moment: &str, verdict: &str, record: &str, seal: &str) -> Self {
        Self {
            finished_at: moment.to_owned(),
            verdict: verdict.to_owned(),
            reason: None,
            record: Some(record.to_owned()),
            seal: Some(seal.to_owned()),
        }
    }

    /// An abandoned run's close: the fixed unknown verdict and its reason.
    fn abandoned(moment: &str) -> Self {
        Self {
            finished_at: moment.to_owned(),
            verdict: VERDICT_ABANDONED.to_owned(),
            reason: Some(REASON_ABANDONED.to_owned()),
            record: None,
            seal: None,
        }
    }
}

/// One run as folded, with its lanes in plan order.
#[derive(Debug, Clone)]
struct RunEntry {
    /// The tenant that owns the run.
    tenant: String,
    /// The repository under test.
    repo: String,
    /// The host the coordinator ran on.
    host: String,
    /// The coordinator's process id.
    coordinator_pid: u32,
    /// The coordinator's lock, relative to the state directory.
    coordinator_lock: String,
    /// When the run started.
    started_at: String,
    /// How the run closed, if it did. One value rather than five options,
    /// because a finished run always names when and with what verdict: five
    /// independent options could claim a verdict with no moment.
    close: Option<StoredClose>,
    /// The lanes in plan order.
    lanes: Vec<LaneEntry>,
}

/// One lane as folded.
#[derive(Debug, Clone)]
struct LaneEntry {
    /// The lane id from the plan.
    id: String,
    /// `pending`, `dispatched` or `ended`.
    state: String,
    /// The outcome, once the lane ended.
    outcome: Option<String>,
    /// The detail, once the lane ended.
    detail: Option<String>,
    /// The captured tail, once the lane ended.
    tail: Option<Vec<u8>>,
}

impl fmt::Debug for TenantStore {
    /// The directory and the folded run count. The rows are not rendered: a
    /// report must not carry another tenant's runs into a reader that asked
    /// where the store is.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TenantStore")
            .field("dir", &self.inner.dir)
            // A poisoned index is still the folded state (see `lock_index`), so the
            // count is the real one rather than a stand-in zero.
            .field("runs", &lock_index(&self.inner.index).runs.len())
            .finish()
    }
}

/// The lock file held across one refresh-apply-sync step.
///
/// RAII on purpose: the step either completes and releases, or its holder died
/// and the file's age says so. Removing on drop is best effort — a removal the
/// device refuses is reported by the next acquirer's staleness check rather
/// than lost.
struct LockGuard {
    /// The lock file to remove on release.
    path: PathBuf,
}

impl Drop for LockGuard {
    /// Release the lock: remove the file, then wake this process's waiters.
    /// The removal is best effort, because `Drop` cannot report, and a leftover
    /// file is diagnosed by age at the next acquire.
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.path));
        let mut generation = lock(&RELEASES.generation);
        *generation = generation.wrapping_add(1);
        RELEASES.released.notify_all();
    }
}

/// The release signal a waiter in this process blocks on.
///
/// A holder in this process wakes every waiter here the moment it releases, so
/// a lock passed between two handles in one process is picked up at once rather
/// than after a timer. A holder in another process has no in-process signal to
/// send; for it, the waiter's bounded wait is what notices the freed file.
struct Releases {
    /// Bumped on every release, so a waiter can tell a release it already saw
    /// from one that happened after it looked.
    generation: Mutex<u64>,
    /// Notified on every release.
    released: Condvar,
}

/// One signal for the process: a waiter retries the claim on any release,
/// because the claim itself, not the signal, decides who holds the file.
static RELEASES: Releases = Releases {
    generation: Mutex::new(0),
    released: Condvar::new(),
};

/// The refusal for a run id the store never began, from a caller that names
/// no tenant, as the row-addressed updates do.
///
/// One constructor because the row-addressed updates ask the same question,
/// and a literal per site would be several chances to name a different tenant
/// for it.
fn unknown_run(run_id: &str) -> TenantStoreError {
    TenantStoreError::NoSuchRun {
        tenant: String::new(),
        run: run_id.to_owned(),
    }
}

/// The refusal for a lane row that is gone or past the step that needs it.
///
/// One constructor because the dispatch and end preconditions ask the same
/// question at different steps, and the step is what tells the caller which.
fn lane_lost(run_id: &str, lane: usize, step: &'static str) -> TenantStoreError {
    TenantStoreError::LaneLost {
        run_id: run_id.to_owned(),
        lane,
        step,
    }
}

/// A closer's whole answer for one run: the frame to commit and whether the
/// run was open.
///
/// One decision for the two closers — finish and abandon — so an already
/// closed run reads the same under both: no frame, `false`, and the first
/// closer's record stands untouched.
fn close_decision(entry: &RunEntry, stored: Stored) -> (Option<Stored>, bool) {
    if entry.close.is_some() {
        (None, false)
    } else {
        (Some(stored), true)
    }
}

/// Whether a lane step may act: the lane exists and its state satisfies the
/// step's own readiness.
///
/// One precondition for the two lane steps, parameterized by the step: the
/// dispatch needs a pending row and the end needs a row that has not ended,
/// and each names itself in the refusal so the caller knows which half of the
/// lane's life it missed.
fn lane_ready(
    entry: &RunEntry,
    run_id: &str,
    lane: usize,
    step: &'static str,
    ready: fn(&str) -> bool,
) -> Result<(), TenantStoreError> {
    let slot = match entry.lanes.get(lane) {
        Some(slot) => slot,
        None => {
            lgwks_std::trace::warn!(
                run_id,
                lane,
                step,
                "tenant_store: lane step refused: the lane row is gone"
            );
            return Err(lane_lost(run_id, lane, step));
        }
    };
    if ready(&slot.state) {
        Ok(())
    } else {
        Err(lane_lost(run_id, lane, step))
    }
}

/// Whether a lane is still pending, for the dispatch precondition.
fn is_pending(state: &str) -> bool {
    state == LANE_PENDING
}

/// Whether a lane has not ended, for the end precondition.
fn is_open_lane(state: &str) -> bool {
    state != LANE_ENDED
}

/// Every row one tenant owns, in map order.
///
/// One definition of tenant membership for the two readers — the listing and
/// the prefix lookup — so the two cannot disagree about which runs belong to
/// whom.
fn tenant_rows<'a>(index: &'a Index, tenant: &'a str) -> impl Iterator<Item = RunRow> + 'a {
    index
        .runs
        .iter()
        .filter(move |&(_, entry)| entry.tenant == tenant)
        .map(|(run_id, entry)| entry.row(run_id))
}

impl RunEntry {
    /// This run as a listed row.
    ///
    /// Takes the id beside the entry because the map holds the key, not the
    /// value: storing it in both would be two spellings of one fact.
    fn row(&self, run_id: &str) -> RunRow {
        RunRow::new(
            run_id.to_owned(),
            self.repo.clone(),
            self.started_at.clone(),
            self.close.clone().map(|close| {
                RunClose::new(
                    close.finished_at,
                    close.verdict,
                    close.reason,
                    close.record,
                    close.seal,
                )
            }),
        )
    }
}

/// The mutex, recovering a poison.
///
/// Recovering is right for the reason it is right in `journal::owner`: no arm
/// in the transaction path panics while the lock is held — every one returns
/// its `Result` first — so a poison is a bug in an unrelated thread, and
/// propagating it would brick a store that is still perfectly readable.
fn lock_file(file: &Mutex<File>) -> MutexGuard<'_, File> {
    match file.lock() {
        Ok(guard) => guard,
        Err(poison) => poison.into_inner(),
    }
}

/// The index lock, recovering a poison for the same reason.
fn lock_index(index: &Mutex<Index>) -> MutexGuard<'_, Index> {
    match index.lock() {
        Ok(guard) => guard,
        Err(poison) => poison.into_inner(),
    }
}

/// Nanos since the epoch for diagnostic labels: the lock file's holder name
/// and the doc-test nonce.
///
/// A label, never a decision and never an identity: the lock's existence is
/// the mutex and its mtime is the staleness, so a reused timestamp degrades
/// forensics, never fencing — which is why this is the wall clock rather
/// than a process id the OS reuses. A clock before the epoch labels zero
/// rather than refusing to label: the label answers "which claim", and a
/// missing answer would be evidence of nothing.
fn now_nanos() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => u64::saturating_from(since.as_nanos()),
        Err(_) => 0,
    }
}

/// Whether the lock file's holder is dead: the file is older than a live
/// holder can ever be.
///
/// A lock file that vanishes between the failed claim and this check was
/// released, not orphaned: it reads as held, and the acquirer parks one slice
/// and retries rather than reporting the vanished file as a device refusal.
/// A device refusal stays a refusal rather than a guess, and a clock that
/// cannot report the file's age — the host's clock runs before the file's
/// timestamp — is read as a live holder, so the acquirer waits out the busy
/// timeout and is refused [`Busy`](TenantStoreError::Busy) rather than
/// stealing a lock that may be held. Stealing what may be live would fork the
/// file; waiting only costs the bound.
fn lock_is_stale(path: &Path) -> Result<bool, TenantStoreError> {
    let modified = match std::fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            lgwks_std::trace::warn!(
                path = %path.display(),
                %error,
                "tenant_store: lock age unreadable; refusing rather than guessing stale"
            );
            return Err(TenantStoreError::storage(error));
        }
    };
    match modified.elapsed() {
        Ok(age) => Ok(age > STALE_LOCK_AFTER),
        Err(_) => Ok(false),
    }
}

// The refusal vocabulary, the caller's rows, the operations and the record
// codec, each in its own file; re-exported so every path stays
// `tenant_store::<Name>`.
mod error;
mod record;
mod rows;
// The operations: every record is a wire frame, so without `wire` there is
// nothing to open, read or write (#687).
#[cfg(feature = "wire")]
mod store;

use record::Stored;

pub use error::TenantStoreError;
pub use rows::{LaneRow, OpenRun, Policy, RunClose, RunRow, RunStart};
