//! INV-FS-7: the predicate walk and its per-entry lstat.

use super::*;

/// The predicate every unpruned walk here runs under.
fn enter(_path: &Path, _kind: FileKind) -> Descend {
    Descend::Enter
}

/// `tmp_tree`'s `a` directory, canonicalized: the root these tests walk.
fn tree_root(tmp: &tempfile::TempDir) -> std::io::Result<PathBuf> {
    tmp.path().join("a").canonicalize()
}

#[test]
fn entering_everything_admits_exactly_the_path_walk() -> std::io::Result<()> {
    let tmp = tmp_tree()?;
    let root = tree_root(&tmp)?;
    let paths = walk_dir(&root, &WalkOptions::default())?;
    let entries = walk_dir_entries(&root, &WalkOptions::default(), None, enter)?;
    let entry_paths: Vec<PathBuf> = entries.into_iter().map(WalkEntry::into_path).collect();
    assert_eq!(
        entry_paths, paths,
        "an always-enter predicate must admit the path walk's entries in its order"
    );
    let report = walk_dir_entries_tolerant(&root, &WalkOptions::default(), None, enter)?;
    assert!(
        report.is_complete() && !report.stopped(),
        "a clean always-enter walk is complete and was not stopped"
    );
    Ok(())
}

#[test]
fn a_skipped_directory_is_listed_but_never_read() -> std::io::Result<()> {
    let tmp = tmp_tree()?;
    let root = tree_root(&tmp)?;
    let skip_b = |path: &Path, _kind: FileKind| {
        if path.ends_with("b") {
            Descend::Skip
        } else {
            Descend::Enter
        }
    };
    let entries = walk_dir_entries(&root, &WalkOptions::default(), None, skip_b)?;
    let relative: Vec<&Path> = entries.iter().map(WalkEntry::relative_path).collect();
    assert_eq!(
        relative,
        [Path::new("b"), Path::new("f.txt")],
        "the skipped directory is recorded and nothing below it is"
    );
    // Two entries fit a two-entry budget only if `b`'s three descendants
    // were never listed: a listed entry is charged before it is kept.
    let limits = WalkLimits::new(2, 2, 4_096, 0);
    let bounded = walk_dir_entries(&root, &WalkOptions::default(), Some(&limits), skip_b)?;
    assert_eq!(
        bounded.len(),
        2,
        "the skipped subtree charged nothing to the budget"
    );
    assert!(
        walk_dir_entries(&root, &WalkOptions::default(), Some(&limits), enter).is_err(),
        "the same budget refuses the unpruned walk, so the bound is tight"
    );
    Ok(())
}

#[test]
fn stop_records_its_entry_and_ends_the_walk() -> std::io::Result<()> {
    let tmp = tmp_tree()?;
    let root = tree_root(&tmp)?;
    let mut offered = 0_usize;
    let report =
        walk_dir_entries_tolerant(&root, &WalkOptions::default(), None, |_path, _kind| {
            offered = offered.saturating_add(1);
            Descend::Stop
        })?;
    assert_eq!(offered, 1, "nothing is offered after a stop");
    let relative: Vec<&Path> = report
        .entries()
        .iter()
        .map(WalkEntry::relative_path)
        .collect();
    assert_eq!(
        relative,
        [Path::new("b")],
        "the stopping entry is recorded and nothing after it"
    );
    assert!(report.stopped(), "the report says the predicate stopped it");
    assert!(
        report.is_complete_within_policy(),
        "a requested stop is the caller's policy, not an omission"
    );
    Ok(())
}

#[test]
fn relative_path_joins_back_onto_the_canonical_root() -> std::io::Result<()> {
    let tmp = tmp_tree()?;
    let root = tree_root(&tmp)?;
    let entries = walk_dir_entries(tmp.path().join("a"), &WalkOptions::default(), None, enter)?;
    assert_eq!(entries.len(), 5, "a holds five entries");
    for entry in &entries {
        let relative = entry.relative_path();
        assert!(
            relative.is_relative() && relative.components().count() >= 1,
            "{} is not a non-empty relative path",
            relative.display()
        );
        assert_eq!(
            root.join(relative),
            entry.path(),
            "the relative path joins back onto the canonical root"
        );
    }
    Ok(())
}

