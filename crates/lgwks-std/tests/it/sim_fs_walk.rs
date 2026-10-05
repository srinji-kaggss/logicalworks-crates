//! Deterministic simulation of the path-based walk (INV-FS-2, INV-FS-5).
//!
//! One seed grows one real directory tree on disk: its width, depth, names
//! (multi-byte, spaced and dotted ones included) and which entries are
//! directories. The same seed chooses the walk policy, the budgets and the
//! faults injected into the tree: directories made unreadable, symlinks left
//! dangling, links that leave the root and links that loop back into it. The
//! shipped `lgwks_std::fs` walk runs against that tree through its public API,
//! and every answer is checked against a model computed from the seed alone:
//! which paths a policy admits, in which order, how many entries and path
//! bytes they charge, and so exactly when a budget must refuse.
//!
//! The model never reads the walk's output to decide what the output should
//! be. A walk that dropped, duplicated or reordered an entry, or reported a
//! budget-limited prefix as complete, disagrees with it for some seed.
//!
//! The seed substrate (`Rng`, `Trace`) is the one the `lgwks_bot` simulation
//! families run on, included by path, so a seed means the same draw sequence
//! in every suite.
#![cfg(feature = "trace")]

#[path = "../../../lgwks-bot/tests/sim/seed.rs"]
mod seed;

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lgwks_std::fs::{
    OmissionStage, WalkFailure, WalkLimits, WalkOptions, WalkReport, walk_dir, walk_dir_bounded,
    walk_dir_tolerant, walk_dir_tolerant_bounded,
};
use seed::{Rng, Trace};

/// What every family returns: a walk or filesystem refusal reports itself.
type TestResult = Result<(), Box<dyn Error>>;

/// Trees grown per family. Each is a real tree on disk, so this is the
/// number of distinct filesystems every property is checked against.
const SEEDS: u64 = 48;

/// The base every family's seeds are derived from.
const BASE_SEED: u64 = 0x5eed_f5a1_2026_0930;

/// Name stems. Multi-byte, spaced, dotted and leading-dash names are the ones
/// a byte-counting or string-splitting walk gets wrong.
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
const MAX_LEVEL: u32 = 4;

/// The widest directory a grown tree holds.
const MAX_WIDTH: u32 = 5;

/// A budget no grown tree reaches: the dimension a test is not probing.
const ROOMY: usize = 1_000_000;

