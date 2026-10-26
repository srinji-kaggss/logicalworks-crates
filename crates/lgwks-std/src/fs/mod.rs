//! `fs` owns recursive filesystem walking and directory traversal, enforcing
//! INV-FS-2: directory walking respects depth bounds, handles symlink
//! loops defensively, applies best-effort root-bounded symlink policy, and
//! requires zero external dependencies like `walkdir`. This path-based API is
//! for trusted trees, not a race-safe sandbox against hostile concurrent path
//! replacement. For that threat model use `fs::capability::Dir`, which opens a
//! directory once and resolves every name beneath it with `openat`/`statat`
//! from that one descriptor, so a name replaced mid-walk cannot redirect the
//! walk off the tree it was admitted to. It is behind the `fs-raw` feature,
//! which is why the path is written out rather than linked: an intra-doc link
//! from this always-on module to a default-off one resolves in one feature set
//! and dangles in every other, and this module is on in all of them.
//!
//! [`walk_dir_entries`] is the same walk for a caller that must not descend
//! everywhere and needs more than a path: a predicate decides per entry whether
//! to [`Descend::Enter`], [`Descend::Skip`] its subtree unread, or
//! [`Descend::Stop`], and each [`WalkEntry`] carries the `lstat` the walk
//! classified it by and its path relative to the root (INV-FS-7).

/// Handle-relative filesystem access, for trees that are being rewritten while
/// you read them. Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(feature = "fs-raw")]
pub mod capability;
/// The traversal engine every walk entry point runs through.
mod engine;
#[cfg(test)]
mod predicate_tests;
/// The streaming walk: entries out as they are read.
mod stream;
#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use self::engine::{WalkMode, enter_everything, run_walk};

pub use stream::{OmissionAction, OmissionPolicy, WalkIter, WalkStreamEntry, walk_dir_iter};

/// Options for configuring a recursive filesystem walk.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WalkOptions {
    /// Maximum directory depth to traverse (0 = only the root directory entries).
    pub max_depth: usize,
    /// Whether to follow symlinks to directories.
    pub follow_symlinks: bool,
    /// Whether to sort entries by filename for deterministic ordering.
    pub sort_alphabetically: bool,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            max_depth: 32,
            follow_symlinks: false,
            sort_alphabetically: true,
        }
    }
}

/// Which walk stage an entry or directory failed at.
///
/// The stages are the places the walk reads or identifies filesystem objects
/// and can come back short: listing a directory, stating one entry, resolving
/// a symlink target, and proving a directory is still the one it was a moment
/// ago. Symlink targets successfully resolved outside the root and already
/// visited directories are policy exclusions, not omissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OmissionStage {
    /// `read_dir` items that arrived as errors, or a directory that could
    /// not be listed at all.
    ReadEntries,
    /// One entry whose file type could not be stated.
    EntryType,
    /// A symlink target could not be read or resolved for the root policy.
    SymlinkTarget,
    /// A directory whose canonical path within the root could
    /// not be established before the read or no longer held after it.
    DirectoryResolution,
    /// The input root could not be canonicalized.
    RootResolution,
    /// A configured producer or retention budget was reached.
    ResourceBudget,
}

/// One place the walk came back short of complete coverage.
#[derive(Debug)]
#[non_exhaustive]
pub struct WalkOmission {
    /// The directory or entry that could not be read. For entry-item errors
    /// inside a successful listing this is the directory being read: a
    /// failed item carries no path of its own.
    pub path: PathBuf,
    /// Which stage failed.
    pub stage: OmissionStage,
    /// The underlying OS error, preserved rather than classified away.
    pub error: io::Error,
}

/// What a filesystem object is, stated from its own `lstat` and never by
/// following it.
///
/// One type for both walkers: the path walk's predicate receives it, and
/// `fs::capability::Dir::kind` returns it (re-exported there as
/// `fs::capability::FileKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link. Its target is not resolved by the classification.
    Symlink,
    /// A named pipe.
    Fifo,
    /// A character device.
    CharDevice,
    /// A block device.
    BlockDevice,
    /// A socket.
    Socket,
    /// A type this platform's file-type bits do not name. Network and
    /// synthetic filesystems produce it, and reporting a guess would be worse
    /// than reporting the uncertainty.
    Unknown,
}

