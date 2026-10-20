//! Windows process primitives through `kernel32` (#263, the Windows half).
//!
//! The Unix primitives this module matches — signal a process, observe it,
//! capture its descendants, name it by its start — have no `std` form on
//! Windows, and the Win32 calls below are the operating system's own boundary
//! for them (the NT syscall numbers are not a stable interface; `kernel32` is).
//! Admitted by the Director's ruling of 2026-10-12: `unsafe` for direct system
//! calls only, behind FFI isolation, with a Miri proof.
//!
//! The isolation is this file:
//!
//! - one `extern "system"` block naming fourteen `kernel32` functions; nothing
//!   else is foreign;
//! - every call goes through [`Api`], a table of function pointers, so the
//!   tests drive these exact `unsafe` call sites against Rust stand-ins under
//!   the `miri-unsafe` gate lane (`cargo +nightly-2026-10-01 miri test
//!   --target x86_64-pc-windows-msvc`);
//! - every handle is an [`Owned`] that closes itself once, on drop;
//! - every buffer handed to the kernel is a `#[repr(C)]` value of the size it
//!   declares (`PROCESSENTRY32W.dwSize`, `FILETIME`), never a byte slice.
//!
//! Windows reuses a dead process's id, and a snapshot's parent id is the
//! *creator's* id at creation, so a stranger can hold a number that reads as a
//! parent. [`snapshot`] therefore drops a pair whose child was created before
//! its parent: provably not a descendant, and never signalled.

use std::ffi::c_void;
use std::io;

use super::capture::{ContainmentMechanism, DescendantSet};

/// A kernel object handle (`HANDLE`).
type Handle = *mut c_void;
/// The Win32 `BOOL`: zero is false.
type Bool = i32;

/// `PROCESS_TERMINATE`.
const PROCESS_TERMINATE: u32 = 0x0001;
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// `PROCESS_SET_QUOTA`: required to assign a process to a job.
const PROCESS_SET_QUOTA: u32 = 0x0100;
/// `SYNCHRONIZE`.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `STILL_ACTIVE`: the exit code of a process that has not exited.
const STILL_ACTIVE: u32 = 259;
/// `WAIT_OBJECT_0`: the waited object is signalled (the process exited).
const WAIT_OBJECT_0: u32 = 0;
/// `TH32CS_SNAPPROCESS`.
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: closing the last job handle kills
/// every process still in the job.
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
/// `JobObjectBasicProcessIdList`.
const JOB_PID_LIST_CLASS: i32 = 3;
/// `JobObjectExtendedLimitInformation`.
const JOB_EXTENDED_CLASS: i32 = 9;
/// `ERROR_MORE_DATA`: the buffer held a prefix of the answer.
const ERROR_MORE_DATA: i32 = 234;
/// `ERROR_INVALID_PARAMETER`: `OpenProcess` on an id no process holds.
pub(super) const ERROR_INVALID_PARAMETER: i32 = 87;
/// `ERROR_ACCESS_DENIED`: the process exists and is not this process's to open.
const ERROR_ACCESS_DENIED: i32 = 5;
/// `ERROR_NO_MORE_FILES`: the snapshot walk reached its end.
const ERROR_NO_MORE_FILES: i32 = 18;
/// The most rows one snapshot keeps, so a host cannot make it grow unbounded.
const MAX_SNAPSHOT_ROWS: usize = 1 << 20;

/// `PROCESSENTRY32W`, as `<tlhelp32.h>` lays it out.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ProcessEntry {
    /// `dwSize`: this struct's size, set before the first call.
    pub(super) size: u32,
    /// `cntUsage` (written by the kernel, unread).
    pub(super) _usage: u32,
    /// `th32ProcessID`.
    pub(super) pid: u32,
    /// `th32DefaultHeapID` (unread).
    pub(super) _heap: usize,
    /// `th32ModuleID` (unread).
    pub(super) _module: u32,
    /// `cntThreads` (unread).
    pub(super) _threads: u32,
    /// `th32ParentProcessID`.
    pub(super) parent: u32,
    /// `pcPriClassBase` (unread).
    pub(super) _priority: i32,
    /// `dwFlags` (unread).
    pub(super) _flags: u32,
    /// `szExeFile` (unread).
    pub(super) _exe: [u16; 260],
}