/// The seed for run `index` of `family`.
///
/// Families take disjoint seed streams, so two families never check the same
/// tree and a failing seed names its family.
fn seed_for(family: u64, index: u64) -> u64 {
    BASE_SEED
        ^ family.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// A `usize` draw in `low..=high`.
fn draw(rng: &mut Rng, low: usize, high: usize) -> usize {
    let low32 = u32::try_from(low).unwrap_or(u32::MAX);
    let high32 = u32::try_from(high).unwrap_or(u32::MAX);
    usize::try_from(rng.between(low32, high32)).unwrap_or(low)
}

/// One grown entry, relative to the tree's root.
struct Node {
    /// Path below the root.
    rel: PathBuf,
    /// Whether the entry is a directory.
    dir: bool,
}

impl Node {
    /// Components below the root: the root's children are level 1.
    fn level(&self) -> usize {
        self.rel.components().count()
    }
}

/// A seeded tree on disk and the model of what it holds.
struct SimTree {
    /// Owns the scratch; removed on drop.
    temp: tempfile::TempDir,
    /// The canonical root every walked path must sit under.
    root: PathBuf,
    /// Every grown entry, in creation order.
    nodes: Vec<Node>,
}

impl SimTree {
    /// Grows the tree for `seed`, drawing everything from `rng`.
    fn grow(rng: &mut Rng) -> io::Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root");
        fs::create_dir(&root)?;
        let root = root.canonicalize()?;
        let mut tree = Self {
            temp,
            root,
            nodes: Vec::new(),
        };
        let max_level = rng.between(1, MAX_LEVEL);
        tree.grow_dir(rng, Path::new(""), 1, max_level)?;
        Ok(tree)
    }

    /// Fills `rel_dir` with a seeded number of files and directories.
    ///
    /// The root always holds at least one entry, so no family degenerates into
    /// walking nothing. A name is a stem plus its index, which keeps names
    /// unique within a directory without the model having to check.
    fn grow_dir(
        &mut self,
        rng: &mut Rng,
        rel_dir: &Path,
        level: u32,
        max_level: u32,
    ) -> io::Result<()> {
        let min_width = u32::from(level == 1);
        let width = rng.between(min_width, MAX_WIDTH);
        for index in 0..width {
            let stem_count = u32::try_from(STEMS.len()).unwrap_or(1);
            let stem = STEMS
                .get(usize::try_from(rng.below(stem_count)).unwrap_or(0))
                .copied()
                .unwrap_or("a");
            let rel = rel_dir.join(format!("{stem}{index}"));
            if level < max_level && rng.chance(400) {
                fs::create_dir(self.root.join(&rel))?;
                self.nodes.push(Node {
                    rel: rel.clone(),
                    dir: true,
                });
                self.grow_dir(rng, &rel, level.saturating_add(1), max_level)?;
            } else {
                fs::write(self.root.join(&rel), rel.as_os_str().as_encoded_bytes())?;
                self.nodes.push(Node { rel, dir: false });
            }
        }
        Ok(())
    }

    /// Whether a walk at `max_depth` lists `node`: the root is read at depth
    /// zero, so an entry at level `n` is listed when `n - 1 <= max_depth`.
    fn admits(node: &Node, max_depth: usize) -> bool {
        node.level() <= max_depth.saturating_add(1)
    }

    /// Every absolute path a walk at `max_depth` must return.
    fn expected(&self, max_depth: usize) -> BTreeSet<PathBuf> {
        self.nodes
            .iter()
            .filter(|node| Self::admits(node, max_depth))
            .map(|node| self.root.join(&node.rel))
            .collect()
    }

    /// The direct children of `rel_dir`, sorted by file name as the walk
    /// sorts them.
    fn children(&self, rel_dir: &Path) -> Vec<&Node> {
        let mut children: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|node| node.rel.parent() == Some(rel_dir))
            .collect();
        children.sort_by_key(|node| node.rel.file_name().map(OsString::from));
        children
    }

    /// The sorted walk's exact output: depth-first preorder, each directory's
    /// entries in file-name order, a directory before its descendants.
    fn preorder(&self, max_depth: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        self.preorder_into(Path::new(""), max_depth, &mut out);
        out
    }

    /// Appends `rel_dir`'s admitted subtree to `out` in walk order.
    fn preorder_into(&self, rel_dir: &Path, max_depth: usize, out: &mut Vec<PathBuf>) {
        for node in self.children(rel_dir) {
            if !Self::admits(node, max_depth) {
                continue;
            }
            out.push(self.root.join(&node.rel));
            if node.dir {
                self.preorder_into(&node.rel, max_depth, out);
            }
        }
    }

    /// The widest directory a walk at `max_depth` reads.
    fn widest(&self, max_depth: usize) -> usize {
        let mut widths = vec![self.children(Path::new("")).len()];
        widths.extend(
            self.nodes
                .iter()
                .filter(|node| node.dir && node.level() <= max_depth)
                .map(|node| self.children(&node.rel).len()),
        );
        widths.into_iter().max().unwrap_or(0)
    }

    /// Path bytes a walk at `max_depth` charges: one charge per listed entry.
    fn path_bytes(&self, max_depth: usize) -> usize {
        self.expected(max_depth)
            .iter()
            .map(|path| path.as_os_str().len())
            .fold(0, usize::saturating_add)
    }

    /// Directory nodes, for a fault to land on.
    fn directories(&self) -> Vec<&Node> {
        self.nodes.iter().filter(|node| node.dir).collect()
    }

    /// Any directory: the root or one of the grown ones.
    fn some_directory(&self, rng: &mut Rng) -> PathBuf {
        let dirs = self.directories();
        let pick = draw(rng, 0, dirs.len());
        dirs.get(pick)
            .map_or_else(PathBuf::new, |node| node.rel.clone())
    }

    /// `paths` relative to the root, for a trace that does not carry the
    /// scratch directory's random name.
    fn relative(&self, paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| {
                path.strip_prefix(&self.root).map_or_else(
                    |_| format!("OUTSIDE:{}", path.display()),
                    |rel| rel.display().to_string(),
                )
            })
            .collect()
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

/// The depth every family uses when depth is not what it probes: deeper than
/// any grown tree.
fn unbounded() -> WalkOptions {
    options(32, false, true)
}

/// The typed failure a strict refusal carries.
fn failure(error: &io::Error) -> Option<&WalkFailure> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<WalkFailure>())
}

/// A budget near `exact`: one under, exact, one over, or anywhere up to two
/// over. The first three are the boundaries a budget check gets wrong.
fn budget_near(rng: &mut Rng, exact: usize) -> usize {
    match rng.below(4) {
        0 => exact.saturating_sub(1),
        1 => exact,
        2 => exact.saturating_add(1),
        _ => draw(rng, 0, exact.saturating_add(2)),
    }
}

