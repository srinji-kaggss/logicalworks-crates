//! `fs` owns recursive filesystem walking and directory traversal, enforcing
//! INV-FS-SAFE-WALK: directory walking respects depth bounds, handles symlink
//! loops defensively, applies best-effort root-bounded symlink policy, and
//! requires zero external dependencies like `walkdir`. This path-based API is
//! for trusted trees, not a race-safe sandbox against hostile concurrent path
//! replacement. Use a handle-relative OS capability API for that threat model.

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
    /// A directory whose identity (canonical path within the root) could
    /// not be established before the read or no longer held after it.
    DirectoryIdentity,
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
/// A report with entries and no omissions is a complete scan. A report with
/// omissions is a partial one, and [`WalkReport::is_complete`] is the one
/// question that distinguishes them: no consumer can read completeness off
/// the entry list alone.
#[derive(Debug)]
#[non_exhaustive]
pub struct WalkReport {
    /// Every entry the policy admitted, in walk order.
    entries: Vec<PathBuf>,
    /// Every place the walk came back short, in walk order.
    omissions: Vec<WalkOmission>,
}

impl WalkReport {
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

    /// Whether the walk covered the whole tree: no omissions recorded.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.omissions.is_empty()
    }
}

/// How the walk engine answers an omission.
///
/// Strict is the fail-closed mode: the first omission refuses the whole
/// walk. Tolerant records the omission and continues with the rest of the
/// tree. The root's own identity is not omittable in either mode: without a
/// resolved root there is no sandbox to be inside of, so both modes refuse.
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
}

/// Recursively walks `root` according to `options`, returning all matching entries.
///
/// This is the strict walk: any omission — an unreadable directory entry,
/// an unstated file type, a directory whose identity cannot be proved —
/// refuses the whole walk rather than returning a silent subset. The root
/// itself must resolve: an unresolvable root is an error, never a walk with
/// no root identity. Successfully resolved symlink targets outside the root
/// and already visited directories are policy exclusions; unresolved symlink
/// targets are omissions. Use
/// [`walk_dir_tolerant`] for the same walk with omissions reported instead
/// of refused.
pub fn walk_dir(root: impl AsRef<Path>, options: &WalkOptions) -> io::Result<Vec<PathBuf>> {
    let canonical_root = root.as_ref().canonicalize()?;
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut omissions = Vec::new();
    let mut ctx = WalkContext {
        canonical_root: &canonical_root,
        options,
        mode: WalkMode::Strict,
        out: &mut out,
        omissions: &mut omissions,
        visited: &mut visited,
    };
    walk_recursive(root.as_ref(), 0, &mut ctx)?;
    debug_assert!(
        ctx.omissions.is_empty(),
        "strict mode records no omissions: the first one returns"
    );
    Ok(out)
}

/// Recursively walks `root` according to `options`, reporting omissions.
///
/// The tolerant walk returns every admitted entry together with every place
/// it came back short. [`WalkReport::is_complete`] tells a complete scan
/// from a partial one. Like the strict walk, an unresolvable root refuses:
/// tolerance covers coverage loss inside the tree, not the absence of the
/// tree's own identity.
pub fn walk_dir_tolerant(root: impl AsRef<Path>, options: &WalkOptions) -> io::Result<WalkReport> {
    let canonical_root = root.as_ref().canonicalize()?;
    let mut report = WalkReport {
        entries: Vec::new(),
        omissions: Vec::new(),
    };
    let mut visited = HashSet::new();
    let mut ctx = WalkContext {
        canonical_root: &canonical_root,
        options,
        mode: WalkMode::Tolerant,
        out: &mut report.entries,
        omissions: &mut report.omissions,
        visited: &mut visited,
    };
    walk_recursive(root.as_ref(), 0, &mut ctx)?;
    Ok(report)
}

/// Refuses an omission under `mode`: strict returns the error, tolerant
/// records it and continues.
///
/// The two arms are the whole of the strict/tolerant contract. Every
/// omission in the engine passes through here, so no new omission site can
/// silently pick a mode: it states one by calling this.
fn omit(
    mode: WalkMode,
    omissions: &mut Vec<WalkOmission>,
    omission: WalkOmission,
) -> io::Result<()> {
    if mode == WalkMode::Strict {
        return Err(omission.error);
    }
    omissions.push(omission);
    Ok(())
}

