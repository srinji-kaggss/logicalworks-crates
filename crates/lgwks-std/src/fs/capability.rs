//! Handle-relative filesystem access: the answer to a hostile tree.
//!
//! [`crate::fs::walk_dir`] identifies files by *path*, and INV-FS-2 says
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
//!
//! [`Dir`]: struct.Dir
//! [`Dir::open_entry`]: struct.Dir.html#method.open_entry
//! [`OpenFlags`]: struct.OpenFlags.html
//! [`SymlinkPolicy`]: enum.SymlinkPolicy.html

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::path::Path;

// The same surface on every target: elsewhere each method reports
// `Unsupported`, which is what the module and `fs` docs promise. Without this
// re-export the stub was compiled and unreachable, so `--features fs-raw` on a
// non-Unix target offered no `Dir` at all.
#[cfg(not(unix))]
pub use unsupported::Dir;

/// A directory held open, for handle-relative access to what it contains.
///
/// Usable as a walk cursor: [`self::Dir::try_clone`] gives another reference to the
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
    /// Follow a final symlink, exactly as the kernel would.
    ///
    /// This is **containment of no kind**. The kernel resolves a relative
    /// target against the link's own directory and then walks `..` freely, so
    /// `../outside` and `../../elsewhere/outside` both leave the directory this
    /// `Dir` was opened on; an absolute target leaves it trivially. An earlier
    /// version of this doc claimed relative targets "stay within this
    /// directory's resolution path", which is false and was the stated reason a
    /// reader might choose this variant.
    ///
    /// The admitted directory constrains how a *name* is resolved, not where a
    /// *link* may point. To keep a walk inside its tree, call
    /// [`self::Dir::read_link`] first, reject targets that are absolute or
    /// contain `..`, and open the result relative to a separately admitted
    /// [`self::Dir`] — a capability you chose to grant, not one this
    /// descriptor confers.
    FollowFinal,
}

/// Ceilings on one directory listing: how many names it keeps and how many
/// name bytes, so a directory an attacker can fill cannot make a listing
/// allocate without bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListLimits {
    /// Maximum entries counted, addressable or not.
    pub max_entries: usize,
    /// Maximum cumulative bytes of the names kept.
    pub max_name_bytes: usize,
}

impl ListLimits {
    /// Explicit ceilings for one listing.
    #[must_use]
    pub const fn new(max_entries: usize, max_name_bytes: usize) -> Self {
        Self {
            max_entries,
            max_name_bytes,
        }
    }
}

impl Default for ListLimits {
    /// 65,536 entries and 16 MiB of names: wider than any directory a person
    /// makes, far below what a hostile one can hold.
    fn default() -> Self {
        Self::new(65_536, 16 << 20)
    }
}

/// One bounded directory listing.
///
/// Names are `String`s because that is the only name type the rest of
/// [`Dir`] accepts: every method takes one `&str` component. A name that is
/// not UTF-8 cannot be passed back to any of them, so it is counted as
/// unaddressable rather than returned as a name nothing here can open.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Listing {
    /// Addressable names, sorted, without `.` and `..`.
    names: Vec<String>,
    /// Entries whose names are not UTF-8.
    unaddressable: usize,
    /// Whether a ceiling stopped the listing before the directory ended.
    truncated: bool,
}

impl Listing {
    /// Addressable names, sorted bytewise. When the listing is truncated these
    /// are the names the directory yielded first, which is filesystem order:
    /// a truncated listing is not reproducible across filesystems, a complete
    /// one is.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Entries seen whose names are not UTF-8 and so cannot be opened here.
    #[must_use]
    pub fn unaddressable(&self) -> usize {
        self.unaddressable
    }

    /// Whether a ceiling stopped the listing early.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Whether every entry was listed and every one is addressable.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.truncated && self.unaddressable == 0
    }
}

/// What an entry's name denotes, stated without following it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory, safe to descend into with [`self::Dir::open_subdir`].
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
///
/// A field combination that the kernel would answer inconsistently is refused
/// before the syscall rather than passed through: see
/// [`OpenFlags::validate`], which the open path calls itself.
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
    ///
    /// Exhaustive rather than defaulted, so adding a fourth case is a compile
    /// error instead of a silent fallthrough. The `(false, false)` case is
    /// refused by [`OpenFlags::validate`] and so is unreachable here.
    #[cfg(unix)]
    fn access(self) -> rustix::fs::OFlags {
        match (self.read, self.write) {
            (true, true) => rustix::fs::OFlags::RDWR,
            (false, true) => rustix::fs::OFlags::WRONLY,
            (true, false) => rustix::fs::OFlags::RDONLY,
            // Refused by `validate`; returning RDONLY here would turn a refused
            // request into a read, and with `truncate` set into data loss.
            (false, false) => rustix::fs::OFlags::RDONLY,
        }
    }
}