/// Runs `family` over [`SEEDS`] grown trees.
fn sweep(family: u64, mut check: impl FnMut(u64, &mut Rng, &SimTree) -> TestResult) -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(family, index);
        let mut rng = Rng::new(seed);
        let tree = SimTree::grow(&mut rng)?;
        check(seed, &mut rng, &tree).map_err(|error| format!("seed {seed:#x}: {error}"))?;
    }
    Ok(())
}

// ── Coverage and order ─────────────────────────────────────────────────────

#[test]
/// A strict walk with no budget returns exactly the grown tree.
fn a_strict_walk_returns_exactly_the_grown_tree() -> TestResult {
    sweep(1, |seed, _, tree| {
        let walked = walk_dir(&tree.root, &unbounded())?;
        let set: BTreeSet<PathBuf> = walked.iter().cloned().collect();
        assert_eq!(
            set.len(),
            walked.len(),
            "seed {seed:#x}: a path was returned twice"
        );
        assert_eq!(
            set,
            tree.expected(32),
            "seed {seed:#x}: walk and model disagree"
        );
        Ok(())
    })
}

#[test]
/// Every returned path is absolute and under the canonical root.
fn every_path_is_absolute_under_the_canonical_root() -> TestResult {
    sweep(2, |seed, _, tree| {
        for path in walk_dir(&tree.root, &unbounded())? {
            assert!(
                path.is_absolute(),
                "seed {seed:#x}: {} is relative",
                path.display()
            );
            assert!(
                path.starts_with(&tree.root),
                "seed {seed:#x}: {} is outside {}",
                path.display(),
                tree.root.display()
            );
        }
        Ok(())
    })
}

#[test]
/// The sorted walk is depth-first preorder with each directory in name order.
fn the_sorted_walk_is_name_ordered_depth_first_preorder() -> TestResult {
    sweep(3, |seed, _, tree| {
        assert_eq!(
            walk_dir(&tree.root, &unbounded())?,
            tree.preorder(32),
            "seed {seed:#x}: the sorted walk is not the model's preorder"
        );
        Ok(())
    })
}

#[test]
/// Turning sorting off changes order at most, never the set of entries.
fn sorting_off_changes_order_never_coverage() -> TestResult {
    sweep(4, |seed, _, tree| {
        let sorted: BTreeSet<PathBuf> = walk_dir(&tree.root, &unbounded())?.into_iter().collect();
        let unsorted: BTreeSet<PathBuf> = walk_dir(&tree.root, &options(32, false, false))?
            .into_iter()
            .collect();
        assert_eq!(
            sorted, unsorted,
            "seed {seed:#x}: sorting changed which entries were found"
        );
        Ok(())
    })
}

#[test]
/// A tolerant walk over a healthy tree is complete and equals the strict walk.
fn a_healthy_tolerant_walk_is_complete_and_equals_the_strict_walk() -> TestResult {
    sweep(5, |seed, _, tree| {
        let report = walk_dir_tolerant(&tree.root, &unbounded())?;
        assert!(
            report.is_complete(),
            "seed {seed:#x}: a healthy tree reported incomplete"
        );
        assert!(
            report.omissions().is_empty(),
            "seed {seed:#x}: a healthy tree has omissions"
        );
        assert!(
            !report.budget_exhausted(),
            "seed {seed:#x}: no budget was set"
        );
        assert_eq!(
            report.entries(),
            walk_dir(&tree.root, &unbounded())?.as_slice(),
            "seed {seed:#x}: tolerant and strict walks disagree on a healthy tree"
        );
        Ok(())
    })
}

#[test]
/// Two walks of one tree return identical output.
fn a_walk_is_reproducible() -> TestResult {
    sweep(6, |seed, rng, tree| {
        let walk = options(draw(rng, 0, 4), false, true);
        assert_eq!(
            walk_dir(&tree.root, &walk)?,
            walk_dir(&tree.root, &walk)?,
            "seed {seed:#x}: the same walk of the same tree differed"
        );
        Ok(())
    })
}

// ── Depth policy ───────────────────────────────────────────────────────────

#[test]
/// A depth limit admits exactly the levels it names, in preorder.
fn a_depth_limit_admits_exactly_the_levels_it_names() -> TestResult {
    sweep(7, |seed, rng, tree| {
        let max_depth = draw(rng, 0, 4);
        assert_eq!(
            walk_dir(&tree.root, &options(max_depth, false, true))?,
            tree.preorder(max_depth),
            "seed {seed:#x}: depth {max_depth} admitted the wrong entries"
        );
        Ok(())
    })
}

