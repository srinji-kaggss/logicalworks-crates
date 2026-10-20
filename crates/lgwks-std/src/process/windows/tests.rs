//! The `kernel32` seam against stand-ins (run by the `miri-unsafe` lane) and
//! the live kernel.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// One process in the stand-in's table.
#[derive(Clone, Copy, Debug)]
struct Fake {
    /// Its creator's id.
    parent: u32,
    /// Its creation instant.
    born: u64,
    /// Whether it has exited.
    exited: bool,
}

/// The stand-in kernel's state for one test.
#[derive(Default)]
struct Kernel {
    /// The process table, by id.
    table: BTreeMap<u32, Fake>,
    /// The error the last failed call left.
    error: i32,
    /// Handles opened and not yet closed.
    open: usize,
    /// Handles closed.
    closed: usize,
    /// The snapshot walk's position.
    cursor: usize,
    /// The processes in the stand-in job.
    members: BTreeSet<u32>,
    /// Whether the job was armed to kill its members when closed.
    armed: bool,
}

thread_local! {
    /// The stand-in kernel this thread's test drives.
    static KERNEL: RefCell<Kernel> = RefCell::new(Kernel::default());
}

/// The handle the stand-in hands out for `id`: never dereferenced, so a
/// provenance-free address is the whole of it.
fn handle_for(id: u32) -> Handle {
    let address = match usize::try_from(id) {
        Ok(id) => id.saturating_add(1),
        Err(_) => 1,
    };
    std::ptr::without_provenance_mut(address)
}

/// The id a stand-in handle names; an address past `u32` names no process.
fn id_of(handle: Handle) -> u32 {
    let Ok(id) = u32::try_from(handle.addr().saturating_sub(1)) else {
        return u32::MAX;
    };
    id
}

/// The id the snapshot handle is given: no process holds it.
const SNAPSHOT: u32 = u32::MAX - 1;

/// The id the job handle is given: no process holds it.
const JOB: u32 = u32::MAX - 2;

/// Fail with `code` as the last error.
fn fail(code: i32) -> Bool {
    KERNEL.with_borrow_mut(|kernel| kernel.error = code);
    0
}

unsafe extern "system" fn stand_open(_access: u32, _inherit: Bool, pid: u32) -> Handle {
    KERNEL.with_borrow_mut(|kernel| {
        if kernel.table.contains_key(&pid) {
            kernel.open = kernel.open.saturating_add(1);
            handle_for(pid)
        } else {
            kernel.error = ERROR_INVALID_PARAMETER;
            std::ptr::null_mut()
        }
    })
}

/// Mark every member of the stand-in job exited, as the kernel does.
fn kill_members(kernel: &mut Kernel) {
    for pid in &kernel.members {
        if let Some(process) = kernel.table.get_mut(pid) {
            process.exited = true;
        }
    }
}

unsafe extern "system" fn stand_close(handle: Handle) -> Bool {
    KERNEL.with_borrow_mut(|kernel| {
        kernel.open = kernel.open.saturating_sub(1);
        kernel.closed = kernel.closed.saturating_add(1);
        if id_of(handle) == JOB && kernel.armed {
            kill_members(kernel);
        }
    });
    1
}

unsafe extern "system" fn stand_terminate(handle: Handle, _code: u32) -> Bool {
    let pid = id_of(handle);
    KERNEL.with_borrow_mut(|kernel| match kernel.table.get_mut(&pid) {
        Some(process) if !process.exited => {
            process.exited = true;
            1
        }
        _ => {
            kernel.error = ERROR_ACCESS_DENIED;
            0
        }
    })
}

unsafe extern "system" fn stand_exit_code(handle: Handle, code: *mut u32) -> Bool {
    let pid = id_of(handle);
    let Some(exited) =
        KERNEL.with_borrow(|kernel| kernel.table.get(&pid).map(|process| process.exited))
    else {
        return fail(ERROR_INVALID_PARAMETER);
    };
    // SAFETY: the module passes a live `u32` for the code.
    if let Some(slot) = unsafe { code.as_mut() } {
        *slot = if exited { 0 } else { STILL_ACTIVE };
    }
    1
}

unsafe extern "system" fn stand_wait(handle: Handle, _milliseconds: u32) -> u32 {
    let pid = id_of(handle);
    let exited =
        KERNEL.with_borrow(|kernel| kernel.table.get(&pid).is_some_and(|process| process.exited));
    if exited { WAIT_OBJECT_0 } else { 258 }
}

