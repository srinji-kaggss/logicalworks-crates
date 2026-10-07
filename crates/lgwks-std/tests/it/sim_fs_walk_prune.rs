//! Deterministic simulation of the predicate walk and its per-entry `lstat`
//! (INV-FS-7), on top of the path walk's own contract (INV-FS-2, INV-FS-5).
//!
//! One `lgwks_std::seeded::Seeded` stream per seed grows one real directory
//! tree on disk — width, depth, names (multi-byte, spaced and dotted ones
//! included) and which entries are directories — and draws the walk policy,
//! the budgets, the faults (unreadable directories, dangling links, in-root
//! aliases), the subtree a predicate skips and the entry it stops at. The
//! shipped `lgwks_std::fs` walk runs against that tree through its public API.
//!
//! Every answer is checked against a model computed from the seed alone, or
//! against an independent observation of the same tree: which entries a pruned
//! walk admits and in which order, which budget it may charge, where a stop
//! ends it, what `symlink_metadata` says about each entry, and that each
//! relative path joins back onto the root. Pruned-subtree faults are the proof
//! that a skipped directory is never read: a read of it would report them.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use lgwks_std::fs::{
    Descend, FileKind, OmissionStage, WalkEntry, WalkFailure, WalkLimits, WalkOmission,
    WalkOptions, walk_dir, walk_dir_bounded, walk_dir_entries, walk_dir_entries_tolerant,
    walk_dir_tolerant, walk_dir_tolerant_bounded,
};
use lgwks_std::seeded::Seeded;

use crate::seeded_sweep::{fold, fold_usize, initial_trace};

/// What every family returns: a walk, draw or filesystem refusal reports itself.
type TestResult = Result<(), Box<dyn Error>>;

/// Trees grown per family. Each is a real tree on disk.
const SEEDS: u64 = 48;

/// The base every family's seeds are derived from.
const BASE_SEED: u64 = 0x5eed_0343_f5a1_2026;

/// Name stems. Multi-byte, spaced, dotted and leading-dash names are the ones
/// a byte-counting or component-splitting relative path gets wrong.
const STEMS: [&str; 10] = [
    "a",
    "b",
    "z",
    "\u{e9}",
    "\u{65e5}\u{672c}",
    "sp ace",
    "dot.ted",
    "-dash",
    "_u",
    "\u{3a9}",
];

/// The deepest level a grown tree reaches, counting the root's children as 1.
const MAX_LEVEL: usize = 4;

/// The widest directory a grown tree holds.
const MAX_WIDTH: usize = 5;

/// A depth deeper than any grown tree: the policy a family does not probe.
const UNBOUNDED_DEPTH: usize = 32;