#[cfg(unix)]
impl OpenFlags {
    /// Check these flags, refusing combinations the kernel answers inconsistently.
    ///
    /// `O_RDONLY | O_TRUNC` is the one that matters. POSIX leaves the
    /// interaction unspecified for a file opened without write access, and the
    /// platforms disagree: Linux refuses it with `EACCES`, while macOS and the
    /// BSDs honour the truncate and empty the file. A caller that got this
    /// wrong on Linux would ship believing the file is intact, and it would not
    /// be. The same applies to `truncate` without `write` in any form, and to a
    /// request for neither read nor write, which asks for a descriptor that can
    /// do nothing.
    ///
    /// The fields stay public because they are data, and a struct literal is
    /// how a caller reads them; this is the one call that must precede the
    /// syscall, which is why [`Dir::open_entry`] makes it itself rather than
    /// trusting a caller to have remembered.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] when the combination cannot be honoured
    /// on every supported platform.
    pub fn validate(&self) -> io::Result<()> {
        if self.truncate && !self.write {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "truncate requires write access: a read-only O_TRUNC empties the \
                 file on macOS and the BSDs, and Linux merely refuses it",
            ));
        }
        if !self.read && !self.write {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "open needs read, write, or both; neither asks for a descriptor \
                 that can do nothing",
            ));
        }
        if self.create && self.symlinks == SymlinkPolicy::FollowFinal {
            // `create` is `O_CREAT` without `O_EXCL`, and `FollowFinal` is
            // `O_NOFOLLOW` absent. Together they mean: follow the name, and if
            // it does not exist, make it. On a symlink that means opening the
            // link's target for writing — and on a *dangling* one, creating
            // whatever the link points at, wherever that is. `create_new` is
            // the safe spelling: it always pairs `O_CREAT` with `O_EXCL` and
            // `O_NOFOLLOW`, so an existing or linked name is an error.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "create cannot follow a final symlink: O_CREAT without O_EXCL \
                 writes through the link, and creates a dangling link's target. \
                 Use Dir::create_new, which pairs O_EXCL with O_NOFOLLOW",
            ));
        }
        Ok(())
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
        flags.validate()?;
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

    /// The names in this directory, bounded by `limits`.
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
    /// The listing is one `getdents64` sequence against this descriptor, so a
    /// concurrent rename cannot make the names belong to a different directory
    /// than the one opened. It is rewound first, so it does not depend on
    /// whether the descriptor has read or created anything earlier. `.` and
    /// `..` are omitted, matching `std::fs::read_dir`. Each entry is charged to
    /// `limits` before it is kept; the first one that would exceed a ceiling
    /// ends the listing, which [`Listing::is_truncated`] then reports.
    ///
    /// # Errors
    ///
    /// The OS error when the directory cannot be read. An error part-way
    /// through the stream is returned and the names already collected are
    /// discarded: a truncated list that looks complete is worse than an error,
    /// and this is the one place a caller cannot otherwise tell a short read
    /// from a small directory.
    #[cfg(target_os = "linux")]
    pub fn entry_names(&self, limits: ListLimits) -> io::Result<Listing> {
        use std::mem::MaybeUninit;

        /// One page is the kernel's own `getdents64` granularity. A wide
        /// directory costs this much stack to enumerate, not one `PathBuf` per
        /// sibling.
        const DIRENT_BUFFER_BYTES: usize = 4_096;

        // A directory descriptor carries a read position, and `getdents64`
        // advances it. `mkdirat` and `openat(O_CREAT)` through this same
        // descriptor move that position too, so a `Dir` that created something
        // and then listed resumed past the first entries and reported a
        // directory holding two files as empty. Rewinding first makes the
        // listing a property of the directory rather than of whatever this
        // descriptor happened to do before.
        rustix::fs::seek(self.fd.as_fd(), rustix::fs::SeekFrom::Start(0)).map_err(errno_to_io)?;

        let mut buffer = [MaybeUninit::<u8>::uninit(); DIRENT_BUFFER_BYTES];
        let mut directory = rustix::fs::RawDir::new(self.fd.as_fd(), &mut buffer);
        let mut listing = Listing {
            names: Vec::new(),
            unaddressable: 0,
            truncated: false,
        };
        let mut entries = 0_usize;
        let mut name_bytes = 0_usize;
        while let Some(entry) = directory.next() {
            let entry = entry.map_err(errno_to_io)?;
            let name = entry.file_name().to_bytes();
            // `RawDir` reports `.` and `..`; `std::fs::read_dir` does not, and
            // no caller can act on either. Passing one back to `kind` or
            // `open_subdir` is refused as a non-component, so a walk driven by
            // this list would log two errors per directory and miss the real
            // entries behind them.
            if name == b"." || name == b".." {
                continue;
            }
            if entries >= limits.max_entries {
                listing.truncated = true;
                break;
            }
            entries = entries.saturating_add(1);
            match std::str::from_utf8(name) {
                Ok(text) => {
                    let charged = name_bytes.saturating_add(text.len());
                    if charged > limits.max_name_bytes {
                        listing.truncated = true;
                        break;
                    }
                    name_bytes = charged;
                    listing.names.push(text.to_owned());
                }
                Err(_) => listing.unaddressable = listing.unaddressable.saturating_add(1),
            }
        }
        listing.names.sort_unstable();
        Ok(listing)
    }

    /// The names in this directory.
    ///
    /// Listing a descriptor needs `getdents64`, which is Linux-only; see the
    /// Linux implementation for why there is no portable form here.
    #[cfg(not(target_os = "linux"))]
    pub fn entry_names(&self, _limits: ListLimits) -> io::Result<Listing> {
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
        // `fcntl_dupfd_cloexec`, not `dup(2)`. `dup` explicitly clears
        // FD_CLOEXEC, and every other descriptor this type creates sets it
        // explicitly, so a plain `dup` would make the clone the one leaked into
        // any child spawned during a walk — the exact case a walk cursor is
        // likely to have. `try_clone` is the method a caller uses to hold two
        // cursors over one directory, so it is the one that has to be right.
        let fd = rustix::io::fcntl_dupfd_cloexec(self.fd.as_fd(), 0).map_err(errno_to_io)?;
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

    use super::{FileKind, OpenFlags};

    /// Handle-relative filesystem access. Unix-only.
    #[derive(Debug)]
    #[non_exhaustive]
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
        pub fn entry_names(&self, _limits: super::ListLimits) -> io::Result<super::Listing> {
            Err(unsupported("Dir::entry_names"))
        }
    }

    /// The refusal every stub returns, naming the method that was called.
    fn unsupported(name: &'static str) -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("fs::capability::Dir::{name} is Unix-only"),
        )
    }
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

