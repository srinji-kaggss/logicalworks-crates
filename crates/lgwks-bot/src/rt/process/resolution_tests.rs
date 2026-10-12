//! Tests of program resolution against PATH and the working directory.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

/// Distinguishes scratch directories made in the same nanosecond.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A shell script whose interpreter exists on every Unix this runs on.
const SCRIPT: &[u8] = b"#!/bin/sh\nexit 0\n";

/// One test's scratch `PATH` entry, removed on drop however the test ends.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// A fresh directory named by the clock and a counter (INV-DEP-6): a
    /// process id is reused by the OS and is not an identity.
    fn new() -> io::Result<Self> {
        let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => since.as_nanos(),
            Err(before) => before.duration().as_nanos(),
        };
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lgwks-resolve-{nanos}-{seq}"));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Write `name` with `body` and exactly `mode`.
    fn file(&self, name: &str, body: &[u8], mode: u32) -> io::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;
        let at = self.dir.join(name);
        std::fs::write(&at, body)?;
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode))?;
        Ok(at)
    }

    /// Make `name` a symlink to `target`.
    fn link(&self, name: &str, target: &Path) -> io::Result<PathBuf> {
        let at = self.dir.join(name);
        std::os::unix::fs::symlink(target, &at)?;
        Ok(at)
    }

    fn entry(&self) -> &OsStr {
        self.dir.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            lgwks_std::trace::debug!(
                path = ?self.dir,
                error = %error,
                "resolution_tests left its scratch dir behind"
            );
        }
    }
}

fn spec_with_path(program: &str, path: &OsStr) -> ProcessSpec {
    let mut spec = ProcessSpec::new(program);
    spec.env("PATH", path);
    spec
}

fn joined(entries: &[&OsStr]) -> OsString {
    let mut path = OsString::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            path.push(":");
        }
        path.push(entry);
    }
    path
}

#[test]
fn a_bare_name_resolves_to_a_script_whose_interpreter_exists() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let witness = scratch.file("witness", SCRIPT, 0o755)?;
    let spec = spec_with_path("witness", scratch.entry());
    assert_eq!(spec.resolved_program(), witness);
    Ok(())
}

#[test]
fn a_script_naming_a_missing_interpreter_stays_bare() -> io::Result<()> {
    // `execve` fails it with ENOENT and `execvp` would try a later entry,
    // so resolving it would change what runs.
    let scratch = Scratch::new()?;
    scratch.file("witness", b"#!/nonexistent/lgwks-interpreter\n", 0o755)?;
    let spec = spec_with_path("witness", scratch.entry());
    assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    Ok(())
}

#[test]
fn a_symlink_to_a_native_binary_resolves_to_the_link() -> io::Result<()> {
    // `/bin/sh` is ELF on Linux and a universal Mach-O on macOS, so this
    // also drives each platform's native-magic arm.
    let scratch = Scratch::new()?;
    let witness = scratch.link("witness", Path::new("/bin/sh"))?;
    let spec = spec_with_path("witness", scratch.entry());
    assert_eq!(spec.resolved_program(), witness);
    Ok(())
}

#[test]
fn a_symlink_is_judged_by_its_target_and_passed_over_like_it() -> io::Result<()> {
    // The link's own mode is 0o777 (0o755 on macOS); the target has no
    // execute bit, so `execve` refuses it and `execvp` moves on.
    let first = Scratch::new()?;
    let target = first.file("real", SCRIPT, 0o644)?;
    first.link("witness", &target)?;
    let second = Scratch::new()?;
    let later = second.file("witness", SCRIPT, 0o755)?;
    let spec = spec_with_path("witness", &joined(&[first.entry(), second.entry()]));
    assert_eq!(spec.resolved_program(), later);
    Ok(())
}

#[test]
fn an_execute_only_entry_is_never_skipped_for_a_later_one() -> io::Result<()> {
    // `execvp` runs the first executable entry whether or not its head is
    // readable. As a user the head is unreadable and the name stays bare;
    // as root it is read and resolves to the first entry. Never the second.
    let first = Scratch::new()?;
    let earlier = first.file("witness", SCRIPT, 0o711)?;
    let second = Scratch::new()?;
    let later = second.file("witness", SCRIPT, 0o755)?;
    let spec = spec_with_path("witness", &joined(&[first.entry(), second.entry()]));
    let resolved = spec.resolved_program();
    assert_ne!(resolved, later, "a later entry shadowed an executable one");
    assert!(
        resolved == earlier || resolved == Path::new("witness"),
        "resolved to {resolved:?}"
    );
    Ok(())
}

#[test]
fn partial_execute_bits_stay_bare_for_the_kernel_to_judge() -> io::Result<()> {
    let first = Scratch::new()?;
    first.file("witness", SCRIPT, 0o710)?;
    let second = Scratch::new()?;
    second.file("witness", SCRIPT, 0o755)?;
    let spec = spec_with_path("witness", &joined(&[first.entry(), second.entry()]));
    assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    Ok(())
}

#[test]
fn a_relative_working_directory_stays_bare() -> io::Result<()> {
    // `Command` would read a relative program against the child's working
    // directory again, doubling it.
    let mut spec = ProcessSpec::new("witness");
    spec.current_dir("relative-lgwks-dir");
    spec.env("PATH", "");
    assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    Ok(())
}