/// The seed for run `index` of `family`. Families take disjoint seed streams,
/// so a failing seed names its family.
fn seed_for(family: u64, index: u64) -> u64 {
    BASE_SEED
        ^ family.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// A uniform value in `low..=high`.
fn between(stream: &mut Seeded, low: usize, high: usize) -> Result<usize, Box<dyn Error>> {
    let span = high.saturating_sub(low).saturating_add(1);
    Ok(low.saturating_add(stream.index(span)?))
}

/// True `per_mille` times in a thousand.
fn chance(stream: &mut Seeded, per_mille: u64) -> Result<bool, Box<dyn Error>> {
    Ok(stream.below(1_000)? < per_mille)
}

/// One grown entry, relative to the tree's root.
struct Node {
    /// Path below the root.
    rel: PathBuf,
    /// Whether the entry is a directory.
    dir: bool,
}

/// A seeded tree on disk and the model of what it holds.
struct Tree {
    /// Owns the scratch; removed on drop. Never read: it exists to be dropped.
    _scratch: tempfile::TempDir,
    /// The canonical root every walked path sits under.
    root: PathBuf,
    /// Every grown entry, in creation order.
    nodes: Vec<Node>,
}

impl Tree {
    /// Grows the tree for one seed, drawing everything from `stream`. The tree
    /// always holds at least one directory, so every family has one to prune.
    fn grow(stream: &mut Seeded) -> Result<Self, Box<dyn Error>> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().join("root");
        fs::create_dir(&root)?;
        let root = root.canonicalize()?;
        let mut tree = Self {
            _scratch: scratch,
            root,
            nodes: Vec::new(),
        };
        let max_level = between(stream, 2, MAX_LEVEL)?;
        tree.grow_dir(stream, Path::new(""), 1, max_level)?;
        if !tree.nodes.iter().any(|node| node.dir) {
            let rel = PathBuf::from("~only-dir");
            fs::create_dir(tree.root.join(&rel))?;
            fs::write(tree.root.join(&rel).join("leaf"), b"leaf")?;
            tree.nodes.push(Node {
                rel: rel.clone(),
                dir: true,
            });
            tree.nodes.push(Node {
                rel: rel.join("leaf"),
                dir: false,
            });
        }
        Ok(tree)
    }

    /// Fills `rel_dir` with a seeded number of files and directories. A name
    /// is a stem plus its index, which keeps names unique within a directory.
    fn grow_dir(
        &mut self,
        stream: &mut Seeded,
        rel_dir: &Path,
        level: usize,
        max_level: usize,
    ) -> Result<(), Box<dyn Error>> {
        let min_width = usize::from(level == 1);
        let width = between(stream, min_width, MAX_WIDTH)?;
        for index in 0..width {
            let stem = STEMS
                .get(stream.index(STEMS.len())?)
                .ok_or("a drawn stem index is inside the table")?;
            let rel = rel_dir.join(format!("{stem}{index}"));
            if level < max_level && chance(stream, 450)? {
                fs::create_dir(self.root.join(&rel))?;
                self.nodes.push(Node {
                    rel: rel.clone(),
                    dir: true,
                });
                self.grow_dir(stream, &rel, level.saturating_add(1), max_level)?;
            } else {
                fs::write(self.root.join(&rel), rel.as_os_str().as_encoded_bytes())?;
                self.nodes.push(Node { rel, dir: false });
            }
        }
        Ok(())
    }

    /// The direct children of `rel_dir`, sorted by file name as the walk sorts.
    fn children(&self, rel_dir: &Path) -> Vec<&Node> {
        let mut children: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|node| node.rel.parent() == Some(rel_dir))
            .collect();
        children.sort_by_key(|node| node.rel.file_name().map(OsString::from));
        children
    }

    /// The sorted walk's exact output, relative to the root: depth-first
    /// preorder, each directory in name order, `pruned` listed and not entered.
    fn preorder(&self, pruned: Option<&Path>) -> Vec<PathBuf> {
        let mut out = Vec::new();
        self.preorder_into(Path::new(""), pruned, &mut out);
        out
    }

    /// Appends `rel_dir`'s subtree to `out` in walk order.
    fn preorder_into(&self, rel_dir: &Path, pruned: Option<&Path>, out: &mut Vec<PathBuf>) {
        for node in self.children(rel_dir) {
            out.push(node.rel.clone());
            if node.dir && Some(node.rel.as_path()) != pruned {
                self.preorder_into(&node.rel, pruned, out);
            }
        }
    }

    /// Whether `rel` is a grown directory.
    fn is_dir(&self, rel: &Path) -> bool {
        self.nodes.iter().any(|node| node.dir && node.rel == rel)
    }

    /// The directories a walk pruned at `pruned` lists: the root, and every
    /// grown directory it enters.
    fn listed_directories(&self, pruned: Option<&Path>) -> Vec<PathBuf> {
        let mut listed = vec![PathBuf::new()];
        listed.extend(
            self.preorder(pruned)
                .into_iter()
                .filter(|rel| self.is_dir(rel) && Some(rel.as_path()) != pruned),
        );
        listed
    }

    /// The widest directory a walk pruned at `pruned` lists.
    fn widest(&self, pruned: Option<&Path>) -> usize {
        self.listed_directories(pruned)
            .iter()
            .map(|rel| self.children(rel).len())
            .fold(0, usize::max)
    }

    /// Path bytes a walk pruned at `pruned` charges: one charge per listed entry.
    fn path_bytes(&self, pruned: Option<&Path>) -> usize {
        self.preorder(pruned)
            .iter()
            .map(|rel| self.root.join(rel).as_os_str().len())
            .fold(0, usize::saturating_add)
    }

    /// A seeded grown directory, relative to the root.
    fn some_directory(&self, stream: &mut Seeded) -> Result<PathBuf, Box<dyn Error>> {
        let dirs: Vec<&Node> = self.nodes.iter().filter(|node| node.dir).collect();
        let pick = stream.index(dirs.len())?;
        Ok(dirs
            .get(pick)
            .ok_or("a drawn directory index is inside the list")?
            .rel
            .clone())
    }

    /// The limits a walk pruned at `pruned` fits exactly.
    fn exact_limits(&self, pruned: Option<&Path>) -> WalkLimits {
        WalkLimits::new(
            self.preorder(pruned).len(),
            self.widest(pruned),
            self.path_bytes(pruned),
            0,
        )
    }
}

/// Walk options with the given depth, symlink and ordering policy.
fn options(max_depth: usize, follow_symlinks: bool, sort_alphabetically: bool) -> WalkOptions {
    let mut options = WalkOptions::default();
    options.max_depth = max_depth;
    options.follow_symlinks = follow_symlinks;
    options.sort_alphabetically = sort_alphabetically;
    options
}