#[test]
/// A depth exclusion is policy, not an omission: the report stays complete and
/// names the depth it applied.
fn a_depth_exclusion_is_complete_within_policy() -> TestResult {
    sweep(8, |seed, rng, tree| {
        let max_depth = draw(rng, 0, 3);
        let report = walk_dir_tolerant(&tree.root, &options(max_depth, false, true))?;
        assert!(
            report.is_complete_within_policy(),
            "seed {seed:#x}: a depth limit made the report incomplete"
        );
        assert_eq!(
            report.policy().max_depth,
            max_depth,
            "seed {seed:#x}: wrong policy depth"
        );
        assert!(
            report.policy().sort_alphabetically,
            "seed {seed:#x}: wrong ordering policy"
        );
        assert!(
            !report.policy().follow_symlinks,
            "seed {seed:#x}: wrong symlink policy"
        );
        Ok(())
    })
}

// ── Budgets ────────────────────────────────────────────────────────────────

#[test]
/// The entry budget refuses exactly when the tree holds more entries.
fn the_entry_budget_refuses_exactly_when_exceeded() -> TestResult {
    sweep(9, |seed, rng, tree| {
        let total = tree.expected(32).len();
        let budget = budget_near(rng, total);
        let limits = WalkLimits::new(budget, ROOMY, ROOMY, ROOMY);
        match walk_dir_bounded(&tree.root, &unbounded(), &limits) {
            Ok(walked) => {
                assert!(
                    total <= budget,
                    "seed {seed:#x}: {total} entries passed a budget of {budget}"
                );
                assert_eq!(
                    walked.len(),
                    total,
                    "seed {seed:#x}: a passing walk lost entries"
                );
            }
            Err(error) => {
                assert!(
                    total > budget,
                    "seed {seed:#x}: {total} entries refused by a budget of {budget}"
                );
                assert_eq!(
                    failure(&error).map(WalkFailure::stage),
                    Some(OmissionStage::ResourceBudget),
                    "seed {seed:#x}: the refusal does not name the budget: {error}"
                );
            }
        }
        Ok(())
    })
}

#[test]
/// The path-byte budget refuses exactly when the listed paths hold more bytes.
fn the_path_byte_budget_refuses_exactly_when_exceeded() -> TestResult {
    sweep(10, |seed, rng, tree| {
        let total = tree.path_bytes(32);
        let budget = budget_near(rng, total);
        let limits = WalkLimits::new(ROOMY, ROOMY, budget, ROOMY);
        let result = walk_dir_bounded(&tree.root, &unbounded(), &limits);
        assert_eq!(
            result.is_ok(),
            total <= budget,
            "seed {seed:#x}: {total} path bytes against a budget of {budget}"
        );
        Ok(())
    })
}

#[test]
/// The per-directory budget refuses exactly when some directory is wider.
fn the_directory_width_budget_refuses_exactly_when_exceeded() -> TestResult {
    sweep(11, |seed, rng, tree| {
        let widest = tree.widest(32);
        let budget = budget_near(rng, widest);
        let limits = WalkLimits::new(ROOMY, budget, ROOMY, ROOMY);
        let result = walk_dir_bounded(&tree.root, &unbounded(), &limits);
        assert_eq!(
            result.is_ok(),
            widest <= budget,
            "seed {seed:#x}: widest directory {widest} against a budget of {budget}"
        );
        Ok(())
    })
}

#[test]
/// Budgets are charged only for what the depth policy reads.
fn a_budget_is_charged_only_within_the_depth_policy() -> TestResult {
    sweep(12, |seed, rng, tree| {
        let max_depth = draw(rng, 0, 2);
        let walk = options(max_depth, false, true);
        let total = tree.expected(max_depth).len();
        let limits = WalkLimits::new(total, tree.widest(max_depth), tree.path_bytes(max_depth), 0);
        assert_eq!(
            walk_dir_bounded(&tree.root, &walk, &limits)?,
            tree.preorder(max_depth),
            "seed {seed:#x}: a budget sized to depth {max_depth} refused"
        );
        Ok(())
    })
}

/// Whether a tree fits `limits` under the unbounded depth policy.
fn fits(tree: &SimTree, limits: &WalkLimits) -> bool {
    tree.expected(32).len() <= limits.max_entries
        && tree.widest(32) <= limits.max_directory_entries
        && tree.path_bytes(32) <= limits.max_path_bytes
}

/// Seeded limits that land on either side of the tree's true size.
fn limits_near(rng: &mut Rng, tree: &SimTree) -> WalkLimits {
    WalkLimits::new(
        budget_near(rng, tree.expected(32).len().saturating_add(1)),
        budget_near(rng, tree.widest(32).saturating_add(1)),
        budget_near(rng, tree.path_bytes(32).saturating_add(8)),
        ROOMY,
    )
}

