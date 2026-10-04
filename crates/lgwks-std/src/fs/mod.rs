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

/// Handle-relative filesystem access, for trees that are being rewritten while
/// you read them. Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(feature = "fs-raw")]
pub mod capability;

use std::collections::HashSet;
use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};

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

/// What a tolerant walk saw: every admitted entry plus every place it came
/// back short.
///
/// A report retains entries, omissions and the policy scope under which the
/// walk ran. Completeness never needs to be inferred from the entry list.
#[derive(Debug)]
#[non_exhaustive]
pub struct WalkReport {
    /// Every entry the policy admitted, in walk order.
    entries: Vec<PathBuf>,
    /// Every place the walk came back short, in walk order.
    omissions: Vec<WalkOmission>,
    /// Whether an applied depth, symlink, or resource policy excluded work.
    policy: WalkPolicy,
    /// Whether a resource budget stopped the walk before its policy scope ended.
    budget_exhausted: bool,
}

impl WalkReport {
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
        }
    }

    /// Every entry the policy admitted, in walk order.
    #[must_use]
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
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
    pub fn into_parts(self) -> (Vec<PathBuf>, Vec<WalkOmission>) {
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
}

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

/// Everything one walk call shares, threaded through the recursion.
///
/// A single context is what keeps the engine's functions under the argument
/// limit as modes and omission sinks join the traversal state: each function
/// takes the path it acts on, its depth, and the walk it belongs to.
struct WalkContext<'a> {
    /// The resolved root directory used by best-effort path checks.
    canonical_root: &'a Path,
    /// Depth bound, symlink policy and ordering.
    options: &'a WalkOptions,
    /// Whether the first omission refuses or is recorded.
    mode: WalkMode,
    /// Entries admitted so far, in walk order.
    out: &'a mut Vec<PathBuf>,
    /// Omissions recorded so far, in walk order.
    omissions: &'a mut Vec<WalkOmission>,
    /// Canonical directories already entered, for loop termination.
    visited: &'a mut HashSet<PathBuf>,
    /// Optional producer ceilings; absent only for legacy materializers.
    limits: Option<&'a WalkLimits>,
    /// Number of directory entries observed so far.
    entries_seen: usize,
    /// Cumulative path-byte charge.
    path_bytes_seen: usize,
    /// Set when tolerant traversal returns a budget-limited prefix.
    budget_exhausted: bool,
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
    let report = run_walk(root.as_ref(), options, WalkMode::Strict, None)?;
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
    run_walk(root.as_ref(), options, WalkMode::Tolerant, None)
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
    let report = run_walk(root.as_ref(), options, WalkMode::Strict, Some(limits))?;
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
    run_walk(root.as_ref(), options, WalkMode::Tolerant, Some(limits))
}

/// Runs both report modes through the same traversal engine.
fn run_walk(
    root: &Path,
    options: &WalkOptions,
    mode: WalkMode,
    limits: Option<&WalkLimits>,
) -> io::Result<WalkReport> {
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
        options,
        mode,
        out: &mut report.entries,
        omissions: &mut report.omissions,
        visited: &mut visited,
        limits,
        entries_seen: 0,
        path_bytes_seen: 0,
        budget_exhausted: false,
    };
    walk_recursive(root, &canonical_root, 0, &mut ctx)?;
    report.budget_exhausted = ctx.budget_exhausted;
    Ok(report)
}