/// The sorted, link-blind policy deeper than any grown tree.
fn unbounded() -> WalkOptions {
    options(UNBOUNDED_DEPTH, false, true)
}

/// The predicate every unpruned walk here runs under.
fn enter(_path: &Path, _kind: FileKind) -> Descend {
    Descend::Enter
}

/// The typed failure a strict refusal carries.
fn failure(error: &io::Error) -> Option<&WalkFailure> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<WalkFailure>())
}

/// A strict refusal as a comparable value: its kind, stage and path.
type Refusal = (io::ErrorKind, Option<OmissionStage>, Option<PathBuf>);

/// What a strict refusal is, as a comparable value.
fn refusal(error: &io::Error) -> Refusal {
    let typed = failure(error);
    (
        error.kind(),
        typed.map(WalkFailure::stage),
        typed.map(|failure| failure.path().to_path_buf()),
    )
}

/// Each omission as its path and stage.
fn omitted(omissions: &[WalkOmission]) -> Vec<(PathBuf, OmissionStage)> {
    omissions
        .iter()
        .map(|omission| (omission.path.clone(), omission.stage))
        .collect()
}

/// Each entry's path relative to its root.
fn relatives(entries: &[WalkEntry]) -> Vec<PathBuf> {
    entries
        .iter()
        .map(|entry| entry.relative_path().to_path_buf())
        .collect()
}

/// Runs `check` over [`SEEDS`] grown trees of `family`.
fn sweep(family: u64, mut check: impl FnMut(u64, &mut Seeded, &Tree) -> TestResult) -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(family, index);
        let mut stream = Seeded::from_seed(seed);
        let tree = Tree::grow(&mut stream)?;
        check(seed, &mut stream, &tree).map_err(|error| format!("seed {seed:#x}: {error}"))?;
    }
    Ok(())
}

/// The answer for one offer from a predicate that skips exactly `pruned`.
fn skip_only(pruned: &Path, path: &Path) -> Descend {
    if path == pruned {
        Descend::Skip
    } else {
        Descend::Enter
    }
}

// ── Equivalence: an always-enter predicate is the path walk ────────────────

#[test]
/// With a predicate that always enters, the metadata walk admits, refuses,
/// omits and charges exactly what the path walk does, under seeded depth,
/// ordering, symlink policy, budgets and faults.
fn sim_an_always_enter_walk_is_the_path_walk() -> TestResult {
    sweep(1, |seed, stream, tree| {
        #[cfg(unix)]
        {
            if chance(stream, 300)? {
                let host = tree.root.join(tree.some_directory(stream)?);
                std::os::unix::fs::symlink("missing-target", host.join("~dangling"))?;
            }
            if chance(stream, 400)? {
                let target = tree.root.join(tree.some_directory(stream)?);
                let host = tree.root.join(tree.some_directory(stream)?);
                std::os::unix::fs::symlink(&target, host.join("~alias"))?;
            }
        }
        let max_depth = if chance(stream, 500)? {
            between(stream, 0, 4)?
        } else {
            UNBOUNDED_DEPTH
        };
        let walk = options(max_depth, chance(stream, 500)?, chance(stream, 700)?);
        let exact = tree.exact_limits(None);
        let limits = WalkLimits::new(
            between(stream, 0, exact.max_entries.saturating_add(2))?,
            between(stream, 0, exact.max_directory_entries.saturating_add(2))?,
            between(stream, 0, exact.max_path_bytes.saturating_add(16))?,
            between(stream, 0, 2)?,
        );
        let bounded = chance(stream, 700)?.then_some(&limits);

        let entries = walk_dir_entries_tolerant(&tree.root, &walk, bounded, enter)?;
        let paths = match bounded {
            Some(limits) => walk_dir_tolerant_bounded(&tree.root, &walk, limits)?,
            None => walk_dir_tolerant(&tree.root, &walk)?,
        };
        let entry_paths: Vec<&Path> = entries.entries().iter().map(WalkEntry::path).collect();
        let path_paths: Vec<&Path> = paths.entries().iter().map(PathBuf::as_path).collect();
        assert_eq!(
            entry_paths, path_paths,
            "seed {seed:#x}: the entries differ from the path walk under {walk:?} {bounded:?}"
        );
        assert_eq!(
            omitted(entries.omissions()),
            omitted(paths.omissions()),
            "seed {seed:#x}: the omissions differ from the path walk"
        );
        assert_eq!(
            entries.budget_exhausted(),
            paths.budget_exhausted(),
            "seed {seed:#x}: budget exhaustion differs from the path walk"
        );
        assert_eq!(
            entries.is_complete(),
            paths.is_complete(),
            "seed {seed:#x}: completeness differs from the path walk"
        );
        assert!(
            !entries.stopped(),
            "seed {seed:#x}: an always-enter walk reported a stop"
        );

        let strict_entries = walk_dir_entries(&tree.root, &walk, bounded, enter);
        let strict_paths = match bounded {
            Some(limits) => walk_dir_bounded(&tree.root, &walk, limits),
            None => walk_dir(&tree.root, &walk),
        };
        match (strict_entries, strict_paths) {
            (Ok(entries), Ok(paths)) => {
                let entry_paths: Vec<PathBuf> =
                    entries.into_iter().map(WalkEntry::into_path).collect();
                assert_eq!(
                    entry_paths, paths,
                    "seed {seed:#x}: strict entries differ from the strict path walk"
                );
            }
            (Err(entries), Err(paths)) => assert_eq!(
                refusal(&entries),
                refusal(&paths),
                "seed {seed:#x}: the two strict walks refused differently"
            ),
            (entries, paths) => {
                return Err(format!(
                    "seed {seed:#x}: one strict walk refused and the other did not: \
                     entries {:?}, paths {:?}",
                    entries.err(),
                    paths.err()
                )
                .into());
            }
        }
        Ok(())
    })
}

