//! The run's scratch directory: a RAM-backed volume for the lanes' `TMPDIR`.
//!
//! The simulation suites write a real file journal and `sync_all` it. On a
//! physical disk those syncs queue behind each other (macOS issues
//! `F_FULLFSYNC`), and the `sim_scale` binary took 104 s on disk against 21.6 s
//! on a RAM disk, same 164 tests, same syscalls. Creating the volume is
//! best-effort: a host that cannot make one still runs every lane, slower and
//! never different.
//!
//! A run owns its volume through an exclusive lock on a lease file beside it.
//! A run that was killed releases the lock with its process, so the next run
//! finds the lease free and reclaims the volume: no process id is read, and no
//! id is reused under a stranger.
//!
//! The name after the prefix is a time-ordered UUID (`Uuid::new_v7`, issue
//! #715). It is distinct from every other run's, a listing of `/Volumes` or the
//! temporary directory shows the leftovers oldest first, and it carries the
//! instant it was made, so a leftover's age is read from its own name and not
//! from a file's modification time that a copy or a `touch` can move. A name
//! written by an older build (64 random bits in hex) is not a UUID and is aged
//! by its lease file, as before.

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(not(target_family = "wasm"))]
use lgwks_std::id::Uuid;

/// The name every scratch volume and lease starts with.
const PREFIX: &str = "lgwks-gate-";

/// 4 GiB of 512-byte sectors. A RAM disk takes memory only as it is written; the
/// simulation suites leave ~400 KB behind and peak far below this.
const RAM_DISK_SECTORS: &str = "8388608";

/// A lease younger than this is never reclaimed: its owner may not have taken
/// the lock yet.
const FRESH: Duration = Duration::from_secs(30);

/// How the volume is let go.
enum Detach {
    /// There is nothing to let go.
    Nothing,
    /// A macOS RAM disk, detached by device.
    Device(String),
    /// A directory under `/dev/shm`, removed.
    Directory(PathBuf),
}

/// The scratch volume of one run.
pub(super) struct Scratch {
    /// The directory the lanes' `TMPDIR` points at, if one could be made.
    path: Option<PathBuf>,
    /// How to let it go.
    detach: Detach,
    /// The held lease; the lock is released when it closes.
    lease: Option<(File, PathBuf)>,
}

impl Scratch {
    /// A run with no scratch volume: the ordinary temporary directory.
    fn none() -> Self {
        Self {
            path: None,
            detach: Detach::Nothing,
            lease: None,
        }
    }

    /// The scratch directory, if there is one.
    pub(super) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Makes a volume when `wanted` and the host allows one.
    pub(super) fn open(wanted: bool) -> Self {
        if !wanted {
            return Self::none();
        }
        let temp = std::env::temp_dir();
        reclaim_stale(&temp);
        let Some(identity) = fresh_identity() else {
            return Self::none();
        };
        let name = format!("{PREFIX}{identity}");
        let Some(lease) = take_lease(&temp, &name) else {
            return Self::none();
        };
        let made = match std::env::consts::OS {
            "macos" => make_ram_disk(&name),
            "linux" => make_shm_directory(&name),
            _ => None,
        };
        match made {
            Some((path, detach)) => Self {
                path: Some(path),
                detach,
                lease: Some(lease),
            },
            None => {
                release_lease(lease);
                Self::none()
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        match std::mem::replace(&mut self.detach, Detach::Nothing) {
            Detach::Nothing => {}
            Detach::Device(device) => run_quietly("hdiutil", &["detach", &device, "-force"]),
            Detach::Directory(directory) => {
                if let Err(error) = fs::remove_dir_all(&directory) {
                    lgwks_std::trace::debug!(%error, path = ?directory, "a scratch directory could not be removed");
                }
            }
        }
        if let Some(lease) = self.lease.take() {
            release_lease(lease);
        }
    }
}

/// WASI has no entropy source and no RAM disk to name, so a scratch name is
/// never made there and the lanes use the system temporary directory.
#[cfg(target_family = "wasm")]
fn fresh_identity() -> Option<String> {
    None
}

/// A name no other run holds, that sorts by when the run started: a time-ordered
/// UUID, 62 random bits behind a millisecond stamp and a counter.
#[cfg(not(target_family = "wasm"))]
fn fresh_identity() -> Option<String> {
    match Uuid::new_v7() {
        Ok(identity) => Some(identity.to_string()),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "no identity for a scratch name; the lanes use the system temporary directory");
            None
        }
    }
}

/// The millisecond the run that made `stem` started, when its name says so.
///
/// WASI makes no scratch name (`fresh_identity` above), so there is nothing to
/// read there.
#[cfg(target_family = "wasm")]
fn started_ms(_stem: &str) -> Option<u64> {
    None
}

