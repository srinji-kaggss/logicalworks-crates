//! The durable, file-backed per-step record store behind a resumable run.
//!
//! [`HostBuilder::run_store`] installs one
//! of these. A step marked [`Scope::remember`]
//! consults it before running its future and appends to it after; that is the
//! whole of the durability claim, and it covers exactly the steps an author asked
//! to be durable.
//!
//! # Why this is not `journal::FileJournal`
//!
//! The *frame grammar* is the estate's and is shared verbatim: [`journal::frame`]
//! holds the length prefix, the 32-byte head, the torn-tail scan and the refusal
//! of a frame no writer produces, and both this store and `journal::file` call
//! those same functions. What cannot be reused is the *record*: `journal::file`
//! frames [`EffectEvent`] and its head chains effect
//! positions, so a step record smuggled through it would claim to be an effect
//! event and chain against a sequence that has nothing to do with steps. So this
//! module states its own record and reuses the frame grammar; the encoding is
//! `lgwks_std::wire` in both. It shares `journal::file`'s storage-owner thread
//! too, so a durable step's `sync_all` costs the device, not the executor.
//!
//! # What a record is
//!
//! One record is the run, the step's key digest, the step's path, the owning
//! tenant, and the archived bytes of the value the step returned. The path is
//! stored beside the digest so an undecodable value is attributable; the digest
//! is what the lookup is on, so two steps cannot read each other's record even
//! if an author gives them the same name.
//!
//! # Bounds
//!
//! Three ceilings, all declared as constants and all reported as typed refusals
//! rather than enforced by truncation: per-record bytes, records per run, and
//! total file bytes. A refusal never deletes, compacts or rewrites a committed
//! record — INV-BOT-14's rule, applied to this store.

use std::collections::HashMap;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lgwks_std::hash::{Digest, Hasher};

use crate::journal::frame::SaturatingFrom;
use lgwks_std::wire::WireError;

use crate::effect::RunId;
use crate::journal::owner::{self, StorageGate, StorageOwner};
use crate::script::run_store::{Appended, RunRecords, StagedRecord};
use crate::script::{FlowError, StepKey};

use super::definition::{DefinitionIdentity, Drift, STORE_FORMAT};

/// The most bytes one archived step record may occupy.
///
/// A step that returns more than this is refused rather than written: the value
/// belongs in the effect journal or in the caller's own storage, not in a record
/// whose whole purpose is to be replayed into memory on resume.
pub const MAX_RECORD_BYTES: usize = 256 * 1024;

/// The most records one run's store retains.
pub const MAX_RECORDS_PER_RUN: u64 = 65_536;

/// The most bytes one run's store file may hold.
pub const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;

/// The constant prefix of every run store's file.
///
/// A file that does not begin with it is not this format and is refused rather
/// than scanned: reading a foreign file as records is how a caller is handed a
/// value nobody wrote. The last byte of the header is the format's version, so a
/// future change is a refusal rather than a misreading. `\x01` was the record
/// without a [`DefinitionIdentity`]; a file in that format still *is* a run
/// store, so it is refused as [`StoreError::FormatVersion`] naming both versions
/// rather than as `NotAStore`, which would claim the bytes were never this
/// store's. See [`check_format_version`] for the rule and the `definition` module
/// for why reading one as the unversioned identity would be the worse answer.
const STORE_MAGIC: &[u8; 16] = b"lgwks-runstore\x00\x00";

/// The header as this crate writes it: the constant prefix and the version.
///
/// Built once so the writer and the reader cannot disagree about which byte is
/// which, which is the only thing a hand-written `OpenOptions` chain would get
/// wrong.
const STORE_HEADER: [u8; 16] = {
    let mut header = *STORE_MAGIC;
    header[15] = STORE_FORMAT;
    header
};

