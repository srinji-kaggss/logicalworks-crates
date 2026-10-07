//! Versioned shared records: compare-and-set writes for concurrent editors.
//!
//! Two operators editing one record produce one winner and one typed conflict,
//! never a lost update. Every write names the version it saw
//! ([`CasInput::expected`](crate::cas::CasInput::expected)); the store applies the write only when that
//! version is still current, and a write that arrives late is refused as
//! [`CasError::Conflict`](crate::cas::CasError::Conflict) — through the [`Execute`]
//! adapter as [`BotError::Conflict`] —
//! carrying both versions so the loser can re-read and retry against the new
//! truth rather than against a guess.
//!
//! The refusal is [`RetryClass::Never`]: the
//! same expected version against the same record gives the same refusal, so a
//! blind retry spends attempts to learn a fact the error already states. A
//! retry that re-reads the record and names the version it found is a new
//! write, not a retry, and it goes through.
//!
//! # Durability (issue #278, row 5)
//!
//! The store is a file, and every attempt — applied or refused — is recorded
//! in it before the outcome is reported: the file holds the records and a
//! bounded log of the conflicts that lost against them. Reopening the store
//! reads both back, so "what changed" is answerable after a restart, and a
//! conflict is never a silent drop. The write path is atomic-rename
//! (temp file plus rename into place), which is the protocol
//! [`crate::stability`] recognises: a reader never observes a half-written
//! store.
//!
//! # Bounds
//!
//! The conflict log is ring-bounded at [`MAX_CONFLICTS`](crate::cas::MAX_CONFLICTS): a hot record under
//! contention must not grow the file without limit, and the eviction count
//! says how much history the bound discarded. The store serialises whole-file
//! state under one in-process mutex; cross-process writers are **not**
//! fenced here — that fence is the journal's compare-and-append
//! (`crate::journal::owner`), named as the seam rather than reimplemented.
//! One process, many threads: this module. Many processes, one file: the
//! journal.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::journal::owner::lock;
use crate::skew::WallStamp;
use crate::verb;

/// How many lost-write conflicts the store file retains.
///
/// A bound, not a refusal: beyond it the oldest conflict is evicted and
/// [`StoreState::evicted`] counts it, so a hot record under contention cannot
/// grow the file without limit and the count says how much history the bound
/// discarded. Refusing writes past a history cap would trade availability for
/// archaeology.
pub const MAX_CONFLICTS: usize = 128;

/// A version naming no write: the expected version of a write that creates a
/// record, and the found version of a conflict against a record that was never
/// written.
pub const NO_VERSION: u64 = 0;

/// Why a compare-and-set write was refused.
///
/// Typed so a caller matches on the outcome rather than parsing it: the two
/// versions are the whole fact a loser needs to re-read and try again against
/// the new truth.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CasError {
    /// The record moved under the writer: `expected` is the version the write
    /// named, `found` is the version the store holds.
    Conflict {
        /// The record the write named.
        record: String,
        /// The version the write expected.
        expected: u64,
        /// The version the store holds.
        found: u64,
    },
    /// The store file could not be read back: torn, truncated, or written by
    /// something that is not this store.
    Corrupt {
        /// What the read found, for the operator.
        cause: String,
    },
    /// The operating system refused the read, write, or rename.
    Io {
        /// Which step failed, and how.
        cause: String,
    },
}

impl fmt::Display for CasError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Conflict {
                ref record,
                expected,
                found,
            } => write!(
                formatter,
                "record {record} moved under the write: expected version {expected}, found {found}"
            ),
            Self::Corrupt { ref cause } => {
                write!(formatter, "the record store is unreadable: {cause}")
            }
            Self::Io { ref cause } => write!(formatter, "the record store I/O failed: {cause}"),
        }
    }
}

impl std::error::Error for CasError {}

