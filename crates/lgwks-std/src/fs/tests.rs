//! Tests of the path walks, the metadata walk, omissions, symlink policy and limits.

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

// ── INV-FS-7: the predicate walk and its per-entry lstat ───────────────

mod predicate;
