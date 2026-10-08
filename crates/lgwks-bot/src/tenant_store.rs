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
//! ```rust
//! use lgwks_bot::tenant_store::TenantStore;
//!
//! let dir = std::env::temp_dir().join(format!(
//!     "lgwks-tenant-store-doc-{}",
//!     lgwks_bot::tenant_store::doc_nonce()
//! ));
//! let store = TenantStore::open(&dir)?;
//! store.init_policy("acme", "[\"fmt\"]", 1, "2026-10-07T00:00:00Z")?;
//! assert!(store.policy("acme")?.is_some());
//! std::fs::remove_dir_all(&dir)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::wire::{AlignedVec, WireError, from_bytes, to_bytes};

use crate::journal::frame::{self, HEAD_BYTES, LENGTH_BYTES, SaturatingFrom};

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

/// How long between lock-acquisition attempts the waiter parks. Small enough
/// that a freed lock is picked up promptly, large enough that waiting is not a
/// spin.
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

/// Why the store refused.
///
/// A read failure is never absence: every arm here is an error the caller must
/// see, never an empty listing that would let a finished run look unfinished.
#[derive(Debug)]
#[non_exhaustive]
pub enum TenantStoreError {
    /// The device refused an open, read, write or sync.
    Storage {
        /// What the device said.
        cause: std::io::Error,
    },
    /// The file exists but is not a tenant store, or its header is short.
    NotAStore,
    /// The file is a tenant store in a format this build does not read.
    ///
    /// Distinct from [`NotAStore`](Self::NotAStore) because the two say
    /// opposite things about the disk: one says these bytes were never this
    /// store's, the other says they were, written by a version of this crate
    /// whose records mean something this build cannot reconstruct. The file is
    /// left unchanged either way: it is never reset.
    FormatVersion {
        /// The version byte the file declares.
        found: u8,
        /// The version this build reads.
        expected: u8,
    },
    /// A committed frame does not follow from the records before it. Those
    /// bytes are acknowledged evidence and are refused, never trimmed.
    Corrupt {
        /// The frame's index, from zero.
        at: u64,
    },
    /// A record could not be archived, or a frame did not decode.
    Encoding {
        /// What the codec said.
        cause: WireError,
    },
    /// A framed record past the store's ceiling. Nothing was written.
    Limit {
        /// What would have been needed.
        requested: u64,
        /// The ceiling.
        limit: u64,
    },
    /// Another coordinator held the lock past the busy timeout. Nothing was
    /// written and nothing was read; the caller waits and retries, or reports.
    Busy {
        /// How long the caller waited before it was refused.
        waited: Duration,
    },
    /// The tenant already has a policy; it was not changed.
    AlreadyInitialised {
        /// The tenant that was already initialised.
        tenant: String,
    },
    /// A run id this store already holds; the second begin wrote nothing.
    DuplicateRun {
        /// The run id that was already begun.
        run: String,
    },
    /// No run of this tenant matches the prefix.
    NoSuchRun {
        /// The tenant that was searched. Empty when the caller named no
        /// tenant, as the row-addressed updates do.
        tenant: String,
        /// The prefix or id that matched nothing.
        run: String,
    },
    /// A prefix names more than one of the tenant's runs.
    Ambiguous {
        /// The prefix that matched more than one run.
        run: String,
    },
    /// A lane row this run wrote at its start no longer matches; nothing was
    /// changed.
    LaneLost {
        /// The run the lane belongs to.
        run_id: String,
        /// The lane position the caller named.
        lane: usize,
        /// The step whose precondition the row failed.
        step: &'static str,
    },
}

impl fmt::Display for TenantStoreError {
    /// The refusal, naming the ceiling, the device, the tenant or the row.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Storage { ref cause } => {
                write!(formatter, "the tenant store's device refused: {cause}")
            }
            Self::NotAStore => formatter
                .write_str("this file is not a tenant store; refusing to read it as run records"),
            Self::FormatVersion { found, expected } => write!(
                formatter,
                "this is a tenant store in format version {found}, and this build reads \
                 version {expected}; it was left unchanged"
            ),
            Self::Corrupt { at } => write!(
                formatter,
                "tenant store frame {at} does not follow from the records before it; \
                 the bytes are refused, not trimmed"
            ),
            Self::Encoding { ref cause } => {
                write!(formatter, "a run record could not be archived: {cause}")
            }
            Self::Limit { requested, limit } => write!(
                formatter,
                "one framed record of {requested} bytes exceeds the ceiling of {limit}"
            ),
            Self::Busy { waited } => write!(
                formatter,
                "another coordinator held the store lock past the busy timeout after \
                 {waited:?}; nothing was written"
            ),
            Self::AlreadyInitialised { ref tenant } => write!(
                formatter,
                "tenant {tenant:?} is already initialised; its policy never changes"
            ),
            Self::DuplicateRun { ref run } => write!(
                formatter,
                "run {run:?} was already begun; the second begin wrote nothing"
            ),
            Self::NoSuchRun {
                ref tenant,
                ref run,
            } => {
                write!(formatter, "tenant {tenant:?} has no run {run:?}")
            }
            Self::Ambiguous { ref run } => {
                write!(
                    formatter,
                    "{run:?} names more than one run; give more of it"
                )
            }
            Self::LaneLost {
                ref run_id,
                lane,
                step,
            } => write!(
                formatter,
                "run {run_id}: lane #{lane} is not in the state its {step} needs; \
                 it was left unchanged"
            ),
        }
    }
}

impl std::error::Error for TenantStoreError {
    /// The device's or the codec's own error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Storage { ref cause } => Some(cause),
            Self::Encoding { ref cause } => Some(cause),
            Self::NotAStore
            | Self::FormatVersion { .. }
            | Self::Corrupt { .. }
            | Self::Limit { .. }
            | Self::Busy { .. }
            | Self::AlreadyInitialised { .. }
            | Self::DuplicateRun { .. }
            | Self::NoSuchRun { .. }
            | Self::Ambiguous { .. }
            | Self::LaneLost { .. } => None,
        }
    }
}

impl TenantStoreError {
    /// A storage refusal carrying the device's own error.
    ///
    /// One constructor rather than a literal per call site, because every
    /// `map_err` here wraps the same fact and a literal per site would be
    /// several chances to name a different variant for it.
    fn storage(cause: std::io::Error) -> Self {
        Self::Storage { cause }
    }
}