/// The index of the version byte in [`STORE_HEADER`].
///
/// Spelled as a name because the writer, the reader and the refusal all have to
/// agree on *which* byte carries the version, and a literal `15` in three places
/// is three chances for one of them to mean a different byte. It is the last
/// byte, which is what leaves the first [`STORE_MAGIC`] free of version bytes.
const VERSION_BYTE: usize = STORE_MAGIC.len() - 1;

/// Refuse a header whose format this build does not read.
///
/// Two refusals, and the order is the argument: a header whose *first* bytes are
/// not this store's magic was never a run store ([`StoreError::NotAStore`]),
/// while a header whose first bytes match and whose version byte does not is a
/// run store this build cannot read ([`StoreError::FormatVersion`], naming both
/// versions). Reporting the second as the first would tell an operator their
/// data was never theirs, when in fact it was written by an earlier version of
/// this very crate — which is the one conclusion that must never be drawn from a
/// refusal, because it is what makes someone delete the file.
///
/// [`STORE_MAGIC`] is constant across versions and the version byte is not, so
/// "the first bytes match" is exactly "this is some version of this format",
/// and the one comparison below is what keeps those two facts from collapsing
/// into one check.
///
/// # Errors
///
/// [`StoreError::NotAStore`] when the magic does not match, and
/// [`StoreError::FormatVersion`] naming the found and expected versions when it
/// does.
fn check_format_version(header: [u8; STORE_HEADER.len()]) -> Result<(), StoreError> {
    if header[..VERSION_BYTE] != STORE_MAGIC[..VERSION_BYTE] {
        let refusal = Err(StoreError::NotAStore);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_format_version: returning an error to the caller");
        return refusal;
    }
    let found = header[VERSION_BYTE];
    if found == STORE_FORMAT {
        return Ok(());
    }
    Err(StoreError::FormatVersion {
        found,
        expected: STORE_FORMAT,
    })
}

/// The chain digest a store starts from: 32 zero bytes, hashed through the same
/// framing as any record so the first head is a real chain step and not a
/// constant every file shares.
fn genesis_head() -> Digest {
    let mut hasher = Hasher::new();
    hasher.write_framed(b"lgwks-runstore/genesis");
    hasher.finalize()
}

/// Which ceiling an append or a replay hit.
///
/// Named rather than collapsed into one "capacity exceeded" because the repair
/// differs per arm: one is a value too big, one is a run that grew, one is a
/// store that filled. A caller that cannot tell them apart raises every cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoreLimitKind {
    /// One archived record exceeded [`MAX_RECORD_BYTES`].
    RecordBytes,
    /// The run already holds [`MAX_RECORDS_PER_RUN`] records.
    Records,
    /// The store file would exceed [`MAX_STORE_BYTES`].
    StoreBytes,
}

