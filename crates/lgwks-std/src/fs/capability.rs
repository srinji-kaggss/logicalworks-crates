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

/// What an entry's name denotes, stated without following it. The same type
/// the path walk's predicate receives, so a kind read through a [`Dir`] and one
/// read by `walk_dir_entries` compare directly. A [`FileKind::Directory`] is
/// safe to descend into with [`self::Dir::open_subdir`].
pub use super::FileKind;

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
            let refusal = Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "truncate requires write access: a read-only O_TRUNC empties the \
             file on macOS and the BSDs, and Linux merely refuses it",
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "validate: returning an error to the caller");
            return refusal;
        }
        if !self.read && !self.write {
            let refusal = Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "open needs read, write, or both; neither asks for a descriptor \
             that can do nothing",
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "validate: returning an error to the caller");
            return refusal;
        }
        if self.create && self.symlinks == SymlinkPolicy::FollowFinal {
            // `create` is `O_CREAT` without `O_EXCL`, and `FollowFinal` is
            // `O_NOFOLLOW` absent. Together they mean: follow the name, and if
            // it does not exist, make it. On a symlink that means opening the
            // link's target for writing — and on a *dangling* one, creating
            // whatever the link points at, wherever that is. `create_new` is
            // the safe spelling: it always pairs `O_CREAT` with `O_EXCL` and
            // `O_NOFOLLOW`, so an existing or linked name is an error.
            let refusal = Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "create cannot follow a final symlink: O_CREAT without O_EXCL \
             writes through the link, and creates a dangling link's target. \
             Use Dir::create_new, which pairs O_EXCL with O_NOFOLLOW",
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "validate: returning an error to the caller");
            return refusal;
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
        let refusal = Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a single path component"),
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "single_component: returning an error to the caller");
        return refusal;
    }
    std::ffi::CString::new(name).map_err(|cause| {
        // The offset is read off the cause before the cause is moved into the
        // error: `CString::new` already located the NUL, and restating a second
        // copy of that number could only disagree with it.
        let at = cause.nul_position();
        io::Error::new(io::ErrorKind::InvalidInput, InteriorNul { cause, at })
    })
}

/// Why a path component could not become a `CString`.
///
/// A named type rather than a `String`: the [`std::ffi::NulError`] travels
/// inside it and is what makes the refusal observable, while the rendered text
/// stays the module's own phrase and never quotes the rejected name.
#[cfg(unix)]
#[derive(Debug)]
struct InteriorNul {
    /// The interior-NUL failure `CString::new` reported, kept as the cause so
    /// `Error::source` reaches it rather than losing it at the translation.
    cause: std::ffi::NulError,
    /// Offset of the NUL byte within the rejected component.
    at: usize,
}

#[cfg(unix)]
impl std::fmt::Display for InteriorNul {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately no `{:?}` of the rejected name: this path is reached
        // with caller-supplied text, and a name carrying CR/LF would forge a
        // second line in whatever log interpolates the `io::Error`.
        write!(
            formatter,
            "path component contains an interior NUL at offset {}",
            self.at
        )
    }
}

#[cfg(unix)]
impl std::error::Error for InteriorNul {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
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
mod tests;