// ── Pruning ─────────────────────────────────────────────────────────────────

/// Runs `body` with `dir` stripped of every permission and restores it after,
/// reporting whether the lock is inert (the process bypasses mode bits).
///
/// The restore runs whatever `body` returned, before any caller assertion,
/// so the scratch can always be removed.
#[cfg(unix)]
fn while_locked<T>(dir: &Path, body: impl FnOnce() -> T) -> io::Result<(T, bool)> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o000))?;
    let inert = fs::read_dir(dir).is_ok();
    let value = body();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    Ok((value, inert))
}

#[test]
#[cfg(unix)]
/// A skipped directory is never read: an unreadable directory and a dangling
/// link planted inside it produce no omission and no refusal, while the same
/// walk without the skip reports them.
fn sim_a_pruned_subtree_is_never_read() -> TestResult {
    sweep(2, |seed, stream, tree| {
        let pruned = tree.some_directory(stream)?;
        let pruned_abs = tree.root.join(&pruned);
        let locked = pruned_abs.join("~locked");
        fs::create_dir(&locked)?;
        fs::write(locked.join("secret"), b"never read")?;
        let dangling = pruned_abs.join("~dangling");
        std::os::unix::fs::symlink("missing-target", &dangling)?;

        let walk = unbounded();
        let mut offered = Vec::new();
        let ((pruned_report, strict, control), inert) = while_locked(&locked, || {
            let pruned_report =
                walk_dir_entries_tolerant(&tree.root, &walk, None, |path, _kind| {
                    offered.push(path.to_path_buf());
                    skip_only(&pruned_abs, path)
                });
            let strict = walk_dir_entries(&tree.root, &walk, None, |path, _kind| {
                skip_only(&pruned_abs, path)
            });
            let control = walk_dir_entries_tolerant(&tree.root, &walk, None, enter);
            (pruned_report, strict, control)
        })?;
        let pruned_report = pruned_report?;
        let control = control?;

        assert!(
            pruned_report.omissions().is_empty() && pruned_report.is_complete(),
            "seed {seed:#x}: a skipped subtree produced omissions {:?}",
            pruned_report.omissions()
        );
        assert_eq!(
            relatives(pruned_report.entries()),
            tree.preorder(Some(&pruned)),
            "seed {seed:#x}: the pruned walk is not the model's pruned preorder"
        );
        assert!(
            offered
                .iter()
                .all(|path| path == &pruned_abs || !path.starts_with(&pruned_abs)),
            "seed {seed:#x}: the predicate was offered an entry below the skipped directory"
        );
        let strict = strict.map_err(|error| format!("seed {seed:#x}: strict refused: {error}"))?;
        assert_eq!(
            relatives(&strict),
            tree.preorder(Some(&pruned)),
            "seed {seed:#x}: the strict pruned walk differs from the model"
        );
        let control_omitted: Vec<&Path> = control
            .omissions()
            .iter()
            .map(|omission| omission.path.as_path())
            .collect();
        assert!(
            control_omitted.contains(&dangling.as_path()),
            "seed {seed:#x}: the planted fault is inert, so the skip proves nothing"
        );
        assert_eq!(
            control_omitted.contains(&locked.as_path()),
            !inert,
            "seed {seed:#x}: the unpruned walk must report the unreadable directory"
        );
        Ok(())
    })
}