#[test]
/// A tolerant bounded walk marks its prefix incomplete exactly when a budget
/// ran out, and the prefix never exceeds the budget.
fn a_tolerant_budget_prefix_is_bounded_and_marked() -> TestResult {
    sweep(13, |seed, rng, tree| {
        let limits = limits_near(rng, tree);
        let report = walk_dir_tolerant_bounded(&tree.root, &unbounded(), &limits)?;
        assert_eq!(
            report.budget_exhausted(),
            !fits(tree, &limits),
            "seed {seed:#x}: exhaustion disagrees with the model for {limits:?}"
        );
        assert_eq!(
            report.is_complete(),
            fits(tree, &limits),
            "seed {seed:#x}: completeness disagrees with the model"
        );
        let expected = tree.expected(32);
        assert!(
            report.entries().iter().all(|path| expected.contains(path)),
            "seed {seed:#x}: the prefix holds a path the tree does not"
        );
        assert!(
            report.entries().len() <= limits.max_entries,
            "seed {seed:#x}: the prefix holds more entries than the budget"
        );
        let bytes = report
            .entries()
            .iter()
            .map(|path| path.as_os_str().len())
            .fold(0, usize::saturating_add);
        assert!(
            bytes <= limits.max_path_bytes,
            "seed {seed:#x}: the prefix holds more path bytes than the budget"
        );
        Ok(())
    })
}

#[test]
/// Whether a bounded walk completes does not depend on entry order.
fn completeness_does_not_depend_on_sorting() -> TestResult {
    sweep(14, |seed, rng, tree| {
        let limits = limits_near(rng, tree);
        let sorted = walk_dir_tolerant_bounded(&tree.root, &unbounded(), &limits)?;
        let unsorted = walk_dir_tolerant_bounded(&tree.root, &options(32, false, false), &limits)?;
        assert_eq!(
            sorted.is_complete(),
            unsorted.is_complete(),
            "seed {seed:#x}: filesystem order decided completeness for {limits:?}"
        );
        Ok(())
    })
}

#[test]
/// A bounded walk with room to spare is the unbounded walk.
fn a_bounded_walk_with_room_is_the_unbounded_walk() -> TestResult {
    sweep(15, |seed, _, tree| {
        let limits = WalkLimits::new(
            tree.expected(32).len(),
            tree.widest(32),
            tree.path_bytes(32),
            0,
        );
        assert_eq!(
            walk_dir_bounded(&tree.root, &unbounded(), &limits)?,
            walk_dir(&tree.root, &unbounded())?,
            "seed {seed:#x}: exact limits changed the output"
        );
        Ok(())
    })
}

// ── Refusals ───────────────────────────────────────────────────────────────

#[test]
/// A root that does not resolve is refused by every entry point, never walked
/// as an empty tree.
fn an_unresolvable_root_is_refused_by_every_entry_point() -> TestResult {
    sweep(16, |seed, _, tree| {
        let missing = tree.root.join("never-created");
        let limits = WalkLimits::new(ROOMY, ROOMY, ROOMY, ROOMY);
        let refusals = [
            walk_dir(&missing, &unbounded()).err(),
            walk_dir_tolerant(&missing, &unbounded()).err(),
            walk_dir_bounded(&missing, &unbounded(), &limits).err(),
            walk_dir_tolerant_bounded(&missing, &unbounded(), &limits).err(),
        ];
        for refusal in refusals {
            let error =
                refusal.ok_or_else(|| format!("seed {seed:#x}: a missing root was walked"))?;
            assert_eq!(
                error.kind(),
                io::ErrorKind::NotFound,
                "seed {seed:#x}: wrong kind"
            );
            let typed =
                failure(&error).ok_or_else(|| format!("seed {seed:#x}: untyped refusal"))?;
            assert_eq!(
                typed.stage(),
                OmissionStage::RootResolution,
                "seed {seed:#x}: wrong stage"
            );
            assert_eq!(
                typed.path(),
                missing.as_path(),
                "seed {seed:#x}: the refusal names another path"
            );
        }
        Ok(())
    })
}

#[test]
/// A file given as the root is refused by strict and reported by tolerant.
fn a_file_root_is_refused_not_walked_as_empty() -> TestResult {
    sweep(17, |seed, rng, tree| {
        let files: Vec<&Node> = tree.nodes.iter().filter(|node| !node.dir).collect();
        let Some(file) = files.get(draw(rng, 0, files.len().saturating_sub(1))) else {
            return Ok(());
        };
        let path = tree.root.join(&file.rel);
        let error = walk_dir(&path, &unbounded())
            .err()
            .ok_or_else(|| format!("seed {seed:#x}: a file was walked as a directory"))?;
        assert_eq!(
            failure(&error).map(WalkFailure::stage),
            Some(OmissionStage::ReadEntries),
            "seed {seed:#x}: {error}"
        );
        let report = walk_dir_tolerant(&path, &unbounded())?;
        assert!(
            !report.is_complete(),
            "seed {seed:#x}: a file root reported a complete walk"
        );
        assert!(
            report.entries().is_empty(),
            "seed {seed:#x}: a file root produced entries"
        );
        Ok(())
    })
}