impl ProcessEntry {
    /// An entry with `dwSize` set, as `Process32FirstW` requires.
    fn sized() -> io::Result<Self> {
        let size = u32::try_from(std::mem::size_of::<Self>()).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("PROCESSENTRY32W size: {error}"),
            )
        })?;
        Ok(Self {
            size,
            _usage: 0,
            pid: 0,
            _heap: 0,
            _module: 0,
            _threads: 0,
            parent: 0,
            _priority: 0,
            _flags: 0,
            _exe: [0; 260],
        })
    }
}

/// `FILETIME`: 100-nanosecond intervals since 1601, split in two words.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct FileTime {
    /// `dwLowDateTime`.
    pub(super) low: u32,
    /// `dwHighDateTime`.
    pub(super) high: u32,
}

/// The `kernel32` calls this module makes, as pointers, so the tests drive
/// the same `unsafe` call sites against stand-ins.
#[derive(Clone, Copy)]
pub(super) struct Api {
    /// `OpenProcess(access, inherit, pid)`.
    pub(super) open_process: unsafe extern "system" fn(u32, Bool, u32) -> Handle,
    /// `CloseHandle(handle)`.
    pub(super) close_handle: unsafe extern "system" fn(Handle) -> Bool,
    /// `TerminateProcess(handle, exit_code)`.
    pub(super) terminate_process: unsafe extern "system" fn(Handle, u32) -> Bool,
    /// `GetExitCodeProcess(handle, out)`.
    pub(super) exit_code: unsafe extern "system" fn(Handle, *mut u32) -> Bool,
    /// `WaitForSingleObject(handle, milliseconds)`.
    pub(super) wait: unsafe extern "system" fn(Handle, u32) -> u32,
    /// `GetProcessTimes(handle, creation, exit, kernel, user)`.
    pub(super) times: unsafe extern "system" fn(
        Handle,
        *mut FileTime,
        *mut FileTime,
        *mut FileTime,
        *mut FileTime,
    ) -> Bool,
    /// `CreateToolhelp32Snapshot(flags, pid)`.
    pub(super) snapshot: unsafe extern "system" fn(u32, u32) -> Handle,
    /// `Process32FirstW(snapshot, entry)`.
    pub(super) first: unsafe extern "system" fn(Handle, *mut ProcessEntry) -> Bool,
    /// `Process32NextW(snapshot, entry)`.
    pub(super) next: unsafe extern "system" fn(Handle, *mut ProcessEntry) -> Bool,
    /// `CreateJobObjectW(attributes, name)`.
    pub(super) create_job: unsafe extern "system" fn(*mut c_void, *const u16) -> Handle,
    /// `SetInformationJobObject(job, class, info, length)`.
    pub(super) set_job_info: unsafe extern "system" fn(Handle, i32, *mut c_void, u32) -> Bool,
    /// `QueryInformationJobObject(job, class, info, length, returned)`.
    pub(super) query_job_info:
        unsafe extern "system" fn(Handle, i32, *mut c_void, u32, *mut u32) -> Bool,
    /// `AssignProcessToJobObject(job, process)`.
    pub(super) assign_job: unsafe extern "system" fn(Handle, Handle) -> Bool,
    /// `TerminateJobObject(job, exit_code)`.
    pub(super) terminate_job: unsafe extern "system" fn(Handle, u32) -> Bool,
    /// The error the last failed call left (`GetLastError`).
    pub(super) last_error: fn() -> io::Error,
}

