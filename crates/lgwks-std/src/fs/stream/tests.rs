//! Unit tests for the streaming walk: agreement with the materializing walk,
//! lazy metadata, withheld subtrees, loop termination and per-kind omissions.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use super::{OmissionAction, OmissionPolicy, WalkOptions, walk_dir_iter};
use crate::fs::{Descend, FileKind, walk_dir_entries};

/// What a unit test reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A scratch tree under the process's temporary directory, unique to this
/// test binary.
fn scratch(name: &str) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("lgwks-walk-stream-{}-{name}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Remove a scratch tree, reporting the removal rather than dropping it.
fn scrub(dir: &Path) -> io::Result<()> {
    std::fs::remove_dir_all(dir)
}

/// One fixed tree: files, nested directories and an empty directory.
fn fixed_tree(root: &Path) -> io::Result<()> {
    std::fs::create_dir_all(root.join("alpha").join("beta"))?;
    std::fs::create_dir_all(root.join("empty"))?;
    std::fs::write(root.join("top.txt"), b"top")?;
    std::fs::write(root.join("alpha").join("mid.txt"), b"mid")?;
    std::fs::write(root.join("alpha").join("beta").join("leaf.txt"), b"leaf")?;
    Ok(())
}

/// The materializing walk's entries as relative paths to kinds.
fn materialized(root: &Path) -> io::Result<BTreeMap<PathBuf, FileKind>> {
    let options = WalkOptions::default();
    let entries = walk_dir_entries(root, &options, None, |_: &Path, _: FileKind| Descend::Enter)?;
    let mut named = BTreeMap::new();
    for entry in entries {
        named.insert(entry.relative_path().to_path_buf(), entry.kind());
    }
    Ok(named)
}

/// The streaming walk agrees with the materializing walk on a fixed tree.
#[test]
fn stream_agrees_with_the_materialized_walk() -> TestResult {
    let root = scratch("agree")?;
    fixed_tree(&root)?;
    let canonical = root.canonicalize()?;
    let expected = materialized(&root)?;
    let options = WalkOptions::default();
    let mut seen = BTreeMap::new();
    for item in walk_dir_iter(&root, &options, OmissionPolicy::default())? {
        let entry = item.map_err(|error| format!("the stream reported: {error}"))?;
        if seen
            .insert(entry.relative_path().to_path_buf(), entry.kind())
            .is_some()
        {
            return Err(format!("the stream yielded {} twice", entry.path().display()).into());
        }
        if canonical.join(entry.relative_path()) != entry.path().to_path_buf() {
            return Err(format!(
                "the relative path does not join back onto the root: {}",
                entry.path().display()
            )
            .into());
        }
    }
    assert_eq!(
        seen, expected,
        "the streaming walk admits exactly the materializing walk's entries with the same kinds"
    );
    scrub(&root)?;
    Ok(())
}

/// Metadata is read lazily: entries collected without it survive the files
/// being removed, and stating them afterwards reads as absence.
#[test]
fn metadata_is_read_lazily_not_up_front() -> TestResult {
    let root = scratch("lazy")?;
    fixed_tree(&root)?;
    let options = WalkOptions::default();
    let mut files = Vec::new();
    for item in walk_dir_iter(&root, &options, OmissionPolicy::default())? {
        let entry = item.map_err(|error| format!("the stream reported: {error}"))?;
        if entry.kind() == FileKind::File {
            files.push(entry);
        }
    }
    if files.is_empty() {
        return Err("the fixed tree holds files the stream must have yielded".into());
    }
    std::fs::remove_file(root.join("alpha").join("mid.txt"))?;
    let mut absent = 0_usize;
    for entry in &files {
        match entry.metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                absent = absent.saturating_add(1);
            }
            Err(error) => return Err(format!("stating a removed file refused: {error}").into()),
        }
    }
    assert_eq!(
        absent, 1,
        "exactly the removed file states as absent, so no metadata was read up front"
    );
    scrub(&root)?;
    Ok(())
}

/// A withheld directory is never listed: nothing under it reaches the caller.
#[test]
fn skip_children_withholds_the_subtree_unread() -> TestResult {
    let root = scratch("skip")?;
    fixed_tree(&root)?;
    std::fs::create_dir_all(root.join("vendor").join("deep"))?;
    std::fs::write(root.join("vendor").join("deep").join("bulk.txt"), b"bulk")?;
    let options = WalkOptions::default();
    let mut walk = walk_dir_iter(&root, &options, OmissionPolicy::default())?;
    let mut seen = Vec::new();
    while let Some(item) = walk.next() {
        let entry = item.map_err(|error| format!("the stream reported: {error}"))?;
        if entry.relative_path() == Path::new("vendor") {
            walk.skip_children();
        }
        seen.push(entry.relative_path().to_path_buf());
    }
    assert!(
        seen.iter().all(|path| !path.starts_with("vendor/deep")),
        "no entry under the withheld directory is yielded: {seen:?}"
    );
    assert!(
        seen.iter().any(|path| path == Path::new("vendor")),
        "the withheld directory itself is still yielded: {seen:?}"
    );
    scrub(&root)?;
    Ok(())
}

