//! `fs` owns recursive filesystem walking and directory traversal, enforcing
//! INV-FS-SAFE-WALK: directory walking respects depth bounds, handles symlink
//! loops defensively, stays within root sandbox bounds, and requires zero
//! external dependencies like `walkdir`.

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

/// Recursively walks `root` according to `options`, returning all matching entries.
pub fn walk_dir(root: impl AsRef<Path>, options: &WalkOptions) -> io::Result<Vec<PathBuf>> {
    let root_path = root.as_ref();
    let canonical_root = root_path.canonicalize().ok();
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk_recursive(
        root_path,
        &canonical_root,
        0,
        options,
        &mut out,
        &mut visited,
    )?;
    Ok(out)
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

/// Reads `dir` and returns its entries, optionally in filename order.
///
/// Entries whose metadata cannot be read are dropped rather than failing the
/// whole walk: a directory that vanished mid-traversal, or one this process may
/// list but not stat, must not abort a scan of the rest of the tree. Sorting by
/// file name is what makes the walk's output deterministic across filesystems,
/// which the determinism test below pins.
fn read_sorted_entries(dir: &Path, sort_alphabetically: bool) -> io::Result<Vec<DirEntry>> {
    let mut entries: Vec<DirEntry> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    if sort_alphabetically {
        entries.sort_by_key(|entry| entry.file_name());
    }
    Ok(entries)
}

/// Resolves a symlink's target, relative to the directory holding the link.
///
/// `read_link` returns the target exactly as stored, so a relative target is
/// meaningful only against the link's own directory. `None` means the link
/// could not be read, which is not an error the walk reports; an unreadable
/// link simply contributes nothing to the output.
fn resolve_symlink_path(dir: &Path, path: &Path) -> Option<PathBuf> {
    let target = fs::read_link(path).ok()?;
    if target.is_relative() {
        Some(dir.join(target))
    } else {
        Some(target)
    }
}

/// Canonicalises `resolved` and rejects it when it escapes the sandbox root.
///
/// Returns the canonical directory path, or `None` when the target cannot be
/// canonicalised, resolves outside `canonical_root`, or is not a directory.
/// Canonicalising before the comparison is what makes the check meaningful: a
/// textual prefix test on an uncanonicalised path is defeated by `..` or by a
/// second symlink. A `None` root means the caller gave no sandbox and only
/// directory-ness is enforced.
fn check_sandbox(resolved: &Path, canonical_root: &Option<PathBuf>) -> Option<PathBuf> {
    let canon = resolved.canonicalize().ok()?;
    if let Some(canon_root) = canonical_root.as_ref()
        && !canon.starts_with(canon_root)
    {
        return None;
    }
    if canon.is_dir() { Some(canon) } else { None }
}

/// Resolves a symlink entry and applies the sandbox rule to its target.
///
/// This is the whole of the follow-symlinks policy: resolve, then canonicalise
/// and bound. `None` means the walk must not descend through this link.
fn resolve_symlink_target(
    dir: &Path,
    path: &Path,
    canonical_root: &Option<PathBuf>,
) -> Option<PathBuf> {
    let resolved = resolve_symlink_path(dir, path)?;
    check_sandbox(&resolved, canonical_root)
}

/// Recurses into a subdirectory that has already been accepted.
///
/// Kept separate from the symlink path so the two ways of descending are
/// readable side by side; `depth` is the depth of `path` itself, and
/// `walk_recursive` is what bounds it against `options.max_depth`.
fn handle_directory_entry(
    path: &Path,
    canonical_root: &Option<PathBuf>,
    depth: usize,
    options: &WalkOptions,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    walk_recursive(path, canonical_root, depth, options, out, visited)
}

/// Descends through a symlink entry when the options allow it.
///
/// The link is followed only when `follow_symlinks` is set *and* the resolved
/// target stays inside the sandbox, so enabling the option widens the walk to
/// links within the root and never to the rest of the filesystem. `depth` is
/// the link's own depth, so a chain of links cannot buy extra levels.
fn handle_symlink_entry(
    dir: &Path,
    path: &Path,
    canonical_root: &Option<PathBuf>,
    depth: usize,
    options: &WalkOptions,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if options.follow_symlinks
        && let Some(target_dir) = resolve_symlink_target(dir, path, canonical_root)
    {
        walk_recursive(&target_dir, canonical_root, depth, options, out, visited)?;
    }
    Ok(())
}

/// Whether a symlink entry may be reported without following it.
///
/// A link is listed only when its target resolves inside the sandbox. This is
/// a separate question from whether the walk descends through it: a symlink
/// whose target is outside the root is neither followed nor reported, so a
/// caller cannot learn the existence of paths outside the tree it asked about.
fn symlink_is_within_sandbox(dir: &Path, path: &Path, canonical_root: &Option<PathBuf>) -> bool {
    let Some(canon_root) = canonical_root.as_ref() else {
        return true;
    };
    let Some(resolved) = resolve_symlink_path(dir, path) else {
        return false;
    };
    let Ok(canon) = resolved.canonicalize() else {
        return false;
    };
    canon.starts_with(canon_root)
}

/// Classifies one directory entry and records it under the walk's policy.
///
/// Directories are recorded and then descended into, symlinks are recorded and
/// possibly followed, and regular files are recorded. An entry whose file type
/// cannot be read is skipped rather than failing the walk, matching the
/// tolerance in `read_sorted_entries`.
fn process_entry(
    entry: DirEntry,
    dir: &Path,
    canonical_root: &Option<PathBuf>,
    current_depth: usize,
    options: &WalkOptions,
    out: &mut Vec<PathBuf>,
    visited_canonical: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let path = entry.path();
    let Ok(file_type) = entry.file_type() else {
        return Ok(());
    };

    // One level deeper than the directory being read. `current_depth` is
    // already bounded by `options.max_depth` when the walk recurses, and a
    // directory tree cannot be deeper than `usize::MAX` levels, so this cannot
    // saturate.
    let next_depth = current_depth.saturating_add(1);
    if file_type.is_dir() {
        out.push(path.clone());
        handle_directory_entry(
            &path,
            canonical_root,
            next_depth,
            options,
            out,
            visited_canonical,
        )?;
    } else if file_type.is_symlink() {
        if symlink_is_within_sandbox(dir, &path, canonical_root) {
            out.push(path.clone());
        }
        handle_symlink_entry(
            dir,
            &path,
            canonical_root,
            next_depth,
            options,
            out,
            visited_canonical,
        )?;
    } else {
        out.push(path.clone());
    }
    Ok(())
}

/// Walks `dir` depth-first, appending every entry the policy admits.
///
/// The recursion has three bounds, and all three are needed for
/// INV-FS-SAFE-WALK: `options.max_depth` caps how deep the walk goes, the
/// canonical-visited set makes a symlink loop terminate, and the sandbox check
/// in `check_sandbox` decides which symlinks are followed at all. `dir` is the
/// directory currently being read and `current_depth` is its depth; the root
/// call is depth zero, so `max_depth: 0` reports only the root's own children.
fn walk_recursive(
    dir: &Path,
    canonical_root: &Option<PathBuf>,
    current_depth: usize,
    options: &WalkOptions,
    out: &mut Vec<PathBuf>,
    visited_canonical: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if current_depth > options.max_depth {
        return Ok(());
    }
    if !track_canonical_visit(dir, visited_canonical) {
        return Ok(());
    }
    let entries = read_sorted_entries(dir, options.sort_alphabetically)?;
    for entry in entries {
        process_entry(
            entry,
            dir,
            canonical_root,
            current_depth,
            options,
            out,
            visited_canonical,
        )?;
    }
    Ok(())
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
}
