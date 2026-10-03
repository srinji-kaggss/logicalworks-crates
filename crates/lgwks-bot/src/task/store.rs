//! The durable, file-backed per-step record store behind a resumable run.
//!
//! [`HostBuilder::run_store`](crate::task::HostBuilder::run_store) installs one
//! of these. A step marked [`Scope::remember`](crate::script::Scope::remember)
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
//! frames [`EffectEvent`](crate::journal::EffectEvent) and its head chains effect
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
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::wire::{WireError, from_bytes, to_bytes};

use crate::effect::RunId;
use crate::journal::frame::{self, HEAD_BYTES, LENGTH_BYTES, Piece, Prefix};
use crate::journal::owner::{self, StorageGate, StorageOwner, SubmitError};
use crate::script::run_store::{Appended, RunRecords, StagedRecord, StoredValue};
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
        return Err(StoreError::NotAStore);
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
    /// [`DefinitionIdentity`](super::DefinitionIdentity) in them, and the only
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
    fn storage(cause: std::io::Error) -> Self {
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
    fn from(error: StoreError) -> Self {
        Self::Failed {
            at: Arc::from(""),
            reason: error.to_string(),
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

/// The state one store owns, shared by every clone of its handle.
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
        formatter
            .debug_struct("RunStore")
            .field("path", &self.inner.path)
            .field(
                "runs",
                &self.inner.index.lock().map_or(0, |index| index.runs.len()),
            )
            .field(
                "committed",
                &self.inner.index.lock().map_or(0, |index| index.committed),
            )
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
        let header = u64::try_from(STORE_HEADER.len()).unwrap_or(u64::MAX);
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
            inner: Arc::new(StoreInner { owner, path, index }),
        })
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
        self.index()
            .runs
            .get(&run)
            .map_or(0, |held| held.steps.len())
    }

    /// The bytes this store has committed, header included.
    #[must_use]
    pub fn committed_bytes(&self) -> u64 {
        self.index().committed
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
        let index = self.index();
        index.runs.get(&run).map(|held| held.definition.clone())
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
}

/// The store as the script module reaches it: a typed lookup and a typed append
/// over archived bytes, with no `lgwks_bot::task` in its own surface.
///
/// The implementation is here; the trait lives in
/// [`script`](crate::script) because the task module is a *consumer* of a
/// durable step, and a step must be markable without the host that runs it.
impl RunRecords for RunStore {
    fn lookup(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
    ) -> Result<Option<StoredValue>, FlowError> {
        let index = self.index();
        let Some(held) = index.runs.get(&run) else {
            return Ok(None);
        };
        if held.tenant != tenant {
            return Err(StoreError::ForeignTenant {
                owner: held.tenant.clone(),
                asked: tenant.to_owned(),
            }
            .into());
        }
        Ok(held
            .steps
            .get(&key.to_hex())
            .map(|step| StoredValue::new(step.path.clone(), step.bytes.clone())))
    }

    fn append(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
        path: &str,
        definition: &DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> Result<Appended, FlowError> {
        let staged = self.owned(RunRecords::stage(
            self, tenant, run, key, path, definition, bytes,
        ));
        self.commit(staged.record, staged.key)
            .map_err(FlowError::from)
    }

    /// Whether the records for `run` were written under `definition`.
    ///
    /// The check every durable step makes before it returns a recorded value. A
    /// run this store holds nothing for is compatible by definition: that is a
    /// first attempt, and refusing it would make the first record unreachable.
    fn compatibility(
        &self,
        run: RunId,
        definition: &DefinitionIdentity,
    ) -> Result<bool, FlowError> {
        self.check_definition(run, definition)
            .map(|()| true)
            .map_err(FlowError::from)
    }

    /// The awaited door, which is the one a durable step takes.
    ///
    /// The `sync_all` behind it is the device's cost, and this is what keeps that
    /// cost off the executor: the step's future waits for the owner's answer
    /// instead of sitting through the flush (INV-BOT-50).
    fn append_async<'a>(
        &'a self,
        record: StagedRecord<'a>,
    ) -> crate::BoxFuture<'a, Result<Appended, FlowError>> {
        let staged = self.owned(record);
        Box::pin(async move {
            self.commit_async(staged.record, staged.key)
                .await
                .map_err(FlowError::from)
        })
    }
}

/// Map one owner refusal onto this store's vocabulary.
///
/// One conversion rather than a match per call site, because every `?` on an owner
/// refusal wraps the same fact — the device, or the handle, said no — and a literal
/// per call site would be four chances to name a different variant for one event.
/// The refusal's own message distinguishes a full queue from a poison from an
/// undelivered answer, so nothing is lost by carrying it as the device's error.
impl From<SubmitError> for StoreError {
    fn from(cause: SubmitError) -> Self {
        Self::Storage {
            cause: cause.into_io(),
        }
    }
}

