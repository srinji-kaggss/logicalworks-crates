//! The walk engine every walk_dir entry point runs: one traversal for path and metadata records, strict or tolerant, root-bounded symlink policy, and the charged limits.

use std::collections::HashSet;
use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};

use super::{
    Descend, FileKind, OmissionStage, WalkEntry, WalkFailure, WalkLimits, WalkOmission,
    WalkOptions, WalkReport,
};

/// How the walk engine answers an omission.
///
/// Strict is the fail-closed mode: the first omission refuses the whole
/// walk. Tolerant records the omission and continues with the rest of the
/// tree. Root resolution is not omittable in either mode: without a resolved
/// root path neither mode can apply its root-bounded symlink policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkMode {
    /// Refuse the first incomplete observation.
    Strict,
    /// Return observed entries and explicitly record omissions.
    Tolerant,
}

/// What the engine records for one admitted entry, and the one read that
/// classifies it.
///
/// This is how the path walks and the metadata walk stay one traversal: a
/// path record classifies from the directory entry's file type, which most
/// platforms answer from the listing itself, and a [`WalkEntry`] classifies
/// from one `lstat` it then keeps. Neither reads an entry twice.
trait Record: Sized {
    /// What classifying one entry reads.
    type Stat;
    /// Reads `entry`'s classification without following a final symlink.
    fn stat(entry: &DirEntry) -> io::Result<Self::Stat>;
    /// The file type the read established.
    fn file_type(stat: &Self::Stat) -> fs::FileType;
    /// The record kept for an admitted entry.
    fn record(path: PathBuf, kind: FileKind, stat: Self::Stat, root_components: usize) -> Self;
}

impl Record for PathBuf {
    type Stat = fs::FileType;

    fn stat(entry: &DirEntry) -> io::Result<fs::FileType> {
        entry.file_type()
    }

    fn file_type(stat: &fs::FileType) -> fs::FileType {
        *stat
    }

    fn record(path: PathBuf, _kind: FileKind, _stat: fs::FileType, _root: usize) -> Self {
        path
    }
}

impl Record for WalkEntry {
    type Stat = fs::Metadata;

    fn stat(entry: &DirEntry) -> io::Result<fs::Metadata> {
        // `DirEntry::metadata` does not traverse a final symlink on any
        // platform std supports: it is the entry's own `lstat`.
        entry.metadata()
    }

    fn file_type(stat: &fs::Metadata) -> fs::FileType {
        stat.file_type()
    }

    fn record(path: PathBuf, kind: FileKind, stat: fs::Metadata, root_components: usize) -> Self {
        Self {
            path,
            kind,
            metadata: stat,
            root_components,
        }
    }
}

/// The predicate the path walks run under: every entry is entered.
fn enter_everything(_path: &Path, _kind: FileKind) -> Descend {
    Descend::Enter
}