unsafe extern "system" fn stand_times(
    handle: Handle,
    creation: *mut FileTime,
    _exit: *mut FileTime,
    _kernel: *mut FileTime,
    _user: *mut FileTime,
) -> Bool {
    let pid = id_of(handle);
    let Some(born) =
        KERNEL.with_borrow(|kernel| kernel.table.get(&pid).map(|process| process.born))
    else {
        return fail(ERROR_INVALID_PARAMETER);
    };
    let [b0, b1, b2, b3, b4, b5, b6, b7] = born.to_le_bytes();
    // SAFETY: the module passes a live `FileTime` for the creation instant.
    if let Some(slot) = unsafe { creation.as_mut() } {
        *slot = FileTime {
            low: u32::from_le_bytes([b0, b1, b2, b3]),
            high: u32::from_le_bytes([b4, b5, b6, b7]),
        };
    }
    1
}

unsafe extern "system" fn stand_snapshot(_flags: u32, _pid: u32) -> Handle {
    KERNEL.with_borrow_mut(|kernel| {
        kernel.cursor = 0;
        kernel.open = kernel.open.saturating_add(1);
    });
    handle_for(SNAPSHOT)
}

/// Write the row at the walk's cursor into `entry`, or end the walk.
fn step(entry: *mut ProcessEntry) -> Bool {
    let row = KERNEL.with_borrow_mut(|kernel| {
        let row = kernel
            .table
            .iter()
            .nth(kernel.cursor)
            .map(|(pid, process)| (*pid, process.parent));
        kernel.cursor = kernel.cursor.saturating_add(1);
        row
    });
    let Some((pid, parent)) = row else {
        return fail(ERROR_NO_MORE_FILES);
    };
    // SAFETY: the module passes a live, sized `PROCESSENTRY32W`.
    if let Some(slot) = unsafe { entry.as_mut() } {
        slot.pid = pid;
        slot.parent = parent;
    }
    1
}

unsafe extern "system" fn stand_first(_snapshot: Handle, entry: *mut ProcessEntry) -> Bool {
    KERNEL.with_borrow_mut(|kernel| kernel.cursor = 0);
    step(entry)
}

unsafe extern "system" fn stand_next(_snapshot: Handle, entry: *mut ProcessEntry) -> Bool {
    step(entry)
}

unsafe extern "system" fn stand_create_job(_attributes: *mut c_void, _name: *const u16) -> Handle {
    KERNEL.with_borrow_mut(|kernel| kernel.open = kernel.open.saturating_add(1));
    handle_for(JOB)
}

unsafe extern "system" fn stand_set_job_info(
    _job: Handle,
    class: i32,
    info: *mut c_void,
    length: u32,
) -> Bool {
    let sized = usize::try_from(length) == Ok(std::mem::size_of::<ExtendedLimit>());
    // SAFETY: the module passes a live `ExtendedLimit` of the length it names.
    let flags = unsafe { info.cast::<ExtendedLimit>().as_ref() }.map(|limit| limit.basic.flags);
    match flags {
        Some(flags) if class == JOB_EXTENDED_CLASS && sized => {
            KERNEL.with_borrow_mut(|kernel| {
                kernel.armed = flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0;
            });
            1
        }
        _ => fail(ERROR_INVALID_PARAMETER),
    }
}