/// One staged append: the record to write and the step key it is for.
///
/// Its own type so the two append doors build the record once between them, and
/// the key travels with it rather than being threaded beside it at each call site.
struct Staged {
    /// The record as it will be archived.
    record: Stored,
    /// The step key the already-recorded and conflict checks are on.
    key: StepKey,
}

/// The in-file chain head of one run, held with the index.
impl RunStore {
    /// The chain step of `stored`: the head of the whole file, which every run
    /// shares, so a frame's head cannot be valid under one run's history and
    /// invalid under the file's.
    ///
    /// One chain for the file rather than one per run, deliberately. A per-run
    /// chain would need the head to be re-found by replaying only that run's
    /// frames, and a caller that read one run's records could then not verify
    /// them without reading every other tenant's records too. One file, one
    /// chain, and the tenant check above is what keeps the reads separated.
    ///
    /// The whole of this runs on the storage owner's thread — the in-memory checks,
    /// the fence, the write, the `sync_all` and the fold into the index — so two
    /// appends to this store are two ordered steps rather than two racing ones,
    /// and none of them parks the executor the `sync_all` does. INV-BOT-50.
    fn commit(&self, stored: Stored, key: StepKey) -> Result<Appended, StoreError> {
        self.inner
            .owner
            .submit(move |file, index| append_on_owner(file, index, &stored, key))
            .map_err(StoreError::from)
    }

    /// The same append, awaited rather than sat through.
    ///
    /// The one a durable step takes, because a step's body is a future and the
    /// `sync_all` its record costs is the device's, not the executor's.
    fn commit_async<'a>(
        &'a self,
        stored: Stored,
        key: StepKey,
    ) -> crate::BoxFuture<'a, Result<Appended, StoreError>> {
        Box::pin(async move {
            self.inner
                .owner
                .submit_async(move |file, index| append_on_owner(file, index, &stored, key))
                .await
                .map_err(StoreError::from)
        })
    }
}

