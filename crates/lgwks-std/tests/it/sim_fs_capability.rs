//! Seeded sweeps over `fs::capability`, the crate's answer to a hostile tree.
//!
//! [`Dir`] promises four things a caller relies on without checking: a name
//! that is not one plain component never reaches the kernel, a symlink is not
//! opened unless the caller says so, what it creates is private to its owner,
//! and the directory it resolves against is the one it opened even after that
//! directory's name is repointed. Each family draws its inputs from one seed,
//! and every one of them checks a parent directory it was never given a handle
//! to, so an escape is observed rather than assumed absent. A further family
//! puts tenants on the same names concurrently, and one checks that a cloned
//! handle is an owner in its own right.

#![cfg(all(unix, feature = "fs-raw"))]

use crate::rng::Rng;

use std::collections::BTreeSet;
use std::error::Error;
use std::io::{ErrorKind, Read, Write};
use std::ops::Range;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use lgwks_std::fs::capability::{Dir, FileKind, OpenFlags};

type TestResult = Result<(), Box<dyn Error>>;

/// Seeds per range: each test sweeps one contiguous, disjoint slice.
const SEEDS_PER_RANGE: u64 = 12;

/// The `index`-th slice of the seed space.
fn seeds(index: u64) -> Range<u64> {
    let first = index.saturating_mul(SEEDS_PER_RANGE);
    first..first.saturating_add(SEEDS_PER_RANGE)
}

/// The bytes a file outside the admitted directory holds. Any read that
/// returns them has escaped.
const OUTSIDE: &[u8] = b"outside the admitted directory";

/// A parent holding the admitted directory `inside` and a file beside it that
/// no handle was opened on.
struct Arena {
    parent: tempfile::TempDir,
}

impl Arena {
    fn new() -> Result<Self, Box<dyn Error>> {
        let parent = tempfile::Builder::new()
            .prefix("lgwks-sim-fscap-")
            .tempdir()?;
        std::fs::create_dir(parent.path().join("inside"))?;
        std::fs::write(parent.path().join("outside.txt"), OUTSIDE)?;
        Ok(Self { parent })
    }

    fn inside(&self) -> std::path::PathBuf {
        self.parent.path().join("inside")
    }

    fn admit(&self) -> std::io::Result<Dir> {
        Dir::open(self.inside())
    }

    /// The parent still holds exactly what it was given, untouched.
    fn assert_parent_untouched(&self, seed: u64) -> TestResult {
        let names = names_in(self.parent.path())?;
        let expected: BTreeSet<String> = ["inside", "outside.txt"].map(String::from).into();
        assert_eq!(
            names, expected,
            "seed {seed}: something was created beside the admitted directory"
        );
        let outside = std::fs::read(self.parent.path().join("outside.txt"))?;
        assert_eq!(
            outside, OUTSIDE,
            "seed {seed}: the file outside was rewritten"
        );
        Ok(())
    }
}

/// The names in `path`, read by path: the observer is independent of [`Dir`].
fn names_in(path: &Path) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(path)? {
        let name = entry?.file_name();
        names.insert(name.to_string_lossy().into_owned());
    }
    Ok(names)
}

/// The fragments a drawn name is spelled from: plain text, and every spelling
/// that would move a resolution out of the directory or end the C string.
const FRAGMENTS: [&str; 10] = ["a", "b7", "é", "x y", "-", ".", "..", "/", "\0", "\n"];

/// A name of zero to four fragments.
fn draw_name(rng: &mut Rng) -> Result<String, Box<dyn Error>> {
    let mut name = String::new();
    for _ in 0..rng.below(5) {
        let fragment = FRAGMENTS
            .get(rng.below(FRAGMENTS.len()))
            .ok_or("a fragment inside the table")?;
        name.push_str(fragment);
    }
    Ok(name)
}

/// Whether `name` is one plain component, by the documented rule.
fn is_component(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0'))
}

/// A plain component unique to `index`, for families that need valid names.
fn plain(rng: &mut Rng, index: usize) -> String {
    format!("n{index}-{:x}", rng.next())
}

#[test]
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_range_0() -> TestResult {
    a_hostile_name_is_refused_and_nothing_leaves_the_directory(seeds(0))
}

#[test]
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_range_1() -> TestResult {
    a_hostile_name_is_refused_and_nothing_leaves_the_directory(seeds(1))
}

#[test]
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_range_2() -> TestResult {
    a_hostile_name_is_refused_and_nothing_leaves_the_directory(seeds(2))
}