#[test]
/// A skipped subtree is never listed, so it is never charged: limits sized
/// exactly to the pruned model admit the pruned walk, one entry fewer refuses
/// it, and the same exact limits refuse the unpruned walk whenever the pruned
/// directory has anything in it.
fn sim_a_pruned_subtree_charges_no_budget() -> TestResult {
    sweep(3, |seed, stream, tree| {
        let pruned = tree.some_directory(stream)?;
        let pruned_abs = tree.root.join(&pruned);
        let skip = |path: &Path, _kind: FileKind| skip_only(&pruned_abs, path);
        let exact = tree.exact_limits(Some(&pruned));
        let walked = walk_dir_entries(&tree.root, &unbounded(), Some(&exact), skip)?;
        assert_eq!(
            relatives(&walked),
            tree.preorder(Some(&pruned)),
            "seed {seed:#x}: limits sized to the pruned model refused or changed it"
        );
        let one_fewer = WalkLimits::new(
            exact.max_entries.saturating_sub(1),
            exact.max_directory_entries,
            exact.max_path_bytes,
            0,
        );
        let error = walk_dir_entries(&tree.root, &unbounded(), Some(&one_fewer), skip)
            .err()
            .ok_or_else(|| format!("seed {seed:#x}: one entry under the model still passed"))?;
        assert_eq!(
            failure(&error).map(WalkFailure::stage),
            Some(OmissionStage::ResourceBudget),
            "seed {seed:#x}: the refusal does not name the budget: {error}"
        );
        if !tree.children(&pruned).is_empty() {
            assert!(
                walk_dir_entries(&tree.root, &unbounded(), Some(&exact), enter).is_err(),
                "seed {seed:#x}: the unpruned walk fit the pruned limits, so they prove nothing"
            );
        }
        Ok(())
    })
}

#[test]
/// A predicate drawing its own seeded answers is offered every admitted entry
/// exactly once, in walk order, with the kind the entry carries, and is never
/// offered anything below a directory it skipped.
fn sim_the_predicate_is_offered_each_admitted_entry_once_in_order() -> TestResult {
    sweep(4, |seed, stream, tree| {
        let mut answers = stream.clone();
        let mut offered: Vec<(PathBuf, FileKind)> = Vec::new();
        let mut skipped: Vec<PathBuf> = Vec::new();
        let report = walk_dir_entries_tolerant(&tree.root, &unbounded(), None, |path, kind| {
            offered.push((path.to_path_buf(), kind));
            let skip = kind == FileKind::Directory && answers.below(3).is_ok_and(|draw| draw == 0);
            if skip {
                skipped.push(path.to_path_buf());
                Descend::Skip
            } else {
                Descend::Enter
            }
        })?;
        let recorded: Vec<(PathBuf, FileKind)> = report
            .entries()
            .iter()
            .map(|entry| (entry.path().to_path_buf(), entry.kind()))
            .collect();
        assert_eq!(
            offered, recorded,
            "seed {seed:#x}: the predicate and the record disagree"
        );
        for &(ref path, kind) in &offered {
            assert!(
                !skipped
                    .iter()
                    .any(|skip| path != skip && path.starts_with(skip)),
                "seed {seed:#x}: {} was offered below a skipped directory",
                path.display()
            );
            let expected = if tree.is_dir(path.strip_prefix(&tree.root)?) {
                FileKind::Directory
            } else {
                FileKind::File
            };
            assert_eq!(
                kind,
                expected,
                "seed {seed:#x}: {} was offered as the wrong kind",
                path.display()
            );
        }
        Ok(())
    })
}

// ── Stop ────────────────────────────────────────────────────────────────────

/// A predicate that enters everything until its `stop_at`-th offer, where it
/// stops, counting every offer in `count`.
fn stop_after(stop_at: usize, count: &mut usize) -> impl FnMut(&Path, FileKind) -> Descend + '_ {
    move |_path, _kind| {
        let index = *count;
        *count = count.saturating_add(1);
        if index == stop_at {
            Descend::Stop
        } else {
            Descend::Enter
        }
    }
}