/// The ordered append, over the file and the index it folds into.
///
/// A free function taking the shared state rather than a method on `&self`, because
/// it runs on the storage owner's thread with a closure that must own everything it
/// touches: the `Arc` the store's state lives in and the record itself.
///
/// # Errors
///
/// Every [`StoreError`] an append can produce, carried as the device's own error so
/// the owner's one reply channel serves both stores.
fn append_on_owner(
    file: &mut File,
    shared: &Arc<Mutex<Index>>,
    stored: &Stored,
    key: StepKey,
) -> std::io::Result<Appended> {
    // Taken for the whole decision, the write and the fold, so a second append on
    // this store cannot observe a half-committed index. The wait is bounded by the
    // device rather than by the map, and it happens on the owner's thread, never on
    // the executor's.
    let mut index = owner::lock(shared);
    decide_and_write(file, &mut index, stored, key)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// The decision and the write, in one ordered step over the file and the index.
///
/// Every check that can refuse in memory runs before a byte moves, and the length
/// fence runs inside the owner's ordered step where no other append can overtake it
/// — which is why the file's `metadata` is read here rather than through the
/// read-only view: the authoritative answer is the one taken at the moment of the
/// write.
///
/// # Errors
///
/// Every [`StoreError`] an append can produce. Nothing is written by any of them,
/// and a refusal leaves the index and the file byte-identical.
fn decide_and_write(
    file: &mut File,
    index: &mut Index,
    stored: &Stored,
    key: StepKey,
) -> Result<Appended, StoreError> {
    let owned = index.runs.get(&stored.run);
    if let Some(owned) = owned {
        if owned.tenant != stored.tenant {
            return Err(StoreError::ForeignTenant {
                owner: owned.tenant.clone(),
                asked: stored.tenant.clone(),
            });
        }
        if let Some(step) = owned.steps.get(&key.to_hex()) {
            // Already recorded. An identical record is the same fact seen
            // twice — a step whose value was returned and whose caller
            // recorded it again — and a different one is a conflict. Neither
            // writes a byte.
            return Ok(if step.bytes == stored.value {
                Appended::AlreadyRecorded
            } else {
                Appended::Conflicting
            });
        }
        if u64::try_from(owned.steps.len()).unwrap_or(u64::MAX) >= MAX_RECORDS_PER_RUN {
            return Err(StoreError::Limit {
                kind: StoreLimitKind::Records,
                requested: u64::try_from(owned.steps.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
                limit: MAX_RECORDS_PER_RUN,
            });
        }
    }

    let previous = tail_head(index);
    let (framed, head) = frame(stored, &previous)?;

    let staged = u64::try_from(framed.len()).unwrap_or(u64::MAX);
    let next = index
        .committed
        .checked_add(staged)
        .ok_or_else(|| store_full(u64::MAX))?;
    if next > MAX_STORE_BYTES {
        return Err(store_full(next));
    }

    // The fence, checked before a byte is written: the length this handle
    // indexed must be the length on the disk, so a file another writer moved
    // under it is refused here rather than forked. It runs on the owner thread,
    // so the check and the write it guards cannot be split by another append.
    let on_disk = file.metadata().map_err(StoreError::storage)?.len();
    if on_disk != index.committed {
        return Err(StoreError::Corrupt {
            at: u64::try_from(index.runs.get(&stored.run).map_or(0, |run| run.steps.len()))
                .unwrap_or(u64::MAX),
        });
    }

    file.write_all(&framed).map_err(StoreError::storage)?;
    file.sync_all().map_err(StoreError::storage)?;

    index.committed = next;
    index.tail = head;
    index.runs.entry(stored.run).or_insert_with(|| RunIndex {
        tenant: stored.tenant.clone(),
        definition: stored.definition.to_identity(),
        steps: HashMap::new(),
    });
    if let Some(run) = index.runs.get_mut(&stored.run) {
        run.steps.insert(
            hex_of(&stored.key),
            Held {
                path: stored.path.clone(),
                bytes: stored.value.clone(),
            },
        );
    }
    Ok(Appended::Recorded)
}

/// The chain head the next frame in this file follows.
///
/// The index carries the tail head beside the committed length, because the head
/// cannot be recomputed from the index: a frame's head is over the *archived*
/// record, and re-archiving a value to hash it would be a second encoding of the
/// same bytes with its own chance of disagreeing with what is on the disk.
fn tail_head(index: &Index) -> Digest {
    index.tail
}

/// The record as it is archived and as it is checked.
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
    /// The run this record belongs to.
    #[rkyv(attr(doc = "The run this record belongs to."))]
    run: RunId,
    /// The step key's digest.
    #[rkyv(attr(doc = "The step key's digest."))]
    key: Vec<u8>,
    /// The tenant that minted the run.
    #[rkyv(attr(doc = "The tenant that minted the run."))]
    tenant: String,
    /// The step's path, for attribution.
    #[rkyv(attr(doc = "The step's path, for attribution."))]
    path: String,
    /// The definition the record was written under, as its fields.
    ///
    /// Archived as fields rather than as one digest because a drift refusal has
    /// to *name* the two sides: a head over the digest could detect the
    /// disagreement but could not report which axis it was, and the fields are
    /// what a caller reads to decide whether it needs a new run or a migration.
    #[rkyv(attr(doc = "The definition the record was written under, as its fields."))]
    definition: StoredDefinition,
    /// The archived value the step returned.
    #[rkyv(attr(doc = "The archived value the step returned."))]
    value: Vec<u8>,
}

/// A [`DefinitionIdentity`] in the shape a record archives.
///
/// Named rather than archived directly so the record's on-disk shape is stated
/// once, in the store that writes it, and a future revision of the definition
/// vocabulary is a new struct here rather than a change to a type every consumer
/// of [`Drift`] is already matching.
#[derive(
    Debug, Clone, lgwks_std::wire::Archive, lgwks_std::wire::Serialize, lgwks_std::wire::Deserialize,
)]
#[rkyv(
    attr(non_exhaustive),
    crate = lgwks_std::wire::rkyv,
    compare(PartialEq),
    derive(Debug)
)]
struct StoredDefinition {
    /// The task name.
    #[rkyv(attr(doc = "The task name."))]
    name: String,
    /// The declared definition revision.
    #[rkyv(attr(doc = "The declared definition revision."))]
    revision: u64,
    /// The input digest.
    #[rkyv(attr(doc = "The input digest."))]
    input: Vec<u8>,
    /// How many durable steps the definition declares.
    #[rkyv(attr(doc = "How many durable steps the definition declares."))]
    steps: u64,
    /// The declared durable-value schema id.
    #[rkyv(attr(doc = "The declared durable-value schema id."))]
    codec: String,
}

/// The 32 bytes a record archived, as the digest the identity carries back.
///
/// A fixed width rather than a slice, because a record whose input field is not
/// 32 bytes is a corrupt frame and the checked decoder is what says so; the
/// conversion cannot silently truncate, because `as` is forbidden in this
/// workspace and this is the honest alternative to it.
fn digest_from_record(bytes: &[u8]) -> Digest {
    let mut digest = [0u8; 32];
    let copied = bytes.len().min(digest.len());
    digest[..copied].copy_from_slice(&bytes[..copied]);
    Digest::from_bytes(digest)
}