#[test]
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_range_3() -> TestResult {
    a_hostile_name_is_refused_and_nothing_leaves_the_directory(seeds(3))
}

#[test]
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_range_4() -> TestResult {
    a_hostile_name_is_refused_and_nothing_leaves_the_directory(seeds(4))
}

#[test]
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_range_5() -> TestResult {
    a_hostile_name_is_refused_and_nothing_leaves_the_directory(seeds(5))
}

#[test]
fn a_symlink_never_opens_what_it_points_at_range_0() -> TestResult {
    a_symlink_never_opens_what_it_points_at(seeds(0))
}

#[test]
fn a_symlink_never_opens_what_it_points_at_range_1() -> TestResult {
    a_symlink_never_opens_what_it_points_at(seeds(1))
}

#[test]
fn a_symlink_never_opens_what_it_points_at_range_2() -> TestResult {
    a_symlink_never_opens_what_it_points_at(seeds(2))
}

#[test]
fn a_symlink_never_opens_what_it_points_at_range_3() -> TestResult {
    a_symlink_never_opens_what_it_points_at(seeds(3))
}

#[test]
fn a_symlink_never_opens_what_it_points_at_range_4() -> TestResult {
    a_symlink_never_opens_what_it_points_at(seeds(4))
}

#[test]
fn a_symlink_never_opens_what_it_points_at_range_5() -> TestResult {
    a_symlink_never_opens_what_it_points_at(seeds(5))
}

#[test]
fn created_entries_are_private_to_their_owner_range_0() -> TestResult {
    created_entries_are_private_to_their_owner(seeds(0))
}

#[test]
fn created_entries_are_private_to_their_owner_range_1() -> TestResult {
    created_entries_are_private_to_their_owner(seeds(1))
}

#[test]
fn created_entries_are_private_to_their_owner_range_2() -> TestResult {
    created_entries_are_private_to_their_owner(seeds(2))
}

#[test]
fn created_entries_are_private_to_their_owner_range_3() -> TestResult {
    created_entries_are_private_to_their_owner(seeds(3))
}

#[test]
fn created_entries_are_private_to_their_owner_range_4() -> TestResult {
    created_entries_are_private_to_their_owner(seeds(4))
}

#[test]
fn created_entries_are_private_to_their_owner_range_5() -> TestResult {
    created_entries_are_private_to_their_owner(seeds(5))
}

#[test]
fn a_held_directory_survives_its_name_being_swapped_range_0() -> TestResult {
    a_held_directory_survives_its_name_being_swapped(seeds(0))
}

#[test]
fn a_held_directory_survives_its_name_being_swapped_range_1() -> TestResult {
    a_held_directory_survives_its_name_being_swapped(seeds(1))
}

#[test]
fn a_held_directory_survives_its_name_being_swapped_range_2() -> TestResult {
    a_held_directory_survives_its_name_being_swapped(seeds(2))
}

#[test]
fn a_held_directory_survives_its_name_being_swapped_range_3() -> TestResult {
    a_held_directory_survives_its_name_being_swapped(seeds(3))
}

#[test]
fn a_held_directory_survives_its_name_being_swapped_range_4() -> TestResult {
    a_held_directory_survives_its_name_being_swapped(seeds(4))
}

#[test]
fn a_held_directory_survives_its_name_being_swapped_range_5() -> TestResult {
    a_held_directory_survives_its_name_being_swapped(seeds(5))
}

#[test]
fn tenants_sharing_names_never_see_each_other_range_0() -> TestResult {
    tenants_sharing_names_never_see_each_other(seeds(0))
}

#[test]
fn tenants_sharing_names_never_see_each_other_range_1() -> TestResult {
    tenants_sharing_names_never_see_each_other(seeds(1))
}

#[test]
fn tenants_sharing_names_never_see_each_other_range_2() -> TestResult {
    tenants_sharing_names_never_see_each_other(seeds(2))
}

#[test]
fn tenants_sharing_names_never_see_each_other_range_3() -> TestResult {
    tenants_sharing_names_never_see_each_other(seeds(3))
}

#[test]
fn tenants_sharing_names_never_see_each_other_range_4() -> TestResult {
    tenants_sharing_names_never_see_each_other(seeds(4))
}

#[test]
fn tenants_sharing_names_never_see_each_other_range_5() -> TestResult {
    tenants_sharing_names_never_see_each_other(seeds(5))
}

#[test]
fn a_cloned_handle_outlives_the_original_range_0() -> TestResult {
    a_cloned_handle_outlives_the_original(seeds(0))
}