/// What a run records before its first lane.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunStart<'a> {
    /// The run id, unique across tenants.
    pub run_id: &'a str,
    /// The tenant that owns the run.
    pub tenant: &'a str,
    /// The repository under test.
    pub repo: &'a str,
    /// The host the coordinator runs on.
    pub host: &'a str,
    /// The coordinator's process id.
    pub coordinator_pid: u32,
    /// The coordinator's lock, relative to the state directory.
    pub coordinator_lock: &'a str,
    /// When the run started, as an opaque string the caller mints.
    pub started_at: &'a str,
}

impl<'a> RunStart<'a> {
    /// The run's opening record, borrowing every field from the caller.
    ///
    /// Borrowed rather than owned because the record is framed and written
    /// inside [`TenantStore::begin_run`] and never outlives the call.
    #[must_use]
    pub fn new(
        run_id: &'a str,
        tenant: &'a str,
        repo: &'a str,
        host: &'a str,
        coordinator_pid: u32,
        coordinator_lock: &'a str,
        started_at: &'a str,
    ) -> Self {
        Self {
            run_id,
            tenant,
            repo,
            host,
            coordinator_pid,
            coordinator_lock,
            started_at,
        }
    }
}

/// A tenant's immutable policy: the gates as stored, the hardness level, and
/// when it was recorded.
///
/// The gates are opaque here — the string `logical_ci` wrote — because
/// decoding them against that crate's family ladder is its job, and this store
/// stays decoupled from it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Policy {
    /// The gate set, exactly as recorded. Private because a policy is
    /// written once and never rewritten; readers use [`gates_json`](Self::gates_json).
    gates_json: String,
    /// The hardness level, exactly as recorded.
    pub hardness: u8,
    /// When the policy was recorded. Private for the same reason as the gates.
    created_at: String,
}

impl Policy {
    /// A policy exactly as the store holds it.
    #[must_use]
    pub fn new(gates_json: String, hardness: u8, created_at: String) -> Self {
        Self {
            gates_json,
            hardness,
            created_at,
        }
    }

    /// The gate set, exactly as recorded.
    #[must_use]
    pub fn gates_json(&self) -> &str {
        &self.gates_json
    }

    /// When the policy was recorded.
    #[must_use]
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
}

/// How a closed run ended: when, with what verdict, and what it sealed.
///
/// Grouped rather than threaded as five arguments through every row
/// constructor, because the five travel together from every closer and a
/// second order of them would be a second definition of one fact.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunClose {
    /// When the run finished. Private; readers use [`finished_at`](Self::finished_at).
    finished_at: String,
    /// The verdict the run closed with. Private; readers use [`verdict`](Self::verdict).
    verdict: String,
    /// Why the run closed the way it did, if it named one.
    pub reason: Option<String>,
    /// The sealed record, if the run sealed one.
    pub record: Option<String>,
    /// The seal, if the run sealed one.
    pub seal: Option<String>,
}

impl RunClose {
    /// How a run closed, exactly as the store holds it.
    #[must_use]
    pub fn new(
        finished_at: String,
        verdict: String,
        reason: Option<String>,
        record: Option<String>,
        seal: Option<String>,
    ) -> Self {
        Self {
            finished_at,
            verdict,
            reason,
            record,
            seal,
        }
    }

    /// When the run finished.
    #[must_use]
    pub fn finished_at(&self) -> &str {
        &self.finished_at
    }

    /// The verdict the run closed with.
    #[must_use]
    pub fn verdict(&self) -> &str {
        &self.verdict
    }
}

/// One run as listed or shown.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunRow {
    /// The run id. Private; readers use [`run_id`](Self::run_id).
    run_id: String,
    /// The repository under test. Private; readers use [`repo`](Self::repo).
    repo: String,
    /// When the run started. Private; readers use [`started_at`](Self::started_at).
    started_at: String,
    /// How the run closed, if it did.
    pub close: Option<RunClose>,
}

impl RunRow {
    /// One run row exactly as the store holds it.
    #[must_use]
    pub fn new(run_id: String, repo: String, started_at: String, close: Option<RunClose>) -> Self {
        Self {
            run_id,
            repo,
            started_at,
            close,
        }
    }

    /// The identifier the coordinator passed at begin: the row key every lane
    /// and close of this run is folded under, so one run's history is one key.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The repository under test.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// When the run started.
    #[must_use]
    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    /// When the run finished, if it did.
    #[must_use]
    pub fn finished_at(&self) -> Option<&str> {
        self.close.as_ref().map(|close| close.finished_at())
    }

    /// The verdict, if the run closed.
    #[must_use]
    pub fn verdict(&self) -> Option<&str> {
        self.close.as_ref().map(|close| close.verdict())
    }

    /// Why the run closed the way it did, if it named one.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.close
            .as_ref()
            .and_then(|close| close.reason.as_deref())
    }

    /// The sealed record, if the run sealed one.
    #[must_use]
    pub fn record(&self) -> Option<&str> {
        self.close
            .as_ref()
            .and_then(|close| close.record.as_deref())
    }

    /// The closer's sealed record, when the run closed with one: `None` while
    /// the run is open and when the closer sealed nothing (an abandonment).
    #[must_use]
    pub fn seal(&self) -> Option<&str> {
        self.close.as_ref().and_then(|close| close.seal.as_deref())
    }
}

/// One lane row of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LaneRow {
    /// The lane id from the plan. Private; readers use [`id`](Self::id).
    id: String,
    /// `pending`, `dispatched` or `ended`. Private; readers use [`state`](Self::state).
    state: String,
    /// The outcome, once the lane ended.
    pub outcome: Option<String>,
    /// The detail, once the lane ended.
    pub detail: Option<String>,
    /// The captured tail, once the lane ended.
    pub tail: Option<Vec<u8>>,
}

impl LaneRow {
    /// One lane row exactly as the store holds it.
    #[must_use]
    pub fn new(
        id: String,
        state: String,
        outcome: Option<String>,
        detail: Option<String>,
        tail: Option<Vec<u8>>,
    ) -> Self {
        Self {
            id,
            state,
            outcome,
            detail,
            tail,
        }
    }

    /// The lane id from the plan.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `pending`, `dispatched` or `ended`.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }
}

/// A run whose coordinator may have died.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenRun {
    /// The run id. Private; readers use [`run_id`](Self::run_id).
    run_id: String,
    /// The coordinator's process id, for the liveness check.
    pub coordinator_pid: u32,
}

impl OpenRun {
    /// An open run exactly as the store holds it.
    #[must_use]
    pub fn new(run_id: String, coordinator_pid: u32) -> Self {
        Self {
            run_id,
            coordinator_pid,
        }
    }