/// The millisecond the run that made `stem` started, when its name says so: the
/// stamp of the time-ordered UUID after the prefix. A name that is not one (an
/// older build's hex) has none.
#[cfg(not(target_family = "wasm"))]
fn started_ms(stem: &str) -> Option<u64> {
    let tail = stem.strip_prefix(PREFIX)?;
    match Uuid::parse(tail) {
        Ok(identity) => identity.unix_millis(),
        Err(error) => {
            lgwks_std::trace::debug!(%error, name = stem, "a scratch name carries no start time; its lease file is aged instead");
            None
        }
    }
}

/// How long ago the run that made `stem` started, read from the name, as of
/// `now`. A name stamped after `now` (a clock that stepped back) is as young as
/// a name can be, so it is never reclaimed on that reading.
fn age_from_name(stem: &str, now: SystemTime) -> Option<Duration> {
    let started = UNIX_EPOCH.checked_add(Duration::from_millis(started_ms(stem)?))?;
    match now.duration_since(started) {
        Ok(age) => Some(age),
        Err(ahead) => {
            lgwks_std::trace::debug!(ahead = ?ahead.duration(), name = stem, "a scratch name is stamped ahead of the clock");
            Some(Duration::ZERO)
        }
    }
}

/// How long ago `entry`'s file was last modified, or nothing when the host
/// cannot say.
fn age_from_file(entry: &fs::DirEntry) -> Option<Duration> {
    match entry
        .metadata()
        .and_then(|meta| meta.modified())
        .map(|stamp| stamp.elapsed())
    {
        Ok(Ok(age)) => Some(age),
        Ok(Err(_)) | Err(_) => None,
    }
}

/// Creates and locks the lease for `name`.
fn take_lease(temp: &Path, name: &str) -> Option<(File, PathBuf)> {
    let path = temp.join(format!("{name}.lock"));
    let file = match File::create_new(&path) {
        Ok(file) => file,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?path, "a scratch lease could not be created");
            return None;
        }
    };
    match file.lock() {
        Ok(()) => Some((file, path)),
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?path, "a scratch lease could not be locked");
            None
        }
    }
}

/// Lets go of a lease: the file goes, then the lock with the handle.
fn release_lease(lease: (File, PathBuf)) {
    if let Err(error) = fs::remove_file(&lease.1) {
        lgwks_std::trace::debug!(%error, path = ?lease.1, "a scratch lease could not be removed");
    }
}

/// Runs a program for its effect, logging a failure.
fn run_quietly(program: &str, args: &[&str]) {
    let outcome = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match outcome {
        Ok(status) if status.success() => {}
        Ok(status) => {
            lgwks_std::trace::debug!(program, ?status, "a scratch command did not succeed")
        }
        Err(error) => lgwks_std::trace::debug!(%error, program, "a scratch command could not run"),
    }
}

/// A macOS APFS volume on a RAM disk, and the device to detach it by.
fn make_ram_disk(name: &str) -> Option<(PathBuf, Detach)> {
    let attached = Command::new("hdiutil")
        .args(["attach", "-nomount", &format!("ram://{RAM_DISK_SECTORS}")])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&attached.stdout).into_owned();
    let device = text
        .split_whitespace()
        .find(|word| word.starts_with("/dev/disk"))?
        .to_owned();
    let erased = Command::new("diskutil")
        .args(["erasevolume", "APFS", name, &device])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let volume = Path::new("/Volumes").join(name);
    if matches!(erased, Ok(status) if status.success()) && volume.is_dir() {
        return Some((volume, Detach::Device(device)));
    }
    run_quietly("hdiutil", &["detach", &device, "-force"]);
    None
}

/// A directory under `/dev/shm` on Linux.
fn make_shm_directory(name: &str) -> Option<(PathBuf, Detach)> {
    let shm = Path::new("/dev/shm");
    if !shm.is_dir() {
        return None;
    }
    let directory = shm.join(name);
    match fs::create_dir(&directory) {
        Ok(()) => Some((directory.clone(), Detach::Directory(directory))),
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?directory, "no shared-memory scratch directory");
            None
        }
    }
}

/// Whether the lease at `path` is held by nobody.
pub(super) fn lease_is_free(path: &Path) -> bool {
    let file = match File::options().write(true).open(path) {
        Ok(file) => file,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?path, "a lease could not be opened");
            return false;
        }
    };
    match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(error)) => {
            lgwks_std::trace::debug!(%error, path = ?path, "a lease could not be probed");
            false
        }
    }
}