/// Why a record could not be read, written, or resumed.
///
/// A read failure is never absence (INV-BOT-7): every arm here is an error the
/// caller must see, never an empty index that would cause a finished step to
/// re-run as though it had never run.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// A declared ceiling refused the write, or the replay. The store is
    /// unchanged: a refusal is not a compaction.
    Limit {
        /// Which ceiling.
        kind: StoreLimitKind,
        /// What would have been needed.
        requested: u64,
        /// The ceiling.
        limit: u64,
    },
    /// The store file could not be opened, read or written.
    Storage {
        /// What the device said.
        cause: std::io::Error,
    },
    /// The file exists but is not a run store, or its header is short.
    NotAStore,
    /// The file *is* a run store, in a format this build does not read.
    ///
    /// Distinct from [`NotAStore`][Self::NotAStore] because the two say opposite
    /// things about what is on the disk: one says these bytes were never this
    /// store's, the other says they were, and were written by a version of this
    /// crate whose records mean something this build cannot reconstruct. A
    /// pre-version store holds records with no
    /// [`DefinitionIdentity`] in them, and the only
    /// ways to read those are to invent an identity they never carried — which
    /// makes every pre-version resume look exactly compatible — or to discard
    /// evidence a running system is relying on. So the store is refused with both
    /// versions named, and the caller is told which this build wants rather than
    /// being handed a corrupt-store error that says nothing about either.
    ///
    /// This is a breaking change for every existing run store, and it is
    /// deliberately not softened into one: the format has never shipped a version
    /// that could lose a record, so there is nothing to convert, and a deployment
    /// that needs its records keeps its own copy and re-runs.
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
    /// The value could not be archived.
    Encoding {
        /// What the codec said.
        cause: WireError,
    },
    /// The run id belongs to a tenant other than the one resuming it.
    ///
    /// A refusal rather than an absence: the run *is* in this store, and reading
    /// its records for the wrong tenant is a cross-tenant read dressed up as a
    /// miss.
    ForeignTenant {
        /// The tenant that minted the run.
        owner: String,
        /// The tenant that asked to resume it.
        asked: String,
    },
    /// The run id names no run this store holds, so it cannot be attributed to
    /// any tenant here.
    ///
    /// Distinct from [`ForeignTenant`][Self::ForeignTenant] because the two say
    /// opposite things: one says the run exists and belongs elsewhere, the other
    /// says this store has never seen it. A resume in either case is refused —
    /// recording a run this host cannot attribute would put one tenant's work
    /// under another's name.
    UnknownRun {
        /// The run id that was asked for.
        run: String,
        /// The store that could not attribute it.
        tenant: String,
    },
    /// The run's records were written under a different definition identity, so
    /// replaying them would answer a question this build never asked.
    ///
    /// Refused before any step runs and with nothing written, because the two
    /// available answers are both worse: replaying is returning a value the
    /// current definition never produced, and re-running is repeating effects a
    /// previous attempt already performed. Which axis disagrees is named, so the
    /// caller knows whether it needs a new run, a decision, a migration or an
    /// edit.
    Incompatible {
        /// The run whose records disagree.
        run: String,
        /// Which axis, and what each side holds.
        drift: Drift,
    },
}

impl StoreError {
    /// A storage refusal carrying the device's own error.
    ///
    /// A constructor rather than seven literals, because every `map_err` in this
    /// module wraps the same fact — the device said no — and a literal per call
    /// site would be seven chances to name a different variant for the same
    /// event.
    ///
    /// `pub(crate)` because the repair ledger beside this store reports device
    /// refusals in the same vocabulary: one error type for both file-backed
    /// stores means a caller handling a device refusal handles it once.
    pub(crate) fn storage(cause: std::io::Error) -> Self {
        Self::Storage { cause }
    }
}

impl fmt::Display for StoreError {
    /// The refusal, naming the ceiling, the device, or the owning tenant.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Limit {
                kind,
                requested,
                limit,
            } => {
                let what = match kind {
                    StoreLimitKind::RecordBytes => "one record's bytes",
                    StoreLimitKind::Records => "records in one run",
                    StoreLimitKind::StoreBytes => "bytes in one run's store",
                };
                write!(
                    formatter,
                    "{what}: {requested} exceeds the ceiling of {limit}"
                )
            }
            Self::Storage { ref cause } => {
                write!(formatter, "the run store's device refused: {cause}")
            }
            Self::NotAStore => {
                formatter.write_str("this file is not a run store; refusing to read it as records")
            }
            Self::FormatVersion { found, expected } => write!(
                formatter,
                "this is a run store in format version {found}, and this build reads version \
                 {expected}; refusing to read records whose definition identity this version \
                 cannot reconstruct"
            ),
            Self::Corrupt { at } => write!(
                formatter,
                "run store frame {at} does not follow from the records before it; \
                 the bytes are refused, not trimmed"
            ),
            Self::Encoding { ref cause } => {
                write!(formatter, "the step's value could not be archived: {cause}")
            }
            Self::ForeignTenant {
                ref owner,
                ref asked,
            } => write!(
                formatter,
                "run belongs to tenant {owner:?}, not {asked:?}; refusing to read its records"
            ),
            Self::UnknownRun {
                ref run,
                ref tenant,
            } => write!(
                formatter,
                "no records for run {run} in tenant {tenant:?}'s store; refusing to resume a \
                 run this store cannot attribute to it"
            ),
            Self::Incompatible { ref run, ref drift } => write!(
                formatter,
                "run {run} was recorded under a different definition: {}; refusing to replay \
                 it under this one",
                drift
            ),
        }
    }
}

