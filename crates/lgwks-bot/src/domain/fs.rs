//! `fs` owns the filesystem domain. Requires `bot.fs`.

use std::path::PathBuf;

use crate::cap::{Auth, Cap};
use crate::error::BotError;
use crate::verb;

/// Observe a filesystem path for changes. Supports Observe and Query.
pub struct Path {
    /// The path inspected on each poll. Resolved at construction, so a relative
    /// path is relative to the working directory as it was when the domain was
    /// built rather than at poll time.
    target: PathBuf,
    /// Forced to `[bot.fs]` by the constructor: a stat is a filesystem
    /// operation and no constructor omits the capability.
    caps: Vec<Cap>,
}

/// Filesystem state returned by observation or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FsState {
    /// Whether the path was modified since last poll.
    pub modified: bool,
    /// Whether the path exists.
    pub exists: bool,
    /// File size in bytes, if it exists.
    pub size: Option<u64>,
}

impl Path {
    /// Create a filesystem path observer.
    pub fn new(target: impl Into<PathBuf>) -> Self {
        Self {
            target: target.into(),
            caps: vec![Cap::fs()],
        }
    }
}

impl verb::Observe for Path {
    type Output = FsState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<FsState, BotError> {
        call.0.check(self.required_caps())?;
        let target = self.target.clone();
        let (exists, size) = lgwks_std::task::spawn_blocking(move || {
            let exists = target.exists();
            let size = if exists {
                std::fs::metadata(&target)
                    .ok()
                    .map(|metadata| metadata.len())
            } else {
                None
            };
            (exists, size)
        })
        .await;
        Ok(FsState {
            modified: false,
            exists,
            size,
        })
    }

    fn domain_id(&self) -> &str {
        "fs::path"
    }
}

impl verb::Query for Path {
    type Input = ();
    type Output = FsState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<FsState, BotError> {
        let (auth, _) = call;
        verb::Observe::poll(self, (auth, ())).await
    }

    fn domain_id(&self) -> &str {
        "fs::path"
    }
}

/// Convenience: create a filesystem path observer.
pub fn path(target: impl Into<PathBuf>) -> Path {
    Path::new(target)
}