// SAFETY (declaration): each function is `kernel32`'s, with the signature
// `<processthreadsapi.h>`, `<handleapi.h>`, `<synchapi.h>` and `<tlhelp32.h>`
// declare, in the `system` (stdcall on x86, C on x64/arm64) convention.
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "OpenProcess"]
    fn open_process(access: u32, inherit: Bool, pid: u32) -> Handle;
    #[link_name = "CloseHandle"]
    fn close_handle(handle: Handle) -> Bool;
    #[link_name = "TerminateProcess"]
    fn terminate_process(handle: Handle, exit_code: u32) -> Bool;
    #[link_name = "GetExitCodeProcess"]
    fn get_exit_code_process(handle: Handle, code: *mut u32) -> Bool;
    #[link_name = "WaitForSingleObject"]
    fn wait_for_single_object(handle: Handle, milliseconds: u32) -> u32;
    #[link_name = "GetProcessTimes"]
    fn get_process_times(
        handle: Handle,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> Bool;
    #[link_name = "CreateToolhelp32Snapshot"]
    fn create_toolhelp32_snapshot(flags: u32, pid: u32) -> Handle;
    #[link_name = "Process32FirstW"]
    fn process32_first_w(snapshot: Handle, entry: *mut ProcessEntry) -> Bool;
    #[link_name = "Process32NextW"]
    fn process32_next_w(snapshot: Handle, entry: *mut ProcessEntry) -> Bool;
    #[link_name = "CreateJobObjectW"]
    fn create_job_object_w(attributes: *mut c_void, name: *const u16) -> Handle;
    #[link_name = "SetInformationJobObject"]
    fn set_information_job_object(job: Handle, class: i32, info: *mut c_void, length: u32) -> Bool;
    #[link_name = "QueryInformationJobObject"]
    fn query_information_job_object(
        job: Handle,
        class: i32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> Bool;
    #[link_name = "AssignProcessToJobObject"]
    fn assign_process_to_job_object(job: Handle, process: Handle) -> Bool;
    #[link_name = "TerminateJobObject"]
    fn terminate_job_object(job: Handle, exit_code: u32) -> Bool;
}

/// The real kernel.
pub(super) const NATIVE: Api = Api {
    open_process,
    close_handle,
    terminate_process,
    exit_code: get_exit_code_process,
    wait: wait_for_single_object,
    times: get_process_times,
    snapshot: create_toolhelp32_snapshot,
    first: process32_first_w,
    next: process32_next_w,
    create_job: create_job_object_w,
    set_job_info: set_information_job_object,
    query_job_info: query_information_job_object,
    assign_job: assign_process_to_job_object,
    terminate_job: terminate_job_object,
    last_error: io::Error::last_os_error,
};

/// A handle this module opened, closed exactly once when dropped.
pub(super) struct Owned<'api> {
    /// The table the handle was opened through, which closes it.
    api: &'api Api,
    /// The open handle: never null, never `INVALID_HANDLE_VALUE`.
    handle: Handle,
}

impl Drop for Owned<'_> {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful open through `api` and is
        // closed here once; a failed close has nothing left to release.
        let _closed = unsafe { (self.api.close_handle)(self.handle) };
    }
}

/// `error` as the `Err` of `call`, traced on the way out so the caller's value
/// is also a signal.
fn refused<T>(error: io::Error, call: &'static str) -> io::Result<T> {
    #[cfg(feature = "trace")]
    crate::trace::debug!(error = %error, call, "windows process call: returning an error to the caller");
    #[cfg(not(feature = "trace"))]
    let _unreported = call;
    Err(error)
}

/// Whether `handle` is `INVALID_HANDLE_VALUE` (all bits set).
fn is_invalid(handle: Handle) -> bool {
    handle.addr() == usize::MAX
}