impl std::error::Error for StoreError {
    /// The device's or the codec's own error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Storage { ref cause } => Some(cause),
            Self::Encoding { ref cause } => Some(cause),
            Self::Limit { .. }
            | Self::NotAStore
            | Self::FormatVersion { .. }
            | Self::Corrupt { .. }
            | Self::ForeignTenant { .. }
            | Self::UnknownRun { .. }
            | Self::Incompatible { .. } => None,
        }
    }
}

impl From<StoreError> for FlowError {
    /// A store refusal is permanent: repeating the step would ask the same store
    /// the same question and get the same refusal.
    ///
    /// Two arms and not one, because the refusal's *kind* is what the caller acts
    /// on. A drift is already the flow's own typed [`FlowError::Incompatible`]
    /// carrying its axis, so it stays that rather than being buried one level
    /// down; every other store refusal — an unreadable device, a ceiling, a
    /// foreign tenant — reaches the caller as `FlowError::Store` wrapping the
    /// store's own error, so a read failure is never reported as a definition
    /// drift and never as a rendered string (INV-BOT-7).
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Incompatible { drift, .. } => Self::incompatible("", drift),
            other => Self::Store {
                at: Arc::from(""),
                source: Box::new(other),
            },
        }
    }
}

// ── The store ───────────────────────────────────────────────────────────────

/// One durable, file-backed set of per-step records.
///
/// Cloning shares the file and the index through an `Arc`, so the host's handles
/// see one store rather than one per clone — the rule the admission budget
/// follows, and for the same reason: a second store over one file would be a
/// second writer.
#[derive(Clone)]
pub struct RunStore {
    /// Where the store lives, and the index every clone shares.
    inner: Arc<StoreInner>,
}

/// The one state every clone of a store handle shares, plus the fault the
/// read-failure injector arms.
struct StoreInner {
    /// The thread that holds the file, and the one door an append reaches the
    /// disk through. Its writes are `write_all` plus `sync_all`, so they belong
    /// off the executor — INV-BOT-50.
    owner: StorageOwner<Arc<Mutex<Index>>, Appended>,
    /// Where the file is, for diagnostics.
    path: PathBuf,
    /// The index every clone reads, and the one the owner folds into: per run, per
    /// step key, the record.
    index: Arc<Mutex<Index>>,
    /// Set once by [`RunStore::fail_next_index_read`], and taken by the first
    /// *record* read that observes it.
    ///
    /// `AtomicBool` rather than a `Mutex<Option<..>>` so arming and firing are
    /// two words wide and a read takes no second lock: the read path must not gain
    /// a lock acquisition purely so a fault can be scheduled, and the injection is
    /// a bounded one-shot — `swap`, not `load` — so two racing reads cannot both
    /// take it and the injector cannot leave the store armed forever for a caller
    /// who never expected a fault.
    ///
    /// Taken by [`Self::step_readable`] alone, and never by [`Self::index`]: the
    /// host's own admission reads (`tenant_of`, `definition_of`) run before the
    /// body and would otherwise spend the fault on themselves, which would test
    /// the host's pre-flight rather than the durable step's replay check. A fault
    /// that fires on the wrong read is a fault that proved the wrong thing.
    unreadable: AtomicBool,
}