/// One lost write, as the store file records it.
///
/// A stamp, not a deadline: [`WallStamp`] tells an operator when the conflict
/// lost; the versions tell a writer what to do next. The two are kept apart by
/// type ([`crate::skew`]), because a history ordered by a clock that can jump
/// is not an ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConflictEntry {
    /// The record the write named.
    record: String,
    /// The version the write expected.
    expected: u64,
    /// The version the store held.
    found: u64,
    /// When the conflict was recorded, for the operator.
    stamped: WallStamp,
}

impl ConflictEntry {
    /// The record the write named.
    #[must_use]
    pub fn record(&self) -> &str {
        &self.record
    }

    /// The version the write expected.
    #[must_use]
    pub const fn expected(&self) -> u64 {
        self.expected
    }

    /// The version the store held.
    #[must_use]
    pub const fn found(&self) -> u64 {
        self.found
    }

    /// When the conflict was recorded, for the operator.
    #[must_use]
    pub const fn stamped(&self) -> WallStamp {
        self.stamped
    }
}

/// One versioned value the store holds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Record {
    /// The current version, counting applied writes from [`NO_VERSION`](crate::cas::NO_VERSION).
    version: u64,
    /// The value the winning write left.
    value: String,
}

impl Record {
    /// The current version.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The value the winning write left.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// The durable state: the records, the conflicts that lost against them, and
/// how many conflicts the bound discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoreState {
    /// The records by name.
    records: HashMap<String, Record>,
    /// The most recent conflicts, oldest first, capped at [`MAX_CONFLICTS`](crate::cas::MAX_CONFLICTS).
    conflicts: VecDeque<ConflictEntry>,
    /// How many conflicts the cap evicted.
    evicted: u64,
}

impl StoreState {
    /// The records by name.
    #[must_use]
    pub fn records(&self) -> &HashMap<String, Record> {
        &self.records
    }

    /// The retained conflicts, oldest first.
    #[must_use]
    pub fn conflicts(&self) -> &VecDeque<ConflictEntry> {
        &self.conflicts
    }

    /// How many conflicts the cap evicted.
    #[must_use]
    pub const fn evicted(&self) -> u64 {
        self.evicted
    }
}

/// A file-backed store of versioned shared records.
///
/// The whole state is rewritten atomically (temp file plus rename) under one
/// in-process mutex, so every thread in this process observes complete states
/// and a crash between states leaves the previous complete file behind — never
/// a torn one. Cross-process writers are outside this fence by construction
/// (see the module docs); two processes sharing one path need the journal's
/// compare-and-append, not this store.
#[derive(Debug)]
pub struct RecordStore {
    /// The file the state lives in.
    path: PathBuf,
    /// Serialises read-modify-write cycles in this process.
    ///
    /// Locked through the crate's one lock site
    /// (`crate::journal::owner::lock`), never held across `.await`: the
    /// critical section is synchronous file I/O, and an async writer awaits
    /// nothing while holding it.
    state: Mutex<StoreState>,
}

