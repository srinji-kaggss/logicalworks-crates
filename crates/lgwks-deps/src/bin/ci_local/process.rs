//! The gate's scheduler: it starts lane groups, one worker thread per group of
//! lanes that share a feature set, and runs each lane through the children
//! registry (`shared/children.rs`), which owns every process it starts.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use super::children::{Children, guard};
use super::lanes::{Lane, child_env, run_lane};
use super::{Console, LaneResult, report};

/// The directory cargo builds into, made absolute under `root`.
fn target_base(root: &Path) -> PathBuf {
    let configured = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    match configured {
        Some(path) if path.is_absolute() => path,
        Some(path) => root.join(path),
        None => root.join("target"),
    }
}

/// One group of lanes that run in order in one target directory.
struct Group<'table> {
    /// `gate-<group>` for a named group, `lane-<id>` for a lane on its own.
    key: String,
    /// The lanes, in the table's order.
    lanes: Vec<&'table Lane>,
}

/// Groups `selected` by feature set, cargo groups first: they are the long poles.
fn group_lanes<'table>(selected: &[&'table Lane]) -> Vec<Group<'table>> {
    let mut groups: Vec<Group<'table>> = Vec::new();
    for lane in selected {
        let key = match lane.group.as_deref() {
            Some(name) => format!("gate-{name}"),
            None => format!("lane-{}", lane.id),
        };
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.lanes.push(lane),
            None => groups.push(Group {
                key,
                lanes: vec![lane],
            }),
        }
    }
    groups.sort_by_key(|group| !group.key.starts_with("gate-"));
    groups
}

/// The pieces a parallel run shares between its workers.
struct Shared<'run> {
    /// The repository root.
    root: &'run Path,
    /// The scratch directory for `TMPDIR`.
    scratch: Option<&'run Path>,
    /// The base of the per-group target directories.
    base: &'run Path,
    /// The children the gate owns.
    children: &'run Children,
    /// Where lines are printed.
    console: &'run Console,
    /// The groups still to run.
    queue: Mutex<VecDeque<Group<'run>>>,
    /// Every finished lane.
    results: Mutex<Vec<LaneResult>>,
}

/// Runs one group's lanes in order.
fn run_group(shared: &Shared<'_>, group: &Group<'_>) {
    let named = group.key.starts_with("gate-");
    let target = shared.base.join(&group.key);
    let env = child_env(
        shared.scratch,
        if named { Some(target.as_path()) } else { None },
    );
    for lane in &group.lanes {
        if shared.children.is_stopping() {
            return;
        }
        let tag = if named {
            format!("  [{}]", group.key)
        } else {
            String::new()
        };
        shared.console.say(&format!("==> {}{tag}", lane.id));
        let log = shared
            .base
            .join("gate-logs")
            .join(format!("{}.log", lane.id));
        let result = run_lane(lane, shared.root, &env, Some(&log), shared.children);
        report(shared.console, &result, Some(&log));
        guard(&shared.results).push(result);
    }
}

/// Runs lane groups concurrently; lanes inside one group run in order.
pub(super) fn run_parallel(
    selected: &[&Lane],
    root: &Path,
    scratch: Option<&Path>,
    jobs: usize,
    children: &Children,
    console: &Console,
) -> Vec<LaneResult> {
    let groups = group_lanes(selected);
    let workers = jobs.max(1).min(groups.len().max(1));
    let base = target_base(root);
    let shared = Shared {
        root,
        scratch,
        base: &base,
        children,
        console,
        queue: Mutex::new(groups.into()),
        results: Mutex::new(Vec::new()),
    };
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = guard(&shared.queue).pop_front();
                    match next {
                        Some(group) => run_group(&shared, &group),
                        None => return,
                    }
                }
            });
        }
    });
    let mut finished = match shared.results.into_inner() {
        Ok(found) => found,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut ordered = Vec::new();
    for lane in selected {
        if let Some(at) = finished.iter().position(|result| result.lane_id == lane.id) {
            ordered.push(finished.swap_remove(at));
        }
    }
    ordered
}