/// The index, and the committed length it accounts for.
#[derive(Debug)]
struct Index {
    /// Per run: the owning tenant and its steps.
    runs: HashMap<RunId, RunIndex>,
    /// The file length every indexed record accounts for.
    ///
    /// Held beside the index rather than recomputed from it, because the append
    /// fence compares *bytes* against the device and a sum over the index would
    /// have to reproduce the frame overhead exactly — a second definition of the
    /// file's length that could drift from the first.
    committed: u64,
    /// The chain head the next frame follows.
    ///
    /// Carried beside the index because it cannot be recovered from it: a head is
    /// over the *archived* record, so recomputing one would re-archive a value
    /// and risk hashing something other than the bytes on the disk.
    tail: Digest,
}

/// One run's records.
#[derive(Debug)]
struct RunIndex {
    /// The tenant that minted the run, and the only one allowed to read it.
    tenant: String,
    /// The definition every record under this run was written under.
    ///
    /// One per run rather than one per record because a run is one attempt to
    /// execute one definition: a record written under a different definition
    /// than the run's first is not a step that changed, it is a different run
    /// that arrived under the same id, and the first record is what the rest are
    /// compared against.
    definition: DefinitionIdentity,
    /// Per step key hex: the step's path and its archived value.
    steps: HashMap<String, Held>,
}

/// One step's record as the index holds it.
#[derive(Debug)]
struct Held {
    /// The step path, kept so a decode failure is attributable.
    path: String,
    /// The archived value, kept so a resume does not re-read the file per step.
    bytes: Vec<u8>,
}

impl fmt::Debug for RunStore {
    /// The path and the indexed run count. The values are not rendered: a report
    /// or a log line must not carry a step's output into a reader that asked
    /// where the store is.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One read of the index, through the same poison-recovering lock every
        // other reader takes, so the two numbers come from one moment and a
        // poison renders the readable index rather than a pair of zeros.
        let (runs, committed) = {
            let index = self.index();
            (index.runs.len(), index.committed)
        };
        formatter
            .debug_struct("RunStore")
            .field("path", &self.inner.path)
            .field("runs", &runs)
            .field("committed", &committed)
            .finish()
    }
}