#[test]
fn a_cloned_handle_outlives_the_original_range_1() -> TestResult {
    a_cloned_handle_outlives_the_original(seeds(1))
}

#[test]
fn a_cloned_handle_outlives_the_original_range_2() -> TestResult {
    a_cloned_handle_outlives_the_original(seeds(2))
}

#[test]
fn a_cloned_handle_outlives_the_original_range_3() -> TestResult {
    a_cloned_handle_outlives_the_original(seeds(3))
}

#[test]
fn a_cloned_handle_outlives_the_original_range_4() -> TestResult {
    a_cloned_handle_outlives_the_original(seeds(4))
}

#[test]
fn a_cloned_handle_outlives_the_original_range_5() -> TestResult {
    a_cloned_handle_outlives_the_original(seeds(5))
}

/// Every operation refuses exactly the names that are not one plain component,
/// with `InvalidInput`, and a valid name lands inside and nowhere else.
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory(range: Range<u64>) -> TestResult {
    for seed in range {
        a_hostile_name_is_refused_and_nothing_leaves_the_directory_at(seed)?;
    }
    Ok(())
}

/// One seed of [`a_hostile_name_is_refused_and_nothing_leaves_the_directory`].
fn a_hostile_name_is_refused_and_nothing_leaves_the_directory_at(seed: u64) -> TestResult {
    let arena = Arena::new()?;
    let dir = arena.admit()?;
    let mut rng = Rng::new(seed);
    let mut created = BTreeSet::new();
    for _ in 0..48 {
        let name = draw_name(&mut rng)?;
        hostile_step(seed, &dir, &name, &mut created)?;
    }
    assert_eq!(
        names_in(&arena.inside())?,
        created,
        "seed {seed}: the directory holds what was created"
    );
    arena.assert_parent_untouched(seed)?;
    Ok(())
}

/// One drawn name through every entry point.
fn hostile_step(seed: u64, dir: &Dir, name: &str, created: &mut BTreeSet<String>) -> TestResult {
    let refused = |result: std::io::Result<()>| matches!(result, Err(ref error) if error.kind() == ErrorKind::InvalidInput);
    if !is_component(name) {
        let attempts = [
            ("create_new", dir.create_new(name).map(drop)),
            ("create_dir", dir.create_dir(name)),
            ("open_subdir", dir.open_subdir(name).map(drop)),
            (
                "open_entry",
                dir.open_entry(name, OpenFlags::create_truncate()).map(drop),
            ),
            ("kind", dir.kind(name).map(drop)),
            ("read_link", dir.read_link(name).map(drop)),
            ("remove_file", dir.remove_file(name)),
            ("remove_dir", dir.remove_dir(name)),
        ];
        for (call, result) in attempts {
            assert!(
                refused(result),
                "seed {seed}: {call}({name:?}) was not refused as a non-component"
            );
        }
        return Ok(());
    }
    if created.contains(name) {
        let again = dir.create_new(name).map(drop);
        assert!(
            matches!(again, Err(ref error) if error.kind() == ErrorKind::AlreadyExists),
            "seed {seed}: create_new({name:?}) twice answered {again:?}"
        );
        dir.remove_file(name)?;
        created.remove(name);
        assert!(
            matches!(dir.kind(name), Err(ref error) if error.kind() == ErrorKind::NotFound),
            "seed {seed}: {name:?} outlived its removal"
        );
        return Ok(());
    }
    dir.create_new(name)?.write_all(name.as_bytes())?;
    assert_eq!(
        dir.kind(name)?,
        FileKind::File,
        "seed {seed}: {name:?} is a file"
    );
    created.insert(name.to_owned());
    Ok(())
}

/// The targets a planted symlink points at: up and out, absolute, and
/// through a subdirectory and back up past the admitted one.
fn draw_target(rng: &mut Rng, arena: &Arena) -> Result<String, Box<dyn Error>> {
    let absolute = arena.parent.path().join("outside.txt");
    let targets = [
        "../outside.txt".to_owned(),
        absolute.to_string_lossy().into_owned(),
        "sub/../../outside.txt".to_owned(),
        "./../inside/../outside.txt".to_owned(),
    ];
    let target = targets
        .get(rng.below(targets.len()))
        .ok_or("a target inside the table")?;
    Ok(target.clone())
}

/// A symlink is reported as a symlink, its target is read back verbatim, and
/// opening it under the default policy never yields what it points at.
fn a_symlink_never_opens_what_it_points_at(range: Range<u64>) -> TestResult {
    for seed in range {
        a_symlink_never_opens_what_it_points_at_at(seed)?;
    }
    Ok(())
}

