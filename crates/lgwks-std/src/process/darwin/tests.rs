//! The decoder, the syscall seam (run under Miri) and the live kernel.

use std::cell::RefCell;

use super::*;

/// One record as the kernel lays it out, for the decoder and the stand-in.
fn record(row: &KinfoRow) -> Vec<u8> {
    let mut bytes = vec![0_u8; KINFO_PROC_BYTES];
    let mut put = |at: usize, value: &[u8]| {
        if let Some(slot) = at
            .checked_add(value.len())
            .and_then(|end| bytes.get_mut(at..end))
        {
            slot.copy_from_slice(value);
        }
    };
    put(OFFSET_PID, &row.pid.to_ne_bytes());
    put(OFFSET_PPID, &row.ppid.to_ne_bytes());
    put(OFFSET_PGID, &row.pgid.to_ne_bytes());
    put(OFFSET_STAT, &[if row.zombie { SZOMB } else { 2 }]);
    put(OFFSET_START_SEC, &row.start_sec.to_ne_bytes());
    put(OFFSET_START_USEC, &row.start_usec.to_ne_bytes());
    bytes
}

/// A drawn table: `count` rows whose fields are a function of the seed.
fn drawn(seed: u64, count: usize) -> Vec<KinfoRow> {
    let mut stream = crate::seeded::Seeded::from_seed(seed);
    (0..count)
        .map(|_| {
            let [b0, b1, b2, b3, b4, b5, b6, b7] = stream.next_u64().to_le_bytes();
            KinfoRow {
                pid: i32::from_le_bytes([b0, b1, b2, 0]),
                ppid: i32::from_le_bytes([b3, b4, 0, 0]),
                pgid: i32::from_le_bytes([b5, b6, 0, 0]),
                zombie: b7 < 51,
                start_sec: i64::from_le_bytes([b1, b2, b3, b4, 0, 0, 0, 0]),
                start_usec: i32::from_le_bytes([b5, b6, 0, 0]),
            }
        })
        .collect()
}

thread_local! {
    /// What the stand-in serves: the table bytes, and a script of answers.
    static SERVED: RefCell<Served> = const { RefCell::new(Served::new()) };
}

/// The stand-in's state for one test.
struct Served {
    /// The table the fill writes.
    table: Vec<u8>,
    /// How many fills answer `ENOMEM` before one succeeds.
    enomem_fills: usize,
    /// Whether the fill reports more bytes than the buffer holds.
    overreport: bool,
    /// How many calls the stand-in has answered.
    calls: usize,
}

impl Served {
    /// An empty script.
    const fn new() -> Self {
        Self {
            table: Vec::new(),
            enomem_fills: 0,
            overreport: false,
            calls: 0,
        }
    }
}

/// The `ENOMEM` the stand-in's `-1` means.
fn enomem() -> io::Error {
    io::Error::from_raw_os_error(ENOMEM)
}

/// A Rust stand-in for `sysctl` that writes like the kernel: the size on a
/// null buffer, otherwise at most `*oldlenp` bytes and the count written.
///
/// # Safety
///
/// `oldlenp` must point to a live `usize`, and a non-null `oldp` to at least
/// `*oldlenp` writable bytes — exactly what `read_bytes` passes.
unsafe extern "C" fn stand_in(
    _name: *mut c_int,
    _namelen: c_uint,
    oldp: *mut c_void,
    oldlenp: *mut usize,
    _newp: *mut c_void,
    _newlen: usize,
) -> c_int {
    SERVED.with_borrow_mut(|served| {
        served.calls = served.calls.saturating_add(1);
        // SAFETY: `read_bytes` passes a live `usize` for `oldlenp`.
        let room = unsafe { oldlenp.as_mut() };
        let Some(room) = room else { return -1 };
        if oldp.is_null() {
            *room = served.table.len();
            return 0;
        }
        if served.enomem_fills > 0 {
            served.enomem_fills = served.enomem_fills.saturating_sub(1);
            return -1;
        }
        let written = served.table.len().min(*room);
        let Some(source) = served.table.get(..written) else {
            return -1;
        };
        // SAFETY: `oldp` is `*room` initialised bytes from `read_bytes`, and
        // `written` is at most `*room`.
        let target = unsafe { std::slice::from_raw_parts_mut(oldp.cast::<u8>(), written) };
        target.copy_from_slice(source);
        *room = if served.overreport {
            room.saturating_add(1)
        } else {
            written
        };
        0
    })
}

/// Serve `table` with the given script for the duration of `body`.
fn serving<T>(
    table: Vec<u8>,
    enomem_fills: usize,
    overreport: bool,
    body: impl FnOnce() -> T,
) -> T {
    SERVED.with_borrow_mut(|served| {
        *served = Served {
            table,
            enomem_fills,
            overreport,
            calls: 0,
        };
    });
    body()
}

/// The stand-in's call count.
fn calls() -> usize {
    SERVED.with_borrow(|served| served.calls)
}

/// Read the whole table through the seam.
fn read_all() -> io::Result<Vec<u8>> {
    read_bytes(stand_in, enomem, &mut [CTL_KERN, KERN_PROC, KERN_PROC_ALL])
}

