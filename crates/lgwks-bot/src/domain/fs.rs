//! `fs` owns the filesystem domain. Requires `bot.fs`.

use std::path::PathBuf;

use crate::cap::Cap;
use crate::error::BotError;
use crate::verb::{self, Observe};

/// Observe a filesystem path for changes. Supports Observe, Evaluate, Execute, Query.
pub struct Path {
    target: PathBuf,
    caps: Vec<Cap>,
}

/// Filesystem state returned by observation or query.
#[derive(Debug, Clone)]
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

    fn poll(&self) -> Result<FsState, BotError> {
        let exists = self.target.exists();
        let size = if exists {
            std::fs::metadata(&self.target).ok().map(|m| m.len())
        } else {
            None
        };
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

    fn query(&self, _: &()) -> Result<FsState, BotError> {
        self.poll()
    }

    fn domain_id(&self) -> &str {
        "fs::path"
    }
}

/// Convenience: create a filesystem path observer.
pub fn path(target: impl Into<PathBuf>) -> Path {
    Path::new(target)
}