/// `pid` opened with `access`, `None` when no process holds the id.
///
/// # Errors
///
/// The open's own error, `ERROR_ACCESS_DENIED` included: a process that exists
/// and is not this process's to open is a fact the caller must see.
fn open(api: &Api, pid: u32, access: u32) -> io::Result<Option<Owned<'_>>> {
    // SAFETY: `OpenProcess` takes three plain values and returns a handle or
    // null; nothing is read through a pointer.
    let handle = unsafe { (api.open_process)(access, 0, pid) };
    if !handle.is_null() {
        return Ok(Some(Owned { api, handle }));
    }
    let error = (api.last_error)();
    if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
        return Ok(None);
    }
    Err(error)
}

/// The exit code `GetExitCodeProcess` reports, `STILL_ACTIVE` while running.
fn exit_code_of(owned: &Owned<'_>) -> io::Result<u32> {
    let mut code: u32 = 0;
    // SAFETY: `owned.handle` is open; `code` is a live `u32` the call writes.
    let read = unsafe { (owned.api.exit_code)(owned.handle, &raw mut code) };
    if read == 0 {
        return refused((owned.api.last_error)(), "GetExitCodeProcess");
    }
    Ok(code)
}

/// Whether `pid` names a process now: open, even if it has exited and its
/// object is still held (the Windows form of a Unix zombie), or present and
/// refused to this process.
pub(super) fn exists(api: &Api, pid: u32) -> io::Result<bool> {
    match open(api, pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(found) => Ok(found.is_some()),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(true),
        Err(error) => Err(error),
    }
}

/// Whether `pid` is still running: its exit code is `STILL_ACTIVE`. A process
/// refused to this process is reported running, the answer that claims less.
pub(super) fn running(api: &Api, pid: u32) -> io::Result<bool> {
    match open(api, pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(Some(owned)) => Ok(exit_code_of(&owned)? == STILL_ACTIVE),
        Ok(None) => Ok(false),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(true),
        Err(error) => Err(error),
    }
}

/// Terminate `pid`. `Ok` when it was stopped or had already exited.
///
/// # Errors
///
/// `ERROR_INVALID_PARAMETER` when no process holds the id — the Windows form of
/// `ESRCH` — and the terminate's own error for a live process it refused.
pub(super) fn terminate(api: &Api, pid: u32) -> io::Result<()> {
    let Some(owned) = open(
        api,
        pid,
        PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
    )?
    else {
        return refused(
            io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER),
            "terminate: OpenProcess",
        );
    };
    // SAFETY: `owned.handle` is open with `PROCESS_TERMINATE`.
    let stopped = unsafe { (owned.api.terminate_process)(owned.handle, 1) };
    if stopped != 0 {
        return Ok(());
    }
    let error = (owned.api.last_error)();
    // A process that already exited refuses termination; its exit is what the
    // signal was for.
    if exit_code_of(&owned)? != STILL_ACTIVE {
        return Ok(());
    }
    Err(error)
}

/// Whether `pid` has exited while something still holds its object, which
/// keeps the id from being reissued — the Windows form of "exited, not reaped".
pub(super) fn exited(api: &Api, pid: u32) -> io::Result<bool> {
    let Some(owned) = open(api, pid, SYNCHRONIZE)? else {
        return refused(
            io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER),
            "exited: OpenProcess",
        );
    };
    // SAFETY: `owned.handle` is open with `SYNCHRONIZE`; a zero timeout polls.
    let waited = unsafe { (owned.api.wait)(owned.handle, 0) };
    Ok(waited == WAIT_OBJECT_0)
}