unsafe extern "system" fn stand_query_job(
    _job: Handle,
    class: i32,
    info: *mut c_void,
    length: u32,
    _returned: *mut u32,
) -> Bool {
    let sized = usize::try_from(length) == Ok(std::mem::size_of::<PidList>());
    // SAFETY: the module passes a live `PidList` of the length it names.
    let Some(list) = (unsafe { info.cast::<PidList>().as_mut() }) else {
        return fail(ERROR_INVALID_PARAMETER);
    };
    if class != JOB_PID_LIST_CLASS || !sized {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let live: Vec<u32> = KERNEL.with_borrow(|kernel| {
        kernel
            .members
            .iter()
            .copied()
            .filter(|pid| kernel.table.get(pid).is_some_and(|process| !process.exited))
            .collect()
    });
    let Ok(assigned) = u32::try_from(live.len()) else {
        return fail(ERROR_INVALID_PARAMETER);
    };
    list.assigned = assigned;
    let mut listed = 0_u32;
    for (slot, pid) in list.ids.iter_mut().zip(&live) {
        let Ok(id) = usize::try_from(*pid) else {
            return fail(ERROR_INVALID_PARAMETER);
        };
        *slot = id;
        listed = listed.saturating_add(1);
    }
    list.listed = listed;
    if live.len() > JOB_LIST_CAPACITY {
        return fail(ERROR_MORE_DATA);
    }
    1
}

unsafe extern "system" fn stand_assign(_job: Handle, process: Handle) -> Bool {
    let pid = id_of(process);
    KERNEL.with_borrow_mut(|kernel| {
        if kernel.table.contains_key(&pid) {
            kernel.members.insert(pid);
            1
        } else {
            kernel.error = ERROR_INVALID_PARAMETER;
            0
        }
    })
}

unsafe extern "system" fn stand_terminate_job(_job: Handle, _code: u32) -> Bool {
    KERNEL.with_borrow_mut(kill_members);
    1
}

/// The last error the stand-in left.
fn stand_last_error() -> io::Error {
    io::Error::from_raw_os_error(KERNEL.with_borrow(|kernel| kernel.error))
}

/// The stand-in table of pointers.
const STAND_IN: Api = Api {
    open_process: stand_open,
    close_handle: stand_close,
    terminate_process: stand_terminate,
    exit_code: stand_exit_code,
    wait: stand_wait,
    times: stand_times,
    snapshot: stand_snapshot,
    first: stand_first,
    next: stand_next,
    create_job: stand_create_job,
    set_job_info: stand_set_job_info,
    query_job_info: stand_query_job,
    assign_job: stand_assign,
    terminate_job: stand_terminate_job,
    last_error: stand_last_error,
};

/// Serve `table` to the stand-in for the rest of the test.
fn serve(table: &[(u32, u32, u64)]) {
    KERNEL.with_borrow_mut(|kernel| {
        *kernel = Kernel::default();
        for &(pid, parent, born) in table {
            kernel.table.insert(
                pid,
                Fake {
                    parent,
                    born,
                    exited: false,
                },
            );
        }
    });
}

/// Handles still open, which must be zero after every call.
fn leaked() -> usize {
    KERNEL.with_borrow(|kernel| kernel.open)
}

#[test]
fn a_reused_parent_id_is_never_read_as_a_parent() -> io::Result<()> {
    // 30 names 10 as its parent but was created before 10: 10 is a later
    // process holding a reused id, and 30 is not its descendant.
    serve(&[(1, 0, 10), (10, 1, 100), (20, 10, 200), (30, 10, 50)]);
    let pairs = snapshot(&STAND_IN)?;
    assert!(
        pairs.contains(&(20, 10)),
        "a child born after its parent is kept"
    );
    assert!(
        !pairs.contains(&(30, 10)),
        "a child born before its parent is a stranger"
    );
    assert_eq!(leaked(), 0, "every handle the snapshot opened was closed");
    Ok(())
}

#[test]
fn an_absent_id_is_absent_not_running_and_refuses_termination() -> io::Result<()> {
    serve(&[(4, 0, 1)]);
    assert!(!exists(&STAND_IN, 9)?, "no process holds 9");
    assert!(!running(&STAND_IN, 9)?, "an absent process is not running");
    assert_eq!(
        terminate(&STAND_IN, 9).map_err(|error| error.raw_os_error()),
        Err(Some(ERROR_INVALID_PARAMETER)),
        "terminating an absent id says it does not exist, the Windows ESRCH"
    );
    assert_eq!(creation(&STAND_IN, 9)?, None, "an absent id has no start");
    Ok(())
}

#[test]
fn a_terminated_process_stops_running_and_reads_as_exited() -> io::Result<()> {
    serve(&[(4, 0, 1)]);
    assert!(running(&STAND_IN, 4)?, "the premise: it runs");
    assert!(!exited(&STAND_IN, 4)?, "a running process has not exited");
    terminate(&STAND_IN, 4)?;
    assert!(!running(&STAND_IN, 4)?, "terminated is not running");
    assert!(
        exited(&STAND_IN, 4)?,
        "and reads as exited while its object is held"
    );
    assert!(
        exists(&STAND_IN, 4)?,
        "an exited, held process still exists, like a zombie"
    );
    terminate(&STAND_IN, 4)?;
    assert_eq!(leaked(), 0, "every handle opened was closed");
    Ok(())
}

#[test]
fn a_creation_instant_round_trips_both_words() -> io::Result<()> {
    let born = 0x0123_4567_89ab_cdef_u64;
    serve(&[(4, 0, born)]);
    assert_eq!(
        creation(&STAND_IN, 4)?,
        Some(born),
        "both halves of FILETIME survive"
    );
    assert_eq!(leaked(), 0, "the handle was closed");
    Ok(())
}

/// Seeded tables: the snapshot keeps exactly the pairs whose child was born
/// no earlier than its parent, and opens no handle it does not close.
#[test]
fn sim_every_seeded_snapshot_drops_exactly_the_strangers() -> io::Result<()> {
    #[cfg(miri)]
    const SEEDS: u64 = 8;
    #[cfg(not(miri))]
    const SEEDS: u64 = 256;
    for seed in 0..SEEDS {
        let mut stream = crate::seeded::Seeded::from_seed(seed);
        let count = stream.below(40).map_err(io::Error::other)?;
        let mut table = Vec::new();
        for pid in 1..=u32::try_from(count).map_err(io::Error::other)? {
            let parent = u32::try_from(stream.below(u64::from(pid)).map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
            let born = stream.below(1000).map_err(io::Error::other)?;
            table.push((pid, parent, born));
        }
        serve(&table);
        let born: BTreeMap<u32, u64> = table.iter().map(|&(pid, _, born)| (pid, born)).collect();
        let expected: Vec<(u32, u32)> = table
            .iter()
            .filter(|&&(_, parent, born_at)| {
                born.get(&parent)
                    .is_none_or(|&parent_born| born_at >= parent_born)
            })
            .map(|&(pid, parent, _)| (pid, parent))
            .collect();
        assert_eq!(snapshot(&STAND_IN)?, expected, "seed {seed}");
        assert_eq!(leaked(), 0, "seed {seed}: every handle closed");
    }
    Ok(())
}

/// The live kernel: this process exists, runs, has a creation instant and is
/// in the snapshot. Not under Miri, which cannot make the foreign calls.
#[test]
#[cfg(not(miri))]
fn the_kernel_names_this_process() -> io::Result<()> {
    let own = std::process::id();
    assert!(exists(&NATIVE, own)?, "this process exists");
    assert!(running(&NATIVE, own)?, "and runs");
    assert!(!exited(&NATIVE, own)?, "and has not exited");
    assert!(
        creation(&NATIVE, own)?.is_some(),
        "and has a creation instant"
    );
    assert!(
        snapshot(&NATIVE)?.iter().any(|&(pid, _)| pid == own),
        "and is in the snapshot"
    );
    Ok(())
}

/// A live child: terminated, it stops running and reads as exited until it
/// is reaped. Not under Miri.
#[test]
#[cfg(not(miri))]
fn a_live_child_is_terminated_and_observed_exited() -> io::Result<()> {
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "ping -n 30 127.0.0.1 > NUL"])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let pid = child.id();
    assert!(running(&NATIVE, pid)?, "the premise: the child runs");
    terminate(&NATIVE, pid)?;
    let mut stopped = false;
    for _ in 0..4000 {
        if !running(&NATIVE, pid)? {
            stopped = true;
            break;
        }
        std::thread::yield_now();
    }
    assert!(stopped, "a terminated child stops running");
    assert!(
        exited(&NATIVE, pid)?,
        "and reads as exited while `child` holds it"
    );
    let _status = child.wait()?;
    Ok(())
}