#[test]
/// A stop at a seeded entry records that entry and nothing after it, offers
/// nothing further, and is reported as a stop rather than an omission, in both
/// the tolerant and the strict walk.
fn sim_stop_ends_the_walk_at_the_chosen_entry() -> TestResult {
    sweep(5, |seed, stream, tree| {
        let order = tree.preorder(None);
        let stop_at = stream.index(order.len())?;
        let expected: Vec<PathBuf> = order
            .iter()
            .take(stop_at.saturating_add(1))
            .cloned()
            .collect();
        let mut offered = 0_usize;
        let report = walk_dir_entries_tolerant(
            &tree.root,
            &unbounded(),
            None,
            stop_after(stop_at, &mut offered),
        )?;
        assert_eq!(
            offered,
            stop_at.saturating_add(1),
            "seed {seed:#x}: the predicate was offered entries after the stop"
        );
        assert_eq!(
            relatives(report.entries()),
            expected,
            "seed {seed:#x}: the stopped walk did not end at entry {stop_at}"
        );
        assert!(report.stopped(), "seed {seed:#x}: the stop is not reported");
        assert!(
            report.omissions().is_empty() && report.is_complete_within_policy(),
            "seed {seed:#x}: a requested stop read as an omission"
        );
        let mut strict_offered = 0_usize;
        let strict = walk_dir_entries(
            &tree.root,
            &unbounded(),
            None,
            stop_after(stop_at, &mut strict_offered),
        )?;
        assert_eq!(
            relatives(&strict),
            expected,
            "seed {seed:#x}: the strict walk stopped elsewhere"
        );
        Ok(())
    })
}

// ── Metadata and relative paths ────────────────────────────────────────────

/// The kind an independent `symlink_metadata` names, by the test's own mapping.
fn independent_kind(metadata: &fs::Metadata) -> FileKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Directory
    } else if file_type.is_file() {
        FileKind::File
    } else {
        FileKind::Unknown
    }
}

#[test]
/// Every entry's metadata is the entry's own `lstat`: an independent
/// `symlink_metadata` of its path agrees on type, size, modification time and,
/// on Unix, inode, device, mode and link count — links and entries reached
/// through a followed alias included.
fn sim_every_entry_carries_its_own_lstat() -> TestResult {
    sweep(6, |seed, stream, tree| {
        #[cfg(unix)]
        {
            let file = tree
                .nodes
                .iter()
                .find(|node| !node.dir)
                .map(|node| tree.root.join(&node.rel));
            if let Some(file) = file {
                std::os::unix::fs::symlink(file, tree.root.join("~file-link"))?;
            }
            let target = tree.root.join(tree.some_directory(stream)?);
            std::os::unix::fs::symlink(target, tree.root.join("~dir-alias"))?;
        }
        let walk = options(UNBOUNDED_DEPTH, chance(stream, 500)?, true);
        let entries = walk_dir_entries(&tree.root, &walk, None, enter)?;
        assert!(
            entries.len() >= tree.nodes.len(),
            "seed {seed:#x}: the walk lost grown entries"
        );
        for entry in &entries {
            let independent = fs::symlink_metadata(entry.path())?;
            let carried = entry.metadata();
            let path = entry.path().display();
            assert_eq!(
                carried.file_type(),
                independent.file_type(),
                "seed {seed:#x}: {path} type"
            );
            assert_eq!(
                entry.kind(),
                independent_kind(&independent),
                "seed {seed:#x}: {path} kind"
            );
            assert_eq!(
                carried.len(),
                independent.len(),
                "seed {seed:#x}: {path} size"
            );
            assert_eq!(
                carried.modified()?,
                independent.modified()?,
                "seed {seed:#x}: {path} mtime"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(
                    (
                        carried.ino(),
                        carried.dev(),
                        carried.mode(),
                        carried.nlink()
                    ),
                    (
                        independent.ino(),
                        independent.dev(),
                        independent.mode(),
                        independent.nlink()
                    ),
                    "seed {seed:#x}: {path} inode, device, mode or link count"
                );
            }
        }
        Ok(())
    })
}

#[test]
/// Every relative path is non-empty, relative, and joins back onto the root to
/// give the absolute path; without links the set of relative paths is the
/// grown tree exactly. A followed alias visits its target's directories once
/// (the walk's existing cycle rule), so each grown entry is reached either at
/// its own relative path or at the same suffix below the alias — whichever the
/// walk reached first — and every extra relative path is led by the alias.
fn sim_relative_paths_join_back_to_the_absolute_path() -> TestResult {
    sweep(7, |seed, stream, tree| {
        let aliased = if cfg!(unix) && chance(stream, 500)? {
            Some(tree.some_directory(stream)?)
        } else {
            None
        };
        #[cfg(unix)]
        if let Some(target) = aliased.as_deref() {
            std::os::unix::fs::symlink(tree.root.join(target), tree.root.join("~alias"))?;
        }
        let walk = options(UNBOUNDED_DEPTH, aliased.is_some(), chance(stream, 500)?);
        let entries = walk_dir_entries(&tree.root, &walk, None, enter)?;
        for entry in &entries {
            let relative = entry.relative_path();
            assert!(
                relative.is_relative() && relative.components().next().is_some(),
                "seed {seed:#x}: {} is not a non-empty relative path",
                relative.display()
            );
            assert_eq!(
                tree.root.join(relative),
                entry.path(),
                "seed {seed:#x}: {} does not join back onto the root",
                relative.display()
            );
        }
        let grown: BTreeSet<PathBuf> = tree.nodes.iter().map(|node| node.rel.clone()).collect();
        let walked: BTreeSet<PathBuf> = relatives(&entries).into_iter().collect();
        if let Some(target) = aliased.as_deref() {
            for rel in &grown {
                let via_alias = rel
                    .strip_prefix(target)
                    .ok()
                    .map(|suffix| Path::new("~alias").join(suffix));
                assert!(
                    walked.contains(rel) || via_alias.is_some_and(|alias| walked.contains(&alias)),
                    "seed {seed:#x}: following the alias lost {}",
                    rel.display()
                );
            }
            assert!(
                walked
                    .difference(&grown)
                    .all(|rel| rel.starts_with("~alias")),
                "seed {seed:#x}: an extra relative path is not led by the alias"
            );
        } else {
            assert_eq!(
                walked, grown,
                "seed {seed:#x}: the relative paths are not the grown tree"
            );
        }
        Ok(())
    })
}