// ── Faults (Unix: permissions and symlinks) ────────────────────────────────

/// Directories made unreadable for one scenario, restored on drop so the
/// scratch can be removed whatever the scenario concluded.
#[cfg(unix)]
struct Locked {
    /// Absolute paths set to mode 000.
    paths: Vec<PathBuf>,
}

#[cfg(unix)]
impl Locked {
    /// Removes every permission from each of `paths`.
    ///
    /// Deepest first: once a parent is mode 000 its children cannot be
    /// reached to lock. `paths` is sorted, so a parent precedes its children
    /// and the reverse order locks children first. The guard is built before
    /// the first lock, so a refusal part-way still restores what was locked.
    fn lock(mut paths: Vec<PathBuf>) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        paths.sort();
        let guard = Self { paths };
        for path in guard.paths.iter().rev() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o000))?;
        }
        Ok(guard)
    }

    /// Whether this process can read a locked directory anyway (it runs with
    /// privileges that bypass mode bits), in which case the fault is inert and
    /// the model is the healthy one.
    fn inert(&self) -> bool {
        self.paths.iter().any(|path| fs::read_dir(path).is_ok())
    }
}

#[cfg(unix)]
impl Drop for Locked {
    /// Parents first, the reverse of locking, so each child is reachable
    /// again by the time its turn comes.
    ///
    /// A `Drop` cannot report a failure, so a permission that will not be
    /// restored is written to stderr rather than dropped: a directory left
    /// read-only makes the next test in the sweep fail with a permission error
    /// that names neither this struct nor the test that caused it. Stderr is the
    /// only sink a destructor has, and this is test code.
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        for path in &self.paths {
            if let Err(refused) = fs::set_permissions(path, fs::Permissions::from_mode(0o755)) {
                // The sanctioned facade, not `eprintln!`: the workspace forbids
                // printing to the terminal, and a destructor has no other way to
                // say a directory is still read-only.
                lgwks_std::trace::warn!(
                    operation = "restore_permissions",
                    path = %path.display(),
                    "sim_fs_walk could not restore a locked directory to 0755: {refused}"
                );
            }
        }
    }
}

/// Seeded distinct grown directories to lock, as paths below the root.
#[cfg(unix)]
fn pick_locks(rng: &mut Rng, tree: &SimTree) -> Vec<PathBuf> {
    let mut picked = BTreeSet::new();
    for node in tree.directories() {
        if rng.chance(500) {
            picked.insert(node.rel.clone());
        }
    }
    picked.into_iter().collect()
}

/// The locked directories a walk actually reaches: those not beneath another
/// locked directory, in walk order.
#[cfg(unix)]
fn reachable_locks(tree: &SimTree, locks: &[PathBuf]) -> Vec<PathBuf> {
    tree.preorder(32)
        .into_iter()
        .filter(|path| {
            path.strip_prefix(&tree.root).is_ok_and(|rel| {
                locks.iter().any(|lock| lock == rel)
                    && !locks
                        .iter()
                        .any(|other| other != rel && rel.starts_with(other))
            })
        })
        .collect()
}

#[test]
#[cfg(unix)]
/// Strict refuses the first unreadable directory in walk order, naming it and
/// the stage.
fn strict_refuses_the_first_unreadable_directory() -> TestResult {
    sweep(18, |seed, rng, tree| {
        let locks = pick_locks(rng, tree);
        let reachable = reachable_locks(tree, &locks);
        let guard = Locked::lock(locks.iter().map(|rel| tree.root.join(rel)).collect())?;
        let result = walk_dir(&tree.root, &unbounded());
        if guard.inert() || reachable.is_empty() {
            assert!(
                result.is_ok(),
                "seed {seed:#x}: nothing unreadable, yet refused"
            );
            return Ok(());
        }
        let error = result
            .err()
            .ok_or_else(|| format!("seed {seed:#x}: an unreadable directory was not refused"))?;
        let typed =
            failure(&error).ok_or_else(|| format!("seed {seed:#x}: untyped refusal {error}"))?;
        assert_eq!(
            typed.stage(),
            OmissionStage::ReadEntries,
            "seed {seed:#x}: wrong stage"
        );
        assert_eq!(
            Some(typed.path()),
            reachable.first().map(PathBuf::as_path),
            "seed {seed:#x}: the refusal names the wrong directory"
        );
        Ok(())
    })
}

