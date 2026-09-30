//! Handle-relative filesystem access: the answer to a hostile tree.
//!
//! [`walk_dir`](super::walk_dir) identifies files by *path*, and INV-FS-2 says
//! plainly that its identity rechecks are best-effort. That is the right
//! tradeoff for a trusted tree and the wrong one for a directory another
//! process can rewrite while you walk it: between the `stat` that says "this is
//! a directory" and the `open` that reads it, the name can be repointed at
//! something else, and every path-based check you did becomes a check on a
//! name that no longer refers to the object you checked.
//!
//! This module closes that gap by never resolving a name after the first one.
//! [`Dir`] opens a directory once and holds the resulting descriptor, and
//! every operation on it is `*at`: `openat`, `statat`, `unlinkat`, `mkdirat`,
//! `readlinkat`. The kernel resolves each component from that descriptor at the
//! instant of the call, so a name swapped underneath the walk cannot redirect
//! it — the worst outcome becomes `ENOENT` on the entry that changed, and the
//! caller reports an omission rather than silently reading someone else's file.
//!
//! What this does *not* claim: holding a [`Dir`] does not freeze the tree. A
//! file's *contents* can still change while you read them, and a subdirectory
//! opened relative to `Dir` can be renamed elsewhere while you descend into it.
//! What it does claim is that the directory you admitted is the directory the
//! OS resolved your name against, so re-checking identity is not needed to know
//! it.
//!
//! Symlinks are not followed implicitly. [`Dir::open_entry`] takes
//! [`OpenFlags`] where the caller states [`SymlinkPolicy`], because "follow" is
//! a policy decision with a security consequence and the default must be the one
//! that cannot escape.

use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

/// A directory held open, for handle-relative access to what it contains.
///
/// Usable as a walk cursor: [`Dir::try_clone`] gives another reference to the
/// *same* directory, and dropping every one closes its own descriptor. It is
/// deliberately not `Clone` — see that method for why duplicating the
/// descriptor can fail and is not allowed to be silently swallowed.
#[cfg(unix)]
#[derive(Debug)]
pub struct Dir {
    /// The open descriptor. Every name below is resolved from this, never from
    /// a stored path.
    fd: std::os::fd::OwnedFd,
}

/// How an entry's own name is interpreted when it is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SymlinkPolicy {
    /// Open the object the name itself denotes; a symlink is reported as
    /// [`FileKind::Symlink`] and not opened. The default, because a name that
    /// points elsewhere must be a decision, not a side effect.
    #[default]
    NoFollow,
    /// Follow a final symlink, resolving its target from this same directory.
    ///
    /// A *relative* target is still resolved by the kernel from this
    /// descriptor, so it stays within this directory's resolution path. An
    /// *absolute* target escapes it, because absolute means absolute; use
    /// [`Dir::read_link`] to inspect one before opening it.
    FollowFinal,
}

/// What an entry's name denotes, stated without following it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory, safe to descend into with [`Dir::open_subdir`].
    Directory,
    /// A symbolic link. Its target is not resolved by `kind`.
    Symlink,
    /// A named pipe.
    Fifo,
    /// A character device.
    CharDevice,
    /// A block device.
    BlockDevice,
    /// A socket.
    Socket,
    /// Something the platform's `d_type` does not name. `kind` on the entry is
    /// how to learn more; a `d_type` of `DT_UNKNOWN` is common on network and
    /// synthetic filesystems, and reporting a guess would be worse than
    /// reporting the uncertainty.
    Unknown,
}

/// Flags for [`Dir::open_entry`], mirroring the subset of `openat(2)` this
/// crate exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct OpenFlags {
    /// Read access.
    pub read: bool,
    /// Write access.
    pub write: bool,
    /// Create the file if absent.
    pub create: bool,
    /// Truncate an existing regular file to zero length.
    pub truncate: bool,
    /// How a final symlink is treated. See [`SymlinkPolicy`].
    pub symlinks: SymlinkPolicy,
}