impl FileKind {
    /// The kind `file_type` names. A symlink is checked first, so a link to a
    /// directory is a `Symlink` and never a `Directory`.
    fn of(file_type: fs::FileType) -> Self {
        if file_type.is_symlink() {
            return Self::Symlink;
        }
        if file_type.is_dir() {
            return Self::Directory;
        }
        if file_type.is_file() {
            return Self::File;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt as _;
            if file_type.is_fifo() {
                return Self::Fifo;
            }
            if file_type.is_char_device() {
                return Self::CharDevice;
            }
            if file_type.is_block_device() {
                return Self::BlockDevice;
            }
            if file_type.is_socket() {
                return Self::Socket;
            }
        }
        Self::Unknown
    }
}

/// What a walk predicate answers for one admitted entry.
///
/// The entry the predicate was asked about is recorded under every answer;
/// the answer governs only what comes after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Descend {
    /// Record the entry and, when it is a directory the policy may descend
    /// into, walk it.
    Enter,
    /// Record the entry and never read below it: a skipped directory is not
    /// listed, stated, charged to a budget or reported as an omission.
    Skip,
    /// Record the entry and end the walk: no further entry is listed, stated
    /// or offered to the predicate.
    Stop,
}

/// The path with the canonical root's leading components stripped, so the path
/// walks and the streaming walk name the same relative path without repeating
/// the walk.
pub(crate) fn relative_to_root(path: &Path, root_components: usize) -> &Path {
    let mut below_root = path.components();
    for _ in 0..root_components {
        below_root.next();
    }
    below_root.as_path()
}

/// One entry a [`walk_dir_entries`] walk admitted, with the `lstat` the walk
/// classified it by.
///
/// The metadata is the walk's own observation, read once from the directory
/// entry without following a final symlink: holding it costs the caller no
/// second stat, and it is a snapshot that does not change if the file does.
///
/// [`WalkStreamEntry::relative_path`] names the same path through
/// [`relative_to_root`], so the two walkers cannot drift into spelling it
/// differently.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WalkEntry {
    /// Absolute logical path, rooted at the canonicalized walk root.
    path: PathBuf,
    /// The kind the metadata names.
    kind: FileKind,
    /// The entry's own `lstat`.
    metadata: fs::Metadata,
    /// Components of the canonicalized root that lead `path`.
    root_components: usize,
}

impl WalkEntry {
    /// The absolute logical path, rooted at the canonicalized walk root. An
    /// entry reached through a followed in-root symlink keeps the link's name
    /// in its path (INV-FS-5).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The same path relative to the canonicalized walk root: joining it onto
    /// that root gives [`WalkEntry::path`] back. Never empty, because the root
    /// itself is not an entry.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        relative_to_root(&self.path, self.root_components)
    }

    /// The kind the walk classified the entry as, from its own `lstat`.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        self.kind
    }

    /// The entry's `lstat`: file type, size, permissions, modification time,
    /// and on Unix (through `std::os::unix::fs::MetadataExt`) mode, inode and
    /// device. A symlink's metadata describes the link, not its target.
    #[must_use]
    pub fn metadata(&self) -> &fs::Metadata {
        &self.metadata
    }

    /// Transfers the absolute path without cloning it.
    #[must_use]
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

/// What a tolerant walk saw: every admitted entry plus every place it came
/// back short.
///
/// A report retains entries, omissions and the policy scope under which the
/// walk ran. Completeness never needs to be inferred from the entry list.
/// `E` is [`PathBuf`] for the path walks and [`WalkEntry`] for
/// [`walk_dir_entries_tolerant`].
#[derive(Debug)]
#[non_exhaustive]
pub struct WalkReport<E = PathBuf> {
    /// Every entry the policy admitted, in walk order.
    entries: Vec<E>,
    /// Every place the walk came back short, in walk order.
    omissions: Vec<WalkOmission>,
    /// Whether an applied depth, symlink, or resource policy excluded work.
    policy: WalkPolicy,
    /// Whether a resource budget stopped the walk before its policy scope ended.
    budget_exhausted: bool,
    /// Whether the caller's predicate answered [`Descend::Stop`].
    stopped: bool,
}

impl<E> WalkReport<E> {
    /// Creates a result container for the configured walk policy.
    fn new(options: &WalkOptions) -> Self {
        Self {
            entries: Vec::new(),
            omissions: Vec::new(),
            policy: WalkPolicy {
                max_depth: options.max_depth,
                follow_symlinks: options.follow_symlinks,
                sort_alphabetically: options.sort_alphabetically,
            },
            budget_exhausted: false,
            stopped: false,
        }
    }