#[test]
#[cfg(unix)]
/// Tolerant reports each unreadable directory once and keeps everything else.
fn tolerant_reports_unreadable_directories_and_keeps_the_rest() -> TestResult {
    sweep(19, |seed, rng, tree| {
        let locks = pick_locks(rng, tree);
        let reachable = reachable_locks(tree, &locks);
        let guard = Locked::lock(locks.iter().map(|rel| tree.root.join(rel)).collect())?;
        let report = walk_dir_tolerant(&tree.root, &unbounded())?;
        let reachable = if guard.inert() { Vec::new() } else { reachable };
        let omitted: Vec<PathBuf> = report
            .omissions()
            .iter()
            .map(|omission| omission.path.clone())
            .collect();
        assert_eq!(
            omitted, reachable,
            "seed {seed:#x}: the omissions are not the unreadable directories"
        );
        assert!(
            report
                .omissions()
                .iter()
                .all(|omission| omission.stage == OmissionStage::ReadEntries),
            "seed {seed:#x}: an unreadable directory was reported at another stage"
        );
        let kept: Vec<PathBuf> = tree
            .preorder(32)
            .into_iter()
            .filter(|path| {
                !reachable
                    .iter()
                    .any(|lock| path != lock && path.starts_with(lock))
            })
            .collect();
        assert_eq!(
            report.entries(),
            kept.as_slice(),
            "seed {seed:#x}: readable entries were lost"
        );
        assert_eq!(
            report.is_complete(),
            reachable.is_empty(),
            "seed {seed:#x}: completeness is wrong"
        );
        Ok(())
    })
}

#[test]
#[cfg(unix)]
/// The omission cap bounds what tolerant records and marks the report.
fn the_omission_cap_bounds_the_report_and_marks_it() -> TestResult {
    sweep(20, |seed, rng, tree| {
        let locks = pick_locks(rng, tree);
        let reachable = reachable_locks(tree, &locks);
        let guard = Locked::lock(locks.iter().map(|rel| tree.root.join(rel)).collect())?;
        if guard.inert() || reachable.is_empty() {
            return Ok(());
        }
        let cap = draw(rng, 0, reachable.len());
        let limits = WalkLimits::new(ROOMY, ROOMY, ROOMY, cap);
        let report = walk_dir_tolerant_bounded(&tree.root, &unbounded(), &limits)?;
        assert!(
            report.omissions().len() <= cap,
            "seed {seed:#x}: more omissions than the cap {cap}"
        );
        assert_eq!(
            report.budget_exhausted(),
            reachable.len() > cap,
            "seed {seed:#x}: {} unreadable directories against a cap of {cap}",
            reachable.len()
        );
        assert!(
            !report.is_complete(),
            "seed {seed:#x}: a walk with an unreadable directory is complete"
        );
        Ok(())
    })
}

#[test]
#[cfg(unix)]
/// A dangling symlink is an omission at its own path, never a listed file.
fn a_dangling_symlink_is_an_omission_not_an_entry() -> TestResult {
    sweep(21, |seed, rng, tree| {
        let host = tree.root.join(tree.some_directory(rng));
        let link = host.join("dangling-link");
        std::os::unix::fs::symlink("target-that-does-not-exist", &link)?;
        let error = walk_dir(&tree.root, &unbounded())
            .err()
            .ok_or_else(|| format!("seed {seed:#x}: a dangling link passed a strict walk"))?;
        assert_eq!(
            failure(&error).map(WalkFailure::stage),
            Some(OmissionStage::SymlinkTarget),
            "seed {seed:#x}: {error}"
        );
        let report = walk_dir_tolerant(&tree.root, &unbounded())?;
        let omitted: Vec<&Path> = report
            .omissions()
            .iter()
            .map(|omission| omission.path.as_path())
            .collect();
        assert_eq!(
            omitted,
            vec![link.as_path()],
            "seed {seed:#x}: the omission names the wrong path"
        );
        assert_eq!(
            report.entries(),
            tree.preorder(32).as_slice(),
            "seed {seed:#x}: the dangling link changed the listed entries"
        );
        Ok(())
    })
}

#[test]
#[cfg(unix)]
/// A link out of the root is a policy exclusion: not listed, not an
/// omission, followed or not.
fn a_link_out_of_the_root_is_excluded_by_policy() -> TestResult {
    sweep(22, |seed, rng, tree| {
        let outside = tree.temp.path().join("outside");
        fs::create_dir_all(outside.join("secret"))?;
        fs::write(outside.join("secret/file"), b"outside")?;
        let host = tree.root.join(tree.some_directory(rng));
        std::os::unix::fs::symlink(&outside, host.join("escape"))?;
        for follow in [false, true] {
            let report = walk_dir_tolerant(&tree.root, &options(32, follow, true))?;
            assert!(
                report.is_complete(),
                "seed {seed:#x}: an outside link made the walk incomplete"
            );
            assert_eq!(
                report.entries(),
                tree.preorder(32).as_slice(),
                "seed {seed:#x}: follow={follow} listed or walked an outside link"
            );
        }
        Ok(())
    })
}

