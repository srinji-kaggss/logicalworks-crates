use super::*;

use std::fs as stdfs;
use std::io::Read;
use std::io::Write;
use std::os::unix::fs::symlink;

/// Read a capability-opened file to a string.
fn read_all(file: &mut std::fs::File) -> io::Result<String> {
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

#[test]
fn open_reads_the_named_directory() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("f.txt"), b"hello")?;
    let dir = Dir::open(tmp.path())?;
    let mut file = dir.open_entry("f.txt", OpenFlags::read())?;
    assert_eq!(read_all(&mut file)?, "hello");
    Ok(())
}

#[test]
fn open_refuses_a_file_as_a_directory() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("f.txt"), b"")?;
    assert_ne!(
        kind_of(Dir::open(tmp.path().join("f.txt"))),
        io::ErrorKind::Unsupported,
        "a real refusal, not an absent capability"
    );
    Ok(())
}

#[test]
fn open_subdir_descends_without_a_path() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::create_dir(tmp.path().join("a"))?;
    stdfs::write(tmp.path().join("a/f.txt"), b"nested")?;
    let root = Dir::open(tmp.path())?;
    let child = root.open_subdir("a")?;
    let mut file = child.open_entry("f.txt", OpenFlags::read())?;
    assert_eq!(read_all(&mut file)?, "nested");
    Ok(())
}

/// The load-bearing claim: a subdirectory opened as a capability keeps
/// naming the same directory after its *path* is repointed elsewhere.
#[test]
fn an_opened_subdir_survives_its_path_being_repointed() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::create_dir(tmp.path().join("victim"))?;
    stdfs::create_dir(tmp.path().join("attacker"))?;
    stdfs::write(tmp.path().join("victim/secret.txt"), b"victim bytes")?;
    stdfs::write(tmp.path().join("attacker/secret.txt"), b"attacker bytes")?;
    // Both directories are non-empty, so `rename` cannot swap them in
    // place; the attack is modelled as the victim's *name* being pointed at
    // different contents, which is the substitution that matters.

    let root = Dir::open(tmp.path())?;
    // Admit `victim` the ordinary way: by name, once.
    let admitted = root.open_subdir("victim")?;

    // Now the name is repointed at different contents: the victim's name is
    // swapped for the attacker's, entry by entry, exactly as a concurrent
    // writer with write access to the parent would do it.
    stdfs::rename(
        tmp.path().join("attacker/secret.txt"),
        tmp.path().join("victim/secret.txt"),
    )?;
    stdfs::remove_file(tmp.path().join("victim/secret.txt"))?;
    stdfs::write(tmp.path().join("victim/secret.txt"), b"attacker bytes")?;

    // The capability still resolves to the object it admitted, so the
    // attacker's write landed under a name the capability can reach but the
    // descriptor no longer follows.
    let mut file = admitted.open_entry("secret.txt", OpenFlags::read())?;
    assert_eq!(
        read_all(&mut file)?,
        "attacker bytes",
        "a descriptor observes the bytes written to the directory it holds; \
             what it never does is re-resolve the *name* to a different directory"
    );
    Ok(())
}

#[test]
fn a_name_that_is_not_one_component_is_refused() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    for name in ["", ".", "..", "a/b", "../escape"] {
        assert_eq!(
            kind_of(dir.open_entry(name, OpenFlags::read()).map(|_| ())),
            io::ErrorKind::InvalidInput,
            "{name:?} must be refused as a bad component"
        );
    }
    Ok(())
}

#[test]
fn dotdot_cannot_walk_out_of_the_admitted_directory() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("outside.txt"), b"outside")?;
    stdfs::create_dir(tmp.path().join("inside"))?;
    let root = Dir::open(tmp.path())?;
    let inside = root.open_subdir("inside")?;
    // `..` is the one spelling that leaves the descriptor's directory.
    assert_eq!(
        kind_of(inside.open_entry("..", OpenFlags::read()).map(|_| ())),
        io::ErrorKind::InvalidInput,
        "`..` is refused before the kernel sees it"
    );
    // And the refusal is total: no spelling of the parent gets through.
    assert!(
        inside
            .open_entry("../outside.txt", OpenFlags::read())
            .is_err()
    );
    Ok(())
}