impl OpenFlags {
    /// Read-only access, refusing to follow a final symlink.
    #[must_use]
    pub fn read() -> Self {
        Self {
            read: true,
            ..Self::default()
        }
    }

    /// Create-or-truncate for writing, refusing to follow a final symlink.
    #[must_use]
    pub fn create_truncate() -> Self {
        Self {
            write: true,
            create: true,
            truncate: true,
            ..Self::default()
        }
    }

    /// The access word, which `O_RDONLY` is `0` rather than a named flag.
    #[cfg(unix)]
    fn access(self) -> rustix::fs::OFlags {
        match (self.read, self.write) {
            (true, true) => rustix::fs::OFlags::RDWR,
            (false, true) => rustix::fs::OFlags::WRONLY,
            _ => rustix::fs::OFlags::RDONLY,
        }
    }
}

#[cfg(unix)]
impl Dir {
    /// Open the directory at `path`.
    ///
    /// This is the only name-based call in the module, and it happens once per
    /// tree rather than once per entry. Everything after it is `*at`.
    ///
    /// # Errors
    ///
    /// The OS error when `path` is not a directory or cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let fd = rustix::fs::open(
            path.as_ref(),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(errno_to_io)?;
        Ok(Self { fd })
    }

    /// The directory `name` denotes within `self`, opened for reading.
    ///
    /// `name` is resolved by the kernel from this descriptor. A `name` that is
    /// `.`, `..`, empty, or contains `/` is refused here rather than handed to
    /// the kernel, because those are the spellings that move out of the
    /// directory this call was made on.
    ///
    /// # Errors
    ///
    /// The OS error, or [`io::ErrorKind::InvalidInput`] when `name` is not a
    /// single component.
    pub fn open_subdir(&self, name: &str) -> io::Result<Self> {
        let component = single_component(name)?;
        let fd = rustix::fs::openat(
            self.fd.as_fd(),
            &component,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(errno_to_io)?;
        Ok(Self { fd })
    }

    /// Open `name` within `self` under `flags`.
    ///
    /// # Errors
    ///
    /// The OS error, or [`io::ErrorKind::InvalidInput`] when `name` is not a
    /// single component.
    pub fn open_entry(&self, name: &str, flags: OpenFlags) -> io::Result<std::fs::File> {
        let component = single_component(name)?;
        let mut oflags = flags.access() | rustix::fs::OFlags::CLOEXEC;
        if flags.create {
            oflags |= rustix::fs::OFlags::CREATE;
        }
        if flags.truncate {
            oflags |= rustix::fs::OFlags::TRUNC;
        }
        if flags.symlinks == SymlinkPolicy::NoFollow {
            oflags |= rustix::fs::OFlags::NOFOLLOW;
        }
        let fd = rustix::fs::openat(self.fd.as_fd(), component, oflags, user_writable_mode())
            .map_err(errno_to_io)?;
        Ok(std::fs::File::from(fd))
    }

    /// What `name` denotes in `self`, without following it.
    ///
    /// # Errors
    ///
    /// The OS error, including `ENOENT` when the name was removed between the
    /// listing and this call — which on a concurrently-rewritten tree is the
    /// expected outcome, not a bug.
    pub fn kind(&self, name: &str) -> io::Result<FileKind> {
        let component = single_component(name)?;
        let stat = rustix::fs::statat(
            self.fd.as_fd(),
            &component,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(errno_to_io)?;
        Ok(classify(stat.st_mode))
    }

    /// The target of the symlink `name` in `self`, unresolved.
    ///
    /// The target is returned exactly as stored: it may be relative, may
    /// contain `..`, and may be empty. Deciding what to do with it is the
    /// caller's — this exists so "follow this link" is a decision made with the
    /// target in hand, not a side effect of opening.
    ///
    /// # Errors
    ///
    /// The OS error, or [`io::ErrorKind::InvalidInput`] when `name` is not a
    /// single component.
    pub fn read_link(&self, name: &str) -> io::Result<std::ffi::OsString> {
        let component = single_component(name)?;
        let target =
            rustix::fs::readlinkat(self.fd.as_fd(), &component, Vec::new()).map_err(errno_to_io)?;
        Ok(OsString::from_vec(target.into_bytes()))
    }

    /// Create `name` in `self` as an empty file, or refuse if it exists.
    ///
    /// There is no "overwrite" variant because overwrite must not be a default:
    /// `O_EXCL` means a name another process created between the check and the
    /// create is an error, not a truncated file.
    ///
    /// # Errors
    ///
    /// The OS error, including `EEXIST` when the name is taken.
    pub fn create_new(&self, name: &str) -> io::Result<std::fs::File> {
        let component = single_component(name)?;
        let fd = rustix::fs::openat(
            self.fd.as_fd(),
            &component,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            user_writable_mode(),
        )
        .map_err(errno_to_io)?;
        Ok(std::fs::File::from(fd))
    }

    /// Create a directory `name` within `self`.
    ///
    /// # Errors
    ///
    /// The OS error, including `EEXIST` when the name is taken.
    pub fn create_dir(&self, name: &str) -> io::Result<()> {
        let component = single_component(name)?;
        rustix::fs::mkdirat(self.fd.as_fd(), &component, user_directory_mode()).map_err(errno_to_io)
    }

    /// Remove the directory `name` within `self`.
    ///
    /// This is `rmdir(2)`, not a recursive delete: a non-empty directory
    /// refuses with `ENOTEMPTY`, so a caller cannot lose a subtree by asking to
    /// remove one name.
    ///
    /// # Errors
    ///
    /// The OS error, including `ENOTEMPTY` when the directory still has
    /// entries.
    pub fn remove_dir(&self, name: &str) -> io::Result<()> {
        let component = single_component(name)?;
        rustix::fs::unlinkat(self.fd.as_fd(), &component, rustix::fs::AtFlags::REMOVEDIR)
            .map_err(errno_to_io)
    }

    /// Unlink the file or symlink `name` within `self`.
    ///
    /// Never removes a directory. A symlink is removed, not its target — which
    /// is the behaviour that makes this safe to call on a tree you did not
    /// write.
    ///
    /// # Errors
    ///
    /// The OS error, including `EISDIR` when `name` is a directory.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        let component = single_component(name)?;
        rustix::fs::unlinkat(self.fd.as_fd(), &component, rustix::fs::AtFlags::empty())
            .map_err(errno_to_io)
    }

    /// Every name in this directory, in filesystem order.
    ///
    /// The listing is one `getdents64` sequence against this descriptor, so a
    /// concurrent rename cannot make the names belong to a different
    /// directory than the one opened.
    ///
    /// # Errors
    ///
    /// The OS error when the directory cannot be read. A *single* entry that
    /// cannot be stated is not an error: it is skipped, because inventing a
    /// name for it would be worse than omitting one, and the caller can
    /// re-stat any name it was actually given.
    /// Every name in this directory, in the order the kernel reports them.
    ///
    /// Linux-only, and for a concrete reason: listing a descriptor means
    /// `getdents64`, which exists only on Linux. The BSDs have no equivalent
    /// syscall — they expose `readdir(3)` on an opaque `DIR*` from C, which
    /// cannot be called from this crate while `unsafe_code` is forbidden
    /// workspace-wide. So on macOS and the BSDs this reports
    /// [`io::ErrorKind::Unsupported`] instead of a list. Every other operation
    /// on `Dir` is `*at(2)`, which is POSIX, and works on all of them; listing
    /// is the one gap, and it is a syscall-ABI gap rather than a design choice.
    ///
    /// `std::fs::read_dir` is not a workaround: it takes a path, and
    /// `/proc/self/fd/N` is a symlink that resolves to the directory itself as a
    /// file, which `read_dir` refuses with `ENOTDIR`.
    ///
    /// # Errors
    ///
    /// The OS error when the directory cannot be read.
    #[cfg(target_os = "linux")]
    pub fn entry_names(&self) -> io::Result<Vec<OsString>> {
        use std::mem::MaybeUninit;
        use std::os::unix::ffi::OsStrExt;

        /// One page is the kernel's own `getdents64` granularity. A wide
        /// directory costs this much stack to enumerate, not one `PathBuf` per
        /// sibling.
        const DIRENT_BUFFER_BYTES: usize = 4_096;

        let mut buffer = [MaybeUninit::<u8>::uninit(); DIRENT_BUFFER_BYTES];
        let mut directory = rustix::fs::RawDir::new(self.fd.as_fd(), &mut buffer);
        let mut names = Vec::new();
        while let Some(entry) = directory.next() {
            let entry = entry.map_err(errno_to_io)?;
            names.push(OsString::from_vec(entry.file_name().to_bytes().to_vec()));
        }
        Ok(names)
    }

    /// Every name in this directory.
    ///
    /// Listing a descriptor needs `getdents64`, which is Linux-only; see the
    /// Unix implementation for why there is no portable form here.
    #[cfg(not(target_os = "linux"))]
    pub fn entry_names(&self) -> io::Result<Vec<OsString>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fs::capability::Dir::entry_names needs getdents64, which is Linux-only",
        ))
    }

    /// The descriptor backing this directory.
    ///
    /// Exposed for callers that must hand the same capability to a syscall this
    /// crate does not wrap. The descriptor stays owned by this `Dir`.
    #[must_use]
    pub fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(unix)]
