//! The macOS process table through one `sysctl(KERN_PROC)` syscall (#345).
//!
//! `ps -A` cost a fork and an exec — 12.5 ms p50 on a host with 861 processes
//! — on every supervised cleanup that read the table. The kernel answers the
//! same question in one syscall: `sysctl({CTL_KERN, KERN_PROC, KERN_PROC_ALL})`
//! fills a buffer with one `struct kinfo_proc` per process, carrying the pid,
//! the parent, the group, the run state and the start instant.
//!
//! **The only `unsafe` in the workspace** (Director, 2026-10-12: unsafe is
//! allowed for direct syscalls behind FFI isolation with a Miri proof). The
//! isolation is this file:
//!
//! - one `extern "C"` declaration, `sysctl` from libSystem, the C entry of the
//!   `__sysctl` syscall; nothing else is foreign;
//! - two `unsafe` blocks, both in [`read_bytes`], each one call; every buffer
//!   they hand the kernel is an initialised `Vec<u8>` of the length they name,
//!   and the length the kernel reports back is checked against it before a
//!   byte is kept;
//! - decoding is safe code over `&[u8]` with checked offsets, taken from the
//!   SDK's own headers (`offsetof` on `struct kinfo_proc`: 648 bytes, pid at
//!   40, state at 36, parent at 560, group at 564, start at 0). A buffer whose
//!   length is not a whole number of records is refused rather than guessed
//!   at, so a layout change fails closed.
//!
//! The Miri proof: [`read_bytes`] takes the syscall as a function pointer, so
//! the tests drive the exact `unsafe` call sites against a Rust stand-in that
//! writes into the buffer the way the kernel does — and lies about the length,
//! and fails with `ENOMEM` — under `cargo +nightly miri test`. Miri cannot run
//! the foreign call itself; the live tests below do, outside Miri.

use std::ffi::{c_int, c_uint, c_void};
use std::io;

/// `sizeof(struct kinfo_proc)` in the macOS SDK.
const KINFO_PROC_BYTES: usize = 648;
/// `offsetof(struct kinfo_proc, kp_proc.p_starttime.tv_sec)`.
const OFFSET_START_SEC: usize = 0;
/// `offsetof(struct kinfo_proc, kp_proc.p_starttime.tv_usec)`.
const OFFSET_START_USEC: usize = 8;
/// `offsetof(struct kinfo_proc, kp_proc.p_stat)`.
const OFFSET_STAT: usize = 36;
/// `offsetof(struct kinfo_proc, kp_proc.p_pid)`.
const OFFSET_PID: usize = 40;
/// `offsetof(struct kinfo_proc, kp_eproc.e_ppid)`.
const OFFSET_PPID: usize = 560;
/// `offsetof(struct kinfo_proc, kp_eproc.e_pgid)`.
const OFFSET_PGID: usize = 564;
/// `SZOMB` in `<sys/proc.h>`: exited, awaiting its reap.
const SZOMB: u8 = 5;

/// `CTL_KERN` in `<sys/sysctl.h>`.
const CTL_KERN: c_int = 1;
/// `KERN_PROC` in `<sys/sysctl.h>`.
const KERN_PROC: c_int = 14;
/// `KERN_PROC_ALL`: every process.
const KERN_PROC_ALL: c_int = 0;
/// `KERN_PROC_PID`: one process, by pid.
const KERN_PROC_PID: c_int = 1;
/// `ENOMEM`: the table grew between the size query and the fill.
const ENOMEM: i32 = 12;

/// Fills a size query and a retry for a table that grew in between.
const MAX_ATTEMPTS: usize = 8;
/// Room beyond the size the kernel reported, for processes forked between the
/// two calls: sixteen records.
const HEADROOM_RECORDS: usize = 16;
/// The largest table this reads: 128 MiB, about 200,000 processes, so a host
/// can never make one cleanup allocate without bound.
const MAX_TABLE_BYTES: usize = 128 * 1024 * 1024;

/// One process as the kernel records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct KinfoRow {
    /// The process id.
    pub(super) pid: i32,
    /// The parent's process id.
    pub(super) ppid: i32,
    /// The process group id.
    pub(super) pgid: i32,
    /// Whether the process has exited and awaits its reap.
    pub(super) zombie: bool,
    /// The start instant: seconds since the epoch.
    pub(super) start_sec: i64,
    /// The start instant: microseconds within the second.
    pub(super) start_usec: i32,
}

/// The shape of `sysctl(3)`: name, name length, old buffer, old length, new
/// buffer, new length.
type SysctlFn =
    unsafe extern "C" fn(*mut c_int, c_uint, *mut c_void, *mut usize, *mut c_void, usize) -> c_int;