/// Records that `dir` has been entered, returning whether it is newly visited.
///
/// The set holds canonical paths, so the same directory reached through two
/// different symlinks (or through `..`) is recognised as one node and a symlink
/// loop terminates instead of recursing until the stack is exhausted. A path
/// that cannot be canonicalised is treated as new rather than skipped, because
/// the caller has already checked it against the sandbox.
fn track_canonical_visit(dir: &Path, visited_canonical: &mut HashSet<PathBuf>) -> bool {
    if let Ok(canonical) = dir.canonicalize()
        && !visited_canonical.insert(canonical)
    {
        return false;
    }
    true
}

/// Reads `dir` and returns its entries, optionally in filename order,
/// together with the items that arrived as errors.
///
/// A failed item carries no path of its own, so the errors come back
/// alongside the entries for the caller to attribute to `dir`. Sorting by
/// file name is what makes the walk's output deterministic across
/// filesystems, which the determinism test below pins.
fn read_sorted_entries(
    dir: &Path,
    sort_alphabetically: bool,
) -> io::Result<(Vec<DirEntry>, Vec<io::Error>)> {
    let mut entries = Vec::new();
    let mut item_errors = Vec::new();
    for item in fs::read_dir(dir)? {
        match item {
            Ok(entry) => entries.push(entry),
            Err(error) => item_errors.push(error),
        }
    }
    if sort_alphabetically {
        entries.sort_by_key(|entry| entry.file_name());
    }
    Ok((entries, item_errors))
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
fn handle_directory_entry(path: &Path, depth: usize, ctx: &mut WalkContext<'_>) -> io::Result<()> {
    walk_recursive(path, depth, ctx)
}

/// Descends through a symlink entry when the options allow it.
///
/// The link is followed only when `follow_symlinks` is set *and* the resolved
/// target stays inside the sandbox, so enabling the option widens the walk to
/// links within the root and never to the rest of the filesystem. `depth` is
/// the link's own depth, so a chain of links cannot buy extra levels.
fn handle_symlink_entry(
    target_dir: Option<PathBuf>,
    depth: usize,
    ctx: &mut WalkContext<'_>,
) -> io::Result<()> {
    if ctx.options.follow_symlinks
        && let Some(target_dir) = target_dir
    {
        walk_recursive(&target_dir, depth, ctx)?;
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
    current_depth: usize,
    ctx: &mut WalkContext<'_>,
) -> io::Result<()> {
    let path = entry.path();
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(error) => {
            return omit(
                ctx.mode,
                ctx.omissions,
                WalkOmission {
                    path,
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
        ctx.out.push(path.clone());
        handle_directory_entry(&path, next_depth, ctx)?;
    } else if file_type.is_symlink() {
        let resolution = match resolve_symlink(dir, &path, ctx.canonical_root) {
            Ok(resolution) => resolution,
            Err(error) => {
                omit(
                    ctx.mode,
                    ctx.omissions,
                    WalkOmission {
                        path: path.clone(),
                        stage: OmissionStage::SymlinkTarget,
                        error,
                    },
                )?;
                return Ok(());
            }
        };
        if resolution.within_root {
            ctx.out.push(path.clone());
        }
        handle_symlink_entry(resolution.directory, next_depth, ctx)?;
    } else {
        ctx.out.push(path.clone());
    }
    Ok(())
}

/// Proves `dir` is still the directory inside the root it was a moment ago.
///
/// Returns the canonical path the read that follows must be attributed to.
/// Failure means the directory cannot be canonicalised, resolves outside the
/// root, or resolves differently than it did before: a concurrently swapped
/// directory, a `..` escape, or a vanished path. The caller treats that as an
/// omission — refused by strict, reported by tolerant — and, critically,
/// reports nothing read under a disproved identity.
fn verify_directory_identity(dir: &Path, canonical_root: &Path) -> io::Result<PathBuf> {
    let canon = dir.canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot resolve walked directory {}: {error}", dir.display()),
        )
    })?;
    if !canon.starts_with(canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "walked directory {} resolves outside the root",
                dir.display()
            ),
        ));
    }
    Ok(canon)
}