    /// The run this claim is fenced to: every lane step checks the authority's
    /// epoch for this id before it commits, and a stale one is refused.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

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
            .field(
                "runs",
                &self.inner.index.lock().map_or(0, |index| index.runs.len()),
            )
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
    /// Release the lock: remove the file. Best effort, because `Drop` cannot
    /// report, and a leftover file is diagnosed by age at the next acquire.
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.path));
    }
}

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

impl TenantStore {
    /// Open (or create) the store under `dir`, waiting up to a minute for
    /// another coordinator's lock.
    ///
    /// The directory is created when absent. A torn final frame is trimmed,
    /// because an interrupted append was never anyone's answer; every earlier
    /// frame is retained.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Storage`] when the device refuses; [`NotAStore`](TenantStoreError::NotAStore)
    /// when the file exists and is not a tenant store;
    /// [`FormatVersion`](TenantStoreError::FormatVersion) when it is one this
    /// build does not read; [`Corrupt`](TenantStoreError::Corrupt) when a
    /// committed frame does not follow from the ones before it.
    pub fn open(dir: &Path) -> Result<Self, TenantStoreError> {
        Self::open_with_busy_timeout(dir, BUSY_WAIT)
    }

    /// [`open`](Self::open) with an explicit lock-wait bound.
    ///
    /// The bound a test waits under: production waits the full minute, and a
    /// test that held a lock for a minute to prove the refusal would be
    /// proving patience rather than fencing.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub fn open_with_busy_timeout(
        dir: &Path,
        busy_wait: Duration,
    ) -> Result<Self, TenantStoreError> {
        std::fs::create_dir_all(dir).map_err(TenantStoreError::storage)?;
        let path = dir.join(STORE_FILE);
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(TenantStoreError::storage)?;
        if !existed || file.metadata().map_err(TenantStoreError::storage)?.len() == 0 {
            file.write_all(&Self::header())
                .and_then(|()| file.sync_all())
                .map_err(TenantStoreError::storage)?;
        }
        let store = Self {
            inner: Arc::new(StoreInner {
                file: Mutex::new(file),
                dir: dir.to_path_buf(),
                index: Mutex::new(Index {
                    policies: BTreeMap::new(),
                    runs: BTreeMap::new(),
                    frames: 0,
                    committed: u64::saturating_from(HEADER_LEN),
                    tail: genesis_head(),
                }),
                busy_wait,
                holder_seq: AtomicU64::new(0),
            }),
        };
        let guard = store.acquire()?;
        {
            let mut inner_file = lock_file(&store.inner.file);
            let mut index = lock_index(&store.inner.index);
            full_replay(&mut inner_file, &mut index)?;
            trim_to_committed(&mut inner_file, &index)?;
        }
        drop(guard);
        Ok(store)
    }

    /// The file header as this build writes it: the constant prefix with the
    /// version in the last byte, built once so the writer and the reader
    /// cannot disagree about which byte is which.
    fn header() -> [u8; HEADER_LEN] {
        let mut header = *STORE_MAGIC;
        header[VERSION_BYTE] = STORE_FORMAT;
        header
    }

    /// Where the lock file lives.
    fn lock_path(&self) -> PathBuf {
        self.inner.dir.join(LOCK_FILE)
    }

    /// Hold the host lock across one step, waiting up to the busy timeout.
    ///
    /// Stale locks are stolen by age: a live holder keeps the lock for one
    /// small write and one sync, so a lock older than [`STALE_LOCK_AFTER`]
    /// was orphaned by a crash. The wait is bounded — a bounded number of
    /// parked slices — and exhaustion is a typed refusal, never a hang.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] when the lock stayed held past the bound;
    /// [`TenantStoreError::Storage`] when the device refuses.
    fn acquire(&self) -> Result<LockGuard, TenantStoreError> {
        let path = self.lock_path();
        let started = Instant::now();
        let slices = self.inner.busy_wait.as_millis().div_ceil(LOCK_SLICE_MS);
        let mut waited_slices: u128 = 0;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut claim) => {
                    let seq = self.inner.holder_seq.fetch_add(1, Ordering::SeqCst);
                    let holder = format!("{}-{seq}", now_nanos());
                    if let Err(error) = claim.write_all(holder.as_bytes()) {
                        drop(std::fs::remove_file(&path));
                        lgwks_std::trace::warn!(
                            path = %path.display(),
                            %error,
                            "tenant_store: lock claim write refused; released the claim"
                        );
                        return Err(TenantStoreError::storage(error));
                    }
                    return Ok(LockGuard { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path)? {
                        drop(std::fs::remove_file(&path));
                    } else if waited_slices >= slices {
                        lgwks_std::trace::warn!(
                            path = %path.display(),
                            waited_slices,
                            "tenant_store: lock stayed held past the busy bound; refusing as busy"
                        );
                        return Err(TenantStoreError::Busy {
                            waited: started.elapsed(),
                        });
                    } else {
                        std::thread::park_timeout(Duration::from_millis(u64::saturating_from(
                            LOCK_SLICE_MS,
                        )));
                        waited_slices = waited_slices.saturating_add(1);
                    }
                }
                Err(error) => {
                    lgwks_std::trace::warn!(
                        path = %path.display(),
                        %error,
                        "tenant_store: lock claim refused by the device"
                    );
                    return Err(TenantStoreError::storage(error));
                }
            }
        }
    }

    /// Run `apply` under the host lock: refresh the index past every frame
    /// another coordinator committed, apply the mutation, and return its
    /// answer. A mutation's frame is synced before the lock is released, so a
    /// returned answer survives power loss.
    ///
    /// # Errors
    ///
    /// Whatever the device, the lock, the replay or the mutation reports.
    /// A refused mutation writes nothing.
    fn transact<T>(
        &self,
        apply: impl FnOnce(&mut Index) -> Result<(Option<Stored>, T), TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        let _guard = self.acquire()?;
        let mut file = lock_file(&self.inner.file);
        let mut index = lock_index(&self.inner.index);
        refresh(&mut file, &mut index)?;
        let (record, answer) = apply(&mut index)?;
        if let Some(stored) = record {
            let (framed, head) = frame_record(&stored, &index.tail)?;
            file.write_all(&framed).map_err(TenantStoreError::storage)?;
            file.sync_all().map_err(TenantStoreError::storage)?;
            index.committed = index
                .committed
                .saturating_add(u64::saturating_from(framed.len()));
            index.tail = head;
            index.frames = index.frames.saturating_add(1);
            fold(&mut index, &stored);
        }
        Ok(answer)
    }

    /// Refresh the index past committed frames, then read through `read`.
    ///
    /// Reads take the lock for the same reason writes do: without it a read
    /// could answer from an index another coordinator has already moved past.
    /// The lock is held for a memory probe, never across an `await` — this
    /// surface is synchronous.
    ///
    /// # Errors
    ///
    /// Whatever the device, the lock or the replay reports.
    fn observe<T>(
        &self,
        read: impl FnOnce(&Index) -> Result<T, TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        let _guard = self.acquire()?;
        let mut file = lock_file(&self.inner.file);
        let mut index = lock_index(&self.inner.index);
        refresh(&mut file, &mut index)?;
        read(&index)
    }

    /// Run a row-addressed update under the host lock: refresh, resolve the
    /// run, and commit the frame the update builds, or refuse.
    ///
    /// One lookup for the four row-addressed updates — dispatch, end, finish,
    /// abandon — so a run the store never began is refused once, in one
    /// place, rather than once per update with four chances to name a
    /// different refusal for it.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun, and
    /// whatever the device, the lock, the replay or the update reports.
    fn transact_on_run<T>(
        &self,
        run_id: &str,
        update: impl FnOnce(&RunEntry) -> Result<(Option<Stored>, T), TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        self.transact(|index| {
            let Some(entry) = index.runs.get(run_id) else {
                lgwks_std::trace::warn!(
                    run_id,
                    "tenant_store: row-addressed update refused: the run was never begun"
                );
                return Err(unknown_run(run_id));
            };
            update(entry)
        })
    }

    /// Read one run under the host lock: refresh, resolve the run, and answer
    /// through `read`.
    ///
    /// The read half of [`transact_on_run`](Self::transact_on_run): one
    /// lookup, one refusal, shared with the lane listing.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun, and
    /// whatever the device, the lock, the replay or the read reports.
    fn observe_run<T>(
        &self,
        run_id: &str,
        read: impl FnOnce(&RunEntry) -> Result<T, TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        self.observe(|index| {
            let Some(entry) = index.runs.get(run_id) else {
                lgwks_std::trace::warn!(
                    run_id,
                    "tenant_store: row-addressed read refused: the run was never begun"
                );
                return Err(unknown_run(run_id));
            };
            read(entry)
        })
    }

    /// Record `tenant`'s policy. A second call for the same tenant is refused
    /// and changes nothing: a policy is written once and never rewritten.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::AlreadyInitialised`] when the tenant has a policy;
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn init_policy(
        &self,
        tenant: &str,
        gates_json: &str,
        hardness: u8,
        now: &str,
    ) -> Result<(), TenantStoreError> {
        self.transact(|index| {
            if index.policies.contains_key(tenant) {
                lgwks_std::trace::warn!(
                    tenant,
                    "tenant_store: policy init refused: the tenant already has one"
                );
                return Err(TenantStoreError::AlreadyInitialised {
                    tenant: tenant.to_owned(),
                });
            }
            let stored = Stored::init_policy(tenant, gates_json, hardness, now);
            Ok((Some(stored), ()))
        })
    }

    /// `tenant`'s policy, if it ran `init`.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn policy(&self, tenant: &str) -> Result<Option<Policy>, TenantStoreError> {
        self.observe(|index| {
            Ok(index.policies.get(tenant).map(|held| {
                Policy::new(
                    held.gates_json.clone(),
                    held.hardness,
                    held.created_at.clone(),
                )
            }))
        })
    }

    /// Record a run and its lanes as pending, before any lane starts.
    ///
    /// One frame for the run row and every pending lane row, so the begin is
    /// atomic: a coordinator killed inside it left either the whole run or no
    /// run, never a run with half its lanes.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::DuplicateRun`] when the run id is already begun;
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// or [`TenantStoreError::Limit`] when the device or the ceiling refuses.
    pub fn begin_run(&self, run: &RunStart<'_>, lanes: &[&str]) -> Result<(), TenantStoreError> {
        self.transact(|index| {
            if index.runs.contains_key(run.run_id) {
                lgwks_std::trace::warn!(
                    run = run.run_id,
                    "tenant_store: begin refused: the run id is already begun"
                );
                return Err(TenantStoreError::DuplicateRun {
                    run: run.run_id.to_owned(),
                });
            }
            let stored = Stored::begin_run(run, lanes);
            Ok((Some(stored), ()))
        })
    }

    /// Durable intent: the lane is about to start. Committed before the
    /// process exists, so a coordinator killed at any instant leaves a record
    /// the next run can reconcile.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::LaneLost`] when the row is gone or already past
    /// dispatch; [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn lane_dispatched(&self, run_id: &str, lane: usize) -> Result<(), TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            lane_ready(entry, run_id, lane, "dispatch", is_pending)?;
            let stored = Stored::lane_op(KIND_LANE_DISPATCHED, run_id, lane);
            Ok((Some(stored), ()))
        })
    }

    /// The lane is terminal. A lane ends once; a row already ended (for
    /// example by reconciliation) is never overwritten.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::LaneLost`] when the row is gone or already ended;
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn lane_ended(
        &self,
        run_id: &str,
        lane: usize,
        outcome: &str,
        detail: &str,
        tail: &[u8],
    ) -> Result<(), TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            lane_ready(entry, run_id, lane, "end", is_open_lane)?;
            let stored = Stored::lane_ended(run_id, lane, outcome, detail, tail);
            Ok((Some(stored), ()))
        })
    }

    /// Store the sealed record. Returns `false`, changing nothing, when the
    /// run already ended (for example because it was reconciled as
    /// abandoned): the closer's record stands.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn finish_run(
        &self,
        run_id: &str,
        verdict: &str,
        finished_at: &str,
        record: &str,
        seal: &str,
    ) -> Result<bool, TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            Ok(close_decision(
                entry,
                Stored::finish_run(run_id, verdict, finished_at, record, seal),
            ))
        })
    }

    /// Runs on `host` that have not finished: the reconciliation set a
    /// successor works after a coordinator died.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn open_runs(&self, host: &str) -> Result<Vec<OpenRun>, TenantStoreError> {
        self.observe(|index| {
            Ok(index
                .runs
                .iter()
                .filter(|&(_, entry)| entry.host == host && entry.close.is_none())
                .map(|(run_id, entry)| OpenRun::new(run_id.clone(), entry.coordinator_pid))
                .collect())
        })
    }

    /// Close an unfinished run whose coordinator died, in one frame.
    ///
    /// A lane that was dispatched but never recorded an end may or may not
    /// have run: its outcome is unknown, never a pass and never a failure. A
    /// lane never dispatched did not run. Returns whether the run was open;
    /// an already-closed run keeps its closer's record untouched.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn abandon(&self, run_id: &str, now: &str) -> Result<bool, TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            Ok(close_decision(entry, Stored::abandon_run(run_id, now)))
        })
    }

    /// The coordinator lock `run_id` was begun under, relative to the state
    /// directory: the name a successor uses to tell whether the coordinator
    /// that minted the run is still alive.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn coordinator_lock(&self, run_id: &str) -> Result<String, TenantStoreError> {
        self.observe_run(run_id, |entry| Ok(entry.coordinator_lock.clone()))
    }

    /// `tenant`'s most recent runs, newest first, capped at 200.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn runs(&self, tenant: &str, limit: u32) -> Result<Vec<RunRow>, TenantStoreError> {
        self.observe(|index| {
            let mut rows: Vec<RunRow> = tenant_rows(index, tenant).collect();
            rows.sort_by(|left, right| {
                right
                    .started_at()
                    .cmp(left.started_at())
                    .then_with(|| right.run_id().cmp(left.run_id()))
            });
            rows.truncate(usize::saturating_from(limit.min(MAX_LISTED)));
            Ok(rows)
        })
    }

    /// The one run of `tenant` whose id starts with `prefix`.
    ///
    /// A prefix match, not a pattern: no escape character, no wildcard, so a
    /// run id cannot smuggle syntax into the lookup.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when nothing matches;
    /// [`TenantStoreError::Ambiguous`] when more than one run matches.
    pub fn run(&self, tenant: &str, prefix: &str) -> Result<RunRow, TenantStoreError> {
        self.observe(|index| {
            let mut matches =
                tenant_rows(index, tenant).filter(|row| row.run_id().starts_with(prefix));
            match (matches.next(), matches.next()) {
                (Some(row), None) => Ok(row),
                (Some(_), Some(_)) => Err(TenantStoreError::Ambiguous {
                    run: prefix.to_owned(),
                }),
                (None, _) => Err(TenantStoreError::NoSuchRun {
                    tenant: tenant.to_owned(),
                    run: prefix.to_owned(),
                }),
            }
        })
    }

    /// The lanes of `run_id`, in plan order.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn lanes(&self, run_id: &str) -> Result<Vec<LaneRow>, TenantStoreError> {
        self.observe_run(run_id, |entry| {
            Ok(entry
                .lanes
                .iter()
                .map(|slot| {
                    LaneRow::new(
                        slot.id.clone(),
                        slot.state.clone(),
                        slot.outcome.clone(),
                        slot.detail.clone(),
                        slot.tail.clone(),
                    )
                })
                .collect())
        })
    }
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