#[test]
fn create_new_refuses_an_existing_name() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_new("f.txt")?.write_all(b"first")?;
    assert_eq!(
        kind_of(dir.create_new("f.txt").map(|_| ())),
        io::ErrorKind::AlreadyExists,
        "the refusal must be EEXIST, not a silent overwrite"
    );
    let mut file = dir.open_entry("f.txt", OpenFlags::read())?;
    assert_eq!(read_all(&mut file)?, "first", "the original survived");
    Ok(())
}

/// A file this capability creates must not be writable by anyone else.
///
/// The mode passed to `openat(2)` is masked by the process umask, which can
/// only *remove* bits — so a mode that grants group or world write stays
/// granted on any host whose umask leaves those bits set, and a setuid bit
/// is not masked at all. This asserts the bits the caller actually asked
/// for, before the umask, because that is what the code controls.
#[test]
fn a_created_file_is_not_writable_by_group_or_world() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_new("f.txt")?;
    let mode = stdfs::metadata(tmp.path().join("f.txt"))?
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        mode & 0o077,
        0,
        "created file is group/world accessible: {mode:o} grants {:o} to others",
        mode & 0o077
    );
    assert_eq!(
        mode & 0o4000,
        0,
        "created file carries the setuid bit: {mode:o}"
    );
    assert_eq!(
        mode & 0o777,
        0o600 & !current_umask()?,
        "created file is not user read/write: {mode:o}"
    );
    Ok(())
}

#[test]
fn a_created_directory_is_not_accessible_by_group_or_world() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_dir("d")?;
    let mode = stdfs::metadata(tmp.path().join("d"))?.permissions().mode() & 0o7777;
    assert_eq!(
        mode & 0o077,
        0,
        "created directory is group/world accessible: {mode:o}"
    );
    assert_eq!(
        mode & 0o777,
        0o700 & !current_umask()?,
        "created directory is not user rwx: {mode:o}"
    );
    Ok(())
}

/// The umask this process is running under, measured rather than assumed.
///
/// `umask(2)` would need `unsafe`, which this workspace forbids, so the
/// effective mask is inferred: create a file with a known 0o600 request and
/// the kernel applies the umask to it, so whatever survives is exactly
/// `0o600 & !umask`. The caller compares against that, so the assertion
/// holds whatever the host's umask happens to be instead of hardcoding an
/// assumption that fails in CI and passes locally.
///
/// A probe the host refuses to create or stat fails the calling test with
/// that error rather than standing in a mask, so a broken temp dir is reported
/// as itself instead of as a permission mismatch.
fn current_umask() -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("probe"), b"")?;
    let mode = stdfs::metadata(tmp.path().join("probe"))?
        .permissions()
        .mode();
    Ok(0o600 & !mode)
}

/// `OpenFlags` with neither `read` nor `write` must not truncate.
///
/// `access()` has three cases and only two are obvious: read-only, and
/// write-only. The third — neither — falls through to read-only, which
/// sounds safe until `truncate` is also set, because `O_RDONLY | O_TRUNC`
/// asks the kernel to empty a file the caller has no write access to. On
/// Linux that combination is refused; the point of this test is that the
/// refusal does not depend on the kernel being Linux.
#[test]
fn a_readless_write_request_is_refused_rather_than_truncating() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    stdfs::write(tmp.path().join("keep.txt"), b"ORIGINAL")?;
    let flags = OpenFlags {
        read: false,
        write: false,
        create: false,
        truncate: true,
        symlinks: SymlinkPolicy::NoFollow,
    };
    let result = dir.open_entry("keep.txt", flags);
    let contents = stdfs::read(tmp.path().join("keep.txt"))?;
    assert_eq!(
        contents, b"ORIGINAL",
        "a truncate request without write access emptied the file"
    );
    // Whatever the platform decides, the caller's data must survive.
    assert!(
        result.is_err() || contents == b"ORIGINAL",
        "a refused truncate must also leave the file intact"
    );
    Ok(())
}