/// One seed of [`a_symlink_never_opens_what_it_points_at`].
fn a_symlink_never_opens_what_it_points_at_at(seed: u64) -> TestResult {
    let arena = Arena::new()?;
    std::fs::create_dir(arena.inside().join("sub"))?;
    let dir = arena.admit()?;
    let mut rng = Rng::new(seed);
    for index in 0..rng.below(6).saturating_add(1) {
        let link = plain(&mut rng, index);
        let target = draw_target(&mut rng, &arena)?;
        plant_link(seed, &arena, &dir, &link, &target)?;
        let opened = dir.open_entry(&link, OpenFlags::read());
        assert!(
            opened.is_err(),
            "seed {seed}: {link} -> {target:?} was opened under the default policy"
        );
        let truncating = dir.open_entry(&link, OpenFlags::create_truncate());
        assert!(
            truncating.is_err(),
            "seed {seed}: {link} -> {target:?} was opened for truncation"
        );
        let descended = dir.open_subdir(&link);
        assert!(descended.is_err(), "seed {seed}: {link} was descended into");
    }
    arena.assert_parent_untouched(seed)?;
    Ok(())
}

/// Plants `link -> target` inside and checks the handle reports it unresolved.
fn plant_link(seed: u64, arena: &Arena, dir: &Dir, link: &str, target: &str) -> TestResult {
    std::os::unix::fs::symlink(target, arena.inside().join(link))?;
    let kind = dir.kind(link)?;
    assert_eq!(kind, FileKind::Symlink, "seed {seed}: {link} is a symlink");
    let stored = dir.read_link(link)?;
    assert_eq!(
        stored,
        std::ffi::OsString::from(target),
        "seed {seed}: {link}'s target"
    );
    Ok(())
}

/// Every file and directory the handle creates carries no group, world or
/// special bits, whatever it is called and however many there are.
fn created_entries_are_private_to_their_owner(range: Range<u64>) -> TestResult {
    for seed in range {
        created_entries_are_private_to_their_owner_at(seed)?;
    }
    Ok(())
}

/// One seed of [`created_entries_are_private_to_their_owner`].
fn created_entries_are_private_to_their_owner_at(seed: u64) -> TestResult {
    let arena = Arena::new()?;
    let dir = arena.admit()?;
    let mut rng = Rng::new(seed);
    for index in 0..rng.below(12).saturating_add(1) {
        let name = plain(&mut rng, index);
        let is_directory = rng.below(2) == 0;
        if is_directory {
            dir.create_dir(&name)?;
        } else {
            dir.create_new(&name)?;
        }
        let mode = std::fs::symlink_metadata(arena.inside().join(&name))?
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7077,
            0,
            "seed {seed}: {name} was created with mode {mode:o}"
        );
        let owner = if is_directory { 0o700 } else { 0o600 };
        assert_eq!(
            mode & 0o700,
            owner,
            "seed {seed}: {name} lacks its owner's bits: {mode:o}"
        );
    }
    arena.assert_parent_untouched(seed)?;
    Ok(())
}

/// After the admitted directory is renamed away and its name given to a new
/// one, everything created through the held handle lands in the original.
fn a_held_directory_survives_its_name_being_swapped(range: Range<u64>) -> TestResult {
    for seed in range {
        a_held_directory_survives_its_name_being_swapped_at(seed)?;
    }
    Ok(())
}

/// One seed of [`a_held_directory_survives_its_name_being_swapped`].
fn a_held_directory_survives_its_name_being_swapped_at(seed: u64) -> TestResult {
    let arena = Arena::new()?;
    let dir = arena.admit()?;
    let mut rng = Rng::new(seed);
    let swaps = rng.below(4).saturating_add(1);
    let mut written = BTreeSet::new();
    let original = arena.parent.path().join("original");
    for swap in 0..swaps {
        let name = plain(&mut rng, swap);
        swap_then_write(&arena, &dir, &original, swap == 0, &name)?;
        written.insert(name);
    }
    assert_eq!(
        names_in(&original)?,
        written,
        "seed {seed}: the held handle wrote into the directory it opened"
    );
    assert!(
        names_in(&arena.inside())?.is_empty(),
        "seed {seed}: the replacement under the old name was written to"
    );
    Ok(())
}