/// Everything one walk call shares, threaded through the recursion.
///
/// A single context is what keeps the engine's functions under the argument
/// limit as modes and omission sinks join the traversal state: each function
/// takes the path it acts on, its depth, and the walk it belongs to.
struct WalkContext<'a, E, P> {
    /// The resolved root directory used by best-effort path checks.
    canonical_root: &'a Path,
    /// Components of `canonical_root`, so a record can name its relative path.
    root_components: usize,
    /// Depth bound, symlink policy and ordering.
    options: &'a WalkOptions,
    /// Whether the first omission refuses or is recorded.
    mode: WalkMode,
    /// Entries admitted so far, in walk order.
    out: &'a mut Vec<E>,
    /// Omissions recorded so far, in walk order.
    omissions: &'a mut Vec<WalkOmission>,
    /// Canonical directories already entered, for loop termination.
    visited: &'a mut HashSet<PathBuf>,
    /// Optional producer ceilings; absent only for legacy materializers.
    limits: Option<&'a WalkLimits>,
    /// The caller's per-entry decision.
    prune: P,
    /// Number of directory entries observed so far.
    entries_seen: usize,
    /// Cumulative path-byte charge.
    path_bytes_seen: usize,
    /// Set when tolerant traversal returns a budget-limited prefix.
    budget_exhausted: bool,
    /// Set when the predicate answered [`Descend::Stop`].
    stopped: bool,
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
/// ```rust
/// use lgwks_std::fs::{walk_dir_bounded, WalkLimits, WalkOptions};
///
/// let limits = WalkLimits::new(10_000, 2_000, 8 * 1024 * 1024, 100);
/// let entries = walk_dir_bounded(".", &WalkOptions::default(), &limits)?;
/// assert!(entries.iter().all(|path| path.is_absolute()));
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// A report can also be consumed by a downstream crate without cloning:
///
/// ```rust
/// use lgwks_std::fs::{walk_dir_tolerant, WalkOptions};
///
/// let report = walk_dir_tolerant(".", &WalkOptions::default())?;
/// let (entries, omissions) = report.into_parts();
/// assert!(!entries.is_empty());
/// assert!(omissions.is_empty());
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// A bounded tolerant report exposes whether the returned prefix reached a
/// producer ceiling:
///
/// ```rust
/// use lgwks_std::fs::{walk_dir_tolerant_bounded, WalkLimits, WalkOptions};
///
/// let limits = WalkLimits::new(100, 20, 64 * 1024, 10);
/// let report = walk_dir_tolerant_bounded(".", &WalkOptions::default(), &limits)?;
/// let policy = report.policy();
/// let complete = report.is_complete_within_policy();
/// let stopped_for_budget = report.budget_exhausted();
/// assert_eq!(policy.max_depth, WalkOptions::default().max_depth);
/// assert_eq!(complete, !stopped_for_budget && report.omissions().is_empty());
/// # Ok::<(), std::io::Error>(())
/// ```
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
/// ```rust
/// use lgwks_std::fs::{walk_dir_entries, Descend, FileKind, WalkOptions};
///
/// let entries = walk_dir_entries(".", &WalkOptions::default(), None, |path, kind| {
///     if kind == FileKind::Directory && path.file_name() == Some(".git".as_ref()) {
///         Descend::Skip
///     } else {
///         Descend::Enter
///     }
/// })?;
/// for entry in &entries {
///     assert!(entry.path().ends_with(entry.relative_path()));
///     assert_eq!(entry.kind() == FileKind::Directory, entry.metadata().is_dir());
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
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

/// Runs every entry point through the same traversal engine.
fn run_walk<E, P>(
    root: &Path,
    options: &WalkOptions,
    mode: WalkMode,
    limits: Option<&WalkLimits>,
    prune: P,
) -> io::Result<WalkReport<E>>
where
    E: Record,
    P: FnMut(&Path, FileKind) -> Descend,
{
    let canonical_root = root.canonicalize().map_err(|source| {
        let kind = source.kind();
        io::Error::new(
            kind,
            WalkFailure {
                path: root.to_path_buf(),
                stage: OmissionStage::RootResolution,
                source,
            },
        )
    })?;
    let mut report = WalkReport::new(options);
    let mut visited = HashSet::new();
    let mut ctx = WalkContext {
        canonical_root: &canonical_root,
        root_components: canonical_root.components().count(),
        options,
        mode,
        out: &mut report.entries,
        omissions: &mut report.omissions,
        visited: &mut visited,
        limits,
        prune,
        entries_seen: 0,
        path_bytes_seen: 0,
        budget_exhausted: false,
        stopped: false,
    };
    walk_recursive(root, &canonical_root, 0, &mut ctx)?;
    report.budget_exhausted = ctx.budget_exhausted;
    report.stopped = ctx.stopped;
    Ok(report)
}