/// The chain digest a store starts from: 32 zero bytes hashed through the
/// same framing as any record, so the first head is a real chain step and not
/// a constant every file shares.
fn genesis_head() -> Digest {
    let mut hasher = Hasher::new();
    hasher.write_framed(b"lgwks-tenant-store/genesis");
    hasher.finalize()
}

/// The record as it is archived and as its chain head is checked.
#[derive(
    Debug, Clone, lgwks_std::wire::Archive, lgwks_std::wire::Serialize, lgwks_std::wire::Deserialize,
)]
#[rkyv(
    attr(non_exhaustive),
    crate = lgwks_std::wire::rkyv,
    compare(PartialEq),
    derive(Debug)
)]
struct Stored {
    /// Which of the six record kinds this is.
    #[rkyv(attr(doc = "Which of the six record kinds this is."))]
    kind: u8,
    /// The tenant the record belongs to.
    #[rkyv(attr(doc = "The tenant the record belongs to."))]
    tenant: String,
    /// The run the record belongs to.
    #[rkyv(attr(doc = "The run the record belongs to."))]
    run_id: String,
    /// The repository under test (begin only).
    #[rkyv(attr(doc = "The repository under test (begin only)."))]
    repo: String,
    /// The coordinator's host (begin only).
    #[rkyv(attr(doc = "The coordinator's host (begin only)."))]
    host: String,
    /// The coordinator's process id (begin only).
    #[rkyv(attr(doc = "The coordinator's process id (begin only)."))]
    coordinator_pid: u32,
    /// The coordinator's lock (begin only).
    #[rkyv(attr(doc = "The coordinator's lock (begin only)."))]
    coordinator_lock: String,
    /// The moment the record names: started, finished, or recorded.
    #[rkyv(attr(doc = "The moment the record names: started, finished, or recorded."))]
    moment: String,
    /// The lane ids in plan order (begin only).
    #[rkyv(attr(doc = "The lane ids in plan order (begin only)."))]
    lanes: Vec<String>,
    /// The lane position the record names (lane ops only).
    #[rkyv(attr(doc = "The lane position the record names (lane ops only)."))]
    lane_idx: u64,
    /// The verdict (finish only).
    #[rkyv(attr(doc = "The verdict (finish only)."))]
    verdict: String,
    /// The sealed record (finish only).
    #[rkyv(attr(doc = "The sealed record (finish only)."))]
    record: String,
    /// The seal (finish only).
    #[rkyv(attr(doc = "The seal (finish only)."))]
    seal: String,
    /// The outcome (lane end only).
    #[rkyv(attr(doc = "The outcome (lane end only)."))]
    outcome: String,
    /// The detail (lane end only).
    #[rkyv(attr(doc = "The detail (lane end only)."))]
    detail: String,
    /// The captured tail (lane end only).
    #[rkyv(attr(doc = "The captured tail (lane end only)."))]
    tail: Vec<u8>,
    /// The gate set, exactly as recorded (policy only).
    #[rkyv(attr(doc = "The gate set, exactly as recorded (policy only)."))]
    gates_json: String,
    /// The hardness level (policy only).
    #[rkyv(attr(doc = "The hardness level (policy only)."))]
    hardness: u8,
}