impl StoredDefinition {
    /// The record's archived form of a live identity.
    fn of(identity: &DefinitionIdentity) -> Self {
        Self {
            name: identity.name().to_owned(),
            revision: identity.revision(),
            input: identity.input().as_bytes().to_vec(),
            steps: u64::try_from(identity.steps()).unwrap_or(u64::MAX),
            codec: identity.codec().to_owned(),
        }
    }

    /// The live identity this record was written under.
    fn to_identity(&self) -> DefinitionIdentity {
        DefinitionIdentity::new(
            &self.name,
            self.revision,
            digest_from_record(&self.input),
            usize::try_from(self.steps).unwrap_or(usize::MAX),
        )
        .with_codec(&self.codec)
    }
}

impl Stored {
    /// This record's chain head over the head before it.
    ///
    /// Framed exactly as the step key is framed, so a record's head cannot be
    /// confused with the digest of the bytes it holds: the run, the key, the
    /// tenant, the path and every definition field are length-framed, and the
    /// value is framed last so no two different records hash the same byte
    /// string. The definition's own bytes go in through the identity's framing
    /// so a head over the record commits to the identity the same way the
    /// identity's digest commits to it.
    fn head_from(&self, previous: &Digest, _archived: &[u8]) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(previous.as_bytes());
        hasher.write_framed(&self.run_key_bytes());
        hasher.write_framed(&self.key);
        hasher.write_framed(self.tenant.as_bytes());
        hasher.write_framed(self.path.as_bytes());
        hasher.write_framed(self.definition.name.as_bytes());
        hasher.write_framed(&self.definition.revision.to_le_bytes());
        hasher.write_framed(&self.definition.input);
        hasher.write_framed(&self.definition.steps.to_le_bytes());
        hasher.write_framed(self.definition.codec.as_bytes());
        hasher.write_framed(&self.value);
        hasher.finalize()
    }

    /// The run id's 16 bytes, as a field the framing can hold.
    ///
    /// The hex spelling rather than the big-endian integer, because
    /// `RunId::to_hex` is the one conversion this crate defines for the role
    /// and a second one here would be a second definition to keep in step.
    fn run_key_bytes(&self) -> Vec<u8> {
        self.run.id().to_hex().into_bytes()
    }
}

/// The frame for one record: length, payload, head.
///
/// The head is stored rather than recomputed on faith. That is what separates a
/// torn tail (dropped, because a write leaves a prefix and a prefix has no head)
/// from a frame that does not follow (refused, because those bytes were
/// acknowledged).
fn frame(stored: &Stored, previous: &Digest) -> Result<(Vec<u8>, Digest), StoreError> {
    // Archive, bound, chain and lay out — the same step the effect journal takes.
    // Only the record's own archiving and its step-chaining head are this store's.
    frame::frame_record(
        stored,
        previous,
        MAX_RECORD_BYTES,
        |stored| {
            to_bytes::<WireError>(stored)
                .map(|bytes| bytes.as_ref().to_vec())
                .map_err(|cause| StoreError::Encoding { cause })
        },
        Stored::head_from,
        record_too_large,
    )
}

/// The refusal for a store that has no room left, named by what it would have
/// needed.
///
/// One constructor because both call sites discovered the same fact by different
/// routes — an addition that overflowed and an addition that came out past the
/// ceiling — and a literal per site would be two chances to name a different bound
/// for the same ceiling.
fn store_full(requested: u64) -> StoreError {
    StoreError::Limit {
        kind: StoreLimitKind::StoreBytes,
        requested,
        limit: MAX_STORE_BYTES,
    }
}

/// The refusal for a record whose archived bytes are past this store's ceiling.
///
/// One constructor, because the message and the two numbers are the same fact
/// whichever call site discovers it, and a literal per site would be two chances to
/// name a different bound for the same ceiling.
fn record_too_large(len: usize) -> StoreError {
    StoreError::Limit {
        kind: StoreLimitKind::RecordBytes,
        requested: u64::try_from(len).unwrap_or(u64::MAX),
        limit: u64::try_from(MAX_RECORD_BYTES).unwrap_or(u64::MAX),
    }
}

/// Read `buf` in full, reporting whether all of it arrived.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, StoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(cause) => return Err(StoreError::storage(cause)),
        }
    }
    Ok(true)
}