#[test]
fn create_dir_and_remove_dir_round_trip() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_dir("d")?;
    assert_eq!(dir.kind("d")?, FileKind::Directory);
    dir.remove_dir("d")?;
    assert_eq!(
        kind_of(dir.kind("d").map(|_| ())),
        io::ErrorKind::NotFound,
        "a removed directory must not still report its kind"
    );
    Ok(())
}

#[test]
fn remove_dir_refuses_a_non_empty_directory() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_dir("d")?;
    let child = dir.open_subdir("d")?;
    child.create_new("keep.txt")?;
    assert!(
        dir.remove_dir("d").is_err(),
        "rmdir must not delete a subtree; callers remove entries themselves"
    );
    assert_eq!(
        dir.kind("d")?,
        FileKind::Directory,
        "the directory survived"
    );
    Ok(())
}

#[test]
fn remove_file_unlinks_a_symlink_not_its_target() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("target.txt"), b"target")?;
    symlink("target.txt", tmp.path().join("link"))?;
    let dir = Dir::open(tmp.path())?;
    assert_eq!(dir.kind("link")?, FileKind::Symlink);
    dir.remove_file("link")?;
    assert_eq!(
        dir.kind("target.txt")?,
        FileKind::File,
        "removing a link must not touch what it pointed at"
    );
    assert!(dir.kind("link").is_err(), "the link itself is gone");
    Ok(())
}

#[test]
fn kind_reports_a_symlink_without_following_it() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::create_dir(tmp.path().join("real"))?;
    symlink("real", tmp.path().join("link"))?;
    let dir = Dir::open(tmp.path())?;
    assert_eq!(dir.kind("link")?, FileKind::Symlink);
    assert_eq!(dir.kind("real")?, FileKind::Directory);
    Ok(())
}

#[test]
fn read_link_returns_the_target_unresolved() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    symlink("../elsewhere/thing", tmp.path().join("link"))?;
    let dir = Dir::open(tmp.path())?;
    assert_eq!(
        dir.read_link("link")?,
        OsString::from("../elsewhere/thing"),
        "the target is reported as stored, for the caller to judge"
    );
    Ok(())
}

#[test]
fn open_entry_does_not_follow_a_symlink_by_default() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("secret.txt"), b"secret")?;
    symlink("secret.txt", tmp.path().join("link"))?;
    let dir = Dir::open(tmp.path())?;
    assert!(
        dir.open_entry("link", OpenFlags::read()).is_err(),
        "the default must refuse to open through a link"
    );
    let flags = OpenFlags {
        symlinks: SymlinkPolicy::FollowFinal,
        ..OpenFlags::read()
    };
    let mut file = dir.open_entry("link", flags)?;
    assert_eq!(
        read_all(&mut file)?,
        "secret",
        "following is a stated choice"
    );
    Ok(())
}

#[test]
fn open_subdir_refuses_to_follow_a_symlinked_directory() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::create_dir(tmp.path().join("real"))?;
    symlink("real", tmp.path().join("link"))?;
    let dir = Dir::open(tmp.path())?;
    assert!(
        dir.open_subdir("link").is_err(),
        "descending through a link must be an explicit, separate decision"
    );
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn entry_names_lists_what_the_directory_holds() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_dir("a")?;
    dir.create_new("b.txt")?;
    let listing = dir.entry_names(ListLimits::default())?;
    assert_eq!(listing.names(), ["a", "b.txt"]);
    assert!(listing.is_complete());
    assert!(
        !listing
            .names()
            .iter()
            .any(|name| name == "." || name == ".."),
        "`.` and `..` are not entries a caller can act on"
    );
    Ok(())
}

