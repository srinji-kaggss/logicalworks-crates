//! The `durable-retry` task's fixed vocabulary: the recovery error.
//!
//! One definition for every arm's solution: the prompt fixes this item, so a
//! copy per reference file is how the copies drift. Reference solutions
//! re-export it; model authors write their own from the prompt.

use std::path::{Path, PathBuf};

/// Why an effect was not applied and recovered.
#[derive(Debug, PartialEq, Eq)]
pub enum RecoverError {
    /// The solution could not tell whether the effect landed. Returned never:
    /// a call that cannot tell must find out from the record rather than
    /// report this.
    Unknown,
    /// The release wait exceeded the deadline; nothing further was appended.
    Deadline,
}

/// The application record for one effect directory.
///
/// One path for the solutions and the oracle alike: the oracle counts this
/// file's lines, so a solution that recorded anywhere else would be counting
/// a different effect than the one being measured.
#[must_use]
pub fn record_path(dir: &Path) -> PathBuf {
    dir.join("applied.log")
}

/// The exact line one application records.
#[must_use]
pub fn applied_line(key: &str) -> String {
    format!("applied:{key}\n")
}

/// Whether the record already holds `key`'s application.
///
/// A missing log means nothing was ever applied. Any other read failure
/// propagates: proceeding without knowing would risk the duplicate the record
/// exists to prevent.
pub fn has_application(dir: &Path, key: &str) -> std::io::Result<bool> {
    let wanted = format!("applied:{key}");
    match std::fs::read_to_string(record_path(dir)) {
        Ok(text) => Ok(text.lines().any(|line| line == wanted)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Append one application line, creating the log when missing.
///
/// Append, never overwrite: overwriting would hide the duplicate a missing
/// read-before-apply creates, which is exactly what the negative control
/// demonstrates.
pub fn append_application(dir: &Path, key: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record_path(dir))?;
    log.write_all(applied_line(key).as_bytes())
}

/// Whether the release file exists yet.
#[must_use]
pub fn is_released(dir: &Path) -> bool {
    dir.join("release").exists()
}