impl Stored {
    /// A policy record.
    fn init_policy(tenant: &str, gates_json: &str, hardness: u8, now: &str) -> Self {
        Self::empty(KIND_INIT_POLICY, tenant, now, gates_json, hardness)
    }

    /// The shared skeleton: every field a kind does not use stays empty, so
    /// the head commits to the absence as well as the presence.
    fn empty(kind: u8, tenant: &str, moment: &str, gates_json: &str, hardness: u8) -> Self {
        Self {
            kind,
            tenant: tenant.to_owned(),
            run_id: String::new(),
            repo: String::new(),
            host: String::new(),
            coordinator_pid: 0,
            coordinator_lock: String::new(),
            moment: moment.to_owned(),
            lanes: Vec::new(),
            lane_idx: 0,
            verdict: String::new(),
            record: String::new(),
            seal: String::new(),
            outcome: String::new(),
            detail: String::new(),
            tail: Vec::new(),
            gates_json: gates_json.to_owned(),
            hardness,
        }
    }

    /// A run-begin record: the run row and every pending lane row in one frame.
    fn begin_run(run: &RunStart<'_>, lanes: &[&str]) -> Self {
        let mut stored = Self::empty(KIND_BEGIN_RUN, run.tenant, run.started_at, "", 0);
        run.run_id.clone_into(&mut stored.run_id);
        run.repo.clone_into(&mut stored.repo);
        run.host.clone_into(&mut stored.host);
        stored.coordinator_pid = run.coordinator_pid;
        run.coordinator_lock
            .clone_into(&mut stored.coordinator_lock);
        stored.lanes = lanes.iter().map(|id| (*id).to_owned()).collect();
        stored
    }

