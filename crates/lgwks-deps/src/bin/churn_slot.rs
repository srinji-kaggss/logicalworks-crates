//! `churn-slot`: the nextest wrapper that holds one of a few machine-wide slots for
//! as long as one test process runs (#272).
//!
//! One macOS kernel serialises file-system namespace work across processes, and
//! the thirteen jobs of a run put every test of several suites on it at once: the
//! full `lgwks_bot` suite takes 40 s alone with its temporary files on a RAM disk
//! and about 300 s beside the other jobs. Throughput of that work falls past about
//! four concurrent processes, and a nextest test group limits one job's processes,
//! not the machine's. This binary limits the machine's: it takes an advisory lock on
//! one of `LWC_CHURN_SLOTS` files under `LWC_CHURN_DIR`, runs the test binary
//! nextest names with the arguments nextest gave it, and gives the slot back when
//! the test exits, however it exits (the operating system drops the lock with the
//! process). A test that finds every slot taken waits on one chosen by its process
//! id, so waiters spread over the slots.
//!
//! It is a wrapper binary and not a shell script because macOS removes `DYLD_*`
//! variables when a protected binary such as `/bin/sh` starts a child, and a test
//! binary that loads the toolchain's `libstd` dylib (a proc-macro crate's tests)
//! then fails to start. The slot is a convenience, so a slot that cannot be taken
//! (the directory cannot be made, the lock is unsupported) runs the test uncapped
//! and says so on standard error: a test that does not run is a test that silently
//! passes.
//!
//! Usage: `churn-slot <program> [argument...]`. The exit status is the program's;
//! a program ended by a signal exits `128 + signal`.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

/// The slots a machine offers when `LWC_CHURN_SLOTS` names none.
const DEFAULT_SLOTS: NonZeroUsize = match NonZeroUsize::new(6) {
    Some(count) => count,
    None => NonZeroUsize::MIN,
};

/// The most slots a caller may name: a larger number is a typo, and every slot is
/// a file this directory keeps.
const MAX_SLOTS: usize = 64;

/// The directory holding the slot files when `LWC_CHURN_DIR` names none. Absolute
/// and outside `TMPDIR` on purpose: every job of the host sets its own `TMPDIR`
/// (a RAM disk of its own), and the slots are shared by all of them.
const DEFAULT_DIR: &str = "/tmp/lwc-churn";

/// The variable naming how many slots there are.
const SLOTS_VAR: &str = "LWC_CHURN_SLOTS";

/// The variable naming the directory that holds the slot files.
const DIR_VAR: &str = "LWC_CHURN_DIR";

/// The exit status of a program that could not be started, as a shell reports one.
const UNRUNNABLE: u8 = 127;

/// The exit status of a call with no program to run.
const USAGE: u8 = 2;

/// Why a slot could not be taken.
#[derive(Debug)]
enum SlotError {
    /// The slot directory could not be made.
    Directory {
        /// The directory.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A slot file could not be opened or locked.
    Slot {
        /// The slot file.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
}

impl fmt::Display for SlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Directory {
                ref path,
                ref cause,
            } => write!(formatter, "cannot make {}: {cause}", path.display()),
            Self::Slot {
                ref path,
                ref cause,
            } => write!(formatter, "cannot lock {}: {cause}", path.display()),
        }
    }
}

