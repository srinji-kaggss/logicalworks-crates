//! Concurrent tenants over shared trees, and the same-seed replay of a whole prune-and-stop scenario.

use super::*;

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