/// Walks `dir` depth-first, appending every entry the policy admits.
///
/// The recursion has four bounds, and all four are needed for
/// INV-FS-SAFE-WALK: `options.max_depth` caps how deep the walk goes, the
/// canonical-visited set makes a symlink loop terminate, the sandbox check
/// in `check_sandbox` decides which symlinks are followed at all, and the
/// identity re-verification below decides whether what was read may be
/// reported. `dir` is the directory currently being read and `current_depth`
/// is its depth; the root call is depth zero, so `max_depth: 0` reports only
/// the root's own children.
///
/// The read happens through the original path, so reported entry paths keep
/// the spelling the caller walked. Identity is proved before and after: a
/// directory swapped for a symlink between the two proves differently, and
/// everything read under the disproved identity is discarded, not reported.
/// A swap forth and back inside the window still defeats this — only a
/// handle-relative traversal (openat2 beneath) closes that — and the docs
/// say so rather than claiming a security containment the code cannot hold.
fn walk_recursive(dir: &Path, current_depth: usize, ctx: &mut WalkContext<'_>) -> io::Result<()> {
    if current_depth > ctx.options.max_depth {
        return Ok(());
    }
    if !track_canonical_visit(dir, ctx.visited) {
        return Ok(());
    }
    let identity = match verify_directory_identity(dir, ctx.canonical_root) {
        Ok(identity) => identity,
        Err(error) => {
            return omit(
                ctx.mode,
                ctx.omissions,
                WalkOmission {
                    path: dir.to_path_buf(),
                    stage: OmissionStage::DirectoryIdentity,
                    error,
                },
            );
        }
    };
    let out_len = ctx.out.len();
    let omissions_len = ctx.omissions.len();
    let listed = match read_sorted_entries(dir, ctx.options.sort_alphabetically) {
        Ok((entries, item_errors)) => {
            for error in item_errors {
                omit(
                    ctx.mode,
                    ctx.omissions,
                    WalkOmission {
                        path: dir.to_path_buf(),
                        stage: OmissionStage::ReadEntries,
                        error,
                    },
                )?;
            }
            entries
        }
        Err(error) => {
            return omit(
                ctx.mode,
                ctx.omissions,
                WalkOmission {
                    path: dir.to_path_buf(),
                    stage: OmissionStage::ReadEntries,
                    error,
                },
            );
        }
    };
    for entry in listed {
        process_entry(entry, dir, current_depth, ctx)?;
    }
    // The second proof: anything read above is reported only if the
    // directory still resolves to the identity it was read under. On a
    // mismatch the entries and any omissions recorded for them are rolled
    // back — a partial attribution to a disproved directory is exactly the
    // outside-content-under-inside-names failure — and the directory itself
    // is recorded as the omission.
    match verify_directory_identity(dir, ctx.canonical_root) {
        Ok(again) if again == identity => Ok(()),
        Ok(_) | Err(_) => {
            ctx.out.truncate(out_len);
            ctx.omissions.truncate(omissions_len);
            omit(
                ctx.mode,
                ctx.omissions,
                WalkOmission {
                    path: dir.to_path_buf(),
                    stage: OmissionStage::DirectoryIdentity,
                    error: io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "walked directory {} changed identity during the read",
                            dir.display()
                        ),
                    ),
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
        let entries = walk_dir(tmp.path().join("a"), &opts)?;
        for entry in &entries {
            assert_eq!(entry.parent(), Some(tmp.path().join("a").as_path()));
        }
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
            "sandbox escape: symlink outside root must be rejected"
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
            "symlink pointing outside sandbox must not appear in output"
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
        assert_eq!(report.omissions()[0].path, locked);
        Ok(())
    }

    #[test]
    fn tolerant_entries_match_strict_on_a_clean_tree() -> std::io::Result<()> {
        let tmp = tmp_tree()?;
        let strict = walk_dir(tmp.path().join("a"), &WalkOptions::default())?;
        let report = walk_dir_tolerant(tmp.path().join("a"), &WalkOptions::default())?;
        assert_eq!(report.entries(), strict);
        assert!(report.is_complete());
        Ok(())
    }

    // ── R15: swaps are detected, never reported ────────────────────────────

    #[test]
    fn unresolvable_root_is_refused() -> std::io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dangling = tmp.path().join("dangling");
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join("gone"), &dangling)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(tmp.path().join("gone"), &dangling)?;
        assert!(walk_dir(&dangling, &WalkOptions::default()).is_err());
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
        assert_eq!(report.omissions()[0].path, link);
        assert_eq!(report.omissions()[0].stage, OmissionStage::SymlinkTarget);
        assert_eq!(
            report.omissions()[0].error.kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!report.is_complete());
        Ok(())
    }
}