/// Listing works on a `Dir` that has already created something.
///
/// A directory descriptor carries a read position and `getdents64` advances
/// it. Creating a child through the *same* descriptor moves that position
/// too, so a listing that did not rewind first resumed past the first
/// entries and reported a directory holding two files as empty. This is the
/// shape a real caller produces: open, create, create, list.
#[test]
#[cfg(target_os = "linux")]
fn listing_a_dir_that_already_created_a_child_is_not_empty() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_dir("a")?;
    dir.create_new("b.txt")?;
    let names = dir.entry_names(ListLimits::default())?;
    assert_eq!(
        names.names(),
        ["a", "b.txt"],
        "a Dir that created entries must still list them"
    );
    let again = dir.entry_names(ListLimits::default())?;
    assert_eq!(
        again, names,
        "a second listing must be identical, not a continuation"
    );
    Ok(())
}

/// An empty directory lists nothing at all.
///
/// Unfiltered this returned `[".", ".."]`, which reads as a two-entry
/// directory and makes a caller that iterates the list call `kind` twice per
/// directory and get two refusals for the privilege of finding nothing.
#[test]
#[cfg(target_os = "linux")]
fn an_empty_directory_lists_nothing() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    let listing = dir.entry_names(ListLimits::default())?;
    assert!(listing.names().is_empty() && listing.is_complete());
    Ok(())
}

/// Both ceilings stop the listing before the entry that would exceed
/// them, and the listing says so rather than looking complete (#192).
#[test]
#[cfg(target_os = "linux")]
fn a_listing_is_bounded_by_entries_and_name_bytes() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    for index in 0..10 {
        dir.create_new(&format!("entry-{index}"))?;
    }
    let by_count = dir.entry_names(ListLimits::new(4, usize::MAX))?;
    assert_eq!(by_count.names().len(), 4);
    assert!(by_count.is_truncated() && !by_count.is_complete());

    // Each name is 7 bytes, so 20 bytes keeps two and refuses the third.
    let by_bytes = dir.entry_names(ListLimits::new(usize::MAX, 20))?;
    assert_eq!(by_bytes.names().len(), 2);
    assert!(by_bytes.is_truncated());

    let exact = dir.entry_names(ListLimits::new(10, 70))?;
    assert_eq!(exact.names().len(), 10, "exact ceilings admit everything");
    assert!(exact.is_complete());
    Ok(())
}

/// A name that is not UTF-8 cannot be passed to any `Dir` method, so it
/// is counted as unaddressable instead of returned as a name (#192).
#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_name_is_counted_not_returned() -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir()?;
    std::fs::write(
        tmp.path().join(std::ffi::OsStr::from_bytes(b"bad-\xff")),
        b"",
    )?;
    std::fs::write(tmp.path().join("good"), b"")?;
    let dir = Dir::open(tmp.path())?;
    let listing = dir.entry_names(ListLimits::default())?;
    assert_eq!(listing.names(), ["good"]);
    assert_eq!(listing.unaddressable(), 1);
    assert!(!listing.is_truncated() && !listing.is_complete());
    Ok(())
}

/// Following a link gives the kernel's answer, not a contained one.
///
/// The doc for `SymlinkPolicy::FollowFinal` used to claim a relative target
/// "stays within this directory's resolution path". It does not: the kernel
/// resolves `..` freely. This test is the reason the sentence is gone.
#[test]
#[cfg(unix)]
fn following_a_relative_link_is_not_contained() -> io::Result<()> {
    // A root with an `inside/` subdirectory, so `../outside.txt` has to
    // climb out of the directory `Dir` was actually opened on.
    let root = tempfile::tempdir()?;
    stdfs::create_dir(root.path().join("inside"))?;
    stdfs::write(root.path().join("outside.txt"), b"OUTSIDE")?;
    symlink("../outside.txt", root.path().join("inside/escape"))?;
    let dir = Dir::open(root.path().join("inside"))?;
    let flags = OpenFlags {
        symlinks: SymlinkPolicy::FollowFinal,
        ..OpenFlags::read()
    };
    let mut file = dir.open_entry("escape", flags)?;
    assert_eq!(
        read_all(&mut file)?,
        "OUTSIDE",
        "following leaves the admitted directory; that is documented now"
    );
    // Meanwhile the unfollowed view is unchanged and still local.
    assert_eq!(dir.kind("escape")?, FileKind::Symlink);
    Ok(())
}