#[test]
#[cfg(unix)]
/// An in-root link to a directory is listed, and not descended unless the
/// policy follows links.
fn an_in_root_link_is_listed_and_not_followed_by_default() -> TestResult {
    sweep(23, |seed, rng, tree| {
        let target = tree.root.join(tree.some_directory(rng));
        let host = tree.root.join(tree.some_directory(rng));
        let link = host.join("~alias");
        std::os::unix::fs::symlink(&target, &link)?;
        let walked = walk_dir(&tree.root, &unbounded())?;
        let mut expected = tree.expected(32);
        expected.insert(link.clone());
        let set: BTreeSet<PathBuf> = walked.iter().cloned().collect();
        assert_eq!(
            set, expected,
            "seed {seed:#x}: the unfollowed link changed coverage"
        );
        assert!(
            !walked
                .iter()
                .any(|path| path != &link && path.starts_with(&link)),
            "seed {seed:#x}: an unfollowed link was descended"
        );
        Ok(())
    })
}

#[test]
#[cfg(unix)]
/// Following links visits each canonical directory once, keeps the alias in
/// the logical path, and terminates on a link back to an ancestor.
fn following_links_visits_each_directory_once_and_terminates() -> TestResult {
    sweep(24, |seed, rng, tree| {
        let target = tree.root.join(tree.some_directory(rng));
        let host = tree.root.join(tree.some_directory(rng));
        let link = host.join("~alias");
        std::os::unix::fs::symlink(&target, &link)?;
        let walked = walk_dir(&tree.root, &options(32, true, true))?;
        let logical: BTreeSet<&PathBuf> = walked.iter().collect();
        assert_eq!(
            logical.len(),
            walked.len(),
            "seed {seed:#x}: a logical path was returned twice"
        );
        let mut canonical = BTreeSet::new();
        let mut through_link = 0_usize;
        for path in &walked {
            if path == &link {
                continue;
            }
            through_link = through_link.saturating_add(usize::from(path.starts_with(&link)));
            assert!(
                canonical.insert(path.canonicalize()?),
                "seed {seed:#x}: {} reached a directory entry a second time",
                path.display()
            );
        }
        assert_eq!(
            canonical,
            tree.expected(32),
            "seed {seed:#x}: following links changed coverage"
        );
        assert_eq!(
            walked.len(),
            tree.expected(32).len().saturating_add(1),
            "seed {seed:#x}: {through_link} entries through the alias; the count is off"
        );
        Ok(())
    })
}

// ── Replay ─────────────────────────────────────────────────────────────────

/// One seed's whole scenario: grow, walk under seeded policy and budgets,
/// record every answer relative to the root.
fn scenario(seed: u64) -> Result<Trace, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let tree = SimTree::grow(&mut rng)?;
    let mut trace = Trace::new();
    trace.record_count("nodes", tree.nodes.len());
    let walk = options(draw(&mut rng, 0, 4), false, true);
    for path in tree.relative(&walk_dir(&tree.root, &walk)?) {
        trace.record(&path);
    }
    let limits = limits_near(&mut rng, &tree);
    let report: WalkReport = walk_dir_tolerant_bounded(&tree.root, &unbounded(), &limits)?;
    trace.record_u64("complete", u64::from(report.is_complete()));
    trace.record_count("prefix", report.entries().len());
    Ok(trace)
}

#[test]
/// The same seed replays to the same trace hash.
fn the_same_seed_replays_to_the_same_trace() -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(25, index);
        let first = scenario(seed)?;
        let second = scenario(seed)?;
        assert!(
            !first.is_empty(),
            "seed {seed:#x}: the scenario recorded nothing"
        );
        assert_eq!(
            first.hash(),
            second.hash(),
            "seed {seed:#x}: the replay diverged"
        );
    }
    Ok(())
}

#[test]
/// Different seeds explore different trees: the sweep is not one tree run
/// forty-eight times.
fn different_seeds_explore_different_trees() -> TestResult {
    let mut hashes = BTreeSet::new();
    for index in 0..SEEDS {
        hashes.insert(scenario(seed_for(26, index))?.hash());
    }
    let distinct = u64::try_from(hashes.len()).unwrap_or(0);
    assert!(
        distinct.saturating_mul(4) >= SEEDS.saturating_mul(3),
        "only {distinct} distinct traces from {SEEDS} seeds"
    );
    Ok(())
}
