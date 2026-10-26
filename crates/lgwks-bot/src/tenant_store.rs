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
//! # Durability with SQLite's properties, without SQLite
//!
//! The three properties `logical_ci` relies on are reproduced here, each
//! owned by the mechanism the platform releases rather than by a guess about
//! liveness:
//!
//! - **Write-ahead.** Every mutation is one framed record appended to
//!   `tenant.store` and `sync_all`'d before the call returns, so a commit
//!   survives power loss (`synchronous = FULL` in SQLite terms). A torn final
//!   frame is an interrupted append and is trimmed on the next open or refresh;
//!   every earlier frame is retained.
//! - **One writer at a time, readers beside it.** Coordinators on one host
//!   serialize through a `tenant.lock` file beside the store, held across
//!   exactly one refresh-apply-sync step — writers under the file's exclusive
//!   lock, readers under its shared lock. The lock is the operating system's:
//!   the kernel releases it when its holder dies, so a slow holder never loses
//!   a lock it still holds and a dead one never keeps a lock it cannot release.
//!   There is no lock file to steal by age, and no second holder ever writes.
//!   Waiting for the lock past the busy timeout is a typed
//!   [`TenantStoreError::Busy`] refusal, which is the `busy_timeout`
//!   behaviour: a caller waits, then is told, and nothing is half-written.
//! - **Open replays the tail, not the history.** A `tenant.snapshot` beside
//!   the store carries the folded index and the file length it accounts for,
//!   rewritten every few hundred frames and by an open that finds it stale.
//!   An open loads the snapshot and drains only what was appended after it; a
//!   missing or damaged snapshot falls back to the full replay it replaces,
//!   because the store file stays the truth and the snapshot only the shortcut.
//! - **Crash windows behave like the SQLite store.** A frame complete on disk
//!   whose writer died before acknowledging it is folded on the next refresh,
//!   and a retried mutation then meets the same refusal a retried SQLite
//!   statement meets (a second begin is refused, a second dispatch finds no
//!   pending row). An acknowledged commit was synced before it was
//!   acknowledged, so recovery never takes back an answer.
//!
//! The estate's SQLite storefront (issue #696) is the durable answer for new
//! state; this file stays the keyed run history `logical_ci` reads through,
//! now with the same locking and replay shape.
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
use lgwks_std::json::{Deserialize, Serialize};

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

/// The store file's name inside the state directory.
const STORE_FILE: &str = "tenant.store";

/// The lock file's name beside it.
const LOCK_FILE: &str = "tenant.lock";

/// The snapshot's name beside them: the folded index and the file length it
/// accounts for, so an open drains only what was appended after it.
const SNAPSHOT_FILE: &str = "tenant.snapshot";

/// The temporary name a snapshot is written under before it is renamed: a
/// crash leaves the previous snapshot or none, never a torn one.
const SNAPSHOT_TMP: &str = "tenant.snapshot.tmp";

/// The snapshot format this build writes and reads.
const SNAPSHOT_FORMAT: u8 = 1;

/// How many frames past the snapshot a mutation may commit before the next
/// transact — or an open that finds it stale — rewrites it. An open then
/// drains at most this many frames, so its cost is bounded by live history
/// rather than by every run the store ever held.
const SNAPSHOT_INTERVAL_FRAMES: u64 = 256;

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
    /// The shared file, index and snapshot position.
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
    /// How many frames the last snapshot accounts for. A transact that moves
    /// the index this far past it rewrites the snapshot under the same lock,
    /// so an open never drains more than one interval of history.
    snapshot_frames: AtomicU64,
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
///
/// Archived into the snapshot beside the store, so an open reads back the
/// same policy it would have folded frame by frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct StoredPolicy {
    /// The gate set, exactly as recorded.
    gates_json: String,
    /// The hardness level, exactly as recorded.
    hardness: u8,
    /// When the policy was recorded.
    created_at: String,
}

/// How a folded run closed, in the same shape [`RunClose`] reports.
///
/// Archived into the snapshot beside the store, like the run that carries it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
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
///
/// Archived into the snapshot beside the store, so an open reads back the
/// same runs its frames would have folded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
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
///
/// Archived into the snapshot beside the store, like the run that carries it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
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

/// The operating-system lock held across one refresh-apply-sync step.
///
/// The guard owns the open lock file, so the kernel holds the lock while the
/// guard lives and releases it when the guard drops — including when its
/// holder dies, which is what makes a stale lock impossible rather than merely
/// unlikely. Nothing is removed, stolen, or judged by age: the claim itself,
/// not a timestamp beside it, decides who holds the file.
struct LockGuard {
    /// The open lock file keeping the advisory lock. Held, never written:
    /// its bytes were the stale-lock protocol's, and the protocol is gone.
    _file: File,
}

impl Drop for LockGuard {
    /// Release the lock by unlocking and closing the file, then wake this
    /// process's waiters. Unlocking releases the advisory lock on every
    /// platform this store builds for — including when the holder died, whose
    /// close the kernel already ran — and the wake only hurries waiters in
    /// this process; a waiter in another process notices on its next attempt.
    fn drop(&mut self) {
        drop(self._file.unlock());
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

/// Open-or-create `path`, writing `header` when new or empty.
///
/// One definition for every file in this store that begins with a magic
/// header: the open leaves an existing file's contents alone, and an empty
/// file gets exactly the header the caller names. `File::options` states the
/// same open through the handle that owns it, so this spelling does not
/// repeat the repair ledger's while it shares its contract; migrating that
/// ledger to this helper is a follow-up outside this cluster.
///
/// # Errors
///
/// Whatever the device refuses.
pub(super) fn open_header_file(path: &Path, header: &[u8]) -> Result<File, std::io::Error> {
    use std::io::Write;
    let existed = path.exists();
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if !existed || file.metadata()?.len() == 0 {
        file.write_all(header)?;
        file.sync_all()?;
    }
    Ok(file)
}

// The refusal vocabulary, the caller's rows, the operations, the record
// codec and the snapshot, each in its own file; re-exported so every path
// stays `tenant_store::<Name>`.
mod error;
mod record;
mod rows;
mod snapshot;
mod store;

use record::Stored;

pub use error::TenantStoreError;
pub use rows::{LaneRow, OpenRun, Policy, RunClose, RunRow, RunStart};