/// Read every committed frame, returning the index it implies.
///
/// The scan is bounded by the file's own length: every iteration consumes at
/// least one byte, and a file that grows underneath the scan cannot extend it,
/// because the reader stops at the length it saw when it began. A frame whose
/// stored head does not follow is [`StoreError::Corrupt`] and is not truncated.
fn replay(file: &mut File) -> Result<Index, StoreError> {
    let total = file.metadata().map_err(StoreError::storage)?.len();
    file.seek(SeekFrom::Start(0)).map_err(StoreError::storage)?;

    let mut header = [0u8; STORE_HEADER.len()];
    if !read_full(file, &mut header)? {
        return Err(StoreError::NotAStore);
    }
    // The two refusals are separated, and the version one is checked first,
    // because they are different facts and only one of them is a version the
    // writer could have meant. Every version of this format shares the whole
    // magic *except* its last byte, which is that format's version — so a file
    // whose first fifteen bytes match is this store's, at some version, and a
    // file whose first fifteen do not match was never a run store at all. That
    // is the whole reason `STORE_MAGIC` is 15 bytes of constant plus a version
    // byte rather than one 16-byte constant.
    check_format_version(header)?;
    let mut index = Index {
        runs: HashMap::new(),
        committed: u64::try_from(STORE_HEADER.len()).unwrap_or(u64::MAX),
        tail: genesis_head(),
    };
    let mut previous = genesis_head();
    let mut at = 0u64;
    loop {
        let mut prefix = [0u8; LENGTH_BYTES];
        let declared = match frame::read_prefix(file, &mut prefix).map_err(StoreError::storage)? {
            Prefix::Eof => break,
            // A partial length prefix is an append that never finished: it was
            // never anyone's answer, so the scan stops here and the caller trims
            // to `committed`.
            Prefix::Torn => break,
            Prefix::Full => frame::declared_length(&prefix),
        };
        if !frame::is_possible_length(declared, MAX_RECORD_BYTES) {
            // A complete prefix naming a frame this store never writes cannot be
            // an interrupted append: a write leaves a prefix of a length it did
            // finish computing, and that length was always legal. Refuse.
            return Err(StoreError::Corrupt { at });
        }
        let mut payload = vec![0u8; declared];
        if let Piece::Interrupted =
            frame::read_piece(file, &mut payload).map_err(StoreError::storage)?
        {
            break;
        }
        let mut head = [0u8; HEAD_BYTES];
        if let Piece::Interrupted =
            frame::read_piece(file, &mut head).map_err(StoreError::storage)?
        {
            break;
        }
        let stored: Stored =
            from_bytes::<Stored, WireError>(&payload).map_err(|_| StoreError::Corrupt { at })?;
        if stored.head_from(&previous, &payload) != Digest::from_bytes(head) {
            return Err(StoreError::Corrupt { at });
        }
        let frame_len = frame::framed_len(declared);
        // A frame that runs past the end of the file is an interrupted append.
        // Readable as a whole above, so this only catches a header that lied
        // about a frame already acknowledged, which is rot rather than a crash.
        if index
            .committed
            .checked_add(frame_len)
            .is_none_or(|end| end > total)
        {
            break;
        }
        index.committed = index.committed.saturating_add(frame_len);
        index.tail = stored.head_from(&previous, &payload);
        previous = stored.head_from(&previous, &payload);
        at = at.saturating_add(1);
        if at > MAX_RECORDS_PER_RUN {
            return Err(StoreError::Limit {
                kind: StoreLimitKind::Records,
                requested: at,
                limit: MAX_RECORDS_PER_RUN,
            });
        }
        let run = index.runs.entry(stored.run).or_insert_with(|| RunIndex {
            tenant: stored.tenant.clone(),
            definition: stored.definition.to_identity(),
            steps: HashMap::new(),
        });
        run.steps.insert(
            hex_of(&stored.key),
            Held {
                path: stored.path.clone(),
                bytes: stored.value.clone(),
            },
        );
    }
    Ok(index)
}

/// Lowercase hex, for a digest used as a map key.
///
/// `lgwks_std::hash::Digest::to_hex` takes a `Digest`, and this is the same 32
/// bytes before the head has been computed; formatting them here rather than
/// reaching into the digest type keeps the key exactly the bytes on the disk.
fn hex_of(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes.iter().copied() {
        hex.push(DIGITS[usize::from(byte >> 4) & 0x0f]);
        hex.push(DIGITS[usize::from(byte) & 0x0f]);
    }
    // Every byte pushed above is an ASCII digit, so this is well-formed UTF-8
    // and the conversion cannot fail; `String::from_utf8` reports the invariant
    // rather than trusting it.
    String::from_utf8(hex).unwrap_or_default()
}
