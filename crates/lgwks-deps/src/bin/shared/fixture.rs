//! A scratch directory claimed with an exclusive create, removed on drop.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A directory one run owns.
pub(crate) struct Fixture {
    /// The directory.
    root: PathBuf,
}

impl Fixture {
    /// Claims the first free `<prefix>-<n>` under `base`: `create_dir` is the
    /// exclusive claim, so concurrent runs never share a directory.
    pub(crate) fn claim(base: &Path, prefix: &str) -> io::Result<Self> {
        for attempt in 0..4096_u32 {
            let root = base.join(format!("{prefix}-{attempt}"));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self { root }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("{}: {error}", root.display()),
                    ));
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "no free scratch directory named {prefix}-<n> under {}",
                base.display()
            ),
        ))
    }

    /// The directory.
    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            lgwks_std::trace::debug!(%error, path = ?self.root, "a scratch directory could not be removed");
        }
    }
}