    /// Every entry the policy admitted, in walk order.
    #[must_use]
    pub fn entries(&self) -> &[E] {
        &self.entries
    }

    /// Whether the caller's predicate ended the walk with [`Descend::Stop`].
    ///
    /// A stop, like a skipped subtree, is the caller's own policy and is not
    /// an omission: [`WalkReport::is_complete_within_policy`] does not count
    /// it, and this is how a reader tells the walk ended early by request. The
    /// path walks never stop.
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Every place the walk came back short, in walk order.
    #[must_use]
    pub fn omissions(&self) -> &[WalkOmission] {
        &self.omissions
    }

    /// Whether the walk is complete within the applied policy.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.is_complete_within_policy()
    }

    /// Whether the report has no omissions or resource-budget refusal within
    /// the supplied walk policy. Depth and outside-root symlink exclusions are
    /// deliberate policy boundaries and do not make this false.
    #[must_use]
    pub fn is_complete_within_policy(&self) -> bool {
        self.omissions.is_empty() && !self.budget_exhausted
    }

    /// The depth and symlink policy applied to this report.
    #[must_use]
    pub fn policy(&self) -> WalkPolicy {
        self.policy
    }

    /// Whether a resource budget stopped traversal before policy coverage ended.
    #[must_use]
    pub fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    /// Transfers the owned entries and omissions without cloning paths or errors.
    #[must_use]
    pub fn into_parts(self) -> (Vec<E>, Vec<WalkOmission>) {
        (self.entries, self.omissions)
    }
}

impl std::fmt::Display for WalkFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "filesystem walk failed at {:?} for {}: {}",
            self.stage,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for WalkFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The depth and symlink scope applied to a filesystem walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalkPolicy {
    /// Maximum directory depth traversed (0 means root entries only).
    pub max_depth: usize,
    /// Whether in-root symlink targets were followed.
    pub follow_symlinks: bool,
    /// Whether entries were sorted by filename within each directory.
    pub sort_alphabetically: bool,
}

/// Producer and retention ceilings for a bounded materializing walk.
///
/// The entry and path-byte budgets are charged before each directory entry is
/// retained for sorting or output. `max_directory_entries` bounds the sorted
/// working set for one directory. The legacy convenience APIs are unbounded
/// materializers; use this type when input width or path volume is untrusted.
///
/// Whether a bounded walk is complete does not depend on the order the
/// filesystem yields entries: a budget runs out exactly when the tree within
/// the policy holds more entries or path bytes than it allows, in any order.
/// One exception: with `follow_symlinks` on and sorting off, a directory
/// reachable through two aliases of different lengths is visited, and its path
/// bytes charged, through whichever alias the filesystem yields first, so a
/// path-byte budget between the two totals can pass in one order and not the
/// other. Sorting makes the visit order, and so completeness, deterministic.
/// *Which* entries an incomplete report retains does depend on that order. In
/// the directory where a budget ran out, the entries kept are the ones the
/// filesystem yielded first, which differs between filesystems; they are
/// sorted only after being charged. Picking a deterministic subset instead
/// would mean reading the rest of that directory, which is the work the budget
/// exists to refuse. A complete report is reproducible; an incomplete one is a
/// bounded prefix, and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalkLimits {
    /// Maximum directory entries read across the full traversal.
    pub max_entries: usize,
    /// Maximum entries retained from any one directory for sorting.
    pub max_directory_entries: usize,
    /// Maximum cumulative encoded path bytes observed and retained.
    pub max_path_bytes: usize,
    /// Maximum omissions recorded in tolerant mode.
    pub max_omissions: usize,
}

impl WalkLimits {
    /// Constructs explicit ceilings for a bounded walk.
    #[must_use]
    pub const fn new(
        max_entries: usize,
        max_directory_entries: usize,
        max_path_bytes: usize,
        max_omissions: usize,
    ) -> Self {
        Self {
            max_entries,
            max_directory_entries,
            max_path_bytes,
            max_omissions,
        }
    }
}

/// Failure context attached to strict walk errors as their typed source.
#[derive(Debug)]
#[non_exhaustive]
pub struct WalkFailure {
    /// The path whose processing failed.
    path: PathBuf,
    /// The stage at which the failure occurred.
    stage: OmissionStage,
    /// The underlying filesystem or budget error.
    source: io::Error,
}