    /// A lane dispatch or any lane op that names only the run and the lane.
    fn lane_op(kind: u8, run_id: &str, lane: usize) -> Self {
        let mut stored = Self::empty(kind, "", "", "", 0);
        run_id.clone_into(&mut stored.run_id);
        stored.lane_idx = u64::saturating_from(lane);
        stored
    }

    /// A lane-end record with its outcome, detail and tail.
    fn lane_ended(run_id: &str, lane: usize, outcome: &str, detail: &str, tail: &[u8]) -> Self {
        let mut stored = Self::lane_op(KIND_LANE_ENDED, run_id, lane);
        outcome.clone_into(&mut stored.outcome);
        detail.clone_into(&mut stored.detail);
        stored.tail = tail.to_vec();
        stored
    }

    /// A run-finish record with its verdict, sealed record and seal.
    fn finish_run(
        run_id: &str,
        verdict: &str,
        finished_at: &str,
        record: &str,
        seal: &str,
    ) -> Self {
        let mut stored = Self::lane_op(KIND_FINISH_RUN, run_id, 0);
        finished_at.clone_into(&mut stored.moment);
        verdict.clone_into(&mut stored.verdict);
        record.clone_into(&mut stored.record);
        seal.clone_into(&mut stored.seal);
        stored
    }

    /// An abandon record: the run and the moment its coordinator was found gone.
    ///
    /// The lane closes are not separate records. They are implied by this one
    /// frame at fold time, which is what makes the close transactional: one
    /// frame, one sync, the run and every open lane closed together.
    fn abandon_run(run_id: &str, now: &str) -> Self {
        let mut stored = Self::lane_op(KIND_ABANDON_RUN, run_id, 0);
        now.clone_into(&mut stored.moment);
        stored
    }

    /// This record's chain head over the head before it and the archived bytes.
    ///
    /// Every field length-framed, so no two different records hash the same
    /// byte string and an empty field commits to its own absence. The archived
    /// bytes arrive because the framing step hands them over rather than
    /// re-encoding inside; this head chains over the fields, and the bytes are
    /// here so the one framing step serves heads that need either.
    fn head_from(record: &Stored, previous: &Digest, _archived: &[u8]) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(previous.as_bytes());
        hasher.write_framed(&[record.kind]);
        hasher.write_framed(record.tenant.as_bytes());
        hasher.write_framed(record.run_id.as_bytes());
        hasher.write_framed(record.repo.as_bytes());
        hasher.write_framed(record.host.as_bytes());
        hasher.write_framed(&record.coordinator_pid.to_le_bytes());
        hasher.write_framed(record.coordinator_lock.as_bytes());
        hasher.write_framed(record.moment.as_bytes());
        for id in &record.lanes {
            hasher.write_framed(id.as_bytes());
        }
        hasher.write_framed(&record.lane_idx.to_le_bytes());
        hasher.write_framed(record.verdict.as_bytes());
        hasher.write_framed(record.record.as_bytes());
        hasher.write_framed(record.seal.as_bytes());
        hasher.write_framed(record.outcome.as_bytes());
        hasher.write_framed(record.detail.as_bytes());
        hasher.write_framed(&record.tail);
        hasher.write_framed(record.gates_json.as_bytes());
        hasher.write_framed(&[record.hardness]);
        hasher.finalize()
    }
}

/// One admitted lane: the id from the plan, still pending.
///
/// One constructor because the begin fold builds one per plan lane, and a
/// literal per lane would restate the pending state for every lane of every
/// run.
fn pending_lane(id: &str) -> LaneEntry {
    LaneEntry {
        id: id.to_owned(),
        state: LANE_PENDING.to_owned(),
        outcome: None,
        detail: None,
        tail: None,
    }
}

/// Close one lane with an outcome and its detail.
///
/// The one closer every end reaches — a lane's own end, a dispatched lane's
/// abandonment, a pending lane's abandonment — so "ended" is stated once and
/// no closer can end a lane without naming why. The tail is the ender's own:
/// only a lane that ran leaves bytes behind.
fn end_lane(slot: &mut LaneEntry, outcome: &str, detail: &str) {
    LANE_ENDED.clone_into(&mut slot.state);
    slot.outcome = Some(outcome.to_owned());
    slot.detail = Some(detail.to_owned());
}

/// Close one run with an already-built close.
///
/// One assignment because the finish and abandon folds close different facts —
/// a verdict the coordinator earned, a verdict its death imposed — and the
/// assignment is what they share rather than what distinguishes them.
fn close_run(entry: &mut RunEntry, close: StoredClose) {
    entry.close = Some(close);
}