/// Map a raw `st_mode` word to the kind this crate reports.
///
/// Delegates to rustix's `FileType::from_raw_mode` rather than decoding the
/// `S_IFMT` bits here, and that is not a style preference. `Stat::st_mode` has
/// type `rustix::fs::RawMode`, which is `c_uint` on rustix's Linux backend and
/// `mode_t` — a `u16` — on its libc backend. A hand-written `u16` parameter
/// compiles on macOS and fails on Linux, which is where this repository's CI
/// runs. Decoding the bits locally also restated seven `S_IF*` constants that
/// rustix already maps, with nothing to catch a transcription error.
#[cfg(unix)]
fn classify(raw: rustix::fs::RawMode) -> FileKind {
    use rustix::fs::FileType as Ft;
    let file_type = Ft::from_raw_mode(raw);
    if file_type == Ft::RegularFile {
        FileKind::File
    } else if file_type == Ft::Directory {
        FileKind::Directory
    } else if file_type == Ft::Symlink {
        FileKind::Symlink
    } else if file_type == Ft::Fifo {
        FileKind::Fifo
    } else if file_type == Ft::CharacterDevice {
        FileKind::CharDevice
    } else if file_type == Ft::BlockDevice {
        FileKind::BlockDevice
    } else if file_type == Ft::Socket {
        FileKind::Socket
    } else {
        FileKind::Unknown
    }
}

/// Mode for a file this process owns: readable and writable by the user only.
///
/// `RUSR | WUSR` is 0o600, and nothing else may be set.
///
/// The previous value was `RUSR | RWXG | RWXO` — rustix's names for *all* the
/// group bits and *all* the world bits, which read like "user read, and then
/// some group and world bits" rather than "owner-only". It is 0o477: the
/// setuid bit, plus group and world rwx. A umask does not rescue that, because
/// it can only clear bits. On the host that caught it, with the usual 0o022
/// umask, `create_new` produced a file at 0o455 — world-readable and
/// world-executable. Anything this process wrote into a shared tree was
/// readable by every other user on the machine.
///
/// Written as the two named user bits rather than as a literal 0o600 because a
/// bare literal is exactly the thing that reads as obviously correct while
/// being wrong, and `create_new` is the path a caller reaches for when it
/// intends the file to be private.
#[cfg(unix)]
fn user_writable_mode() -> rustix::fs::Mode {
    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR
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
            0o600 & !current_umask(),
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
            0o700 & !current_umask(),
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
    fn current_umask() -> u32 {
        // A temp dir we cannot create is not a reason to skip the permission
        // check; report no mask and let the 0o600 assertion fail loudly rather
        // than silently passing.
        let Ok(tmp) = tempfile::tempdir() else {
            return 0;
        };
        if stdfs::write(tmp.path().join("probe"), b"").is_err() {
            return 0;
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = stdfs::metadata(tmp.path().join("probe"))
            .map_or(0o600, |meta| meta.permissions().mode());
        0o600 & !mode
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
}
