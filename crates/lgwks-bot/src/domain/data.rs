//! `data` owns the data store domain. Requires `bot.fs`.

use crate::cap::{Auth, Cap};
use crate::error::BotError;
use crate::verb;

/// A JSON store backed by a file path. Supports Observe and Query.
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
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DataState {
    /// Whether the store contents changed since last poll.
    pub changed: bool,
    /// The raw JSON string, exactly as read. It is not parsed here — a store
    /// whose contents are malformed JSON is reported, not rejected, so a
    /// condition can act on the malformed state.
    pub raw: String,
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
        let raw = lgwks_std::task::spawn_blocking(move || std::fs::read_to_string(&path))
            .await
            .map_err(|error| BotError::DomainError {
                domain: self.domain_id().into(),
                cause: error.to_string(),
            })?;
        Ok(DataState {
            changed: false,
            raw,
        })
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