#[test]
fn a_job_is_armed_to_kill_on_close_and_dropping_it_kills_its_members() -> io::Result<()> {
    serve(&[(4, 0, 1), (5, 4, 2), (6, 0, 3)]);
    let job = ProcessJob::with_api(STAND_IN)?;
    job.assign(4)?;
    job.assign(5)?;
    let members = job.members()?;
    assert_eq!(
        members.pids(),
        &[4, 5],
        "the job names exactly what was assigned"
    );
    assert_eq!(members.mechanism(), ContainmentMechanism::JobObject);
    assert!(!members.is_truncated(), "two members fit the list");
    drop(job);
    assert!(!running(&STAND_IN, 4)?, "closing the job killed its member");
    assert!(!running(&STAND_IN, 5)?, "and its other member");
    assert!(
        running(&STAND_IN, 6)?,
        "a process never assigned is untouched"
    );
    assert_eq!(
        leaked(),
        0,
        "the job handle and every opened handle were closed"
    );
    Ok(())
}

#[test]
fn terminating_a_job_empties_it_and_the_list_says_so() -> io::Result<()> {
    serve(&[(4, 0, 1), (5, 4, 2)]);
    let job = ProcessJob::with_api(STAND_IN)?;
    job.assign(4)?;
    job.assign(5)?;
    job.terminate()?;
    let members = job.members()?;
    assert!(members.is_empty(), "no member survives the terminate");
    assert_eq!(members.mechanism(), ContainmentMechanism::JobObject);
    Ok(())
}