#[test]
fn a_whole_table_round_trips_through_the_syscall_seam() -> io::Result<()> {
    let rows = drawn(7, 40);
    let bytes: Vec<u8> = rows.iter().flat_map(record).collect();
    let read = serving(bytes, 0, false, read_all)?;
    assert_eq!(decode(&read)?, rows, "the rows read are the rows served");
    assert_eq!(calls(), 2, "one size query and one fill");
    Ok(())
}

#[test]
fn a_table_that_grew_between_the_calls_is_read_again() -> io::Result<()> {
    let rows = drawn(11, 3);
    let bytes: Vec<u8> = rows.iter().flat_map(record).collect();
    let read = serving(bytes, 2, false, read_all)?;
    assert_eq!(decode(&read)?, rows, "the retried read is complete");
    assert_eq!(calls(), 6, "two refused fills, each re-sized, then a fill");
    Ok(())
}

#[test]
fn a_table_that_never_stops_growing_is_refused_after_the_bound() {
    let refused = serving(vec![0; KINFO_PROC_BYTES], usize::MAX, false, read_all);
    assert_eq!(
        refused.map_err(|error| error.kind()),
        Err(io::ErrorKind::OutOfMemory),
        "a table that outgrows every buffer is refused, not read for ever"
    );
    assert_eq!(
        calls(),
        MAX_ATTEMPTS.saturating_mul(2),
        "the bound is exact"
    );
}

#[test]
fn a_length_past_the_buffer_is_refused_and_no_byte_is_kept() {
    let refused = serving(vec![0; KINFO_PROC_BYTES], 0, true, read_all);
    assert_eq!(
        refused.map_err(|error| error.kind()),
        Err(io::ErrorKind::InvalidData),
        "a reported length past the buffer is a lie the reader refuses"
    );
}

#[test]
fn a_partial_record_is_refused_rather_than_decoded() {
    let bytes = vec![0_u8; KINFO_PROC_BYTES.saturating_add(1)];
    assert_eq!(
        decode(&bytes).map_err(|error| error.kind()),
        Err(io::ErrorKind::InvalidData),
        "a buffer that is not whole records fails closed"
    );
    assert_eq!(
        decode(&[]).map(|rows| rows.len()).ok(),
        Some(0),
        "an empty buffer is an empty table"
    );
}

#[test]
fn the_state_byte_alone_decides_a_zombie() -> io::Result<()> {
    let mut row = drawn(3, 1)
        .first()
        .copied()
        .ok_or_else(|| io::Error::other("no row"))?;
    row.zombie = true;
    assert!(
        decode(&record(&row))?
            .first()
            .is_some_and(|read| read.zombie),
        "SZOMB reads as a zombie"
    );
    row.zombie = false;
    assert!(
        decode(&record(&row))?
            .first()
            .is_some_and(|read| !read.zombie),
        "any other state reads as running"
    );
    Ok(())
}

/// Seeded tables of every size from empty to 64 rows, read through the seam
/// with up to two refused fills, decode to exactly the rows encoded, and the
/// same seed replays to the same rows.
#[test]
fn sim_every_seeded_table_round_trips_exactly() -> io::Result<()> {
    // Miri interprets every byte the stand-in writes, so it walks the first
    // sixteen seeds of the band; the native run walks all 256.
    #[cfg(miri)]
    const SEEDS: u64 = 16;
    #[cfg(not(miri))]
    const SEEDS: u64 = 256;
    for seed in 0..SEEDS {
        let count = usize::try_from(
            seed.checked_rem(65)
                .ok_or_else(|| io::Error::other("rem"))?,
        )
        .map_err(io::Error::other)?;
        let refusals = usize::try_from(seed.checked_rem(3).ok_or_else(|| io::Error::other("rem"))?)
            .map_err(io::Error::other)?;
        let rows = drawn(seed, count);
        let bytes: Vec<u8> = rows.iter().flat_map(record).collect();
        let read = serving(bytes, refusals, false, read_all)?;
        assert_eq!(
            decode(&read)?,
            rows,
            "seed {seed}: the rows read are the rows served"
        );
        assert_eq!(drawn(seed, count), rows, "seed {seed} replays");
    }
    Ok(())
}

/// The live kernel: this process is in the table under its real parent, and
/// the one-pid read agrees with the table. Not under Miri, which cannot make
/// the foreign call.
#[test]
#[cfg(not(miri))]
fn the_kernel_table_names_this_process_under_its_parent() -> io::Result<()> {
    let own = i32::try_from(std::process::id()).map_err(io::Error::other)?;
    let parent = i32::try_from(std::os::unix::process::parent_id()).map_err(io::Error::other)?;
    let listed = table()?
        .into_iter()
        .find(|row| row.pid == own)
        .ok_or_else(|| io::Error::other("this process is missing from the table"))?;
    assert_eq!(
        listed.ppid, parent,
        "the kernel names this process's parent"
    );
    assert!(!listed.zombie, "a running process is not a zombie");
    assert_eq!(
        one(own)?,
        Some(listed),
        "the one-pid read agrees with the table"
    );
    Ok(())
}

/// A pid above every ceiling has no record. Not under Miri.
#[test]
#[cfg(not(miri))]
fn a_vacant_pid_has_no_record() -> io::Result<()> {
    assert_eq!(one(i32::MAX)?, None, "no process holds i32::MAX");
    Ok(())
}