/// Removes the volumes of runs whose lease nobody holds (kill -9, power loss).
fn reclaim_stale(temp: &Path) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = file_name
            .strip_suffix(".lock")
            .filter(|stem| stem.starts_with(PREFIX))
        else {
            continue;
        };
        let age = age_from_name(name, SystemTime::now()).or_else(|| age_from_file(&entry));
        let old_enough = age.is_some_and(|found| found >= FRESH);
        if !old_enough || !lease_is_free(&entry.path()) {
            continue;
        }
        run_quietly("diskutil", &["eject", &format!("/Volumes/{name}")]);
        let shm = Path::new("/dev/shm").join(name);
        if shm.is_dir()
            && let Err(error) = fs::remove_dir_all(&shm)
        {
            lgwks_std::trace::debug!(%error, path = ?shm, "a stale scratch directory could not be removed");
        }
        if let Err(error) = fs::remove_file(entry.path()) {
            lgwks_std::trace::debug!(%error, "a stale lease could not be removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{lease_is_free, take_lease};
    use std::error::Error;
    use std::fs;

    #[cfg(not(target_family = "wasm"))]
    use super::{PREFIX, age_from_name};
    #[cfg(not(target_family = "wasm"))]
    use lgwks_std::id::Uuid;
    #[cfg(not(target_family = "wasm"))]
    use std::time::{Duration, UNIX_EPOCH};

    type TestResult = Result<(), Box<dyn Error>>;

    /// A scratch name as `fresh_identity` makes it, with the millisecond it
    /// carries.
    #[cfg(not(target_family = "wasm"))]
    fn made_name() -> Result<(String, u64), Box<dyn Error>> {
        let identity = Uuid::new_v7()?;
        let stamp = identity.unix_millis().ok_or("a new id carries no stamp")?;
        Ok((format!("{PREFIX}{identity}"), stamp))
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    /// RFC 9562 section 5.7: the first 48 bits of a version 7 id are the
    /// milliseconds since the Unix epoch, so a scratch name is as old as the
    /// reader's clock is past its stamp.
    fn a_name_reads_back_how_long_ago_its_run_started() -> TestResult {
        let (name, stamp) = made_name()?;
        let started = UNIX_EPOCH
            .checked_add(Duration::from_millis(stamp))
            .ok_or("a stamp past the clock's range")?;
        let later = started
            .checked_add(Duration::from_secs(90))
            .ok_or("a clock past its range")?;
        assert_eq!(
            age_from_name(&name, later),
            Some(Duration::from_secs(90)),
            "a run that started 90 s before the reading is 90 s old"
        );
        assert_eq!(
            age_from_name(&name, started),
            Some(Duration::ZERO),
            "a reading at the stamp is no age at all"
        );
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    /// A reader whose clock stepped back sees a name stamped ahead of it; that
    /// name must read as new, or a live run's volume would be ejected.
    fn a_name_stamped_ahead_of_the_clock_is_as_young_as_a_name_can_be() -> TestResult {
        let (name, stamp) = made_name()?;
        let behind = UNIX_EPOCH
            .checked_add(Duration::from_millis(stamp.saturating_sub(5_000)))
            .ok_or("a stamp past the clock's range")?;
        assert_eq!(
            age_from_name(&name, behind),
            Some(Duration::ZERO),
            "a name five seconds ahead of the clock is zero seconds old"
        );
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    /// An older build named its volumes with 64 random bits in hex, and a stray
    /// file may carry the prefix with anything after it; neither carries a start
    /// time, so neither is aged by its name.
    fn a_name_that_is_not_a_time_ordered_id_has_no_start_time() -> TestResult {
        let (made, stamp) = made_name()?;
        let now = UNIX_EPOCH
            .checked_add(Duration::from_millis(stamp))
            .ok_or("a stamp past the clock's range")?;
        let foreign = made.replacen(PREFIX, "other-", 1);
        for name in [
            format!("{PREFIX}0123456789abcdef"),
            PREFIX.to_owned(),
            format!("{PREFIX}not-an-id"),
            foreign,
        ] {
            assert_eq!(
                age_from_name(&name, now),
                None,
                "{name} must be aged by its lease file"
            );
        }
        Ok(())
    }

    #[test]
    fn a_held_lease_is_not_free_and_a_released_one_is() -> TestResult {
        let base = std::env::temp_dir();
        // The first name nobody holds: `create_new` is the exclusive claim, so
        // concurrent runs of this test never share a lease.
        let (file, path) = (0..4096_u32)
            .find_map(|attempt| take_lease(&base, &format!("lgwks-gate-test-lease-{attempt}")))
            .ok_or("no lease could be taken")?;
        assert!(!lease_is_free(&path), "a lease its owner holds is not free");
        drop(file);
        assert!(lease_is_free(&path), "a lease its owner dropped is free");
        fs::remove_file(&path)?;
        Ok(())
    }
}
