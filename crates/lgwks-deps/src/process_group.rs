//! Process-group observation, kept as a forward to its one owner.
//!
//! The probe lives in `lgwks_std::process` beside `kill_process_group`, so the
//! kill and the check that follows it go through one syscall binding with one
//! error mapping (#263). This module stays because it is public API of a 1.x
//! crate; it adds nothing to what it forwards to.

use std::io;

/// Return whether a positive process-group identifier currently names a group.
///
/// Forwards to [`lgwks_std::process::process_group_exists`], which documents
/// the semantics: `EPERM` counts as present, `ESRCH` as absent, and a zero or
/// negative id is refused as [`io::ErrorKind::InvalidInput`]. Off Unix it
/// reports [`io::ErrorKind::Unsupported`].
#[deprecated(
    since = "1.1.0",
    note = "use lgwks_std::process::process_group_exists, the one process-group backend"
)]
pub fn exists(pgid: i32) -> io::Result<bool> {
    lgwks_std::process::process_group_exists(pgid)
}