/// Refuses an omission under `mode`: strict returns the error, tolerant
/// records it and continues.
///
/// The two arms are the whole of the strict/tolerant contract. Every
/// omission in the engine passes through here, so no new omission site can
/// silently pick a mode: it states one by calling this.
fn omit<E, P>(ctx: &mut WalkContext<'_, E, P>, omission: WalkOmission) -> io::Result<()> {
    if ctx.mode == WalkMode::Strict {
        let kind = omission.error.kind();
        let refusal = Err(io::Error::new(
            kind,
            WalkFailure {
                path: omission.path,
                stage: omission.stage,
                source: omission.error,
            },
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "omit: returning an error to the caller");
        return refusal;
    }
    if let Some(limits) = ctx.limits
        && ctx.omissions.len() >= limits.max_omissions
    {
        ctx.budget_exhausted = true;
        return Ok(());
    }
    ctx.omissions.push(omission);
    Ok(())
}

/// Charges an observed path and entry before the directory list retains it.
fn charge_entry<E, P>(
    ctx: &mut WalkContext<'_, E, P>,
    path: &Path,
    directory_entries: usize,
) -> io::Result<bool> {
    let Some(limits) = ctx.limits else {
        return Ok(true);
    };
    let next_count = ctx.entries_seen.saturating_add(1);
    let next_path_bytes = ctx.path_bytes_seen.saturating_add(path.as_os_str().len());
    let exceeded = next_count > limits.max_entries
        || directory_entries >= limits.max_directory_entries
        || next_path_bytes > limits.max_path_bytes;
    if exceeded {
        if ctx.mode == WalkMode::Strict {
            omit(
                ctx,
                WalkOmission {
                    path: path.to_path_buf(),
                    stage: OmissionStage::ResourceBudget,
                    error: io::Error::new(
                        io::ErrorKind::OutOfMemory,
                        "filesystem walk budget reached",
                    ),
                },
            )?;
        } else {
            ctx.budget_exhausted = true;
        }
        return Ok(false);
    }
    ctx.entries_seen = next_count;
    ctx.path_bytes_seen = next_path_bytes;
    Ok(true)
}

/// Records that `dir` has been entered, returning whether it is newly visited.
///
/// The set holds canonical paths, so the same directory reached through two
/// different symlinks (or through `..`) is recognised as one node and a symlink
/// loop terminates instead of recursing until the stack is exhausted. A path
/// that cannot be canonicalised is treated as new rather than skipped; the
/// caller reports any later failure to resolve or read it.
fn track_canonical_visit(dir: &Path, visited_canonical: &mut HashSet<PathBuf>) -> bool {
    if let Ok(canonical) = dir.canonicalize()
        && !visited_canonical.insert(canonical)
    {
        return false;
    }
    true
}

/// Reads a directory into a bounded working set and records item errors.
///
/// Sorting by filename makes output deterministic across filesystems. With
/// limits present, entry count, per-directory width and cumulative path bytes
/// are charged before each `DirEntry` is retained.
fn read_sorted_entries<E, P>(
    dir: &Path,
    logical_dir: &Path,
    sort_alphabetically: bool,
    ctx: &mut WalkContext<'_, E, P>,
) -> io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)? {
        match item {
            Ok(entry) => {
                let output_path = logical_dir.join(entry.file_name());
                if !charge_entry(ctx, &output_path, entries.len())? {
                    break;
                }
                entries.push(entry);
            }
            Err(error) => {
                omit(
                    ctx,
                    WalkOmission {
                        path: logical_dir.to_path_buf(),
                        stage: OmissionStage::ReadEntries,
                        error,
                    },
                )?;
                if ctx.budget_exhausted {
                    break;
                }
            }
        }
    }
    if sort_alphabetically {
        entries.sort_by_key(|entry| entry.file_name());
    }
    Ok(entries)
}

/// Resolves a symlink's target, relative to the directory holding the link.
///
/// `read_link` returns the target exactly as stored, so a relative target is
/// meaningful only against the link's own directory. A failed read is an
/// omission, not evidence that the link is outside the root.
fn resolve_symlink_path(dir: &Path, path: &Path) -> io::Result<PathBuf> {
    let target = fs::read_link(path)?;
    if target.is_relative() {
        Ok(dir.join(target))
    } else {
        Ok(target)
    }
}

/// What one successful symlink resolution establishes.
struct SymlinkResolution {
    /// Whether the target lies within the root and may be listed.
    within_root: bool,
    /// The canonical in-root directory to traverse, when the target is one.
    directory: Option<PathBuf>,
}

/// Resolves a symlink once for both its listed and traversed decisions.
///
/// A failed `read_link` or canonicalization is an omission; a successfully
/// resolved target outside the root is a policy exclusion. Canonicalizing
/// before comparing avoids textual-prefix mistakes, while the public contract
/// remains best-effort against concurrent replacement.
fn resolve_symlink(
    dir: &Path,
    path: &Path,
    canonical_root: &Path,
) -> io::Result<SymlinkResolution> {
    let resolved = resolve_symlink_path(dir, path)?;
    let canon = resolved.canonicalize()?;
    let within_root = canon.starts_with(canonical_root);
    let directory = (within_root && canon.is_dir()).then_some(canon);
    Ok(SymlinkResolution {
        within_root,
        directory,
    })
}