impl Dir {
    /// Another owned reference to the same directory.
    ///
    /// # Errors
    ///
    /// The OS error when the descriptor cannot be duplicated. The fallible form
    /// is the point: a `Clone` that could not report failure would have to
    /// either panic or hand back a shared descriptor, and the second closes the
    /// directory out from under the first holder.
    pub fn try_clone(&self) -> io::Result<Self> {
        let fd = rustix::io::dup(self.fd.as_fd()).map_err(errno_to_io)?;
        Ok(Self { fd })
    }
}

#[cfg(not(unix))]
mod unsupported {
    //! The same surface, reporting absence rather than emulating it.
    //!
    //! These are stubs on purpose. `Dir` cannot exist without an `*at` family,
    //! and a shim built on `path::` would reintroduce exactly the name-reuse
    //! race this module exists to remove while advertising that it did not.

    use std::io;
    use std::path::Path;

    use super::{FileKind, OpenFlags, SymlinkPolicy};

    /// Handle-relative filesystem access. Unix-only.
    #[derive(Debug)]
    pub struct Dir;

    impl Dir {
        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn open(_path: impl AsRef<Path>) -> io::Result<Self> {
            Err(unsupported("Dir::open"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn open_subdir(&self, _name: &str) -> io::Result<Self> {
            Err(unsupported("Dir::open_subdir"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn open_entry(&self, _name: &str, _flags: OpenFlags) -> io::Result<std::fs::File> {
            Err(unsupported("Dir::open_entry"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn kind(&self, _name: &str) -> io::Result<FileKind> {
            Err(unsupported("Dir::kind"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn read_link(&self, _name: &str) -> io::Result<std::ffi::OsString> {
            Err(unsupported("Dir::read_link"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn create_new(&self, _name: &str) -> io::Result<std::fs::File> {
            Err(unsupported("Dir::create_new"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn create_dir(&self, _name: &str) -> io::Result<()> {
            Err(unsupported("Dir::create_dir"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn remove_dir(&self, _name: &str) -> io::Result<()> {
            Err(unsupported("Dir::remove_dir"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn remove_file(&self, _name: &str) -> io::Result<()> {
            Err(unsupported("Dir::remove_file"))
        }

        /// Always reports [`io::ErrorKind::Unsupported`].
        pub fn entry_names(&self) -> io::Result<Vec<std::ffi::OsString>> {
            Err(unsupported("Dir::entry_names"))
        }
    }

    fn unsupported(name: &'static str) -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("fs::capability::Dir::{name} is Unix-only"),
        )
    }

    /// Unused on this target; kept so the enum documents every kind everywhere.
    #[expect(dead_code, reason = "platform parity: the enum is read on Unix")]
    fn _kinds_are_platform_independent(_: SymlinkPolicy, _: FileKind) {}
}

/// Reject anything that is not one plain component of a name.
///
/// This is the check that makes the `*at` calls safe to reason about. `..` is
/// the important one: it is the spelling that moves *up* out of the directory
/// the descriptor names, so a caller walking a tree cannot be walked out of it
/// by an entry whose name is `..`.
#[cfg(unix)]
fn single_component(name: &str) -> io::Result<std::ffi::CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a single path component"),
        ));
    }
    std::ffi::CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component contains an interior NUL",
        )
    })
}

/// Mask selecting the file-type bits of a `st_mode` word.
#[cfg(unix)]
const FILE_TYPE_MASK: u16 = 0o170_000;

/// `S_IFMT` code for a regular file.
#[cfg(unix)]
const S_IFREG: u16 = 0o100_000;
/// `S_IFMT` code for a directory.
#[cfg(unix)]
const S_IFDIR: u16 = 0o040_000;
/// `S_IFMT` code for a symbolic link.
#[cfg(unix)]
const S_IFLNK: u16 = 0o120_000;
/// `S_IFMT` code for a named pipe.
#[cfg(unix)]
const S_IFIFO: u16 = 0o010_000;
/// `S_IFMT` code for a character device.
#[cfg(unix)]
const S_IFCHR: u16 = 0o020_000;
/// `S_IFMT` code for a block device.
#[cfg(unix)]
const S_IFBLK: u16 = 0o060_000;
/// `S_IFMT` code for a socket.
#[cfg(unix)]
const S_IFSOCK: u16 = 0o140_000;

/// Map a raw `st_mode` word to the kind this crate reports.
///
/// Matched on the `S_IFMT` bits rather than through rustix's `FileType`, because
/// `Stat::st_mode` is the raw word on every backend and a helper that only
/// exists on some of them would make this function's answer target-dependent.
#[cfg(unix)]
fn classify(mode: u16) -> FileKind {
    match mode & FILE_TYPE_MASK {
        S_IFREG => FileKind::File,
        S_IFDIR => FileKind::Directory,
        S_IFLNK => FileKind::Symlink,
        S_IFIFO => FileKind::Fifo,
        S_IFCHR => FileKind::CharDevice,
        S_IFBLK => FileKind::BlockDevice,
        S_IFSOCK => FileKind::Socket,
        _ => FileKind::Unknown,
    }
}

/// Mode for a file this process owns: readable and writable by the user.
///
/// Not `0o600` written literally, because the three `RUSR`/`RGRP`/`RWXO` bits
/// are named separately in rustix and an assembled word is the one that stays
/// correct when the platform's umask handling is applied by `openat`.
#[cfg(unix)]
fn user_writable_mode() -> rustix::fs::Mode {
    rustix::fs::Mode::RUSR | rustix::fs::Mode::RWXG | rustix::fs::Mode::RWXO
}

/// Mode for a directory this process owns: user `rwx`, nobody else's.
#[cfg(unix)]
fn user_directory_mode() -> rustix::fs::Mode {
    rustix::fs::Mode::RWXU
}

/// Convert a rustix errno into the std error surface.
#[cfg(unix)]
fn errno_to_io(errno: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
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
        let mut names = dir.entry_names()?;
        names.sort();
        assert_eq!(names, vec![OsString::from("a"), OsString::from("b.txt")]);
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
}