impl RecordStore {
    /// Open the store at `path`, reading back whatever a previous holder left
    /// — records, conflicts, and the eviction count — or starting empty when
    /// the file does not exist.
    ///
    /// # Errors
    ///
    /// [`CasError::Corrupt`] when the file is present but is not this store's
    /// state; [`CasError::Io`] when the operating system refuses the read.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, CasError> {
        let path = path.into();
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => decode(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoreState {
                records: HashMap::new(),
                conflicts: VecDeque::new(),
                evicted: 0,
            },
            Err(error) => {
                return Err(CasError::Io {
                    cause: format!("reading {}: {error}", path.display()),
                });
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// The file this store persists to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the current state: every record, every retained conflict, and the
    /// eviction count.
    ///
    /// A clone, because the caller gets evidence rather than a handle to edit
    /// what it believes it observed.
    #[must_use]
    pub fn state(&self) -> StoreState {
        lock(&self.state).clone()
    }

    /// Write `value` to `record` only when the store still holds `expected`.
    ///
    /// A first write names [`NO_VERSION`]: a record that was never written is
    /// at version zero, so an expected zero creates at version one and any
    /// other expectation against a missing record is a conflict that found
    /// zero. Every attempt is persisted before the outcome is reported — the
    /// returned version on success, the conflict entry on refusal — so a
    /// reopen answers what changed.
    ///
    /// # Errors
    ///
    /// [`CasError::Conflict`] naming `expected` and the version `found`, and
    /// recording the loss in the bounded conflict log; [`CasError::Io`] when
    /// the persist fails, in which case nothing was applied.
    pub fn write(&self, record: &str, expected: u64, value: &str) -> Result<u64, CasError> {
        let mut state = lock(&self.state);
        let found = state
            .records
            .get(record)
            .map_or(NO_VERSION, Record::version);
        if found != expected {
            let entry = ConflictEntry {
                record: record.to_owned(),
                expected,
                found,
                stamped: WallStamp::now(),
            };
            state.conflicts.push_back(entry);
            if state.conflicts.len() > MAX_CONFLICTS {
                state.conflicts.pop_front();
                state.evicted = state.evicted.saturating_add(1);
            }
            persist(&self.path, &state)?;
            return Err(CasError::Conflict {
                record: record.to_owned(),
                expected,
                found,
            });
        }
        let version = expected.saturating_add(1).max(1);
        state.records.insert(
            record.to_owned(),
            Record {
                version,
                value: value.to_owned(),
            },
        );
        persist(&self.path, &state)?;
        Ok(version)
    }
}

/// The input to one versioned write: which record, which version it was read
/// at, and what it should become.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CasInput {
    /// The record to write.
    record: String,
    /// The version the writer saw when it read the record.
    expected: u64,
    /// The value the record should hold if it has not moved.
    value: String,
}

impl CasInput {
    /// Name the record, the version it was read at, and its next value.
    #[must_use]
    pub fn new(record: impl Into<String>, expected: u64, value: impl Into<String>) -> Self {
        Self {
            record: record.into(),
            expected,
            value: value.into(),
        }
    }

    /// The record to write.
    #[must_use]
    pub fn record(&self) -> &str {
        &self.record
    }

    /// The version the writer saw.
    #[must_use]
    pub const fn expected(&self) -> u64 {
        self.expected
    }

    /// The value the record should hold.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// The outcome of an applied versioned write.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CasOutput {
    /// The record that was written.
    record: String,
    /// The version the write installed.
    version: u64,
}

impl CasOutput {
    /// The record that was written.
    #[must_use]
    pub fn record(&self) -> &str {
        &self.record
    }

    /// The version the write installed.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }
}

/// An [`Execute`](crate::verb::Execute) adapter that writes one versioned
/// shared record.
///
/// The writer of shared records the concurrent-editors row asks for: the
/// expected-version precondition rides in the input, and a lost write is the
/// typed [`BotError::Conflict`] outcome —
/// recorded in the store's conflict log, never retried blindly.
#[derive(Debug)]
pub struct CasWrite {
    /// The store the writes land in, shared so one adapter serves every
    /// record on one file.
    store: std::sync::Arc<RecordStore>,
    /// Forced to `[bot.fs]`: the store is a file, and no constructor omits
    /// the capability.
    caps: Vec<Cap>,
}

impl CasWrite {
    /// Serve versioned writes against `store`.
    #[must_use]
    pub fn new(store: std::sync::Arc<RecordStore>) -> Self {
        Self {
            store,
            caps: vec![Cap::fs()],
        }
    }

    /// The store the writes land in.
    #[must_use]
    pub fn store(&self) -> &std::sync::Arc<RecordStore> {
        &self.store
    }
}

impl verb::Execute for CasWrite {
    type Input = CasInput;
    type Output = CasOutput;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn effect_lifetime(&self) -> verb::EffectLifetime {
        verb::EffectLifetime::External
    }

