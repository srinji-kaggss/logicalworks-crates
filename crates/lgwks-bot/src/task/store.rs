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
//! The frame shape is deliberately the estate's — a `u32` big-endian payload
//! length, the archived bytes, then a 32-byte head — and the torn-tail
//! discipline is `journal::file`'s: an interrupted final append is a prefix cut
//! short, so it is dropped, while a frame that does not follow from the ones
//! before it is refused rather than trimmed. What cannot be reused is the
//! *record*: `journal::file` frames [`EffectEvent`](crate::journal::EffectEvent)
//! and its head chains effect positions, so a step record smuggled through it
//! would claim to be an effect event and chain against a sequence that has
//! nothing to do with steps. Polluting the effect ladder with a pseudo-effect
//! would make `EventKind::next` answer a question it should not be asked, and
//! forking the file module into a generic frame codec would duplicate the torn
//! scan this module would then have to keep in step with it. So this module
//! states its own record and reuses the discipline; the encoding is
//! `lgwks_std::wire` in both.
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
use crate::script::run_store::{Appended, RunRecords, StoredValue};
use crate::script::{FlowError, StepKey};

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

/// The magic at the head of every run store's file.
///
/// A file that does not begin with it is not this format and is refused rather
/// than scanned: reading a foreign file as records is how a caller is handed a
/// value nobody wrote. The trailing `\x01` is this format's version, so a future
/// change is a refusal rather than a misreading.
const STORE_MAGIC: &[u8; 16] = b"lgwks-runstore\x00\x01";

/// The byte width of a frame's length prefix.
const LENGTH_BYTES: usize = 4;