// SAFETY (declaration): `sysctl` is libSystem's entry for the `__sysctl`
// syscall with exactly this C signature (`<sys/sysctl.h>`); it reads `namelen`
// ints from `name`, writes at most `*oldlenp` bytes to `oldp` and updates
// `*oldlenp`, and reads nothing from `newp` when `newlen` is zero.
unsafe extern "C" {
    fn sysctl(
        name: *mut c_int,
        namelen: c_uint,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
}

/// Every process on the host, in one syscall.
///
/// # Errors
///
/// The syscall's own error, [`io::ErrorKind::InvalidData`] for a buffer that is
/// not a whole number of records, and [`io::ErrorKind::OutOfMemory`] for a
/// table past [`MAX_TABLE_BYTES`] or one that kept growing for
/// [`MAX_ATTEMPTS`] reads.
pub(super) fn table() -> io::Result<Vec<KinfoRow>> {
    let mut name = [CTL_KERN, KERN_PROC, KERN_PROC_ALL];
    decode(&read_bytes(sysctl, io::Error::last_os_error, &mut name)?)
}

/// The process `pid` names, or `None` when the kernel holds no record for it.
///
/// # Errors
///
/// As [`table`].
pub(super) fn one(pid: i32) -> io::Result<Option<KinfoRow>> {
    let mut name = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid];
    let rows = decode(&read_bytes(sysctl, io::Error::last_os_error, &mut name)?)?;
    Ok(rows.into_iter().find(|row| row.pid == pid))
}

/// The bytes `call` writes for `name`: a size query, then a fill with headroom,
/// retried while the table outgrows the buffer.
///
/// `call` is the syscall in production and a stand-in under test, so Miri runs
/// these exact `unsafe` call sites; `last_error` reads the errno a `-1` return
/// leaves, which the stand-in supplies instead.
fn read_bytes(
    call: SysctlFn,
    last_error: fn() -> io::Error,
    name: &mut [c_int],
) -> io::Result<Vec<u8>> {
    let length = c_uint::try_from(name.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "sysctl name is too long"))?;
    for _ in 0..MAX_ATTEMPTS {
        let mut needed: usize = 0;
        // SAFETY: `name` is `length` initialised ints; a null `oldp` asks only
        // for the size, written to `needed`, a live `usize`; `newp` is null
        // with `newlen` zero, so nothing is read from it.
        let sized = unsafe {
            call(
                name.as_mut_ptr(),
                length,
                std::ptr::null_mut(),
                &raw mut needed,
                std::ptr::null_mut(),
                0,
            )
        };
        if sized != 0 {
            return Err(last_error());
        }
        let capacity = needed
            .saturating_add(needed.saturating_div(8))
            .saturating_add(KINFO_PROC_BYTES.saturating_mul(HEADROOM_RECORDS));
        if capacity > MAX_TABLE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!(
                    "the process table needs {capacity} bytes, past the {MAX_TABLE_BYTES}-byte bound"
                ),
            ));
        }
        let mut buffer = vec![0_u8; capacity];
        let mut filled = capacity;
        // SAFETY: `buffer` is `capacity` initialised bytes and `filled` says
        // so; the kernel writes at most `filled` bytes and stores how many it
        // wrote; `newp` is null with `newlen` zero.
        let read = unsafe {
            call(
                name.as_mut_ptr(),
                length,
                buffer.as_mut_ptr().cast::<c_void>(),
                &raw mut filled,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            let error = last_error();
            if error.raw_os_error() == Some(ENOMEM) {
                continue;
            }
            return Err(error);
        }
        if filled > capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("sysctl reported {filled} bytes written into a {capacity}-byte buffer"),
            ));
        }
        buffer.truncate(filled);
        return Ok(buffer);
    }
    Err(io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!("the process table outgrew its buffer on {MAX_ATTEMPTS} reads in a row"),
    ))
}

/// The records in `bytes`, in kernel order.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when `bytes` is not a whole number of
/// records: a layout this decoder was not written for.
fn decode(bytes: &[u8]) -> io::Result<Vec<KinfoRow>> {
    let (records, rest) = bytes.as_chunks::<KINFO_PROC_BYTES>();
    if !rest.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} bytes is not a whole number of {KINFO_PROC_BYTES}-byte kinfo_proc records",
                bytes.len()
            ),
        ));
    }
    records.iter().map(row).collect()
}

/// One record's fields.
fn row(record: &[u8; KINFO_PROC_BYTES]) -> io::Result<KinfoRow> {
    Ok(KinfoRow {
        pid: i32::from_ne_bytes(field(record, OFFSET_PID)?),
        ppid: i32::from_ne_bytes(field(record, OFFSET_PPID)?),
        pgid: i32::from_ne_bytes(field(record, OFFSET_PGID)?),
        zombie: u8::from_ne_bytes(field(record, OFFSET_STAT)?) == SZOMB,
        start_sec: i64::from_ne_bytes(field(record, OFFSET_START_SEC)?),
        start_usec: i32::from_ne_bytes(field(record, OFFSET_START_USEC)?),
    })
}

/// The decoder, the syscall seam under Miri, and the live kernel.
#[cfg(test)]
mod tests;

/// The `N` bytes at `at`, or a refusal naming the offset.
fn field<const N: usize>(record: &[u8], at: usize) -> io::Result<[u8; N]> {
    at.checked_add(N)
        .and_then(|end| record.get(at..end))
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a kinfo_proc record has no {N} bytes at offset {at}"),
            )
        })
}