#[test]
fn an_absent_or_non_positive_id_is_refused_by_assign() -> io::Result<()> {
    serve(&[(4, 0, 1)]);
    let job = ProcessJob::with_api(STAND_IN)?;
    assert_eq!(
        job.assign(9).map_err(|error| error.raw_os_error()),
        Err(Some(ERROR_INVALID_PARAMETER)),
        "no process holds 9"
    );
    for refused in [0, -1] {
        assert_eq!(
            job.assign(refused).map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidInput),
            "{refused} names no process"
        );
    }
    assert!(job.members()?.is_empty(), "a refused assign adds no member");
    Ok(())
}

#[test]
fn a_job_past_the_list_capacity_reports_a_prefix_not_the_whole() -> io::Result<()> {
    let count = u32::try_from(JOB_LIST_CAPACITY).map_err(io::Error::other)? + 20;
    let table: Vec<(u32, u32, u64)> = (1..=count).map(|pid| (pid, 0, 1)).collect();
    serve(&table);
    let job = ProcessJob::with_api(STAND_IN)?;
    for pid in 1..=count {
        job.assign(i32::try_from(pid).map_err(io::Error::other)?)?;
    }
    let members = job.members()?;
    assert!(members.is_truncated(), "more members than the list holds");
    assert_eq!(
        members.len(),
        JOB_LIST_CAPACITY,
        "the prefix is the whole list"
    );
    Ok(())
}

#[test]
fn a_member_whose_id_is_not_a_pid_is_never_read_as_an_empty_job() -> io::Result<()> {
    // An id past `i32::MAX` cannot be named as a pid, so the reading holds no
    // pid; the job is not empty, and the reading has to say it left one out.
    let wide: u32 = 2_147_483_657;
    serve(&[(wide, 0, 1)]);
    KERNEL.with_borrow_mut(|kernel| {
        kernel.members.insert(wide);
    });
    let job = ProcessJob::with_api(STAND_IN)?;
    let members = job.members()?;
    assert!(
        members.is_empty(),
        "no pid is named for an id that is not one"
    );
    assert!(
        members.is_truncated(),
        "a reading that dropped a member is a prefix, never an empty job"
    );
    Ok(())
}

/// The live kernel's job: a grandchild started after the assignment is a
/// member, and terminating the job stops every member. Not under Miri.
#[test]
#[cfg(not(miri))]
fn a_live_job_holds_a_grandchild_and_terminate_stops_the_tree() -> io::Result<()> {
    let job = ProcessJob::new()?;
    // The first ping lasts about a second, so the second starts after the
    // assignment and is born inside the job.
    let mut child = std::process::Command::new("cmd")
        .args([
            "/C",
            "ping -n 2 127.0.0.1 > NUL & ping -n 600 127.0.0.1 > NUL",
        ])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    job.assign(i32::try_from(child.id()).map_err(io::Error::other)?)?;
    let started = std::time::Instant::now();
    let settle = std::time::Duration::from_secs(30);
    let mut seen = job.members()?;
    while seen.len() < 2 && started.elapsed() < settle {
        std::thread::yield_now();
        seen = job.members()?;
    }
    assert!(
        seen.len() >= 2,
        "the job holds the shell and a descendant: {:?}",
        seen.pids()
    );
    job.terminate()?;
    let status = child.wait()?;
    assert!(
        !status.success(),
        "the terminated shell did not finish on its own"
    );
    let mut gone = false;
    while !gone && started.elapsed() < settle {
        gone = job.members()?.is_empty();
        std::thread::yield_now();
    }
    assert!(gone, "no member survives the job's terminate");
    Ok(())
}

/// Dropping the job is enough: its kill-on-close stops a child nobody killed.
/// Not under Miri.
#[test]
#[cfg(not(miri))]
fn a_live_job_dropped_kills_a_child_nobody_killed() -> io::Result<()> {
    let job = ProcessJob::new()?;
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "ping -n 600 127.0.0.1 > NUL"])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    job.assign(i32::try_from(child.id()).map_err(io::Error::other)?)?;
    drop(job);
    let status = child.wait()?;
    assert!(!status.success(), "kill-on-close stopped the child");
    Ok(())
}