// ── Concurrency and tenants ────────────────────────────────────────────────

/// Trees the concurrent tenants share.
const SHARED_TREES: usize = 8;

/// One tenant's walk: a shared tree, the directory it skips and where it stops.
struct Tenant {
    /// Which shared tree it walks.
    tree: usize,
    /// The absolute directory its predicate skips.
    pruned: PathBuf,
    /// The offer at which it stops, if it stops at all.
    stop_at: Option<usize>,
}

/// What one tenant's walk recorded: relative paths, and whether it stopped.
type TenantView = (Vec<PathBuf>, bool);

/// One tenant's walk, run through the public API.
fn tenant_walk(root: &Path, tenant: &Tenant) -> io::Result<TenantView> {
    let mut offered = 0_usize;
    let report = walk_dir_entries_tolerant(root, &unbounded(), None, |path, _kind| {
        let index = offered;
        offered = offered.saturating_add(1);
        if Some(index) == tenant.stop_at {
            Descend::Stop
        } else {
            skip_only(&tenant.pruned, path)
        }
    })?;
    Ok((relatives(report.entries()), report.stopped()))
}

/// A release gate the spawned tenants wait on, so every walk is in flight at
/// once. A condition variable rather than a barrier: a spawn that fails must
/// still be able to release the threads already waiting.
struct Gate {
    /// Whether the walks may start.
    open: Mutex<bool>,
    /// Woken when `open` turns true.
    opened: Condvar,
}

impl Gate {
    /// Blocks until the gate opens. A poisoned lock still guards a `bool`
    /// that only ever turns true, so its value is read rather than discarded.
    fn wait(&self) {
        let guard = match self.open.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let waited = self.opened.wait_while(guard, |open| !*open);
        drop(waited);
    }

    /// Opens the gate for every waiter.
    fn release(&self) {
        match self.open.lock() {
            Ok(mut open) => *open = true,
            Err(poisoned) => *poisoned.into_inner() = true,
        }
        self.opened.notify_all();
    }
}

/// `tenants` tenants walk the shared trees at once, each with its own skip and
/// stop, and every answer must equal that tenant's walk run alone. Returns the
/// number of walks that were in flight together.
fn concurrent_tenants(tenants: usize, family: u64) -> Result<usize, Box<dyn Error>> {
    let mut stream = Seeded::from_seed(seed_for(family, 0));
    let mut trees = Vec::new();
    for _ in 0..SHARED_TREES {
        trees.push(Tree::grow(&mut stream)?);
    }
    let mut plans: Vec<(Tenant, TenantView)> = Vec::new();
    for index in 0..tenants {
        let tree_index = index.rem_euclid(SHARED_TREES);
        let tree = trees.get(tree_index).ok_or("a shared tree index")?;
        let pruned = tree.root.join(tree.some_directory(&mut stream)?);
        let stop_at = if chance(&mut stream, 300)? {
            Some(stream.index(tree.nodes.len())?)
        } else {
            None
        };
        let plan = Tenant {
            tree: tree_index,
            pruned,
            stop_at,
        };
        let alone = tenant_walk(&tree.root, &plan)?;
        plans.push((plan, alone));
    }
    let gate = Gate {
        open: Mutex::new(false),
        opened: Condvar::new(),
    };
    let outcomes: Vec<Result<bool, String>> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut refused = None;
        for planned in &plans {
            let plan = &planned.0;
            let alone = &planned.1;
            let Some(tree) = trees.get(plan.tree) else {
                refused = Some(String::from("a tenant named a tree that does not exist"));
                break;
            };
            let gate = &gate;
            let spawned = std::thread::Builder::new()
                .name(String::from("sim-tenant-walk"))
                .spawn_scoped(scope, move || {
                    gate.wait();
                    tenant_walk(&tree.root, plan)
                        .map(|together| &together == alone)
                        .map_err(|error| error.to_string())
                });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    refused = Some(format!("spawn refused at {}: {error}", handles.len()));
                    break;
                }
            }
        }
        gate.release();
        let mut outcomes: Vec<Result<bool, String>> = Vec::new();
        for handle in handles {
            outcomes.push(match handle.join() {
                Ok(outcome) => outcome,
                Err(_panic) => Err(String::from("a tenant walk panicked")),
            });
        }
        if let Some(refused) = refused {
            outcomes.push(Err(refused));
        }
        outcomes
    });
    let mut reached = 0_usize;
    for outcome in outcomes {
        let agreed = outcome?;
        assert!(
            agreed,
            "tenant {reached}'s concurrent walk differed from its walk alone"
        );
        reached = reached.saturating_add(1);
    }
    Ok(reached)
}