impl RunStore {
    /// Open (or create) the store at `path`.
    ///
    /// The file is created with a header if it does not exist, and replayed if
    /// it does. A torn final record is dropped, because an interrupted append is
    /// a prefix cut short and was never anyone's answer; every earlier record is
    /// retained.
    ///
    /// # Errors
    ///
    /// [`StoreError::Storage`] when the device refuses, or when a torn tail
    /// cannot be trimmed; [`StoreError::NotAStore`] when `path` exists and is
    /// not a run store; [`StoreError::Corrupt`] when a committed frame does not
    /// follow from the ones before it.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::open_impl(path.into(), false)
    }

    /// Open a store whose device does not answer a flush until its gate is
    /// released.
    ///
    /// A fault injector, and public for the reason
    /// [`FileJournal::open_with_stalled_storage`](crate::journal::FileJournal::open_with_stalled_storage)
    /// is: "what does this run do while its record store has stopped answering" is
    /// a question an operator has to be able to ask on a real process, and a probe
    /// that only exists inside the crate's own test binary cannot answer it. The
    /// bytes written are real and the frames are the ones the store always writes;
    /// only the device's answer is held back, which is exactly what a stalled
    /// device does. Nothing is acknowledged until the release.
    ///
    /// # Errors
    ///
    /// As [`RunStore::open`].
    pub fn open_with_stalled_device(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::open_impl(path.into(), true)
    }

    /// One constructor for both: a stalled device is a property of the storage
    /// owner, not a second way to open a store.
    ///
    /// # Errors
    ///
    /// As [`RunStore::open`].
    fn open_impl(path: PathBuf, stalled: bool) -> Result<Self, StoreError> {
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(StoreError::storage)?;
        let header = u64::saturating_from(STORE_HEADER.len());
        if !existed || file.metadata().map_err(StoreError::storage)?.len() == 0 {
            file.write_all(&STORE_HEADER)
                .and_then(|()| file.sync_all())
                .map_err(StoreError::storage)?;
        }
        let index = replay(&mut file)?;
        // Trim the incomplete tail rather than appending after it: a write past
        // an interrupted frame leaves a file whose prefix is a prefix of a
        // record and whose middle is another record, which no reader could fold.
        if file.metadata().map_err(StoreError::storage)?.len() != index.committed {
            file.set_len(index.committed).map_err(StoreError::storage)?;
            file.sync_all().map_err(StoreError::storage)?;
        }
        debug_assert!(
            index.committed >= header,
            "the header is part of the committed length"
        );
        // The index is shared, not copied: the owner folds each committed record
        // into it inside the same ordered step as the write, and a lookup reads it
        // from an executor thread. That is not a lock across an `await` — a lookup
        // takes it for a hash-map probe and gives it straight back, and the write
        // it can wait for is the *file's*, which is what the owner thread exists to
        // absorb. Parking on a mutex a few hundred nanoseconds wide is not
        // parking on an `fsync`; it is the second thing the fix is about.
        let index = Arc::new(Mutex::new(index));
        let owner =
            StorageOwner::spawn(file, Arc::clone(&index), stalled).map_err(StoreError::storage)?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                owner,
                path,
                index,
                unreadable: AtomicBool::new(false),
            }),
        })
    }

    /// Make the next *durable step's* record read fail, as an unreadable device would.
    ///
    /// A fault injector, and public for the reason
    /// [`RunStore::open_with_stalled_device`](Self::open_with_stalled_device) is:
    /// "what does a durable step do when its store cannot be read" is a question
    /// INV-BOT-7 answers in a fault nobody can schedule on a real process, and a
    /// probe that exists only inside the crate's own test binary cannot answer it.
    /// The records themselves are untouched — only the *read* of the in-memory
    /// index fails — so the store is exactly one it holds records for, and the
    /// question under test is what the step does with an answer it did not get.
    ///
    /// Aimed at the step rather than at every reader, because the host reads the
    /// same index before the body runs: a fault spent on the host's admission
    /// pre-flight would report `Disposition::Refused` at admission and would never
    /// reach the check whose behaviour this exists to observe.
    ///
    /// One-shot, and bounded by construction: the first step read that observes it
    /// takes it with a `swap`, so a caller that arms it and never reads is not left
    /// with a store that fails forever, and two concurrent reads cannot both take
    /// the same fault.
    pub fn fail_next_index_read(&self) {
        self.inner.unreadable.store(true, Ordering::SeqCst);
    }

    /// A handle that can release this store's parked device, independently of
    /// the store.
    ///
    /// Returned rather than only as [`Self::release_device`] because the caller
    /// that most needs to un-stick the device is the one awaiting a record on it,
    /// and that caller holds the store's borrow for the whole wait.
    #[must_use]
    pub fn storage_gate(&self) -> StorageGate {
        self.inner.owner.gate()
    }

    /// Let the outstanding flush proceed, and every flush after it.
    ///
    /// After this the handle is an ordinary store: the stall is over and is not
    /// re-armed. A caller awaiting a record on this store cannot reach this
    /// method — the borrow the append holds is the same one — and needs the gate
    /// [`Self::storage_gate`] hands back instead.
    pub fn release_device(&self) {
        self.storage_gate().release();
    }

    /// Arm a refusal of the next batch's covering `sync_all`.
    ///
    /// The seam the storage owner's `fail_next_flush` reaches through, so the
    /// all-or-nothing answer of a failed group commit — every member refused, no
    /// fold run, one poison latched, and a reopen that reads the file rather than
    /// the handle — is proved on the shipped store rather than a copy of it. A
    /// test-only door for the same reason `StorageOwner::fail_next_commit` is: no
    /// filesystem refuses a flush on demand.
    #[cfg(test)]
    pub(crate) fn fail_next_flush(&self) {
        self.inner.owner.fail_next_flush();
    }

    /// Open a store under `dir`, named by the tenant that owns it.
    ///
    /// The form [`HostBuilder::run_store`](crate::task::HostBuilder::run_store)
    /// uses. The tenant is part of the file name, so two tenants pointed at one
    /// directory never share a file — and a store installed under two names is
    /// two stores, which is why the cross-tenant refusal below is enforced on the
    /// record rather than on the path.
    ///
    /// # Errors
    ///
    /// As [`RunStore::open`].
    pub fn open_in(dir: &Path, tenant: &str) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).map_err(StoreError::storage)?;
        Self::open(dir.join(format!("{tenant}.runstore")))
    }

    /// Where this store lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// How many records `run` holds.
    #[must_use]
    pub fn record_count(&self, run: RunId) -> usize {
        // A run this store holds nothing for has no records, not an unknown count.
        match self.index().runs.get(&run) {
            Some(held) => held.steps.len(),
            None => 0,
        }
    }

    /// The archived bytes committed for `key` under `run`, or `None` when there
    /// are none.
    ///
    /// The reader's door onto a durable step's value, and the other half of
    /// [`RunStore::record_count`]: a resumed run needs to read back *what* a
    /// previous instance recorded, not merely *that* it recorded something, and
    /// without this the only way to see a record's contents would be to re-run the
    /// step that wrote it.
    ///
    /// `Err` is a read failure — a run another tenant owns — never a miss, so a
    /// caller never reads a refusal as absence (INV-BOT-7).
    ///
    /// # Errors
    ///
    /// [`FlowError`] when `run` belongs to a tenant other than `tenant`.
    pub fn lookup(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
    ) -> Result<Option<Vec<u8>>, FlowError> {
        Ok(RunRecords::lookup(self, tenant, run, key)?.map(|stored| stored.into_bytes()))
    }

    /// The bytes this store has committed, header included.
    #[must_use]
    pub fn committed_bytes(&self) -> u64 {
        self.index().committed
    }

    /// How many `fsync`s this store has paid, and how many records it has staged.
    ///
    /// The evidence that a group commit formed rather than a claim that one could:
    /// the first number counts the `sync_all` calls the storage owner actually
    /// made, the second counts the records it actually staged. A store that flushed
    /// once per record has a ratio of one; a store that batched has a ratio below
    /// one, and the smaller it is the more records each flush carried.
    ///
    /// Both are counted on the owner thread rather than by the callers, so a caller
    /// that gave up on its answer is still counted as a staged record — leaving it
    /// out would report a better batching factor than the store achieved.
    #[must_use]
    pub fn flush_counts(&self) -> (u64, u64) {
        self.inner.owner.flush_counts()
    }

    /// Whether this store has a record under `run`.
    ///
    /// The check `Host::resume` makes before it admits a run: a run id this
    /// store never wrote is one it cannot attribute to a tenant, and resuming it
    /// would be recording another host's run under this host's tenant.
    #[must_use]
    pub fn knows_run(&self, run: RunId) -> bool {
        self.index().runs.contains_key(&run)
    }

    /// Who minted `run`, when the store knows.
    ///
    /// The answer a caller checks before deciding a run id belongs to someone
    /// else, so it names the tenant from the record rather than from the path.
    #[must_use]
    pub fn tenant_of(&self, run: RunId) -> Option<String> {
        let index = self.index();
        index.runs.get(&run).map(|held| held.tenant.clone())
    }

    /// The definition `run`'s records were written under.
    ///
    /// The answer a resume compares its own identity against, through
    /// [`Self::check_definition`]. `None` for a run this store never wrote, which
    /// is not a refusal to read — a process killed before its first record left
    /// nothing to disagree with.
    #[must_use]
    pub fn definition_of(&self, run: RunId) -> Option<DefinitionIdentity> {
        self.index()
            .runs
            .get(&run)
            .map(|held| held.definition.clone())
    }

    /// Refuse `identity` for `run` if the records disagree with it.
    ///
    /// The pre-flight a resume makes before admission: a run whose recorded
    /// definition, input, step count or value schema is not the one being resumed
    /// is refused with [`StoreError::Incompatible`] and nothing is written, so no
    /// step body is polled and no record is added. Called both at the host, before
    /// the body is ever constructed, and by every durable step before it replays a
    /// value — the second because a host with no store is not the only door, and a
    /// store handed a run from elsewhere must not answer from another definition's
    /// records either.
    ///
    /// # Errors
    ///
    /// [`StoreError::Incompatible`] naming the axis that disagrees. A run this
    /// store has no record for is not refused.
    pub fn check_definition(
        &self,
        run: RunId,
        identity: &DefinitionIdentity,
    ) -> Result<(), StoreError> {
        let Some(recorded) = self.definition_of(run) else {
            return Ok(());
        };
        match identity.drift_from(&recorded) {
            None => Ok(()),
            Some(drift) => Err(StoreError::Incompatible {
                run: run.id().to_hex(),
                drift,
            }),
        }
    }

    /// Turn a borrowed record into one that can cross onto the storage owner's
    /// thread.
    ///
    /// The stored record owns its tenant and path, so the hop is the only part that
    /// has to be `'static`, and it takes the record by value rather than borrowing
    /// the caller's strings for the length of a flush.
    fn owned(&self, record: StagedRecord<'_>) -> Staged {
        Staged {
            record: Stored {
                run: record.run(),
                key: record.key().as_bytes().to_vec(),
                tenant: record.tenant().to_owned(),
                path: record.path().to_owned(),
                definition: StoredDefinition::of(record.definition()),
                value: record.bytes().to_vec(),
            },
            key: record.key(),
        }
    }

    /// The index's lock, recovering a poison.
    ///
    /// Recovering is right here for the reason it is right in `journal::owner`: no
    /// arm in the append path panics while the lock is held — every one returns its
    /// `Result` first — so a poison is a bug in an unrelated thread, and
    /// propagating it would brick a store that is still perfectly readable.
    fn index(&self) -> MutexGuard<'_, Index> {
        owner::lock(&self.inner.index)
    }

    /// Refuse a durable step's read when the injector is armed.
    ///
    /// The injector is aimed at the [`RunRecords`] doors rather than at the index
    /// itself, and that placement is the whole design. Both the host's admission
    /// pre-flight and the durable step's replay check read the same in-memory
    /// index through [`Self::check_definition`], so a fault aimed at "the index"
    /// would be spent by whichever of the two ran first — which for a resumed run
    /// is always the host, and a fault the host absorbs tests the host's refusal
    /// rather than the step's, proving nothing about `remember_at`. The
    /// [`RunRecords`] trait is reached only from `Scope::remember`, so arming it
    /// here makes the fault land on the check under test whatever else the run
    /// path has already read.
    ///
    /// One-shot, and bounded by construction: the first step read that observes it
    /// takes it with a `swap`, so a caller that arms it and never reads is not left
    /// with a store that fails forever, and two concurrent reads cannot both take
    /// the same fault.
    fn step_readable(&self) -> Result<(), StoreError> {
        if self.inner.unreadable.swap(false, Ordering::SeqCst) {
            let refusal = Err(StoreError::storage(std::io::Error::other(
                "the run store's device refused a read of its index",
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "step_readable: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }
}

// The append path and the archived record live in their own files.
mod append;
mod record;

use append::Staged;
use record::{Stored, StoredDefinition, replay};

/// A frame whose length prefix lies is refused, and an append that was cut is
/// repaired (#262). The file is built frame by frame with the store's own framing,
/// then one prefix is changed: the bytes are the acknowledged ones, so nothing but
/// the stored head can say whether the length is the writer's.
#[cfg(test)]
mod tests;