/// Recurses into a subdirectory that has already been accepted.
///
/// Kept separate from the symlink path so the two ways of descending are
/// readable side by side; `depth` is the depth of `path` itself, and
/// `walk_recursive` is what bounds it against `options.max_depth`.
fn handle_directory_entry<E, P>(
    path: &Path,
    logical_path: &Path,
    depth: usize,
    ctx: &mut WalkContext<'_, E, P>,
) -> io::Result<()>
where
    E: Record,
    P: FnMut(&Path, FileKind) -> Descend,
{
    walk_recursive(path, logical_path, depth, ctx)
}

/// Descends through a symlink entry when the options allow it.
///
/// The link is followed only when `follow_symlinks` is set *and* the resolved
/// target stays within the root policy, so enabling the option widens the walk to
/// links within the root and never to the rest of the filesystem. `depth` is
/// the link's own depth, so a chain of links cannot buy extra levels.
fn handle_symlink_entry<E, P>(
    target_dir: Option<PathBuf>,
    logical_path: &Path,
    depth: usize,
    ctx: &mut WalkContext<'_, E, P>,
) -> io::Result<()>
where
    E: Record,
    P: FnMut(&Path, FileKind) -> Descend,
{
    if ctx.options.follow_symlinks
        && let Some(target_dir) = target_dir
    {
        walk_recursive(&target_dir, logical_path, depth, ctx)?;
    }
    Ok(())
}

/// Offers one admitted entry to the caller's predicate and records it.
///
/// The entry is recorded under every answer; the answer is whether the walk
/// may go below it. A stop is latched on the context, which every loop in the
/// engine checks before it reads anything further.
fn admit<E, P>(
    ctx: &mut WalkContext<'_, E, P>,
    logical_path: PathBuf,
    kind: FileKind,
    stat: E::Stat,
) -> bool
where
    E: Record,
    P: FnMut(&Path, FileKind) -> Descend,
{
    let decision = (ctx.prune)(&logical_path, kind);
    ctx.out
        .push(E::record(logical_path, kind, stat, ctx.root_components));
    match decision {
        Descend::Enter => true,
        Descend::Skip => false,
        Descend::Stop => {
            ctx.stopped = true;
            false
        }
    }
}

/// Classifies one directory entry and records it under the walk's policy.
///
/// Directories are recorded and then descended into, symlinks are recorded and
/// possibly followed, and regular files are recorded; the caller's predicate
/// decides, per admitted entry, whether the walk goes below it. An entry whose
/// file type cannot be read is an omission under both modes — refused by
/// strict, reported by tolerant — never a silent skip: the walk observed a
/// name it cannot classify, and guessing "file" would misreport a directory
/// the walk then fails to descend into.
fn process_entry<E, P>(
    entry: DirEntry,
    dir: &Path,
    logical_dir: &Path,
    current_depth: usize,
    ctx: &mut WalkContext<'_, E, P>,
) -> io::Result<()>
where
    E: Record,
    P: FnMut(&Path, FileKind) -> Descend,
{
    let path = entry.path();
    let logical_path = logical_dir.join(entry.file_name());
    let stat = match E::stat(&entry) {
        Ok(stat) => stat,
        Err(error) => {
            return omit(
                ctx,
                WalkOmission {
                    path: logical_path,
                    stage: OmissionStage::EntryType,
                    error,
                },
            );
        }
    };
    let file_type = E::file_type(&stat);
    let kind = FileKind::of(file_type);

    // One level deeper than the directory being read. `current_depth` is
    // already bounded by `options.max_depth` when the walk recurses, and a
    // directory tree cannot be deeper than `usize::MAX` levels, so this cannot
    // saturate.
    let next_depth = current_depth.saturating_add(1);
    if file_type.is_dir() {
        if admit(ctx, logical_path.clone(), kind, stat) {
            handle_directory_entry(&path, &logical_path, next_depth, ctx)?;
        }
    } else if file_type.is_symlink() {
        let resolution = match resolve_symlink(dir, &path, ctx.canonical_root) {
            Ok(resolution) => resolution,
            Err(error) => {
                omit(
                    ctx,
                    WalkOmission {
                        path: logical_path.clone(),
                        stage: OmissionStage::SymlinkTarget,
                        error,
                    },
                )?;
                return Ok(());
            }
        };
        // A link outside the root is a policy exclusion: never listed, never
        // offered to the predicate, and its `directory` is `None`.
        if resolution.within_root && admit(ctx, logical_path.clone(), kind, stat) {
            handle_symlink_entry(resolution.directory, &logical_path, next_depth, ctx)?;
        }
    } else {
        admit(ctx, logical_path, kind, stat);
    }
    Ok(())
}