#[test]
/// One hundred tenants walk eight shared trees at once, each with its own
/// skip and stop; every walk equals the same tenant's walk run alone.
fn sim_one_hundred_tenants_walk_at_once_with_their_own_predicates() -> TestResult {
    let reached = concurrent_tenants(100, 8)?;
    assert_eq!(
        reached, 100,
        "requested 100 walks in flight, reached {reached}"
    );
    Ok(())
}

#[test]
/// One thousand tenants walk eight shared trees at once, each with its own
/// skip and stop; every walk equals the same tenant's walk run alone.
fn sim_a_thousand_tenants_walk_at_once_with_their_own_predicates() -> TestResult {
    let reached = concurrent_tenants(1_000, 9)?;
    assert_eq!(
        reached, 1_000,
        "requested 1000 walks in flight, reached {reached}"
    );
    Ok(())
}

// ── Replay ─────────────────────────────────────────────────────────────────

/// A kind as a trace word.
fn kind_word(kind: FileKind) -> u64 {
    match kind {
        FileKind::File => 1,
        FileKind::Directory => 2,
        FileKind::Symlink => 3,
        _ => 4,
    }
}

/// One seed's whole scenario: grow, skip a seeded directory, stop at a seeded
/// offer, and fold every recorded entry relative to the root into a trace.
fn scenario(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut stream = Seeded::from_seed(seed);
    let tree = Tree::grow(&mut stream)?;
    let pruned = tree.root.join(tree.some_directory(&mut stream)?);
    let stop_at = stream.index(tree.nodes.len().saturating_add(1))?;
    let plan = Tenant {
        tree: 0,
        pruned,
        stop_at: Some(stop_at),
    };
    let mut offered = 0_usize;
    let report = walk_dir_entries_tolerant(&tree.root, &unbounded(), None, |path, _kind| {
        let index = offered;
        offered = offered.saturating_add(1);
        if Some(index) == plan.stop_at {
            Descend::Stop
        } else {
            skip_only(&plan.pruned, path)
        }
    })?;
    let mut trace = initial_trace();
    fold_usize(&mut trace, tree.nodes.len());
    fold_usize(&mut trace, plan.tree);
    for entry in report.entries() {
        for byte in entry.relative_path().as_os_str().as_encoded_bytes() {
            fold(&mut trace, u64::from(*byte));
        }
        fold(&mut trace, kind_word(entry.kind()));
        if entry.kind() == FileKind::File {
            fold(&mut trace, entry.metadata().len());
        }
    }
    fold(&mut trace, u64::from(report.stopped()));
    fold_usize(&mut trace, report.omissions().len());
    fold_usize(&mut trace, offered);
    Ok(trace)
}

#[test]
/// The same seed replays to the same trace hash, on a freshly grown tree.
fn sim_the_same_seed_replays_to_the_same_trace_hash() -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(10, index);
        assert_eq!(
            scenario(seed)?,
            scenario(seed)?,
            "seed {seed:#x}: the replay diverged"
        );
    }
    Ok(())
}

#[test]
/// Distinct seeds explore distinct trees, skips and stops: the sweep is not
/// one scenario run forty-eight times.
fn sim_distinct_seeds_diverge_in_their_trace() -> TestResult {
    let mut traces = BTreeSet::new();
    let mut seeds = 0_usize;
    for index in 0..SEEDS {
        traces.insert(scenario(seed_for(11, index))?);
        seeds = seeds.saturating_add(1);
    }
    let distinct = traces.len();
    assert!(
        distinct.saturating_mul(4) >= seeds.saturating_mul(3),
        "only {distinct} distinct traces from {seeds} seeds"
    );
    Ok(())
}