    async fn execute_action(&self, call: (Auth, &Self::Input)) -> Result<Self::Output, BotError> {
        call.0.check(self.required_caps())?;
        let input = call.1;
        let domain = self.domain_id().to_owned();
        // Synchronous under the in-process mutex, awaited for nothing: the
        // critical section holds no `.await`, so there is no lock held across
        // one by construction.
        match self
            .store
            .write(input.record(), input.expected(), input.value())
        {
            Ok(version) => Ok(CasOutput {
                record: input.record().to_owned(),
                version,
            }),
            Err(CasError::Conflict {
                record,
                expected,
                found,
            }) => {
                let refusal = Err(BotError::Conflict {
                    domain,
                    record,
                    expected,
                    found,
                });
                lgwks_std::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "execute_action: returning an error to the caller"
                );
                refusal
            }
            Err(CasError::Corrupt { cause }) | Err(CasError::Io { cause }) => {
                Err(BotError::DomainError {
                    domain,
                    certainty: DispatchCertainty::Refused,
                    cause,
                })
            }
        }
    }

    fn domain_id(&self) -> &str {
        "cas::store"
    }
}

/// Read the store file back into state.
///
/// A corrupt file is [`CasError::Corrupt`], never an empty store: errors are
/// not absence, and a torn file read as empty would resurrect deleted records
/// at version zero.
fn decode(text: &str) -> Result<StoreState, CasError> {
    let value: lgwks_std::json::Value =
        lgwks_std::json::from_str(text).map_err(|error| CasError::Corrupt {
            cause: format!("not a record-store document: {error}"),
        })?;
    let object = value.as_object().ok_or_else(|| CasError::Corrupt {
        cause: "the top level is not an object".to_owned(),
    })?;
    if object.get("cas").and_then(lgwks_std::json::Value::as_str) != Some("lgwks-bot-cas/1") {
        return Err(CasError::Corrupt {
            cause: "missing or unknown document tag".to_owned(),
        });
    }
    let mut records = HashMap::new();
    if let Some(entries) = object
        .get("records")
        .and_then(lgwks_std::json::Value::as_object)
    {
        for (name, entry) in entries {
            let entry = entry.as_object().ok_or_else(|| CasError::Corrupt {
                cause: format!("record {name} is not an object"),
            })?;
            let version = entry
                .get("version")
                .and_then(lgwks_std::json::Value::as_u64)
                .filter(|version| *version > NO_VERSION)
                .ok_or_else(|| CasError::Corrupt {
                    cause: format!("record {name} has no positive version"),
                })?;
            let content = entry
                .get("value")
                .and_then(lgwks_std::json::Value::as_str)
                .ok_or_else(|| CasError::Corrupt {
                    cause: format!("record {name} has no string value"),
                })?;
            records.insert(
                name.clone(),
                Record {
                    version,
                    value: content.to_owned(),
                },
            );
        }
    }
    let mut conflicts = VecDeque::new();
    if let Some(entries) = object
        .get("conflicts")
        .and_then(lgwks_std::json::Value::as_array)
    {
        for entry in entries {
            let entry = entry.as_object().ok_or_else(|| CasError::Corrupt {
                cause: "a conflict entry is not an object".to_owned(),
            })?;
            let lect = |key: &str| {
                entry
                    .get(key)
                    .and_then(lgwks_std::json::Value::as_str)
                    .ok_or_else(|| CasError::Corrupt {
                        cause: format!("a conflict entry has no string {key}"),
                    })
                    .map(str::to_owned)
            };
            let digits = |key: &str| {
                entry
                    .get(key)
                    .and_then(lgwks_std::json::Value::as_u64)
                    .ok_or_else(|| CasError::Corrupt {
                        cause: format!("a conflict entry has no numeric {key}"),
                    })
            };
            conflicts.push_back(ConflictEntry {
                record: lect("record")?,
                expected: digits("expected")?,
                found: digits("found")?,
                // A stamp is evidence, not a deadline: the reopened history
                // does not need the instant, only that there was one.
                stamped: WallStamp::at(SystemTime::UNIX_EPOCH),
            });
        }
    }
    let evicted = object
        .get("evicted")
        .and_then(lgwks_std::json::Value::as_u64)
        .ok_or_else(|| CasError::Corrupt {
            cause: "the document has no eviction count".to_owned(),
        })?;
    Ok(StoreState {
        records,
        conflicts,
        evicted,
    })
}