/// The byte width of a frame's stored head.
const HEAD_BYTES: usize = 32;

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
            | Self::Corrupt { .. }
            | Self::ForeignTenant { .. } => None,
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
    /// The file, under the lock that makes one writer.
    file: Mutex<File>,
    /// Where the file is, for diagnostics.
    path: PathBuf,
    /// The index every clone reads: per run, per step key, the record.
    index: Mutex<Index>,
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
        let path: PathBuf = path.into();
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(StoreError::storage)?;
        let header = u64::try_from(STORE_MAGIC.len()).unwrap_or(u64::MAX);
        if !existed || file.metadata().map_err(StoreError::storage)?.len() == 0 {
            file.write_all(STORE_MAGIC)
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
        Ok(Self {
            inner: Arc::new(StoreInner {
                file: Mutex::new(file),
                path,
                index: Mutex::new(index),
            }),
        })
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

    /// Who minted `run`, when the store knows.
    ///
    /// The answer a caller checks before deciding a run id belongs to someone
    /// else, so it names the tenant from the record rather than from the path.
    #[must_use]
    pub fn tenant_of(&self, run: RunId) -> Option<String> {
        let index = self.index();
        index.runs.get(&run).map(|held| held.tenant.clone())
    }

    /// The index's lock, recovering a poison.
    fn index(&self) -> MutexGuard<'_, Index> {
        self.inner
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The file's lock, recovering a poison.
    ///
    /// Recovering is right here for the reason it is right in `journal::file`: no
    /// arm in the append path panics while the lock is held — every one returns
    /// its `Result` first — so a poison is a bug in an unrelated thread, and
    /// propagating it would brick a store that is still perfectly readable.
    fn file(&self) -> MutexGuard<'_, File> {
        self.inner
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        bytes: Vec<u8>,
    ) -> Result<Appended, FlowError> {
        let stored = Stored {
            run,
            key: key.as_bytes().to_vec(),
            tenant: tenant.to_owned(),
            path: path.to_owned(),
            value: bytes,
        };
        self.commit(&stored, key).map_err(FlowError::from)
    }
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
    fn commit(&self, stored: &Stored, key: StepKey) -> Result<Appended, StoreError> {
        let mut file = self.file();
        let mut index = self.index();

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

        let previous = tail_head(&index);
        let frame = frame(stored, &previous)?;

        let next = index
            .committed
            .checked_add(u64::try_from(frame.len()).unwrap_or(u64::MAX))
            .ok_or(StoreError::Limit {
                kind: StoreLimitKind::StoreBytes,
                requested: u64::MAX,
                limit: MAX_STORE_BYTES,
            })?;
        if next > MAX_STORE_BYTES {
            return Err(StoreError::Limit {
                kind: StoreLimitKind::StoreBytes,
                requested: next,
                limit: MAX_STORE_BYTES,
            });
        }

        // The fence, checked before a byte is written: the length this handle
        // indexed must be the length on the disk, so a file another writer moved
        // under it is refused here rather than forked. `journal::file` makes the
        // same check for the same reason, and this is the operation it cannot do
        // for us because this store has no owner thread.
        let on_disk = file.metadata().map_err(StoreError::storage)?.len();
        if on_disk != index.committed {
            return Err(StoreError::Corrupt {
                at: u64::try_from(index.runs.get(&stored.run).map_or(0, |run| run.steps.len()))
                    .unwrap_or(u64::MAX),
            });
        }

        file.write_all(&frame).map_err(StoreError::storage)?;
        file.sync_all().map_err(StoreError::storage)?;

        index.committed = next;
        index.runs.entry(stored.run).or_insert_with(|| RunIndex {
            tenant: stored.tenant.clone(),
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
    /// The archived value the step returned.
    #[rkyv(attr(doc = "The archived value the step returned."))]
    value: Vec<u8>,
}

impl Stored {
    /// This record's chain head over the head before it.
    ///
    /// Framed exactly as the step key is framed, so a record's head cannot be
    /// confused with the digest of the bytes it holds: the run, the key, the
    /// tenant and the path are each length-framed, and the value is framed last
    /// so no two different records hash the same byte string.
    fn head_from(&self, previous: &Digest) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(previous.as_bytes());
        hasher.write_framed(&self.run_key_bytes());
        hasher.write_framed(&self.key);
        hasher.write_framed(self.tenant.as_bytes());
        hasher.write_framed(self.path.as_bytes());
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
fn frame(stored: &Stored, previous: &Digest) -> Result<Vec<u8>, StoreError> {
    let payload = to_bytes::<WireError>(stored).map_err(|cause| StoreError::Encoding { cause })?;
    let Ok(length) = u32::try_from(payload.len()) else {
        return Err(StoreError::Limit {
            kind: StoreLimitKind::RecordBytes,
            requested: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            limit: u64::try_from(MAX_RECORD_BYTES).unwrap_or(u64::MAX),
        });
    };
    if payload.len() > MAX_RECORD_BYTES {
        return Err(StoreError::Limit {
            kind: StoreLimitKind::RecordBytes,
            requested: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            limit: u64::try_from(MAX_RECORD_BYTES).unwrap_or(u64::MAX),
        });
    }
    let head = stored.head_from(previous);
    let mut frame = Vec::with_capacity(
        LENGTH_BYTES
            .saturating_add(payload.len())
            .saturating_add(HEAD_BYTES),
    );
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(head.as_bytes());
    Ok(frame)
}

/// How a length prefix read ended.
enum Prefix {
    /// Nothing at all: a clean end of records.
    Eof,
    /// Some of the prefix arrived: an interrupted append.
    Torn,
    /// The whole prefix arrived.
    Full,
}

/// Read the four-byte length prefix, classifying the three ways it can end.
fn read_prefix(reader: &mut impl Read, buf: &mut [u8]) -> Result<Prefix, StoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                return Ok(if filled == 0 {
                    Prefix::Eof
                } else {
                    Prefix::Torn
                });
            }
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(cause) => return Err(StoreError::storage(cause)),
        }
    }
    Ok(Prefix::Full)
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

    let mut header = [0u8; STORE_MAGIC.len()];
    if !read_full(file, &mut header)? || header != *STORE_MAGIC {
        return Err(StoreError::NotAStore);
    }
    let mut index = Index {
        runs: HashMap::new(),
        committed: u64::try_from(STORE_MAGIC.len()).unwrap_or(u64::MAX),
        tail: genesis_head(),
    };
    let mut previous = genesis_head();
    let mut at = 0u64;
    loop {
        let mut prefix = [0u8; LENGTH_BYTES];
        let declared = match read_prefix(file, &mut prefix)? {
            Prefix::Eof => break,
            // A partial length prefix is an append that never finished: it was
            // never anyone's answer, so the scan stops here and the caller trims
            // to `committed`.
            Prefix::Torn => break,
            Prefix::Full => usize::try_from(u32::from_be_bytes(prefix)).unwrap_or(usize::MAX),
        };
        if declared == 0 || declared > MAX_RECORD_BYTES {
            // A complete prefix naming a frame this store never writes cannot be
            // an interrupted append: a write leaves a prefix of a length it did
            // finish computing, and that length was always legal. Refuse.
            return Err(StoreError::Corrupt { at });
        }
        let mut payload = vec![0u8; declared];
        if !read_full(file, &mut payload)? {
            break;
        }
        let mut head = [0u8; HEAD_BYTES];
        if !read_full(file, &mut head)? {
            break;
        }
        let stored: Stored =
            from_bytes::<Stored, WireError>(&payload).map_err(|_| StoreError::Corrupt { at })?;
        if stored.head_from(&previous) != Digest::from_bytes(head) {
            return Err(StoreError::Corrupt { at });
        }
        let frame_len = u64::try_from(
            LENGTH_BYTES
                .saturating_add(declared)
                .saturating_add(HEAD_BYTES),
        )
        .unwrap_or(u64::MAX);
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
        index.tail = stored.head_from(&previous);
        previous = stored.head_from(&previous);
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
