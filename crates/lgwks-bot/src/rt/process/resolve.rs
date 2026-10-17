//! Resolving a bare program against the child's `PATH` the way `execvp`
//! would, so `std` can spawn through `posix_spawn` instead of `fork`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use super::EnvDelta;

/// How many `PATH` entries [`resolve_on_path`] walks before giving up.
///
/// A `PATH` is caller data of unbounded length; the walk is a spawn input and
/// is bounded like every other one. Past the cap the program stays bare and
/// the spawn takes the fork path, exactly as before this resolution existed.
#[cfg(unix)]
const MAX_PATH_ENTRIES: usize = 128;

/// How many bytes of a candidate's head [`probe_entry`] reads: enough for a
/// `#!` line naming an interpreter by an ordinary absolute path.
#[cfg(unix)]
const MAX_HEAD_BYTES: usize = 256;

/// Resolve `program` against the child's `PATH` the way `execvp` would, or
/// `None` to spawn it bare.
///
/// `None` means the name holds a `/`, the path is unknown, the base directory
/// is not absolute, nothing executable was found, or an entry was found whose
/// fate only the kernel can decide. Every `None` takes the pre-existing fork
/// path, so resolution can only add the fast path, never change what runs.
#[cfg(unix)]
pub(super) fn resolve_on_path(
    program: &OsStr,
    deltas: &[EnvDelta],
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    if program.is_empty() || program.as_bytes().contains(&b'/') {
        return None;
    }
    // A spec that leaves `PATH` alone already spawns through `posix_spawnp`,
    // which searches the inherited path itself: walking it here would only add
    // a stat, an open and a read to every ordinary spawn. `std` falls back to
    // the fork exactly when the path was changed, so that is when this walks.
    if !deltas.iter().any(EnvDelta::changes_path) {
        return None;
    }
    let path = effective_path(deltas)?;
    let base = match cwd {
        Some(dir) => dir.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(error) => {
                lgwks_std::trace::debug!(
                    error = %error,
                    "resolve_on_path left the program bare: no working directory to read PATH against"
                );
                return None;
            }
        },
    };
    if !base.is_absolute() {
        return None;
    }
    for (index, entry) in path.as_bytes().split(|byte| *byte == b':').enumerate() {
        if index >= MAX_PATH_ENTRIES {
            return None;
        }
        // An empty entry is the child's working directory; a relative one is
        // read against it; an absolute one replaces it.
        let candidate = base.join(OsStr::from_bytes(entry)).join(program);
        match probe_entry(&candidate) {
            Entry::PassedOver => {}
            Entry::Spawnable => return Some(candidate),
            Entry::Undecided => return None,
        }
    }
    None
}

#[cfg(unix)]
impl EnvDelta {
    /// Whether this delta changes the child's `PATH`, the condition under which
    /// `std` gives up `posix_spawnp` for a bare program.
    fn changes_path(&self) -> bool {
        match *self {
            Self::Set { ref key, .. } | Self::Remove { ref key } => key.as_os_str() == "PATH",
            Self::Clear => true,
        }
    }
}

/// The `PATH` the child will see: the inherited one folded through `deltas`.
///
/// `Clear` drops the inherited path and everything before it; a later `Set`
/// rebuilds it. `None` is "unknown": cleared-and-unset, removed, or never
/// inherited. Callers leave the program bare then, for the platform default.
#[cfg(unix)]
fn effective_path(deltas: &[EnvDelta]) -> Option<OsString> {
    let mut path = std::env::var_os("PATH");
    for delta in deltas {
        match *delta {
            EnvDelta::Set { ref key, ref value } if key.as_os_str() == "PATH" => {
                path = Some(value.as_os_str().to_os_string());
            }
            EnvDelta::Remove { ref key } if key.as_os_str() == "PATH" => {
                path = None;
            }
            EnvDelta::Clear => {
                path = None;
            }
            EnvDelta::Set { .. } | EnvDelta::Remove { .. } => {}
        }
    }
    path
}