impl WalkFailure {
    /// Path whose processing failed.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stage at which processing failed.
    #[must_use]
    pub fn stage(&self) -> OmissionStage {
        self.stage
    }

    /// Underlying filesystem or budget error.
    #[must_use]
    pub fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Build the failure a streaming omission reports.
    ///
    /// Crate-internal: the streaming walk owns every omission it yields, and
    /// the materializing walks own theirs, so neither builds the other's.
    pub(crate) fn for_stream(path: PathBuf, stage: OmissionStage, source: io::Error) -> Self {
        Self {
            path,
            stage,
            source,
        }
    }
}

/// Recursively walks `root` according to `options`, returning all matching entries.
///
/// This is the strict walk: any omission — an unreadable directory entry,
/// an unstated file type, or a directory path that cannot be resolved —
/// refuses the whole walk rather than returning a silent subset. The root
/// itself must resolve: an unresolvable root is an error, never a walk with
/// no root path. Successfully resolved symlink targets outside the root
/// and already visited directories are policy exclusions; unresolved symlink
/// targets are omissions. Use
/// [`walk_dir_tolerant`] for the same walk with omissions reported instead
/// of refused. Every returned path is absolute under the canonicalized input
/// root; descendants reached through an in-root alias keep that alias in their
/// logical path. This replaces the previous mixed relative/canonical spelling.
/// The convenience API materializes the entire result; use
/// [`walk_dir_bounded`] when input width or path volume is untrusted.
///
/// A bounded strict walk of a directory under generous ceilings returns only
/// absolute paths. A tolerant report can be consumed by a downstream crate
/// without cloning, through [`WalkReport::into_parts`]. A bounded tolerant
/// report exposes whether the returned prefix reached a producer ceiling, and
/// it is complete within its policy exactly when no budget stopped it and no
/// omission was recorded. The unit test `the_documented_walks_hold` runs each of
/// those over the crate's own directory.
pub fn walk_dir(root: impl AsRef<Path>, options: &WalkOptions) -> io::Result<Vec<PathBuf>> {
    let report = run_walk(
        root.as_ref(),
        options,
        WalkMode::Strict,
        None,
        enter_everything,
    )?;
    Ok(report.into_parts().0)
}

/// Recursively walks `root` according to `options`, reporting omissions.
///
/// The tolerant walk returns every admitted entry together with every place
/// it came back short. Completeness is evaluated within the returned policy:
/// depth limits and outside-root symlinks are deliberate exclusions, while a
/// budget stop is explicitly incomplete. Like the strict walk, an
/// unresolvable root refuses:
/// tolerance covers coverage loss inside the tree, not failure to resolve
/// the tree's root path.
pub fn walk_dir_tolerant(root: impl AsRef<Path>, options: &WalkOptions) -> io::Result<WalkReport> {
    run_walk(
        root.as_ref(),
        options,
        WalkMode::Tolerant,
        None,
        enter_everything,
    )
}

/// Walks with strict refusal and explicit producer and path-retention ceilings.
///
/// Sorting retains at most `max_directory_entries` from one directory. Entry
/// count and cumulative path-byte budgets are charged before retention. Paths
/// are absolute and rooted at the canonicalized input root. This API
/// materializes the admitted output; its retained output is bounded by the
/// configured limits.
pub fn walk_dir_bounded(
    root: impl AsRef<Path>,
    options: &WalkOptions,
    limits: &WalkLimits,
) -> io::Result<Vec<PathBuf>> {
    let report = run_walk(
        root.as_ref(),
        options,
        WalkMode::Strict,
        Some(limits),
        enter_everything,
    )?;
    Ok(report.into_parts().0)
}

/// Walks tolerantly with explicit producer and path-retention ceilings.
///
/// Reaching a resource ceiling returns the admitted prefix with
/// [`WalkReport::budget_exhausted`] set; that prefix is never complete. Paths
/// are absolute and rooted at the canonicalized input root.
pub fn walk_dir_tolerant_bounded(
    root: impl AsRef<Path>,
    options: &WalkOptions,
    limits: &WalkLimits,
) -> io::Result<WalkReport> {
    run_walk(
        root.as_ref(),
        options,
        WalkMode::Tolerant,
        Some(limits),
        enter_everything,
    )
}