impl std::error::Error for SlotError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
fn fail<T>(error: SlotError, site: &'static str) -> Result<T, SlotError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// The machine's slots: how many, and where their files are.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slots {
    /// The directory holding the slot files.
    dir: PathBuf,
    /// How many slots there are.
    count: NonZeroUsize,
}

/// A slot held: the lock lives as long as this file does.
#[derive(Debug)]
struct Held {
    /// The slot's file; closing it releases the lock.
    file: File,
}

impl Slots {
    /// The slots a caller's environment names. A count that is unset, not a
    /// number, zero or past [`MAX_SLOTS`] is the default, and so is a directory
    /// that is unset or empty.
    fn from_env(slots: Option<OsString>, dir: Option<OsString>) -> Self {
        let named = slots
            .as_deref()
            .and_then(|text| text.to_str())
            .map(|text| text.trim().parse::<usize>());
        let count = match named {
            Some(Ok(number)) if number <= MAX_SLOTS => match NonZeroUsize::new(number) {
                Some(count) => count,
                None => DEFAULT_SLOTS,
            },
            _ => DEFAULT_SLOTS,
        };
        let dir = match dir {
            Some(text) if !text.is_empty() => PathBuf::from(text),
            _ => PathBuf::from(DEFAULT_DIR),
        };
        Self { dir, count }
    }

    /// The file of slot `index`.
    fn path_of(&self, index: usize) -> PathBuf {
        self.dir.join(format!("slot-{index}"))
    }

    /// Opens slot `index`, making its file when it is absent. The file is kept
    /// between runs and never removed: a lock on a file another process then
    /// unlinks and recreates would be a lock on a name nobody else holds.
    fn open(&self, index: usize) -> Result<File, SlotError> {
        let path = self.path_of(index);
        match OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
        {
            Ok(file) => Ok(file),
            Err(cause) => fail(SlotError::Slot { path, cause }, "Slots::open"),
        }
    }

    /// Takes a free slot without waiting, trying each from `start`, or `None` when
    /// every slot is held.
    fn try_take(&self, start: usize) -> Result<Option<Held>, SlotError> {
        for step in 0..self.count.get() {
            let index = start.wrapping_add(step) % self.count;
            let file = self.open(index)?;
            match file.try_lock() {
                Ok(()) => return Ok(Some(Held { file })),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(cause)) => {
                    let path = self.path_of(index);
                    return fail(SlotError::Slot { path, cause }, "Slots::try_take");
                }
            }
        }
        Ok(None)
    }

    /// Takes a slot, waiting on the one `start` names when every slot is held.
    fn take(&self, start: usize) -> Result<Held, SlotError> {
        if let Err(cause) = fs::create_dir_all(&self.dir) {
            let path = self.dir.clone();
            return fail(SlotError::Directory { path, cause }, "Slots::take");
        }
        if let Some(held) = self.try_take(start)? {
            return Ok(held);
        }
        let index = start % self.count;
        let file = self.open(index)?;
        match file.lock() {
            Ok(()) => Ok(Held { file }),
            Err(cause) => {
                let path = self.path_of(index);
                fail(SlotError::Slot { path, cause }, "Slots::take")
            }
        }
    }
}

impl Drop for Held {
    /// Gives the slot back. Closing the file would too; releasing it by name
    /// keeps the intent in one place, and a refusal changes nothing because the
    /// operating system drops the lock when the process ends.
    fn drop(&mut self) {
        match self.file.unlock() {
            Ok(()) | Err(_) => {}
        }
    }
}

impl Held {
    /// The slot's file, so a test can see that the lock is on a real file.
    #[cfg(test)]
    fn file(&self) -> &File {
        &self.file
    }
}

/// The exit status a wrapper reports for `status`: the program's own, or
/// `128 + signal` for one a signal ended, as a shell reports it.
fn exit_status_of(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return low_byte(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return low_byte(signal.saturating_add(128));
        }
    }
    1
}

/// The low byte of `value`, which is all an exit status carries.
fn low_byte(value: i32) -> u8 {
    let [low, _, _, _] = value.to_le_bytes();
    low
}

/// Writes one line to standard error through a locked handle. A refusal to write
/// is not worth failing a test for, so it is dropped.
fn say(line: &str) {
    let mut handle = io::stderr().lock();
    match writeln!(handle, "churn-slot: {line}") {
        Ok(()) | Err(_) => {}
    }
}

/// The slot a taker tries first, and waits on when every slot is held: a random
/// one, so waiters spread over the slots. A process id would do the same but is
/// reused by the operating system, which is why the estate draws from
/// `lgwks_std::random`. A source that refuses starts every taker at the first
/// slot, which is still correct, only less spread. A WASI build has no entropy
/// module (`lgwks_std::random` refuses wasm) and no host to share slots with, so
/// it starts at the first slot.
#[cfg(target_family = "wasm")]
fn start_slot() -> usize {
    0
}

/// The slot a taker tries first: see the WASI variant above.
#[cfg(not(target_family = "wasm"))]
fn start_slot() -> usize {
    match lgwks_std::random::bytes::<8>() {
        Ok(bytes) => match usize::try_from(u64::from_le_bytes(bytes)) {
            Ok(start) => start,
            Err(error) => {
                lgwks_std::trace::debug!(error = ?error, "start_slot: the draw does not fit a usize");
                0
            }
        },
        Err(error) => {
            lgwks_std::trace::debug!(error = ?error, "start_slot: no entropy for a start slot");
            0
        }
    }
}

