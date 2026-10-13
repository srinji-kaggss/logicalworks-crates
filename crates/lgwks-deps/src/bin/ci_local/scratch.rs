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

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

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

/// A name no other run holds: 64 random bits, in hex.
#[cfg(not(target_family = "wasm"))]
fn fresh_identity() -> Option<String> {
    match lgwks_std::random::bytes::<8>() {
        Ok(bytes) => Some(format!("{:016x}", u64::from_le_bytes(bytes))),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "no entropy for a scratch name; the lanes use the system temporary directory");
            None
        }
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
        let old_enough = match entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|stamp| stamp.elapsed())
        {
            Ok(Ok(age)) => age >= FRESH,
            Ok(Err(_)) | Err(_) => false,
        };
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

    type TestResult = Result<(), Box<dyn Error>>;

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