#[test]
fn the_metadata_is_the_walks_snapshot_not_a_later_stat() -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = tempfile::tempdir()?;
    let file = tmp.path().join("grows");
    stdfs::write(&file, b"abc")?;
    let entries = walk_dir_entries(tmp.path(), &WalkOptions::default(), None, enter)?;
    stdfs::OpenOptions::new()
        .append(true)
        .open(&file)?
        .write_all(b"defgh")?;
    let entry = entries
        .first()
        .ok_or_else(|| io::Error::other("the walk lost the one file"))?;
    assert_eq!(
        entry.metadata().len(),
        3,
        "the entry carries the size the walk read, not one read on access"
    );
    assert_eq!(
        stdfs::symlink_metadata(&file)?.len(),
        8,
        "the file really did grow afterwards"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn an_entry_carries_the_lstat_of_a_symlink_not_its_target() -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let tmp = tempfile::tempdir()?;
    stdfs::write(tmp.path().join("target.txt"), b"target bytes")?;
    std::os::unix::fs::symlink("target.txt", tmp.path().join("link"))?;
    let entries = walk_dir_entries(tmp.path(), &WalkOptions::default(), None, enter)?;
    let link = entries
        .iter()
        .find(|entry| entry.relative_path() == Path::new("link"))
        .ok_or_else(|| io::Error::other("the in-root link was not admitted"))?;
    assert_eq!(
        link.kind(),
        FileKind::Symlink,
        "a link is classified as one"
    );
    assert!(
        link.metadata().file_type().is_symlink(),
        "the metadata is the link's own lstat"
    );
    assert_eq!(
        link.metadata().ino(),
        stdfs::symlink_metadata(tmp.path().join("link"))?.ino(),
        "the inode is the link's"
    );
    assert_ne!(
        link.metadata().ino(),
        stdfs::metadata(tmp.path().join("target.txt"))?.ino(),
        "the inode is not the target's"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_socket_is_classified_as_a_socket() -> std::io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let _listener = std::os::unix::net::UnixListener::bind(tmp.path().join("sock"))?;
    let entries = walk_dir_entries(tmp.path(), &WalkOptions::default(), None, enter)?;
    let kinds: Vec<FileKind> = entries.iter().map(WalkEntry::kind).collect();
    assert_eq!(
        kinds,
        [FileKind::Socket],
        "a socket is named, not guessed as a file"
    );
    Ok(())
}

/// A directory the walk may list but not search: its names are readable
/// and its entries cannot be stated.
#[test]
#[cfg(unix)]
fn an_entry_the_walk_cannot_lstat_is_an_entry_type_omission() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir()?;
    let listed = tmp.path().join("listed");
    stdfs::create_dir(&listed)?;
    stdfs::write(listed.join("inside"), b"")?;
    stdfs::set_permissions(&listed, stdfs::Permissions::from_mode(0o444))?;
    let inert = stdfs::symlink_metadata(listed.join("inside")).is_ok();
    let report = walk_dir_entries_tolerant(tmp.path(), &WalkOptions::default(), None, enter);
    let strict = walk_dir_entries(tmp.path(), &WalkOptions::default(), None, enter);
    stdfs::set_permissions(&listed, stdfs::Permissions::from_mode(0o755))?;
    if inert {
        return Ok(());
    }
    let report = report?;
    let omitted: Vec<(&Path, OmissionStage)> = report
        .omissions()
        .iter()
        .map(|omission| (omission.path.as_path(), omission.stage))
        .collect();
    let inside = tmp.path().canonicalize()?.join("listed/inside");
    assert_eq!(
        omitted,
        [(inside.as_path(), OmissionStage::EntryType)],
        "a name the walk listed but could not lstat is an omission, never dropped"
    );
    let error = strict
        .err()
        .ok_or_else(|| io::Error::other("strict accepted an entry it could not lstat"))?;
    assert_eq!(
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WalkFailure>())
            .map(WalkFailure::stage),
        Some(OmissionStage::EntryType),
        "strict refuses at the entry-type stage"
    );
    Ok(())
}