/// Fold one verified record into the index.
///
/// Inserts dispatch once: the two records that create rows (policy, begin)
/// return early, and the four that mutate a run share the one lookup below.
/// A mutation for a run the file never began is impossible here — the writer
/// refused it before framing — so a missing row returns without folding rather
/// than inventing one. An unknown kind is rot, refused at read time rather
/// than folded, because skipping would hand the caller a history with a hole
/// in it. See [`next_frame`].
fn fold(index: &mut Index, stored: &Stored) {
    if stored.kind == KIND_INIT_POLICY {
        index.policies.insert(
            stored.tenant.clone(),
            StoredPolicy {
                gates_json: stored.gates_json.clone(),
                hardness: stored.hardness,
                created_at: stored.moment.clone(),
            },
        );
        return;
    }
    if stored.kind == KIND_BEGIN_RUN {
        index.runs.insert(
            stored.run_id.clone(),
            RunEntry {
                tenant: stored.tenant.clone(),
                repo: stored.repo.clone(),
                host: stored.host.clone(),
                coordinator_pid: stored.coordinator_pid,
                coordinator_lock: stored.coordinator_lock.clone(),
                started_at: stored.moment.clone(),
                close: None,
                lanes: stored.lanes.iter().map(|id| pending_lane(id)).collect(),
            },
        );
        return;
    }
    let Some(entry) = index.runs.get_mut(&stored.run_id) else {
        return;
    };
    if stored.kind == KIND_LANE_DISPATCHED {
        if let Some(slot) = lane_slot(entry, &stored.lane_idx) {
            LANE_DISPATCHED.clone_into(&mut slot.state);
        }
    } else if stored.kind == KIND_LANE_ENDED {
        if let Some(slot) = lane_slot(entry, &stored.lane_idx) {
            end_lane(slot, &stored.outcome, &stored.detail);
            slot.tail = Some(stored.tail.clone());
        }
    } else if stored.kind == KIND_FINISH_RUN {
        close_run(
            entry,
            StoredClose::finished(
                &stored.moment,
                &stored.verdict,
                &stored.record,
                &stored.seal,
            ),
        );
    } else if stored.kind == KIND_ABANDON_RUN {
        close_run(entry, StoredClose::abandoned(&stored.moment));
        for slot in &mut entry.lanes {
            if slot.state == LANE_DISPATCHED {
                end_lane(slot, OUTCOME_UNKNOWN, DETAIL_UNKNOWN);
            } else if slot.state == LANE_PENDING {
                end_lane(slot, OUTCOME_UNMEASURED, DETAIL_NEVER_STARTED);
            }
        }
    }
}

/// The lane slot a lane op names, or `None` when the position is past the plan.
///
/// `None` here is decided by the caller as `LaneLost`: the fold itself never
/// refuses, because replay folds what the writer already decided.
fn lane_slot<'a>(entry: &'a mut RunEntry, idx: &u64) -> Option<&'a mut LaneEntry> {
    entry.lanes.get_mut(usize::saturating_from(*idx))
}

/// The frame for one record: archive it, refuse it past the ceiling, chain its
/// head, and lay it out through the estate's one frame grammar.
fn frame_record(stored: &Stored, previous: &Digest) -> Result<(Vec<u8>, Digest), TenantStoreError> {
    frame::frame_record(
        stored,
        previous,
        MAX_FRAME_BYTES,
        |record| {
            to_bytes::<WireError>(record)
                .map(|bytes| bytes.as_ref().to_vec())
                .map_err(|cause| TenantStoreError::Encoding { cause })
        },
        Stored::head_from,
        |len| TenantStoreError::Limit {
            requested: u64::saturating_from(len),
            limit: u64::saturating_from(MAX_FRAME_BYTES),
        },
    )
}

/// Decode one frame payload, copying only when the archive demands it.
///
/// An archive read from a misaligned slice is refused rather than decoded,
/// which would make a frame the writer really framed look like noise — so a
/// payload that does not sit at an archive-aligned offset is copied into one
/// that does. This is the alignment half of the estate's frame reading, stated
/// here because the shared helper travels behind the `script` gate and this
/// store builds without it.
fn decode_stored(payload: &[u8], at: u64) -> Result<Stored, TenantStoreError> {
    const ARCHIVE_ALIGN: usize = 16;
    let corrupt = || TenantStoreError::Corrupt { at };
    if payload.as_ptr().align_offset(ARCHIVE_ALIGN) == 0 {
        from_bytes::<Stored, WireError>(payload).map_err(|error| {
            lgwks_std::trace::warn!(
                at,
                %error,
                "tenant_store: stored payload failed to decode; refusing frame as corrupt"
            );
            corrupt()
        })
    } else {
        let mut copy: AlignedVec = AlignedVec::with_capacity(payload.len());
        copy.extend_from_slice(payload);
        from_bytes::<Stored, WireError>(&copy).map_err(|error| {
            lgwks_std::trace::warn!(
                at,
                %error,
                "tenant_store: realigned payload failed to decode; refusing frame as corrupt"
            );
            corrupt()
        })
    }
}

/// Read `buf` in full: `Ok(true)` when every byte arrived, `Ok(false)` when
/// the file ended first. A device refusal is an error, never evidence the
/// bytes were never written.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, TenantStoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(cause) => {
                lgwks_std::trace::warn!(
                    filled,
                    wanted = buf.len(),
                    %cause,
                    "tenant_store: frame read refused by the device mid-frame"
                );
                return Err(TenantStoreError::storage(cause));
            }
        }
    }
    Ok(true)
}

/// One verified frame: the decoded record and the head it committed to.
struct Framed {
    /// The record the frame's payload decoded to.
    stored: Stored,
    /// The chain head the frame stored for itself.
    head: Digest,
}

/// Whether `kind` is one of the six records this format produces.
///
/// A seventh kind is bytes no writer of this format produced: rot or a hand,
/// refused rather than folded, because the fold is total over the six and
/// skipping would hand the caller a history with a hole in it.
fn is_known_kind(kind: u8) -> bool {
    kind == KIND_INIT_POLICY
        || kind == KIND_BEGIN_RUN
        || kind == KIND_LANE_DISPATCHED
        || kind == KIND_LANE_ENDED
        || kind == KIND_FINISH_RUN
        || kind == KIND_ABANDON_RUN
}