/// What `execvp` would do with one `PATH` candidate.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// `execvp` moves on to the next entry: absent, a directory, no execute
    /// bit, or a search it may not make.
    PassedOver,
    /// The kernel runs it directly, for any caller: the walk resolves here.
    Spawnable,
    /// `execvp` would try it, and only the kernel knows what happens: the
    /// walk stops and the program stays bare.
    Undecided,
}

/// Judge one candidate the way `execve` would, symlinks followed.
#[cfg(unix)]
fn probe_entry(candidate: &Path) -> Entry {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = match std::fs::metadata(candidate) {
        Ok(metadata) => metadata,
        // The three errors `execvp` searches past; any other is the kernel's.
        Err(error) => {
            return match error.kind() {
                io::ErrorKind::NotFound
                | io::ErrorKind::NotADirectory
                | io::ErrorKind::PermissionDenied => Entry::PassedOver,
                _ => {
                    lgwks_std::trace::debug!(
                        path = ?candidate,
                        error = %error,
                        "resolve_on_path left the program bare at a candidate it could not stat"
                    );
                    Entry::Undecided
                }
            };
        }
    };
    if metadata.is_dir() {
        return Entry::PassedOver;
    }
    match metadata.permissions().mode() & 0o111 {
        0 => return Entry::PassedOver,
        0o111 => {}
        // Executable by someone: whether by this caller is the kernel's call.
        _ => return Entry::Undecided,
    }
    if !metadata.is_file() {
        return Entry::Undecided;
    }
    let mut head = Vec::with_capacity(MAX_HEAD_BYTES);
    if let Err(error) = read_head(candidate, &mut head) {
        // An execute-only file is still executable: an unreadable head is the
        // kernel's to judge, never a reason to try the next entry.
        lgwks_std::trace::debug!(
            path = ?candidate,
            error = %error,
            "resolve_on_path left the program bare at a head it could not read"
        );
        return Entry::Undecided;
    }
    if is_direct_head(&head) {
        Entry::Spawnable
    } else {
        Entry::Undecided
    }
}

/// Read up to [`MAX_HEAD_BYTES`] of `candidate` into `head`.
#[cfg(unix)]
fn read_head(candidate: &Path, head: &mut Vec<u8>) -> io::Result<()> {
    use std::io::Read as _;
    let file = std::fs::File::open(candidate)?;
    let limit = u64::try_from(MAX_HEAD_BYTES).map_err(io::Error::other)?;
    file.take(limit).read_to_end(head)?;
    Ok(())
}

/// Whether the kernel executes a file with this head without a shell: a
/// `#!` line whose interpreter is an existing regular file with every execute
/// bit, or this platform's native binary magic.
#[cfg(unix)]
fn is_direct_head(head: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(line) = head.strip_prefix(b"#!") {
        // A `#!` line longer than the head names no ordinary interpreter.
        let Some(end) = line.iter().position(|byte| *byte == b'\n') else {
            return false;
        };
        let Some((line, _)) = line.split_at_checked(end) else {
            return false;
        };
        let Some(interpreter) = line
            .split(|byte| *byte == b' ' || *byte == b'\t')
            .find(|word| !word.is_empty())
        else {
            return false;
        };
        let interpreter = Path::new(OsStr::from_bytes(interpreter));
        if !interpreter.is_absolute() {
            return false;
        }
        // An interpreter that cannot be stat'ed is the kernel's to judge, so
        // the head is not direct and the program stays bare.
        return match std::fs::metadata(interpreter) {
            Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 == 0o111,
            Err(error) => {
                lgwks_std::trace::debug!(
                    path = ?interpreter,
                    error = %error,
                    "resolve_on_path could not stat a #! interpreter"
                );
                false
            }
        };
    }
    is_native_binary(head)
}

