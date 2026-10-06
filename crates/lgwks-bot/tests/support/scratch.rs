//! A scratch directory that dies with the run that made it.
//!
//! One definition of "a directory under the temp root that this run owns" for
//! every test family and every measurement example, included by path, so no
//! harness can be the one that invents a second naming scheme or forgets to
//! clean up.
//!
//! The name is random rather than derived from a pid or a clock: the OS reuses
//! both, so two runs on one machine would otherwise share a directory and read
//! each other's durable records or pid files (INV-BOT-116). A name the entropy
//! source cannot draw is refused rather than reused.

use std::path::{Path, PathBuf};

/// A directory under the system temp root, removed when this value is dropped.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A fresh, empty directory named for `tag`.
    ///
    /// # Errors
    ///
    /// Whatever the entropy source or the filesystem reports. A directory that
    /// cannot be named uniquely is refused rather than reused, and a directory
    /// that cannot be created is not handed to a run.
    pub fn new(tag: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let unique = lgwks_std::random::bytes::<8>()?;
        let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("lgwks-{tag}-{hex}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    /// Where the directory is.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    /// Remove the directory and everything a run put in it.
    ///
    /// Best effort, and deliberately so: every assertion has already been made,
    /// a directory the system refuses to remove is litter under the temp root
    /// rather than a wrong answer, and a removal error raised from `Drop` could
    /// not be reported to the caller anyway.
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}