/// Runs `program` under a slot and returns its exit status.
fn run(slots: &Slots, program: &OsString, arguments: &[OsString]) -> ExitCode {
    let held = match slots.take(start_slot()) {
        Ok(held) => Some(held),
        Err(error) => {
            say(&format!("running uncapped: {error}"));
            None
        }
    };
    let status = Command::new(program).args(arguments).status();
    drop(held);
    match status {
        Ok(status) => ExitCode::from(exit_status_of(status)),
        Err(error) => {
            say(&format!(
                "cannot start {}: {error}",
                Path::new(program).display()
            ));
            ExitCode::from(UNRUNNABLE)
        }
    }
}

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let Some(program) = arguments.next() else {
        say("usage: churn-slot <program> [argument...]");
        return ExitCode::from(USAGE);
    };
    let rest: Vec<OsString> = arguments.collect();
    let slots = Slots::from_env(std::env::var_os(SLOTS_VAR), std::env::var_os(DIR_VAR));
    run(&slots, &program, &rest)
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A directory name of this test's own under the system temporary directory,
    /// distinguished by a random draw so two runs on one machine never share one.
    fn scratch(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let distinguisher = u64::from_le_bytes(lgwks_std::random::bytes::<8>()?);
        Ok(std::env::temp_dir().join(format!("churn-slot-{name}-{distinguisher:016x}")))
    }

    /// Removes what a test made; a directory that is already gone is fine.
    fn clear(dir: &Path) {
        match fs::remove_dir_all(dir) {
            Ok(()) | Err(_) => {}
        }
    }

    fn slots_in(dir: &Path, count: usize) -> Result<Slots, Box<dyn std::error::Error>> {
        let count = NonZeroUsize::new(count).ok_or("a slot count of zero")?;
        Ok(Slots {
            dir: dir.to_path_buf(),
            count,
        })
    }

    #[test]
    fn the_environment_names_the_slots_and_a_bad_value_is_the_default() {
        let named = Slots::from_env(Some("3".into()), Some("/somewhere".into()));
        assert_eq!(named.count.get(), 3);
        assert_eq!(named.dir, PathBuf::from("/somewhere"));
        for bad in ["0", "65", "-1", "many", "", "  "] {
            let fallback = Slots::from_env(Some(bad.into()), Some("".into()));
            assert_eq!(fallback.count, DEFAULT_SLOTS, "{bad:?} is not a slot count");
            assert_eq!(fallback.dir, PathBuf::from(DEFAULT_DIR));
        }
        assert_eq!(Slots::from_env(None, None).count, DEFAULT_SLOTS);
        assert_eq!(Slots::from_env(Some(" 4 ".into()), None).count.get(), 4);
        assert_eq!(
            Slots::from_env(Some(MAX_SLOTS.to_string().into()), None)
                .count
                .get(),
            MAX_SLOTS
        );
    }

    #[test]
    fn every_slot_is_held_once_and_a_further_try_finds_none()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch("full")?;
        let slots = slots_in(&dir, 3)?;
        fs::create_dir_all(&dir)?;
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(slots.try_take(0)?.ok_or("a slot was free")?);
        }
        assert!(
            slots.try_take(0)?.is_none(),
            "a fourth taker must find every slot held"
        );
        assert!(held.iter().all(|slot| slot.file().metadata().is_ok()));
        clear(&dir);
        Ok(())
    }

    #[test]
    fn a_dropped_slot_is_free_for_the_next_taker() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch("drop")?;
        let slots = slots_in(&dir, 1)?;
        fs::create_dir_all(&dir)?;
        let first = slots.try_take(0)?.ok_or("the only slot was free")?;
        assert!(slots.try_take(0)?.is_none());
        drop(first);
        assert!(
            slots.try_take(0)?.is_some(),
            "releasing the slot must free it"
        );
        clear(&dir);
        Ok(())
    }

    #[test]
    fn takers_on_threads_never_exceed_the_slots() -> Result<(), Box<dyn std::error::Error>> {
        const SLOTS: usize = 3;
        const TAKERS: usize = 16;
        let dir = scratch("threads")?;
        let slots = slots_in(&dir, SLOTS)?;
        let inside = AtomicUsize::new(0);
        let widest = AtomicUsize::new(0);
        let failures = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for taker in 0..TAKERS {
                let (slots, inside, widest, failures) = (&slots, &inside, &widest, &failures);
                scope.spawn(move || match slots.take(taker) {
                    Ok(held) => {
                        let now = inside.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                        widest.fetch_max(now, Ordering::SeqCst);
                        for _ in 0..50 {
                            std::thread::yield_now();
                        }
                        inside.fetch_sub(1, Ordering::SeqCst);
                        drop(held);
                    }
                    Err(_) => {
                        failures.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(failures.load(Ordering::SeqCst), 0);
        let widest = widest.load(Ordering::SeqCst);
        assert!(
            (1..=SLOTS).contains(&widest),
            "{widest} takers held a slot at once, with {SLOTS} slots"
        );
        clear(&dir);
        Ok(())
    }

    #[test]
    fn a_directory_that_is_a_file_is_an_error_the_wrapper_survives()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch("blocked")?;
        fs::write(&dir, b"not a directory")?;
        let slots = slots_in(&dir, 2)?;
        assert!(matches!(slots.take(0), Err(SlotError::Directory { .. })));
        fs::remove_file(&dir)?;
        Ok(())
    }

    #[test]
    fn a_program_that_exits_reports_its_own_status() -> Result<(), Box<dyn std::error::Error>> {
        for code in [0_i32, 1, 7, 101] {
            let status = Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("exit {code}"))
                .status()?;
            assert_eq!(i32::from(exit_status_of(status)), code);
        }
        Ok(())
    }
}