/// Checks that `dir` currently resolves to an in-root directory.
///
/// Returns the canonical path string observed by this path-based check. Failure
/// means the directory cannot be canonicalised or resolves outside the
/// root. The caller treats that as an omission — refused by strict, reported
/// by tolerant. This is not a filesystem-object identity, and a later
/// path-based read is not bound to this pathname; the
/// surrounding before/after checks are best-effort only.
fn check_directory_path(dir: &Path, canonical_root: &Path) -> io::Result<PathBuf> {
    let canon = dir.canonicalize()?;
    if !canon.starts_with(canonical_root) {
        let refusal = Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "walked directory {} resolves outside the root",
                dir.display()
            ),
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "check_directory_path: returning an error to the caller");
        return refusal;
    }
    Ok(canon)
}

/// Walks `dir` depth-first, appending every entry the policy admits.
///
/// The recursion has four bounds, and all four are needed for
/// INV-FS-SAFE-WALK: `options.max_depth` caps how deep the walk goes, the
/// canonical-visited set makes a symlink loop terminate, the root check
/// decides which symlinks are followed at all, and the
/// before/after canonical-path checks determine whether what was read may be
/// reported. `dir` is the directory currently being read and `current_depth`
/// is its depth; the root call is depth zero, so `max_depth: 0` reports only
/// the root's own children.
///
/// The read happens through the original path, so reported entry paths keep
/// the spelling the caller walked. The canonical path string is checked
/// before and after; if those observations differ, entries read in between
/// are discarded. This does not establish filesystem-object identity. A
/// swap forth and back inside the window, or replacement at the same path,
/// remains invisible. A handle-relative platform API is required to close
/// that threat model; the public contract says trusted-tree listing.
fn walk_recursive<E, P>(
    dir: &Path,
    logical_dir: &Path,
    current_depth: usize,
    ctx: &mut WalkContext<'_, E, P>,
) -> io::Result<()>
where
    E: Record,
    P: FnMut(&Path, FileKind) -> Descend,
{
    if current_depth > ctx.options.max_depth || ctx.budget_exhausted || ctx.stopped {
        return Ok(());
    }
    if !track_canonical_visit(dir, ctx.visited) {
        return Ok(());
    }
    let canonical_path = match check_directory_path(dir, ctx.canonical_root) {
        Ok(canonical_path) => canonical_path,
        Err(error) => {
            return omit(
                ctx,
                WalkOmission {
                    path: logical_dir.to_path_buf(),
                    stage: OmissionStage::DirectoryResolution,
                    error,
                },
            );
        }
    };
    let out_len = ctx.out.len();
    let omissions_len = ctx.omissions.len();
    let listed = match read_sorted_entries(dir, logical_dir, ctx.options.sort_alphabetically, ctx) {
        Ok(entries) => entries,
        Err(error) => {
            if error
                .get_ref()
                .is_some_and(|source| source.is::<WalkFailure>())
            {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "walk_recursive: returning an error to the caller");
                return refusal;
            }
            return omit(
                ctx,
                WalkOmission {
                    path: logical_dir.to_path_buf(),
                    stage: OmissionStage::ReadEntries,
                    error,
                },
            );
        }
    };
    let budget_reached_during_listing = ctx.budget_exhausted;
    for entry in listed {
        if ctx.stopped || (ctx.budget_exhausted && !budget_reached_during_listing) {
            break;
        }
        process_entry(entry, dir, logical_dir, current_depth, ctx)?;
    }
    // The second canonical-path observation: anything read above is reported
    // only if the path string still resolves the same way. On a
    // mismatch the entries and any omissions recorded for them are rolled
    // back — a partial attribution to a path with changed canonical
    // resolution is exactly the
    // outside-content-under-inside-names failure — and the directory itself
    // is recorded as the omission.
    match check_directory_path(dir, ctx.canonical_root) {
        Ok(again) if again == canonical_path => Ok(()),
        changed => {
            ctx.out.truncate(out_len);
            ctx.omissions.truncate(omissions_len);
            let error = match changed {
                Ok(_) => io::Error::new(
                    io::ErrorKind::InvalidData,
                    "walked directory changed canonical path during the read",
                ),
                Err(error) => error,
            };
            omit(
                ctx,
                WalkOmission {
                    path: logical_dir.to_path_buf(),
                    stage: OmissionStage::DirectoryResolution,
                    error,
                },
            )
        }
    }
}
