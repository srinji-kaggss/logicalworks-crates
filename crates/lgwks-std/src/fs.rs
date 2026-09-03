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

fn track_canonical_visit(dir: &Path, visited_canonical: &mut HashSet<PathBuf>) -> bool {
    if let Ok(canonical) = dir.canonicalize()
        && !visited_canonical.insert(canonical)
    {
        return false;
    }
    true
}

fn read_sorted_entries(dir: &Path, sort_alphabetically: bool) -> io::Result<Vec<DirEntry>> {
    let mut entries: Vec<DirEntry> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    if sort_alphabetically {
        entries.sort_by_key(|entry| entry.file_name());
    }
    Ok(entries)
}

fn resolve_symlink_path(dir: &Path, path: &Path) -> Option<PathBuf> {
    let target = fs::read_link(path).ok()?;
    if target.is_relative() {
        Some(dir.join(target))
    } else {
        Some(target)
    }
}

fn check_sandbox(resolved: &Path, canonical_root: &Option<PathBuf>) -> Option<PathBuf> {
    let canon = resolved.canonicalize().ok()?;
    if let Some(canon_root) = canonical_root
        && !canon.starts_with(canon_root)
    {
        return None;
    }
    if canon.is_dir() { Some(canon) } else { None }
}

fn resolve_symlink_target(
    dir: &Path,
    path: &Path,
    canonical_root: &Option<PathBuf>,
) -> Option<PathBuf> {
    let resolved = resolve_symlink_path(dir, path)?;
    check_sandbox(&resolved, canonical_root)
}

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

fn symlink_is_within_sandbox(dir: &Path, path: &Path, canonical_root: &Option<PathBuf>) -> bool {
    let Some(canon_root) = canonical_root else {
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

    let next_depth = current_depth + 1;
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs as stdfs;

    fn tmp_tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        stdfs::create_dir_all(root.join("a/b/c")).unwrap();
        stdfs::write(root.join("a/f.txt"), b"").unwrap();
        stdfs::write(root.join("a/b/g.txt"), b"").unwrap();
        stdfs::write(root.join("a/b/c/h.txt"), b"").unwrap();
        tmp
    }

    #[test]
    fn walks_directory_deterministically() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_dir = manifest_dir.join("src");
        let entries1 = walk_dir(&src_dir, &WalkOptions::default()).expect("walks src");
        let entries2 = walk_dir(&src_dir, &WalkOptions::default()).expect("walks src");
        assert!(!entries1.is_empty());
        assert_eq!(entries1, entries2);
    }

    #[test]
    fn max_depth_zero_returns_only_immediate_children() {
        let tmp = tmp_tree();
        let opts = WalkOptions {
            max_depth: 0,
            ..Default::default()
        };
        let entries = walk_dir(tmp.path().join("a"), &opts).unwrap();
        for e in &entries {
            assert_eq!(e.parent().unwrap(), tmp.path().join("a"));
        }
    }

    #[test]
    fn max_depth_bounds_traversal() {
        let tmp = tmp_tree();
        let opts = WalkOptions {
            max_depth: 1,
            ..Default::default()
        };
        let entries = walk_dir(tmp.path().join("a"), &opts).unwrap();
        let deepest: Vec<_> = entries.iter().filter(|p| p.ends_with("h.txt")).collect();
        assert!(deepest.is_empty(), "depth-2 file h.txt should be excluded");
    }

    #[test]
    #[cfg(unix)]
    fn symlink_loop_terminates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        stdfs::create_dir(root.join("d")).unwrap();
        std::os::unix::fs::symlink(root.join("d"), root.join("d/loop")).unwrap();
        let opts = WalkOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let entries = walk_dir(root, &opts).unwrap();
        assert!(!entries.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn symlink_outside_root_is_rejected() {
        let inner = tempfile::tempdir().unwrap();
        let outer = tempfile::tempdir().unwrap();
        stdfs::write(outer.path().join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(outer.path(), inner.path().join("escape")).unwrap();
        let opts = WalkOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let entries = walk_dir(inner.path(), &opts).unwrap();
        let escaped: Vec<_> = entries
            .iter()
            .filter(|p| p.to_string_lossy().contains("secret"))
            .collect();
        assert!(
            escaped.is_empty(),
            "sandbox escape: symlink outside root must be rejected"
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlink_outside_root_excluded_from_output() {
        let inner = tempfile::tempdir().unwrap();
        let outer = tempfile::tempdir().unwrap();
        stdfs::write(outer.path().join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(outer.path(), inner.path().join("escape")).unwrap();
        let opts = WalkOptions {
            follow_symlinks: false,
            ..Default::default()
        };
        let entries = walk_dir(inner.path(), &opts).unwrap();
        let escape_entry: Vec<_> = entries
            .iter()
            .filter(|p| p.to_string_lossy().contains("escape"))
            .collect();
        assert!(
            escape_entry.is_empty(),
            "symlink pointing outside sandbox must not appear in output"
        );
    }

    #[test]
    fn nonexistent_directory_returns_error() {
        let result = walk_dir(
            "/nonexistent-path-that-does-not-exist",
            &WalkOptions::default(),
        );
        assert!(result.is_err());
    }
}
