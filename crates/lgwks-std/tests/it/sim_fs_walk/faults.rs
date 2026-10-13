//! Unix faults under the seeded walk: locked directories, the omission cap, and dangling or looping symlinks.

use super::*;

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
        .filter(|path| match path.strip_prefix(&tree.root) {
            Ok(rel) => {
                locks.iter().any(|lock| lock == rel)
                    && !locks
                        .iter()
                        .any(|other| other != rel && rel.starts_with(other))
            }
            // The preorder joins every path onto the root; one outside it is
            // not a directory of this tree, so it is no lock the walk reaches.
            Err(_) => false,
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
    trace.record_number("nodes", tree.nodes.len());
    let walk = options(draw(&mut rng, 0, 4), false, true);
    for path in tree.relative(&walk_dir(&tree.root, &walk)?) {
        trace.record(&path);
    }
    let limits = limits_near(&mut rng, &tree);
    let report: WalkReport = walk_dir_tolerant_bounded(&tree.root, &unbounded(), &limits)?;
    trace.record_number("complete", u64::from(report.is_complete()));
    trace.record_number("prefix", report.entries().len());
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
    // The seeds drawn are counted here rather than read back from the constant:
    // the count is then a count on both sides of the comparison, with nothing
    // narrowed between the sweep and the number it reports.
    let mut seeds = 0_usize;
    for index in 0..SEEDS {
        hashes.insert(scenario(seed_for(26, index))?.hash());
        seeds = seeds.saturating_add(1);
    }
    let distinct = hashes.len();
    assert!(
        distinct.saturating_mul(4) >= seeds.saturating_mul(3),
        "only {distinct} distinct traces from {seeds} seeds"
    );
    Ok(())
}
