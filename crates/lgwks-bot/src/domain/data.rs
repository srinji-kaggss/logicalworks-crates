//! `data` owns the data store domain. Requires `bot.fs`.
//!
//! # A store read is only reported once it has settled
//!
//! The store is a file somebody else is writing. Reading it while a writer is
//! half-way through an update produces bytes no one ever held as a whole — a
//! truncated document, a row missing its tail — and reporting those as the
//! current state makes the substrate commit a value and fire a chain against a
//! document that does not exist. So every poll reads through
//! [`crate::stability::read_stable_file`]: two independent reads must agree on
//! the file's length, its modification time and the digest of its bytes before
//! any of it is reported. A file that never settles is
//! [`BotError::UnstableObservation`] — pending, not a change — so the chain
//! keeps the value it already holds and reads again next tick.
//!
//! **Not claimed:** that a settled reading is a *valid* one. Whether the JSON
//! parses is the caller's judgement, because a store whose contents are
//! malformed is a state a bot may legitimately have to act on.

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::stability::{ReadFailure, read_stable_file};
use crate::verb;

/// A JSON store backed by a file path. Supports Observe and Query.
#[derive(Debug)]
pub struct JsonStore {
    /// The file read on each poll. Resolved at construction and retained, so a
    /// relative path is relative to the process working directory at the time
    /// the domain was built, not at poll time.
    path: std::path::PathBuf,
    /// Forced to `[bot.fs]` by the constructor: reading the store is a
    /// filesystem operation and no constructor omits the capability.
    caps: Vec<Cap>,
}

/// Data state returned by observation or query.
///
/// `#[non_exhaustive]`: the store's reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
///
/// Both fields are private behind accessors because the pair is one reading: the
/// flag says the store's contents moved, and the bytes are what it moved to. A
/// caller that could edit `raw` while leaving `changed` alone would hold a
/// reading that claims to be new and is not, and the `changed` condition would
/// fire on a body nobody read from the store.
///
/// ```
/// use lgwks_bot::domain::data::DataState;
///
/// let reading = DataState::new(false, "{\"ok\":true}");
/// assert!(!reading.changed());
/// assert_eq!(reading.raw(), "{\"ok\":true}");
/// ```
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct DataState {
    /// Whether the store contents changed since last poll.
    changed: bool,
    /// The raw JSON string, exactly as read. It is not parsed here: a store
    /// whose contents are malformed JSON is reported, not rejected, so a
    /// condition can act on the malformed state.
    raw: String,
}

impl DataState {
    /// Build the reading a poll or query of the store produced.
    ///
    /// The one constructor, because the fields are private: a `DataState` is
    /// assembled whole, so the flag and the bytes it describes cannot drift
    /// apart between the producer that set them and the condition that reads
    /// them.
    #[must_use]
    pub fn new(changed: bool, raw: impl Into<String>) -> Self {
        Self {
            changed,
            raw: raw.into(),
        }
    }

    /// Whether the store contents changed since last poll.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// The store's bytes exactly as they were read, unparsed.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl JsonStore {
    /// Create a JSON store domain.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            caps: vec![Cap::fs()],
        }
    }
}

impl verb::Observe for JsonStore {
    type Output = DataState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<DataState, BotError> {
        call.0.check(self.required_caps())?;
        let path = self.path.clone();
        let domain = self.domain_id().to_owned();
        let reading = read_stable_file(&path)
            .await
            .map_err(|failure| match failure {
                ReadFailure::Unstable(unstable) => {
                    BotError::UnstableObservation { domain, unstable }
                }
                ReadFailure::Unreadable { cause } => BotError::DomainError {
                    domain,
                    certainty: DispatchCertainty::NotDelivered,
                    cause: cause.to_string(),
                },
            })?;
        let raw = String::from_utf8(reading.bytes().to_vec()).map_err(|error| {
            let refusal = BotError::DomainError {
                domain: self.domain_id().to_owned(),
                certainty: DispatchCertainty::NotDelivered,
                cause: error.to_string(),
            };
            lgwks_std::trace::debug!(
                error = ?Some(&refusal),
                "poll: returning an error to the caller"
            );
            refusal
        })?;
        Ok(DataState::new(false, raw))
    }

    fn domain_id(&self) -> &str {
        "data::json_store"
    }
}

impl verb::Query for JsonStore {
    type Input = ();
    type Output = DataState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<DataState, BotError> {
        let (auth, _) = call;
        verb::Observe::poll(self, (auth, ())).await
    }

    fn domain_id(&self) -> &str {
        "data::json_store"
    }
}

/// Convenience: create a JSON store domain.
pub fn json_store(path: impl Into<std::path::PathBuf>) -> JsonStore {
    JsonStore::new(path)
}