/// The creation instant of `pid` in 100 ns intervals since 1601, `None` when
/// no process holds the id.
pub(super) fn creation(api: &Api, pid: u32) -> io::Result<Option<u64>> {
    let Some(owned) = open(api, pid, PROCESS_QUERY_LIMITED_INFORMATION)? else {
        return Ok(None);
    };
    let mut created = FileTime::default();
    let mut ended = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: `owned.handle` is open; the four out-pointers are live
    // `FileTime`s the call writes.
    let read = unsafe {
        (owned.api.times)(
            owned.handle,
            &raw mut created,
            &raw mut ended,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if read == 0 {
        return refused((owned.api.last_error)(), "GetProcessTimes");
    }
    let [l0, l1, l2, l3] = created.low.to_le_bytes();
    let [h0, h1, h2, h3] = created.high.to_le_bytes();
    Ok(Some(u64::from_le_bytes([l0, l1, l2, l3, h0, h1, h2, h3])))
}

/// Every `(pid, parent)` pair of one Toolhelp snapshot, with each pair whose
/// child was created before its parent dropped as a reused-id stranger.
///
/// # Errors
///
/// The snapshot's own error, and a walk that ended on anything but
/// `ERROR_NO_MORE_FILES`.
pub(super) fn snapshot(api: &Api) -> io::Result<Vec<(u32, u32)>> {
    let rows = snapshot_rows(api)?;
    let mut born: std::collections::BTreeMap<u32, Option<u64>> = std::collections::BTreeMap::new();
    let mut created = |pid: u32| -> io::Result<Option<u64>> {
        if let Some(known) = born.get(&pid) {
            return Ok(*known);
        }
        // An unreadable start (access denied, or a process that ended since
        // the snapshot) proves nothing, so the pair is kept: it is dropped
        // only when both starts were read and the child predates its parent.
        let read = creation(api, pid).ok().flatten();
        born.insert(pid, read);
        Ok(read)
    };
    let mut pairs = Vec::with_capacity(rows.len());
    for (pid, parent) in rows {
        let child_born = created(pid)?;
        let parent_born = created(parent)?;
        // Kept unless provably a stranger: a child cannot predate its parent.
        let stranger =
            matches!((child_born, parent_born), (Some(child), Some(parent)) if child < parent);
        if !stranger {
            pairs.push((pid, parent));
        }
    }
    Ok(pairs)
}

/// The raw `(pid, parent)` rows of one Toolhelp snapshot, bounded.
fn snapshot_rows(api: &Api) -> io::Result<Vec<(u32, u32)>> {
    // SAFETY: `CreateToolhelp32Snapshot` takes two plain values and returns a
    // handle or `INVALID_HANDLE_VALUE`.
    let handle = unsafe { (api.snapshot)(TH32CS_SNAPPROCESS, 0) };
    if handle.is_null() || is_invalid(handle) {
        return refused((api.last_error)(), "CreateToolhelp32Snapshot");
    }
    let owned = Owned { api, handle };
    let mut entry = ProcessEntry::sized()?;
    let mut rows = Vec::new();
    // SAFETY: `owned.handle` is the open snapshot; `entry` is a live
    // `PROCESSENTRY32W` whose `dwSize` names its size.
    let mut more = unsafe { (owned.api.first)(owned.handle, &raw mut entry) };
    while more != 0 {
        if rows.len() >= MAX_SNAPSHOT_ROWS {
            return refused(
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    format!("the process snapshot passed its {MAX_SNAPSHOT_ROWS}-row bound"),
                ),
                "Process32NextW",
            );
        }
        rows.push((entry.pid, entry.parent));
        // SAFETY: as for the first call; `entry` is still live and sized.
        more = unsafe { (owned.api.next)(owned.handle, &raw mut entry) };
    }
    let ended = (owned.api.last_error)();
    if ended.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
        return Ok(rows);
    }
    Err(ended)
}

/// `pid` as the unsigned id Windows uses, refusing anything not positive.
pub(super) fn pid_u32(pid: i32) -> io::Result<u32> {
    match u32::try_from(pid) {
        Ok(positive) if positive > 0 => Ok(positive),
        _ => {
            let refusal = super::invalid_pid();
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = %refusal, "windows pid: returning an error to the caller");
            Err(refusal)
        }
    }
}

/// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BasicLimit {
    /// `PerProcessUserTimeLimit`.
    per_process_user_time: i64,
    /// `PerJobUserTimeLimit`.
    per_job_user_time: i64,
    /// `LimitFlags`.
    flags: u32,
    /// `MinimumWorkingSetSize`.
    minimum_working_set: usize,
    /// `MaximumWorkingSetSize`.
    maximum_working_set: usize,
    /// `ActiveProcessLimit`.
    active_process_limit: u32,
    /// `Affinity`.
    affinity: usize,
    /// `PriorityClass`.
    priority_class: u32,
    /// `SchedulingClass`.
    scheduling_class: u32,
}

/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` (the `IO_COUNTERS` are six words).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ExtendedLimit {
    /// `BasicLimitInformation`.
    basic: BasicLimit,
    /// `IoInfo`, unused.
    io: [u64; 6],
    /// `ProcessMemoryLimit`.
    process_memory: usize,
    /// `JobMemoryLimit`.
    job_memory: usize,
    /// `PeakProcessMemoryUsed`.
    peak_process: usize,
    /// `PeakJobMemoryUsed`.
    peak_job: usize,
}

// The size `<winnt.h>` gives the struct on a 64-bit target.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::size_of::<ExtendedLimit>() == 144,
    "JOBOBJECT_EXTENDED_LIMIT_INFORMATION is 144 bytes on a 64-bit target"
);

/// How many member ids one [`ProcessJob::members`] read holds; a job with more
/// reports itself truncated.
const JOB_LIST_CAPACITY: usize = 1024;

/// `JOBOBJECT_BASIC_PROCESS_ID_LIST`, with room for one read.
#[repr(C)]
struct PidList {
    /// `NumberOfAssignedProcesses`.
    assigned: u32,
    /// `NumberOfProcessIdsInList`.
    listed: u32,
    /// `ProcessIdList`.
    ids: [usize; JOB_LIST_CAPACITY],
}

/// A Windows job object that kills every process in it when it is closed.
///
/// The kernel-level owner the Unix process group cannot be: a process assigned
/// to the job stays in it however it forks, and a child cannot leave a job
/// unless breakaway is granted, which this job never grants. Dropping the job
/// (or its owner dying, even by `TerminateProcess`) closes the handle, and
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` then kills every member. The one gap is
/// the one Unix has: a process forked *before* [`Self::assign`] ran is not a
/// member, so a caller assigns a child immediately after it spawns and sweeps
/// the rest with `capture_descendants`.
pub struct ProcessJob {
    /// The table the job was made through, which also closes it.
    api: Api,
    /// The open job handle, closed once on drop.
    handle: Handle,
}

// SAFETY: a job handle names a kernel object that every thread of the process
// may use; the calls made through it are thread-safe kernel calls and this
// struct holds no thread-affine state.
unsafe impl Send for ProcessJob {}
// SAFETY: as for `Send`; every method takes `&self` and only calls the kernel.
unsafe impl Sync for ProcessJob {}

impl Drop for ProcessJob {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful `CreateJobObjectW` and is
        // closed here once; closing it is what kills the members.
        let _closed = unsafe { (self.api.close_handle)(self.handle) };
    }
}

impl ProcessJob {
    /// A new, empty job whose members die when it is dropped.
    ///
    /// # Errors
    ///
    /// The operating system's error when the job cannot be created or armed.
    pub fn new() -> io::Result<Self> {
        Self::with_api(NATIVE)
    }