/// An ELF image: the native format of every non-Apple Unix this crate builds
/// for. A Mach-O there is `ENOEXEC`, which `execvp` hands to the shell.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn is_native_binary(head: &[u8]) -> bool {
    head.starts_with(b"\x7fELF")
}

/// A Mach-O image, thin or universal. A universal header shares its magic with
/// a Java class file, whose next word is a version (45 or more) where a
/// universal binary's is its architecture count, so only a small count is a
/// binary. An ELF image here is `ENOEXEC`, which `execvp` hands to the shell.
#[cfg(target_vendor = "apple")]
fn is_native_binary(head: &[u8]) -> bool {
    /// More architectures than any universal binary carries, and fewer than
    /// the first Java class-file version.
    const MAX_FAT_ARCHS: u32 = 20;
    let Some((magic, rest)) = head.split_first_chunk::<4>() else {
        return false;
    };
    match *magic {
        [0xcf | 0xce, 0xfa, 0xed, 0xfe] | [0xfe, 0xed, 0xfa, 0xce | 0xcf] => true,
        [0xca, 0xfe, 0xba, 0xbe | 0xbf] => match rest.first_chunk::<4>() {
            Some(count) => (1..MAX_FAT_ARCHS).contains(&u32::from_be_bytes(*count)),
            // A universal magic with no count word is a truncated head.
            None => false,
        },
        _ => false,
    }
}

#[cfg(all(test, unix))]
mod resolution_tests {
    use super::*;
    use crate::rt::process::ProcessSpec;

    /// A shell script whose interpreter exists on every Unix this runs on.
    const SCRIPT: &[u8] = b"#!/bin/sh\nexit 0\n";

    /// One test's scratch `PATH` entry, removed on drop however the test ends.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        /// A fresh directory named by 64 random bits (INV-BOT-116). A clock and a
        /// per-process counter are not an identity: nextest runs every test in
        /// its own process, the counter restarts at zero in each, and macOS
        /// ticks in microseconds, so two tests starting in one tick shared a
        /// directory and overwrote each other's `witness`. `create_dir` refuses
        /// a name that exists, so a collision is an error and never shared.
        fn new() -> io::Result<Self> {
            let unique = lgwks_std::random::bytes::<8>().map_err(io::Error::other)?;
            let dir = std::env::temp_dir()
                .join(format!("lgwks-resolve-{:016x}", u64::from_be_bytes(unique)));
            std::fs::create_dir(&dir)?;
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
    fn scratch_names_are_random_not_a_clock_and_a_counter() -> io::Result<()> {
        // Incident: run 39040434563, job on lwc-5, 2026-10-17. Two tests in two
        // processes drew `lgwks-resolve-<micros>-0` in one clock tick, shared a
        // directory, and `a_script_naming_a_missing_interpreter_stays_bare`
        // resolved the other test's script. A name that cannot repeat across
        // processes is 64 random bits; this holds 256 live at once.
        let mut held = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for _ in 0..256 {
            let scratch = Scratch::new()?;
            let name = scratch
                .dir
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("a scratch directory name is not UTF-8"))?
                .to_owned();
            let digits = name
                .strip_prefix("lgwks-resolve-")
                .ok_or_else(|| io::Error::other("a scratch directory lost its prefix"))?;
            assert_eq!(digits.len(), 16, "{name} is not 16 hex digits");
            assert!(
                digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{name}"
            );
            assert!(names.insert(name), "two scratch directories shared a name");
            held.push(scratch);
        }
        assert_eq!(held.len(), names.len());
        Ok(())
    }

    #[test]
    fn a_bare_program_with_no_path_arm_holds_no_slash() {
        // Negative control: without the `Set PATH` arm the same name stays
        // bare, so every positive assertion above exercises the new code.
        let spec = ProcessSpec::new("witness");
        assert!(!spec.resolved_program().to_string_lossy().contains('/'));
    }
}