#[test]
fn a_shell_fallback_script_stays_bare() -> io::Result<()> {
    let scratch = Scratch::new()?;
    scratch.file("witness", b"exit 0\n", 0o755)?;
    let spec = spec_with_path("witness", scratch.entry());
    assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    Ok(())
}

#[test]
fn a_foreign_binary_and_a_class_file_stay_bare() -> io::Result<()> {
    // The other platform's magic is ENOEXEC here, and a Java class file
    // shares the universal Mach-O magic with a version where the count is.
    #[cfg(target_vendor = "apple")]
    let foreign: &[u8] = b"\x7fELF\x02\x01\x01\x00";
    #[cfg(not(target_vendor = "apple"))]
    let foreign: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
    let class: &[u8] = &[0xca, 0xfe, 0xba, 0xbe, 0x00, 0x00, 0x00, 0x41];
    for head in [foreign, class] {
        let scratch = Scratch::new()?;
        scratch.file("witness", head, 0o755)?;
        let spec = spec_with_path("witness", scratch.entry());
        assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    }
    Ok(())
}

#[test]
fn a_name_holding_a_slash_is_never_rewritten() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let spelled = scratch.file("witness", SCRIPT, 0o755)?;
    let mut spec = ProcessSpec::new(spelled.as_os_str());
    spec.env("PATH", scratch.entry());
    assert_eq!(spec.resolved_program(), spelled);
    Ok(())
}

#[test]
fn a_name_found_nowhere_stays_bare_for_the_platform_default() -> io::Result<()> {
    let scratch = Scratch::new()?;
    scratch.file("witness", SCRIPT, 0o755)?;
    let spec = spec_with_path("no-such-program-lgwks", scratch.entry());
    assert_eq!(
        spec.resolved_program(),
        PathBuf::from("no-such-program-lgwks")
    );
    Ok(())
}

#[test]
fn a_clear_that_sets_no_path_stays_bare_despite_the_inherited_path() {
    let mut spec = ProcessSpec::new("sh");
    spec.env_clear();
    assert_eq!(spec.resolved_program(), PathBuf::from("sh"));
}

#[test]
fn a_removed_path_stays_bare() -> io::Result<()> {
    let scratch = Scratch::new()?;
    scratch.file("witness", SCRIPT, 0o755)?;
    let mut spec = spec_with_path("witness", scratch.entry());
    spec.env_remove("PATH");
    assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    Ok(())
}

#[test]
fn directories_and_bit_less_files_are_passed_over_for_a_later_entry() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = Scratch::new()?;
    let decoy = directory.dir.join("witness");
    std::fs::create_dir_all(&decoy)?;
    std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755))?;
    let bitless = Scratch::new()?;
    bitless.file("witness", SCRIPT, 0o644)?;
    let found = Scratch::new()?;
    let witness = found.file("witness", SCRIPT, 0o755)?;
    let path = joined(&[directory.entry(), bitless.entry(), found.entry()]);
    let spec = spec_with_path("witness", &path);
    assert_eq!(spec.resolved_program(), witness);
    Ok(())
}

#[test]
fn an_empty_entry_searches_the_spec_working_directory() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let witness = scratch.file("witness", SCRIPT, 0o755)?;
    let mut spec = ProcessSpec::new("witness");
    spec.current_dir(&scratch.dir);
    spec.env("PATH", "");
    assert_eq!(spec.resolved_program(), witness);
    Ok(())
}

#[test]
fn a_path_longer_than_the_walk_bound_stays_bare() -> io::Result<()> {
    let scratch = Scratch::new()?;
    scratch.file("witness", SCRIPT, 0o755)?;
    let absent = scratch.dir.join("absent");
    // `misses` absent entries, then the one that holds the witness.
    let path_after = |misses: usize| {
        let mut entries: Vec<&OsStr> = vec![absent.as_os_str(); misses];
        entries.push(scratch.entry());
        joined(&entries)
    };
    let spec = spec_with_path("witness", &path_after(MAX_PATH_ENTRIES));
    assert_eq!(spec.resolved_program(), PathBuf::from("witness"));
    // One miss fewer puts the witness on the last entry the walk reads.
    let spec = spec_with_path("witness", &path_after(MAX_PATH_ENTRIES.saturating_sub(1)));
    assert_eq!(spec.resolved_program(), scratch.dir.join("witness"));
    Ok(())
}

#[test]
fn an_inherited_path_is_left_to_posix_spawnp() {
    // `sh` is on every inherited `PATH`: a walk here would resolve it and
    // pay for the search `posix_spawnp` already makes.
    let spec = ProcessSpec::new("sh");
    assert_eq!(spec.resolved_program(), PathBuf::from("sh"));
    let mut other = ProcessSpec::new("sh");
    other.env("LGWKS_UNRELATED", "1");
    assert_eq!(other.resolved_program(), PathBuf::from("sh"));
}

#[test]
fn a_bare_program_with_no_path_arm_holds_no_slash() {
    // Negative control: without the `Set PATH` arm the same name stays
    // bare, so every positive assertion above exercises the new code.
    let spec = ProcessSpec::new("witness");
    assert!(!spec.resolved_program().to_string_lossy().contains('/'));
}