/// Walks strictly, asking `prune` about every admitted entry and returning
/// each with the `lstat` the walk classified it by.
///
/// This is [`walk_dir`] (or, with `limits`, [`walk_dir_bounded`]) with two
/// additions, and nothing else changes: the same depth and symlink policy, the
/// same refusals and the same budget charges. With a predicate that always
/// answers [`Descend::Enter`] it admits exactly the paths [`walk_dir`] returns.
///
/// - `prune` is called once per admitted entry, in walk order, with the entry's
///   absolute logical path and the [`FileKind`] of its own `lstat`, before the
///   walk goes below it. [`Descend::Skip`] on a directory means it is never
///   listed: nothing under it is read, charged to `limits`, or reported. An
///   in-root symlink is offered after its target is resolved (resolution is
///   how the policy decides whether a link is listed at all), and a skipped
///   link is not followed. Entries outside the policy — beyond the depth bound,
///   or links leaving the root — are never offered.
/// - Each [`WalkEntry`] carries that one `lstat` and its path relative to the
///   canonicalized root, so neither needs a second stat or a strip. Reading the
///   `lstat` is the one cost the path walks do not pay; an entry the walk
///   lists but cannot `lstat` is an [`OmissionStage::EntryType`] refusal.
///
/// A predicate that skips every directory named `.git` and enters the rest
/// returns entries whose path ends with their relative path and whose kind is a
/// directory exactly when their `lstat` says so; the unit test
/// `the_documented_predicate_walk_holds` runs that over the crate's own directory.
///
/// # Errors
///
/// The first omission, as [`walk_dir`] reports it: an `io::Error` whose source
/// is a [`WalkFailure`] naming the path and stage. An unresolvable root is
/// always refused.
pub fn walk_dir_entries<P>(
    root: impl AsRef<Path>,
    options: &WalkOptions,
    limits: Option<&WalkLimits>,
    prune: P,
) -> io::Result<Vec<WalkEntry>>
where
    P: FnMut(&Path, FileKind) -> Descend,
{
    let report = run_walk(root.as_ref(), options, WalkMode::Strict, limits, prune)?;
    Ok(report.into_parts().0)
}

/// Walks tolerantly, asking `prune` about every admitted entry and returning
/// each with the `lstat` the walk classified it by.
///
/// [`walk_dir_entries`] with omissions reported rather than refused, exactly as
/// [`walk_dir_tolerant`] (or, with `limits`, [`walk_dir_tolerant_bounded`])
/// reports them. A skipped subtree is never read, so nothing inside it can be
/// an omission; [`WalkReport::stopped`] says whether `prune` ended the walk.
///
/// # Errors
///
/// An unresolvable root; every other shortfall is an omission in the report.
pub fn walk_dir_entries_tolerant<P>(
    root: impl AsRef<Path>,
    options: &WalkOptions,
    limits: Option<&WalkLimits>,
    prune: P,
) -> io::Result<WalkReport<WalkEntry>>
where
    P: FnMut(&Path, FileKind) -> Descend,
{
    run_walk(root.as_ref(), options, WalkMode::Tolerant, limits, prune)
}

// ── Raw filesystem capacity ─────────────────────────────────────────────────

/// Bytes available to an unprivileged process on the filesystem holding `path`.
///
/// Stable `std` has no portable equivalent (`std::fs::available_space` is
/// unstable), so this is the `fs-raw` feature's one primitive. It reports the
/// bytes the calling user may actually write (`f_bavail * f_frsize`), not the
/// filesystem's total size.
///
/// This is an advisory snapshot, not a reservation, quota guarantee, or
/// promise that a later write will succeed.
///
/// # Errors
///
/// Returns the underlying OS error when `path` cannot be stat'ed. On non-Unix
/// targets the capability is absent and this returns [`io::ErrorKind::Unsupported`]
/// rather than a fabricated value.
#[cfg(all(unix, feature = "fs-raw"))]
pub fn available_space(path: impl AsRef<Path>) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path.as_ref())?;
    let block = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    };
    Ok(stat.f_bavail.saturating_mul(block))
}

/// Bytes available to an unprivileged process on the filesystem holding `path`.
///
/// The `fs-raw` capability is Unix-only; other targets report
/// [`io::ErrorKind::Unsupported`] rather than a fabricated value.
#[cfg(all(not(unix), feature = "fs-raw"))]
pub fn available_space(path: impl AsRef<Path>) -> io::Result<u64> {
    let _ = path;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fs-raw available_space is Unix-only",
    ))
}