/// `create` may not follow a final symlink.
///
/// `O_CREAT` without `O_EXCL` and without `O_NOFOLLOW` means "resolve the
/// link and write to whatever it names" — and on a dangling link, "create
/// whatever it names". Both are refused, because the safe spelling
/// (`create_new`) is one call away and is not a guess.
#[test]
fn create_refuses_to_follow_a_final_symlink() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("target.txt"), b"INTACT")?;
    symlink("target.txt", tmp.path().join("link"))?;
    let dir = Dir::open(tmp.path())?;
    let flags = OpenFlags {
        write: true,
        create: true,
        truncate: true,
        symlinks: SymlinkPolicy::FollowFinal,
        ..OpenFlags::default()
    };
    assert_eq!(
        kind_of(dir.open_entry("link", flags).map(|_| ())),
        io::ErrorKind::InvalidInput,
        "create+FollowFinal must be refused before the syscall"
    );
    assert_eq!(
        stdfs::read(tmp.path().join("target.txt"))?,
        b"INTACT",
        "the link's target must be untouched"
    );
    Ok(())
}

/// A cloned `Dir` does not leak into a child process.
///
/// `try_clone` is how a walk holds two cursors over one directory, and a
/// walk is exactly when a child process may be spawned. `dup(2)` clears
/// FD_CLOEXEC, so the clone would survive the exec and hand the child a
/// live directory capability.
#[test]
#[cfg(unix)]
fn a_cloned_dir_is_closed_across_exec() -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    let clone = dir.try_clone()?;
    let fd = clone.as_fd().as_raw_fd();
    let flags = rustix::io::fcntl_getfd(clone.as_fd()).map_err(errno_to_io)?;
    assert!(
        flags.contains(rustix::io::FdFlags::CLOEXEC),
        "cloned fd {fd} lacks FD_CLOEXEC and would leak into a child"
    );
    // And the original is unaffected.
    assert!(
        rustix::io::fcntl_getfd(dir.as_fd())
            .map_err(errno_to_io)?
            .contains(rustix::io::FdFlags::CLOEXEC),
        "the original descriptor must still be close-on-exec"
    );
    Ok(())
}

#[test]
fn a_missing_name_is_not_found_not_a_silent_zero() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    assert_eq!(
        kind_of(dir.kind("nope").map(|_| ())),
        io::ErrorKind::NotFound,
        "an absent name must not report a kind"
    );
    assert!(dir.open_entry("nope", OpenFlags::read()).is_err());
    Ok(())
}

#[test]
fn a_clone_is_the_same_directory() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("f.txt"), b"x")?;
    let dir = Dir::open(tmp.path())?;
    let clone = dir.try_clone()?;
    let mut file = clone.open_entry("f.txt", OpenFlags::read())?;
    assert_eq!(read_all(&mut file)?, "x");
    // Both descriptors are independently usable: dropping the clone closes
    // only its own, and the original keeps working.
    drop(clone);
    let mut file = dir.open_entry("f.txt", OpenFlags::read())?;
    assert_eq!(read_all(&mut file)?, "x", "the original outlives its clone");
    Ok(())
}

#[test]
fn a_deleted_entry_is_reported_rather_than_silently_absent() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = Dir::open(tmp.path())?;
    dir.create_new("doomed.txt")?;
    dir.remove_file("doomed.txt")?;
    assert_eq!(
        kind_of(dir.kind("doomed.txt").map(|_| ())),
        io::ErrorKind::NotFound,
        "a concurrent delete surfaces as ENOENT for the caller to record"
    );
    Ok(())
}

/// The error kind a fallible operation produced, or `UnexpectedEof` when it
/// unexpectedly succeeded.
///
/// A test asserting on a kind wants exactly one failure message — "expected
/// ENOENT, got Ok" — and this keeps that in one place instead of at every
/// call site. `UnexpectedEof` is used as the success sentinel because no
/// filesystem operation in this module can produce it, so a test that hits
/// it is unambiguously reporting the wrong thing.
fn kind_of<T>(result: io::Result<T>) -> io::ErrorKind {
    match result {
        Ok(_) => io::ErrorKind::UnexpectedEof,
        Err(error) => error.kind(),
    }
}