    /// [`Self::new`] through `api`, so the tests drive the same call sites.
    pub(super) fn with_api(api: Api) -> io::Result<Self> {
        // SAFETY: null attributes and a null name make an unnamed job with the
        // default security; nothing is read through a pointer.
        let handle = unsafe { (api.create_job)(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() {
            return refused((api.last_error)(), "CreateJobObjectW");
        }
        let job = Self { api, handle };
        let mut limit = ExtendedLimit::default();
        limit.basic.flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(std::mem::size_of::<ExtendedLimit>()).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("job limit size: {error}"),
            )
        })?;
        // SAFETY: `job.handle` is open; `limit` is a live `#[repr(C)]` value of
        // the `size` bytes named, which the call only reads.
        let armed = unsafe {
            (job.api.set_job_info)(
                job.handle,
                JOB_EXTENDED_CLASS,
                (&raw mut limit).cast::<c_void>(),
                size,
            )
        };
        if armed == 0 {
            return refused((job.api.last_error)(), "SetInformationJobObject");
        }
        Ok(job)
    }

    /// Put the process `pid` in the job, with every descendant it forks later.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a non-positive id, `ERROR_INVALID_PARAMETER` when no
    /// process holds it, and the operating system's own refusal otherwise.
    pub fn assign(&self, pid: i32) -> io::Result<()> {
        let pid = pid_u32(pid)?;
        let Some(owned) = open(&self.api, pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)? else {
            return refused(
                io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER),
                "assign: OpenProcess",
            );
        };
        // SAFETY: both handles are open; the job was made by this struct and
        // `owned` carries the rights `AssignProcessToJobObject` requires.
        let assigned = unsafe { (self.api.assign_job)(self.handle, owned.handle) };
        if assigned == 0 {
            return refused((self.api.last_error)(), "AssignProcessToJobObject");
        }
        Ok(())
    }

    /// Kill every process in the job at once.
    ///
    /// # Errors
    ///
    /// The operating system's error when the job cannot be terminated.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: `handle` is the open job; the exit code is a plain value.
        let stopped = unsafe { (self.api.terminate_job)(self.handle, 1) };
        if stopped == 0 {
            return refused((self.api.last_error)(), "TerminateJobObject");
        }
        Ok(())
    }

    /// Every process now in the job, read from the kernel and not from a table
    /// snapshot, so the set names a `setsid`-style escapee the group cannot.
    ///
    /// An exited process leaves the list; an empty set after
    /// [`Self::terminate`] is the completeness claim.
    ///
    /// # Errors
    ///
    /// The operating system's error when the list cannot be read.
    pub fn members(&self) -> io::Result<DescendantSet> {
        let mut list = Box::new(PidList {
            assigned: 0,
            listed: 0,
            ids: [0; JOB_LIST_CAPACITY],
        });
        let size = u32::try_from(std::mem::size_of::<PidList>()).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("job list size: {error}"),
            )
        })?;
        let mut returned: u32 = 0;
        // SAFETY: `handle` is the open job; `list` is a live `#[repr(C)]` value
        // of the `size` bytes named, and `returned` a live `u32`; the call
        // writes at most `size` bytes.
        let read = unsafe {
            (self.api.query_job_info)(
                self.handle,
                JOB_PID_LIST_CLASS,
                (&raw mut *list).cast::<c_void>(),
                size,
                &raw mut returned,
            )
        };
        let mut more = false;
        if read == 0 {
            let error = (self.api.last_error)();
            if error.raw_os_error() != Some(ERROR_MORE_DATA) {
                return refused(error, "QueryInformationJobObject");
            }
            more = true;
        }
        let listed = match usize::try_from(list.listed) {
            Ok(listed) => listed.min(JOB_LIST_CAPACITY),
            Err(_) => JOB_LIST_CAPACITY,
        };
        let mut pids: Vec<i32> = Vec::new();
        // A member whose id does not fit the pid type cannot be named, and a
        // reading that dropped it must not read as an empty job.
        let mut unnamed = false;
        if let Some(ids) = list.ids.get(..listed) {
            for id in ids {
                match i32::try_from(*id) {
                    Ok(pid) => pids.push(pid),
                    Err(_) => unnamed = true,
                }
            }
        }
        pids.sort_unstable();
        pids.dedup();
        Ok(DescendantSet {
            mechanism: ContainmentMechanism::JobObject,
            truncated: more || unnamed || list.assigned > list.listed,
            pids,
        })
    }
}

/// The stand-ins, the seam under Miri, and the live kernel.
#[cfg(test)]
mod tests;