/// Read the frame at ordinal `at` from the file's current position, or `None`
/// where the file stops holding a whole frame.
///
/// A partial prefix, payload or head is an append that never finished: the
/// scan stops and the caller trims to `committed`. A complete prefix naming a
/// frame this store never writes, a payload that does not decode, an unknown
/// kind, or a head that does not follow is rot in acknowledged bytes and is
/// refused.
fn next_frame(
    file: &mut File,
    at: u64,
    previous: &Digest,
) -> Result<Option<Framed>, TenantStoreError> {
    let corrupt = || TenantStoreError::Corrupt { at };
    let mut prefix = [0u8; LENGTH_BYTES];
    match frame::read_prefix(file, &mut prefix).map_err(TenantStoreError::storage)? {
        frame::Prefix::Eof => return Ok(None),
        frame::Prefix::Torn => return Ok(None),
        frame::Prefix::Full => {}
    }
    let declared = frame::declared_length(&prefix);
    if !frame::is_possible_length(declared, MAX_FRAME_BYTES) {
        lgwks_std::trace::warn!(
            at,
            declared,
            limit = MAX_FRAME_BYTES,
            "tenant_store: frame refused: the declared length is one this store never writes"
        );
        return Err(corrupt());
    }
    let mut payload = vec![0u8; declared];
    if !read_full(file, &mut payload)? {
        return Ok(None);
    }
    let mut head = [0u8; HEAD_BYTES];
    if !read_full(file, &mut head)? {
        return Ok(None);
    }
    let stored = decode_stored(&payload, at)?;
    if !is_known_kind(stored.kind) {
        lgwks_std::trace::warn!(
            at,
            kind = stored.kind,
            "tenant_store: frame refused: unknown record kind in acknowledged bytes"
        );
        return Err(corrupt());
    }
    if Stored::head_from(&stored, previous, &payload) != Digest::from_bytes(head) {
        lgwks_std::trace::warn!(
            at,
            "tenant_store: frame refused: the chain head does not follow the previous one"
        );
        return Err(corrupt());
    }
    Ok(Some(Framed {
        stored,
        head: Digest::from_bytes(head),
    }))
}

/// Refuse a header this build does not read: a magic mismatch was never a
/// tenant store, while a matching magic with a foreign version byte is one at
/// some version. Reporting the second as the first would tell an operator
/// their data was never theirs, which is what makes someone delete the file.
fn check_header(header: &[u8; HEADER_LEN]) -> Result<(), TenantStoreError> {
    if header[..VERSION_BYTE] != STORE_MAGIC[..VERSION_BYTE] {
        lgwks_std::trace::warn!("tenant_store: open refused: the file magic is not a tenant store");
        return Err(TenantStoreError::NotAStore);
    }
    if header[VERSION_BYTE] != STORE_FORMAT {
        lgwks_std::trace::warn!(
            found = header[VERSION_BYTE],
            expected = STORE_FORMAT,
            "tenant_store: open refused: foreign store format version"
        );
        return Err(TenantStoreError::FormatVersion {
            found: header[VERSION_BYTE],
            expected: STORE_FORMAT,
        });
    }
    Ok(())
}

/// Fold every whole frame from the file's current position until the length
/// seen when the drain began or a torn tail, and return the head the drain
/// ends at.
///
/// One definition of the fold loop for the two callers — the full replay from
/// the start and the tail refresh past `committed` — so the two cannot disagree
/// about what folding a frame means. Bounded by `total`: every iteration
/// consumes at least one byte, and a file that grows underneath cannot extend
/// the drain.
fn drain(
    file: &mut File,
    index: &mut Index,
    total: u64,
    mut previous: Digest,
) -> Result<Digest, TenantStoreError> {
    while index.committed < total {
        let at = index.frames;
        let Some(framed) = next_frame(file, at, &previous)? else {
            break;
        };
        let advance = frame_len_of(&framed.stored, at)?;
        fold(index, &framed.stored);
        index.committed = index.committed.saturating_add(advance);
        index.tail = framed.head;
        previous = framed.head;
        index.frames = index.frames.saturating_add(1);
    }
    Ok(previous)
}

/// Read every committed frame and fold the index it implies, from the start.
fn full_replay(file: &mut File, index: &mut Index) -> Result<(), TenantStoreError> {
    let total = file.metadata().map_err(TenantStoreError::storage)?.len();
    file.seek(SeekFrom::Start(0))
        .map_err(TenantStoreError::storage)?;
    let mut header = [0u8; HEADER_LEN];
    if !read_full(file, &mut header)? {
        lgwks_std::trace::warn!("tenant_store: replay refused: the file holds no whole header");
        return Err(TenantStoreError::NotAStore);
    }
    check_header(&header)?;
    index.policies.clear();
    index.runs.clear();
    index.frames = 0;
    index.committed = u64::saturating_from(HEADER_LEN);
    index.tail = genesis_head();
    drain(file, index, total, genesis_head())?;
    Ok(())
}

/// The bytes one folded frame accounts for: prefix, payload and head.
///
/// Re-archived from the decoded record through the one grammar, so the reader
/// and the accountant cannot disagree about what a frame is.
fn frame_len_of(stored: &Stored, at: u64) -> Result<u64, TenantStoreError> {
    let payload = to_bytes::<WireError>(stored)
        .map(|bytes| bytes.as_ref().to_vec())
        .map_err(|error| {
            lgwks_std::trace::warn!(
                at,
                %error,
                "tenant_store: frame length unaccountable: the record no longer archives"
            );
            TenantStoreError::Corrupt { at }
        })?;
    Ok(frame::framed_len(payload.len()))
}

/// Fold frames appended since `committed`; trim a torn tail.
///
/// When the file shrank under the index — another handle trimmed a torn tail
/// this handle never saw — the offsets no longer line up and the only honest
/// answer is a full replay from the start.
fn refresh(file: &mut File, index: &mut Index) -> Result<(), TenantStoreError> {
    let total = file.metadata().map_err(TenantStoreError::storage)?.len();
    if total < index.committed {
        return full_replay(file, index);
    }
    if total == index.committed {
        return Ok(());
    }
    file.seek(SeekFrom::Start(index.committed))
        .map_err(TenantStoreError::storage)?;
    drain(file, index, total, index.tail)?;
    trim_to_committed(file, index)
}

/// Truncate a torn tail past the committed length, so the next append does not
/// write past an interrupted frame and fork the file.
fn trim_to_committed(file: &mut File, index: &Index) -> Result<(), TenantStoreError> {
    if file.metadata().map_err(TenantStoreError::storage)?.len() != index.committed {
        file.set_len(index.committed)
            .map_err(TenantStoreError::storage)?;
        file.sync_all().map_err(TenantStoreError::storage)?;
    }
    Ok(())
}

/// A nonce for doc-test directories: unique per call, so parallel doc tests
/// never share a state directory.
fn doc_counter() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::SeqCst)
}

/// A process-unique nonce for doc examples: the wall clock beside a counter,
/// so parallel doc tests never share a state directory.
#[doc(hidden)]
#[must_use]
pub fn doc_nonce() -> u64 {
    now_nanos().saturating_add(doc_counter().saturating_mul(1024))
}