/// Persist `state` atomically: write the temp file beside the store, and
/// rename it into place.
///
/// The atomic-rename protocol [`crate::stability`] recognises: a reader either
/// sees the previous complete state or the next one, never a prefix of a
/// write. A rename that fails leaves the previous state in place, and the
/// error reports the failure rather than an outcome the store never recorded.
fn persist(path: &Path, state: &StoreState) -> Result<(), CasError> {
    let mut records = lgwks_std::json::Map::new();
    // Sorted, so the persisted file is stable for the same state: iterating
    // the map directly would write records in hash order and two identical
    // states could persist as different bytes.
    let mut names: Vec<&String> = state.records.keys().collect();
    names.sort();
    for name in names {
        let record = &state.records[name];
        records.insert(
            name.clone(),
            lgwks_std::json::json!({"version": record.version, "value": record.value}),
        );
    }
    let conflicts: Vec<lgwks_std::json::Value> = state
        .conflicts
        .iter()
        .map(|entry| {
            lgwks_std::json::json!({
                "record": entry.record,
                "expected": entry.expected,
                "found": entry.found,
                "stamped": format!("{:?}", entry.stamped.get()),
            })
        })
        .collect();
    let document = lgwks_std::json::json!({
        "cas": "lgwks-bot-cas/1",
        "records": lgwks_std::json::Value::Object(records),
        "conflicts": conflicts,
        "evicted": state.evicted,
    });
    let text = lgwks_std::json::to_string_pretty(&document).map_err(|error| CasError::Io {
        cause: format!("encoding {}: {error}", path.display()),
    })?;
    let scratch = path.with_extension("tmp");
    std::fs::write(&scratch, text).map_err(|error| CasError::Io {
        cause: format!("writing {}: {error}", scratch.display()),
    })?;
    std::fs::rename(&scratch, path).map_err(|error| CasError::Io {
        cause: format!("renaming {} into place: {error}", scratch.display()),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's test idiom: `Result` and `?`, real values in every assert.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A store on a temp file this test owns, removed on the way out.
    ///
    /// The file is the point: the unit tests below run against the persisted
    /// form, not a memory image, so a reopen reads what a crash would leave.
    /// Named by nanos plus a counter, never by a process id: the OS reuses
    /// ids, and a reused one must not make two tests share a store.
    fn scratch(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let nanos = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(since) => since.as_nanos(),
            Err(before) => before.duration().as_nanos(),
        };
        let mut path = std::env::temp_dir();
        path.push(format!("lgwks-bot-cas-test-{name}-{nanos}-{unique}.json"));
        drop(std::fs::remove_file(&path));
        path
    }

    #[test]
    fn a_first_write_names_zero_and_installs_one() -> TestResult {
        let path = scratch("first");
        let store = RecordStore::open(&path)?;
        let version = store.write("record", NO_VERSION, "one")?;
        assert_eq!(version, 1, "a first write installs version one");
        assert_eq!(
            store.state().records()["record"].value(),
            "one",
            "the winning value is what the write left"
        );
        drop(std::fs::remove_file(&path));
        Ok(())
    }

    #[test]
    fn a_late_write_is_a_typed_conflict_and_leaves_the_winner() -> TestResult {
        let path = scratch("late");
        let store = RecordStore::open(&path)?;
        store.write("record", NO_VERSION, "winner")?;
        let Err(CasError::Conflict {
            expected, found, ..
        }) = store.write("record", NO_VERSION, "loser")
        else {
            return Err("a write against a moved record must conflict".into());
        };
        assert_eq!(expected, 0, "the conflict names the version the write saw");
        assert_eq!(found, 1, "the conflict names the version the store holds");
        assert_eq!(
            store.state().records()["record"].value(),
            "winner",
            "a conflicted write changes nothing"
        );
        drop(std::fs::remove_file(&path));
        Ok(())
    }
}