/// Moves the admitted directory's name onto a fresh directory — the first
/// swap renames the original away, later ones replace the previous stand-in —
/// then writes `name` through the held handle.
fn swap_then_write(
    arena: &Arena,
    dir: &Dir,
    original: &Path,
    first: bool,
    name: &str,
) -> TestResult {
    if first {
        std::fs::rename(arena.inside(), original)?;
    } else {
        std::fs::remove_dir(arena.inside())?;
    }
    std::fs::create_dir(arena.inside())?;
    let mut file = dir.create_new(name)?;
    file.write_all(name.as_bytes())?;
    Ok(())
}

/// Tenants each holding their own directory write the same names at once;
/// each reads back only what it wrote.
fn tenants_sharing_names_never_see_each_other(range: Range<u64>) -> TestResult {
    for seed in range {
        tenants_sharing_names_never_see_each_other_at(seed)?;
    }
    Ok(())
}

/// One seed of [`tenants_sharing_names_never_see_each_other`].
fn tenants_sharing_names_never_see_each_other_at(seed: u64) -> TestResult {
    let arena = Arena::new()?;
    let root = arena.admit()?;
    let mut rng = Rng::new(seed);
    let tenants = rng.below(15).saturating_add(2);
    let names: Vec<String> = (0..rng.below(8).saturating_add(1))
        .map(|index| plain(&mut rng, index))
        .collect();
    for tenant in 0..tenants {
        root.create_dir(&format!("tenant-{tenant}"))?;
    }
    let outcomes: Vec<Result<(), String>> = std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(tenants);
        for tenant in 0..tenants {
            let (root, names) = (&root, &names);
            workers.push(scope.spawn(move || {
                tenant_writes(root, tenant, names).map_err(|error| error.to_string())
            }));
        }
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|panic| format!("a tenant panicked: {panic:?}"))?
            })
            .collect()
    });
    for outcome in outcomes {
        outcome?;
    }
    for tenant in 0..tenants {
        let own = root.open_subdir(&format!("tenant-{tenant}"))?;
        for name in &names {
            let mut text = String::new();
            own.open_entry(name, OpenFlags::read())?
                .read_to_string(&mut text)?;
            assert_eq!(
                text,
                format!("tenant {tenant}"),
                "seed {seed}: tenant {tenant} read another tenant's {name}"
            );
        }
    }
    arena.assert_parent_untouched(seed)?;
    Ok(())
}

/// One tenant writes its identity under every shared name in its own directory.
fn tenant_writes(root: &Dir, tenant: usize, names: &[String]) -> std::io::Result<()> {
    let own = root.open_subdir(&format!("tenant-{tenant}"))?;
    for name in names {
        own.open_entry(name, OpenFlags::create_truncate())?
            .write_all(format!("tenant {tenant}").as_bytes())?;
    }
    Ok(())
}

/// A clone is an owner in its own right: dropping the original, or any number
/// of other clones, leaves it able to resolve and create.
fn a_cloned_handle_outlives_the_original(range: Range<u64>) -> TestResult {
    for seed in range {
        a_cloned_handle_outlives_the_original_at(seed)?;
    }
    Ok(())
}

/// One seed of [`a_cloned_handle_outlives_the_original`].
fn a_cloned_handle_outlives_the_original_at(seed: u64) -> TestResult {
    let arena = Arena::new()?;
    let mut rng = Rng::new(seed);
    let mut handles = vec![arena.admit()?];
    for _ in 0..rng.below(6).saturating_add(1) {
        let source = handles
            .get(rng.below(handles.len()))
            .ok_or("a handle to clone")?;
        let clone = source.try_clone()?;
        handles.push(clone);
    }
    let mut made = BTreeSet::new();
    while handles.len() > 1 {
        let name = plain(&mut rng, made.len());
        drop_one_then_write(seed, &mut rng, &mut handles, &name)?;
        made.insert(name);
    }
    assert_eq!(
        names_in(&arena.inside())?,
        made,
        "seed {seed}: every survivor wrote into the one directory"
    );
    arena.assert_parent_untouched(seed)?;
    Ok(())
}

/// Drops one handle chosen by the seed, then creates `name` through another.
fn drop_one_then_write(seed: u64, rng: &mut Rng, handles: &mut Vec<Dir>, name: &str) -> TestResult {
    let turn = rng.below(handles.len());
    handles.rotate_left(turn);
    let dropped = handles.pop().ok_or("a handle to drop")?;
    drop(dropped);
    let survivor = handles
        .get(rng.below(handles.len()))
        .ok_or("a surviving handle")?;
    survivor.create_new(name)?;
    let kind = survivor.kind(name)?;
    assert_eq!(
        kind,
        FileKind::File,
        "seed {seed}: a survivor resolves what it made"
    );
    Ok(())
}
