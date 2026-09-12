//! `data` owns the data store domain. Requires `bot.fs`.

use crate::cap::{Auth, Cap};
use crate::error::BotError;
use crate::verb;

/// A JSON store backed by a file path. Supports Observe and Query.
pub struct JsonStore {
    path: std::path::PathBuf,
    caps: Vec<Cap>,
}

/// Data state returned by observation or query.
#[derive(Debug, Clone)]
pub struct DataState {
    /// Whether the store contents changed since last poll.
    pub changed: bool,
    /// The raw JSON string.
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
            .map_err(|e| BotError::DomainError {
                domain: self.domain_id().into(),
                cause: e.to_string(),
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