/// A symlink loop terminates under follow: the same directory reached twice
/// is entered once.
#[test]
#[cfg(unix)]
fn a_symlink_loop_terminates_under_follow() -> TestResult {
    let root = scratch("loop")?;
    fixed_tree(&root)?;
    std::os::unix::fs::symlink(root.join("alpha"), root.join("alpha").join("cycle"))?;
    let options = WalkOptions {
        follow_symlinks: true,
        ..WalkOptions::default()
    };
    let mut count = 0_usize;
    for item in walk_dir_iter(&root, &options, OmissionPolicy::default())? {
        let _ = item.map_err(|error| format!("the stream reported: {error}"))?;
        count = count.saturating_add(1);
        if count > 1_000 {
            return Err("a two-link loop must terminate after entering each directory once".into());
        }
    }
    if count == 0 {
        return Err("the walk over a non-empty tree must yield entries".into());
    }
    scrub(&root)?;
    Ok(())
}

/// A directory removed after it was listed is absence by default, not an
/// error item.
#[test]
fn not_found_is_absence_by_default() -> TestResult {
    let root = scratch("absent")?;
    std::fs::create_dir_all(root.join("later"))?;
    std::fs::write(root.join("later").join("gone.txt"), b"gone")?;
    let options = WalkOptions::default();
    let mut walk = walk_dir_iter(&root, &options, OmissionPolicy::default())?;
    let first = walk
        .next()
        .ok_or("the walk over a non-empty tree yields a first entry")?
        .map_err(|error| format!("the stream reported: {error}"))?;
    assert_eq!(
        first.relative_path(),
        Path::new("later"),
        "the single top-level directory is yielded first"
    );
    std::fs::remove_dir_all(root.join("later"))?;
    for item in walk.by_ref() {
        if let Err(error) = item {
            return Err(format!("absence must be skipped by default, got: {error}").into());
        }
    }
    scrub(&root)?;
    Ok(())
}

/// A caller that asks for absence as a report gets the omission as an error
/// item, and the walk continues after it.
#[test]
fn a_reported_absence_surfaces_as_an_error_item() -> TestResult {
    let root = scratch("report-absent")?;
    std::fs::create_dir_all(root.join("later"))?;
    std::fs::write(root.join("later").join("gone.txt"), b"gone")?;
    let options = WalkOptions {
        sort_alphabetically: false,
        ..WalkOptions::default()
    };
    let policy = OmissionPolicy::new(
        OmissionAction::Report,
        OmissionAction::Report,
        OmissionAction::Report,
    );
    let mut walk = walk_dir_iter(&root, &options, policy)?;
    let only = walk
        .next()
        .ok_or("the walk over a one-entry tree yields a first entry")?
        .map_err(|error| format!("the stream reported: {error}"))?;
    assert_eq!(
        only.relative_path(),
        Path::new("later"),
        "the single top-level directory is yielded first"
    );
    std::fs::remove_dir_all(root.join("later"))?;
    let mut reports = 0_usize;
    let mut entries = 0_usize;
    for item in walk.by_ref() {
        match item {
            Ok(_) => {
                entries = entries.saturating_add(1);
            }
            Err(error) if error.source_error().kind() == io::ErrorKind::NotFound => {
                reports = reports.saturating_add(1);
            }
            Err(error) => return Err(format!("only the absence is reported, got: {error}").into()),
        }
    }
    assert_eq!(reports, 1, "the removed directory is reported exactly once");
    assert_eq!(entries, 0, "nothing else remains to yield after the report");
    scrub(&root)?;
    Ok(())
}

/// An unresolvable root is refused before the first entry, as the strict
/// walk refuses it.
#[test]
fn an_unresolvable_root_is_refused() -> TestResult {
    let missing =
        std::env::temp_dir().join(format!("lgwks-walk-stream-missing-{}", std::process::id()));
    if missing.exists() {
        std::fs::remove_dir_all(&missing)?;
    }
    let options = WalkOptions::default();
    match walk_dir_iter(&missing, &options, OmissionPolicy::default()) {
        Ok(_) => Err("an unresolvable root must be refused".into()),
        Err(_) => Ok(()),
    }
}

/// Every entry's relative path joins back onto the canonical root.
#[test]
fn relative_paths_join_back_onto_the_root() -> TestResult {
    let root = scratch("join")?;
    fixed_tree(&root)?;
    let canonical = root.canonicalize()?;
    let options = WalkOptions::default();
    for item in walk_dir_iter(&root, &options, OmissionPolicy::default())? {
        let entry = item.map_err(|error| format!("the stream reported: {error}"))?;
        assert_eq!(
            canonical.join(entry.relative_path()),
            entry.path().to_path_buf(),
            "the relative path joins back onto the canonical root"
        );
    }
    scrub(&root)?;
    Ok(())
}