/// Refuses an omission under `mode`: strict returns the error, tolerant
/// records it and continues.
///
/// The two arms are the whole of the strict/tolerant contract. Every
/// omission in the engine passes through here, so no new omission site can
/// silently pick a mode: it states one by calling this.
fn omit(ctx: &mut WalkContext<'_>, omission: WalkOmission) -> io::Result<()> {
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
fn charge_entry(
    ctx: &mut WalkContext<'_>,
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
fn read_sorted_entries(
    dir: &Path,
    logical_dir: &Path,
    sort_alphabetically: bool,
    ctx: &mut WalkContext<'_>,
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
fn handle_directory_entry(
    path: &Path,
    logical_path: &Path,
    depth: usize,
    ctx: &mut WalkContext<'_>,
) -> io::Result<()> {
    walk_recursive(path, logical_path, depth, ctx)
}

/// Descends through a symlink entry when the options allow it.
///
/// The link is followed only when `follow_symlinks` is set *and* the resolved
/// target stays within the root policy, so enabling the option widens the walk to
/// links within the root and never to the rest of the filesystem. `depth` is
/// the link's own depth, so a chain of links cannot buy extra levels.
fn handle_symlink_entry(
    target_dir: Option<PathBuf>,
    logical_path: &Path,
    depth: usize,
    ctx: &mut WalkContext<'_>,
) -> io::Result<()> {
    if ctx.options.follow_symlinks
        && let Some(target_dir) = target_dir
    {
        walk_recursive(&target_dir, logical_path, depth, ctx)?;
    }
    Ok(())
}

/// Classifies one directory entry and records it under the walk's policy.
///
/// Directories are recorded and then descended into, symlinks are recorded and
/// possibly followed, and regular files are recorded. An entry whose file type
/// cannot be read is an omission under both modes — refused by strict,
/// reported by tolerant — never a silent skip: the walk observed a name it
/// cannot classify, and guessing "file" would misreport a directory the walk
/// then fails to descend into.
fn process_entry(
    entry: DirEntry,
    dir: &Path,
    logical_dir: &Path,
    current_depth: usize,
    ctx: &mut WalkContext<'_>,
) -> io::Result<()> {
    let path = entry.path();
    let logical_path = logical_dir.join(entry.file_name());
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
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

    // One level deeper than the directory being read. `current_depth` is
    // already bounded by `options.max_depth` when the walk recurses, and a
    // directory tree cannot be deeper than `usize::MAX` levels, so this cannot
    // saturate.
    let next_depth = current_depth.saturating_add(1);
    if file_type.is_dir() {
        ctx.out.push(logical_path.clone());
        handle_directory_entry(&path, &logical_path, next_depth, ctx)?;
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
        if resolution.within_root {
            ctx.out.push(logical_path.clone());
        }
        handle_symlink_entry(resolution.directory, &logical_path, next_depth, ctx)?;
    } else {
        ctx.out.push(logical_path);
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
fn walk_recursive(
    dir: &Path,
    logical_dir: &Path,
    current_depth: usize,
    ctx: &mut WalkContext<'_>,
) -> io::Result<()> {
    if current_depth > ctx.options.max_depth || ctx.budget_exhausted {
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
        if ctx.budget_exhausted && !budget_reached_during_listing {
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs as stdfs;

    /// A tree of `a/f.txt`, `a/b/g.txt`, and `a/b/c/h.txt` under a temp root.
    fn tmp_tree() -> std::io::Result<tempfile::TempDir> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        stdfs::create_dir_all(root.join("a/b/c"))?;
        stdfs::write(root.join("a/f.txt"), b"")?;
        stdfs::write(root.join("a/b/g.txt"), b"")?;
        stdfs::write(root.join("a/b/c/h.txt"), b"")?;
        Ok(tmp)
    }

    // These tests return `Result` rather than unwrapping: a walk or filesystem
    // refusal reports its own `Debug` on failure, which is the same report
    // `.unwrap` would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn walks_directory_deterministically() -> std::io::Result<()> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_dir = manifest_dir.join("src");
        let entries1 = walk_dir(&src_dir, &WalkOptions::default())?;
        let entries2 = walk_dir(&src_dir, &WalkOptions::default())?;
        assert!(!entries1.is_empty());
        assert_eq!(entries1, entries2);
        Ok(())
    }

    #[test]
    fn max_depth_zero_returns_only_immediate_children() -> std::io::Result<()> {
        let tmp = tmp_tree()?;
        let opts = WalkOptions {
            max_depth: 0,
            ..Default::default()
        };
        let root = tmp.path().join("a").canonicalize()?;
        let entries = walk_dir(&root, &opts)?;
        for entry in &entries {
            assert_eq!(
                entry.parent(),
                Some(root.as_path()),
                "depth-zero paths remain under the resolved root"
            );
        }
        let report = walk_dir_tolerant(&root, &opts)?;
        assert!(
            report.is_complete_within_policy(),
            "depth exclusion is complete within the selected policy"
        );
        assert_eq!(
            report.policy().max_depth,
            0,
            "the report states its depth boundary"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn alias_order_keeps_one_absolute_logical_path_basis() -> std::io::Result<()> {
        let cwd = std::env::current_dir()?;
        let temp = tempfile::tempdir_in(&cwd)?;
        let root = temp.path().join("before");
        stdfs::create_dir_all(root.join("z-target"))?;
        stdfs::write(root.join("z-target/file"), b"content")?;
        std::os::unix::fs::symlink("z-target", root.join("b-middle"))?;
        std::os::unix::fs::symlink("b-middle", root.join("a-alias"))?;

        let options = WalkOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let absolute = walk_dir(&root, &options)?;
        let relative_root = root.strip_prefix(&cwd).map_err(io::Error::other)?;
        let relative = walk_dir(relative_root, &options)?;
        let tolerant = walk_dir_tolerant(&root, &options)?;
        assert_eq!(
            absolute, relative,
            "relative and absolute roots share one output basis"
        );
        assert_eq!(
            absolute,
            tolerant.entries(),
            "strict and tolerant clean walks agree"
        );
        assert!(
            absolute.iter().all(|path| path.is_absolute()),
            "every emitted path is absolute"
        );
        assert!(
            absolute.iter().any(|path| path.ends_with("a-alias/file")),
            "alias provenance remains visible"
        );
        assert!(
            !absolute.iter().any(|path| path.ends_with("z-target/file")),
            "canonical target is deduplicated after the earlier alias"
        );

        let after = temp.path().join("after");
        stdfs::create_dir_all(after.join("a-target"))?;
        stdfs::write(after.join("a-target/file"), b"content")?;
        std::os::unix::fs::symlink("a-target", after.join("z-alias"))?;
        let entries = walk_dir(&after, &options)?;
        assert!(
            entries.iter().any(|path| path.ends_with("a-target/file")),
            "earlier target traversal keeps its logical path"
        );
        assert!(
            !entries.iter().any(|path| path.ends_with("z-alias/file")),
            "later alias does not duplicate a visited target"
        );
        let depth_one = WalkOptions {
            max_depth: 1,
            follow_symlinks: options.follow_symlinks,
            sort_alphabetically: options.sort_alphabetically,
        };
        let limited = walk_dir_tolerant(&after, &depth_one)?;
        assert!(
            limited.is_complete_within_policy(),
            "depth exclusion is complete within its selected policy"
        );
        assert_eq!(
            limited.policy().max_depth,
            1,
            "the report exposes the applied depth"
        );
        Ok(())
    }

    #[test]
    fn bounded_walk_marks_prefix_and_strict_refusal() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        for name in ["a", "b", "c"] {
            stdfs::write(temp.path().join(name), b"x")?;
        }
        let options = WalkOptions::default();
        let limits = WalkLimits::new(2, 2, 1024, 1);
        let report = walk_dir_tolerant_bounded(temp.path(), &options, &limits)?;
        assert_eq!(
            report.entries().len(),
            2,
            "entry ceiling bounds the retained prefix"
        );
        assert!(
            report.budget_exhausted(),
            "budget exhaustion is visible in the report"
        );
        assert!(
            !report.is_complete_within_policy(),
            "a budget-limited prefix is incomplete"
        );
        let error = walk_dir_bounded(temp.path(), &options, &limits)
            .err()
            .ok_or_else(|| io::Error::other("strict bounded walk accepted an incomplete tree"))?;
        let failure = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WalkFailure>())
            .ok_or_else(|| io::Error::other("strict error did not retain WalkFailure"))?;
        assert_eq!(
            failure.stage(),
            OmissionStage::ResourceBudget,
            "strict budget refusal preserves its stage"
        );
        assert_eq!(
            failure.source_error().kind(),
            io::ErrorKind::OutOfMemory,
            "budget source remains typed as an io error"
        );
        let path_limits = WalkLimits::new(100, 100, 1, 10);
        let path_report = walk_dir_tolerant_bounded(temp.path(), &options, &path_limits)?;
        assert!(
            path_report.entries().is_empty(),
            "path-byte ceiling is charged before entry retention"
        );
        assert!(
            path_report.budget_exhausted(),
            "path-byte exhaustion is visible"
        );
        let directory_limits = WalkLimits::new(100, 1, 1024, 10);
        let directory_report = walk_dir_tolerant_bounded(temp.path(), &options, &directory_limits)?;
        assert_eq!(
            directory_report.entries().len(),
            1,
            "per-directory sorting storage is independently bounded"
        );
        assert!(
            directory_report.budget_exhausted(),
            "per-directory overflow is reported"
        );

        let long_path_root = tempfile::tempdir()?;
        let long_name = "x".repeat(180);
        stdfs::write(long_path_root.path().join(long_name), b"x")?;
        let long_path_limits = WalkLimits::new(100, 100, 128, 10);
        let long_path_report =
            walk_dir_tolerant_bounded(long_path_root.path(), &options, &long_path_limits)?;
        assert!(
            long_path_report.entries().is_empty(),
            "long entry paths are refused before materialization"
        );
        assert!(
            long_path_report.budget_exhausted(),
            "long-path budget refusal is visible"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn omission_budget_marks_report_incomplete() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        for index in 0..5 {
            let name = format!("broken-{index}");
            std::os::unix::fs::symlink(temp.path().join("missing"), temp.path().join(name))?;
        }
        let limits = WalkLimits::new(10, 10, 1024, 1);
        let report = walk_dir_tolerant_bounded(temp.path(), &WalkOptions::default(), &limits)?;
        assert_eq!(
            report.omissions().len(),
            1,
            "omission flood is capped before growth"
        );
        assert!(
            report.budget_exhausted(),
            "dropped omission is reported as budget exhaustion"
        );
        assert!(
            !report.is_complete_within_policy(),
            "budget omission cannot look complete"
        );
        Ok(())
    }

    #[test]
    fn max_depth_bounds_traversal() -> std::io::Result<()> {
        let tmp = tmp_tree()?;
        let opts = WalkOptions {
            max_depth: 1,
            ..Default::default()
        };
        let entries = walk_dir(tmp.path().join("a"), &opts)?;
        assert!(
            !entries.iter().any(|path| path.ends_with("h.txt")),
            "depth-2 file h.txt should be excluded"
        );
        let deep = tempfile::tempdir()?;
        let mut current = deep.path().to_path_buf();
        for _ in 0..24 {
            current.push("d");
            stdfs::create_dir(&current)?;
        }
        stdfs::write(current.join("leaf"), b"")?;
        let deep_entries = walk_dir(deep.path(), &opts)?;
        assert!(
            !deep_entries.iter().any(|path| path.ends_with("leaf")),
            "depth policy excludes leaves on a deep chain"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn symlink_loop_terminates() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        stdfs::create_dir(root.join("d"))?;
        std::os::unix::fs::symlink(root.join("d"), root.join("d/loop"))?;
        let opts = WalkOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let entries = walk_dir(root, &opts)?;
        assert!(!entries.is_empty());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn symlink_outside_root_is_rejected() -> std::io::Result<()> {
        let inner = tempfile::tempdir()?;
        let outer = tempfile::tempdir()?;
        stdfs::write(outer.path().join("secret.txt"), b"secret")?;
        std::os::unix::fs::symlink(outer.path(), inner.path().join("escape"))?;
        let opts = WalkOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let entries = walk_dir(inner.path(), &opts)?;
        assert!(
            !entries
                .iter()
                .any(|path| path.to_string_lossy().contains("secret")),
            "a symlink outside the root policy must be rejected"
        );
        let report = walk_dir_tolerant(inner.path(), &opts)?;
        assert!(
            report.is_complete_within_policy(),
            "outside-root symlink exclusion is complete within the declared policy"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn symlink_outside_root_excluded_from_output() -> std::io::Result<()> {
        let inner = tempfile::tempdir()?;
        let outer = tempfile::tempdir()?;
        stdfs::write(outer.path().join("secret.txt"), b"secret")?;
        std::os::unix::fs::symlink(outer.path(), inner.path().join("escape"))?;
        let opts = WalkOptions {
            follow_symlinks: false,
            ..Default::default()
        };
        let entries = walk_dir(inner.path(), &opts)?;
        assert!(
            !entries
                .iter()
                .any(|path| path.to_string_lossy().contains("escape")),
            "symlink pointing outside the root must not appear in output"
        );
        let report = walk_dir_tolerant(inner.path(), &opts)?;
        assert!(
            report.is_complete_within_policy(),
            "disabled symlink following is complete within the declared policy"
        );
        Ok(())
    }

    #[test]
    fn nonexistent_directory_returns_error() {
        let result = walk_dir(
            "/nonexistent-path-that-does-not-exist",
            &WalkOptions::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    #[cfg(all(unix, feature = "fs-raw"))]
    fn available_space_reports_positive_bytes() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let free = available_space(tmp.path())?;
        assert!(free > 0, "available space must be positive, got {free}");
        Ok(())
    }

    #[test]
    #[cfg(feature = "fs-raw")]
    fn available_space_on_missing_path_is_an_error() {
        let result = available_space("/nonexistent-path-that-does-not-exist");
        assert!(result.is_err());
    }

    // ── R16: strict refuses, tolerant reports ─────────────────────────────

    /// The parent of `locked`, or a refusal when the path has none.
    #[cfg(unix)]
    fn locked_parent(locked: &Path) -> std::io::Result<PathBuf> {
        locked.parent().map(Path::to_path_buf).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "locked dir must have a parent",
            )
        })
    }

    /// A tree with one listable file and one unlistable directory.
    /// Returns `None` when the lock does not hold (root-owned processes),
    /// so the test degrades to a pass rather than a false failure.
    #[cfg(unix)]
    fn locked_tree() -> std::io::Result<Option<(tempfile::TempDir, PathBuf)>> {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        stdfs::write(root.join("ok.txt"), b"")?;
        let locked = root.join("locked");
        stdfs::create_dir(&locked)?;
        stdfs::write(locked.join("secret.txt"), b"")?;
        stdfs::set_permissions(&locked, stdfs::Permissions::from_mode(0o000))?;
        if stdfs::read_dir(&locked).is_ok() {
            stdfs::set_permissions(&locked, stdfs::Permissions::from_mode(0o755))?;
            return Ok(None);
        }
        Ok(Some((tmp, locked)))
    }

    #[cfg(unix)]
    fn unlock(locked: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        stdfs::set_permissions(locked, stdfs::Permissions::from_mode(0o755))
    }

    #[test]
    #[cfg(unix)]
    fn strict_refuses_a_directory_it_cannot_list() -> std::io::Result<()> {
        let Some((_tmp, locked)) = locked_tree()? else {
            return Ok(());
        };
        let result = walk_dir(locked_parent(&locked)?, &WalkOptions::default());
        unlock(&locked)?;
        assert!(result.is_err(), "strict walk must refuse partial coverage");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn tolerant_reports_an_unlistable_subdirectory() -> std::io::Result<()> {
        let Some((_tmp, locked)) = locked_tree()? else {
            return Ok(());
        };
        let report = walk_dir_tolerant(locked_parent(&locked)?, &WalkOptions::default())?;
        unlock(&locked)?;
        assert!(
            report.entries().iter().any(|path| path.ends_with("ok.txt")),
            "readable entries must survive an unlistable sibling"
        );
        assert!(
            !report.is_complete(),
            "one omission must mark the report incomplete"
        );
        assert_eq!(report.omissions().len(), 1);
        assert_eq!(report.omissions()[0].stage, OmissionStage::ReadEntries);
        assert_eq!(
            report.omissions()[0].path,
            locked.canonicalize()?,
            "omission paths use the canonical-root coordinate basis"
        );
        Ok(())
    }

    #[test]
    fn tolerant_entries_match_strict_on_a_clean_tree() -> std::io::Result<()> {
        let tmp = tmp_tree()?;
        let strict = walk_dir(tmp.path().join("a"), &WalkOptions::default())?;
        let report = walk_dir_tolerant(tmp.path().join("a"), &WalkOptions::default())?;
        assert_eq!(report.entries(), strict);
        assert!(report.is_complete());
        assert!(
            report.is_complete_within_policy(),
            "clean tree is complete within its policy"
        );
        assert_eq!(
            report.policy().max_depth,
            WalkOptions::default().max_depth,
            "report carries applied depth policy"
        );
        Ok(())
    }

    // ── R15: roots fail closed and unproved symlinks are not reported ──────

    #[test]
    fn unresolvable_root_is_refused() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dangling = tmp.path().join("dangling");
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join("gone"), &dangling)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(tmp.path().join("gone"), &dangling)?;
        let error = walk_dir(&dangling, &WalkOptions::default())
            .err()
            .ok_or_else(|| io::Error::other("strict walk accepted an unresolved root"))?;
        let failure = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WalkFailure>())
            .ok_or_else(|| io::Error::other("root refusal did not retain typed context"))?;
        assert_eq!(
            failure.path(),
            dangling,
            "root failure retains the requested path"
        );
        assert_eq!(
            failure.stage(),
            OmissionStage::RootResolution,
            "root failure has its own stage"
        );
        assert!(walk_dir_tolerant(&dangling, &WalkOptions::default()).is_err());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn broken_symlink_is_a_strict_refusal_or_a_tolerant_omission() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let link = tmp.path().join("broken-link");
        std::os::unix::fs::symlink(tmp.path().join("missing-target"), &link)?;

        let strict = walk_dir(tmp.path(), &WalkOptions::default());
        let error = strict.err().ok_or_else(|| {
            std::io::Error::other("strict walk silently accepted an unresolved symlink")
        })?;
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        let failure = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WalkFailure>())
            .ok_or_else(|| io::Error::other("strict error did not retain typed context"))?;
        let canonical_link = tmp.path().canonicalize()?.join("broken-link");
        assert_eq!(
            failure.path(),
            canonical_link,
            "strict error retains the canonical-root logical link path"
        );
        assert_eq!(
            failure.stage(),
            OmissionStage::SymlinkTarget,
            "strict error retains the failure stage"
        );
        assert_eq!(
            failure.source_error().kind(),
            io::ErrorKind::NotFound,
            "strict error preserves the original source"
        );

        let report = walk_dir_tolerant(tmp.path(), &WalkOptions::default())?;
        assert_eq!(
            report.entries().len(),
            0,
            "the unproved target is not reported"
        );
        assert_eq!(
            report.omissions().len(),
            1,
            "the unread target is visible once"
        );
        assert_eq!(
            report.omissions()[0].path,
            canonical_link,
            "tolerant omission uses the same canonical-root path basis"
        );
        assert_eq!(report.omissions()[0].stage, OmissionStage::SymlinkTarget);
        assert_eq!(
            report.omissions()[0].error.kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!report.is_complete());
        Ok(())
    }
}
