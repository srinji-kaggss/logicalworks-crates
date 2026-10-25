//! Which direct dependency each locked package comes through.
//!
//! A flat list of stale packages gives its reader nothing to act on: a package
//! deep in the graph is fixed by whoever depends on it, and the lock already
//! records who that is. The answer is the *direct* dependency, a registry
//! package a workspace member itself declares, from which the package is
//! reachable (the package itself, when a member declares it). Workspace
//! members and path dependencies are looked through, because their releases
//! are the reader's own to cut; the first registry package below them is the
//! release the reader waits for.
//!
//! This module reads the lock's own `dependencies` arrays and nothing else.

use std::collections::{BTreeMap, BTreeSet};

/// One locked package, as the graph needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    /// The package name.
    pub(crate) name: String,
    /// The resolved version.
    pub(crate) version: String,
    /// The lock's `source`, or `None` for a workspace member or a path
    /// dependency.
    pub(crate) source: Option<String>,
    /// The lock's `dependencies` entries: `name`, `name version` or
    /// `name version (source)`.
    pub(crate) dependencies: Vec<String>,
}

/// The lock's graph, answered once for every package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Graph {
    /// For each package, in the order the packages were given, the direct
    /// dependencies it comes through.
    via: Vec<Vec<String>>,
}

impl Graph {
    /// Reads the edges of `nodes` and computes every package's `via`.
    ///
    /// An entry that names no package of the lock is ignored: Cargo writes none,
    /// and a hand-edited lock that holds one has an edge to nowhere, which
    /// reaches nothing.
    pub(crate) fn new(nodes: Vec<Node>) -> Self {
        let edges = edges(&nodes);
        let ambiguous = ambiguous_names(&nodes);
        let directs = direct_dependencies(&nodes, &edges);
        let mut via: Vec<BTreeSet<String>> = nodes.iter().map(|_| BTreeSet::new()).collect();
        for direct in &directs {
            let Some(node) = nodes.get(*direct) else {
                continue;
            };
            let label = label(node, &ambiguous);
            for reached in reachable(*direct, &nodes, &edges) {
                if let Some(slot) = via.get_mut(reached) {
                    slot.insert(label.clone());
                }
            }
        }
        Self {
            via: via
                .into_iter()
                .map(|labels| labels.into_iter().collect())
                .collect(),
        }
    }

    /// The direct dependencies the package at `position` comes through, sorted.
    ///
    /// Empty for a workspace member, for a path dependency, and for a position
    /// the graph was not built with.
    pub(crate) fn via(&self, position: usize) -> &[String] {
        match self.via.get(position) {
            Some(labels) => labels.as_slice(),
            None => &[],
        }
    }
}

/// The names that more than one locked package carries, which are written with
/// their version so the two cannot be mistaken for each other.
fn ambiguous_names(nodes: &[Node]) -> BTreeSet<&str> {
    let mut seen = BTreeSet::new();
    let mut repeated = BTreeSet::new();
    for node in nodes {
        if !seen.insert(node.name.as_str()) {
            repeated.insert(node.name.as_str());
        }
    }
    repeated
}

/// How a direct dependency is written: its name, and its version when another
/// locked package shares the name.
fn label(node: &Node, ambiguous: &BTreeSet<&str>) -> String {
    if ambiguous.contains(node.name.as_str()) {
        format!("{} {}", node.name, node.version)
    } else {
        node.name.clone()
    }
}

/// For each package, the packages it depends on, by position.
fn edges(nodes: &[Node]) -> Vec<Vec<usize>> {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (position, node) in nodes.iter().enumerate() {
        by_name
            .entry(node.name.as_str())
            .or_default()
            .push(position);
    }
    nodes
        .iter()
        .map(|node| {
            let mut targets = BTreeSet::new();
            for entry in &node.dependencies {
                let (name, version, source) = split_entry(entry);
                for position in by_name.get(name).into_iter().flatten() {
                    let Some(candidate) = nodes.get(*position) else {
                        continue;
                    };
                    let version_ok = version.is_none_or(|wanted| wanted == candidate.version);
                    let source_ok =
                        source.is_none_or(|wanted| candidate.source.as_deref() == Some(wanted));
                    if version_ok && source_ok {
                        targets.insert(*position);
                    }
                }
            }
            targets.into_iter().collect()
        })
        .collect()
}

/// A `dependencies` entry as its name, its version and its source.
///
/// Cargo writes the shortest form that is unambiguous: `name` when the lock
/// holds one package of that name, `name version` when it holds several, and
/// `name version (source)` when two of them share a version.
fn split_entry(entry: &str) -> (&str, Option<&str>, Option<&str>) {
    let (name, rest) = match entry.split_once(' ') {
        Some((name, rest)) => (name, Some(rest)),
        None => (entry, None),
    };
    let (version, source) = match rest.map(|rest| rest.split_once(' ')) {
        Some(Some((version, source))) => (Some(version), Some(source)),
        Some(None) => (rest, None),
        None => (None, None),
    };
    let source = source.and_then(|source| {
        source
            .strip_prefix('(')
            .and_then(|inner| inner.strip_suffix(')'))
    });
    (name, version, source)
}

/// The registry packages a workspace member or path dependency declares.
fn direct_dependencies(nodes: &[Node], edges: &[Vec<usize>]) -> BTreeSet<usize> {
    let mut directs = BTreeSet::new();
    for (position, node) in nodes.iter().enumerate() {
        if node.source.is_some() {
            continue;
        }
        for target in edges.get(position).into_iter().flatten() {
            if nodes
                .get(*target)
                .is_some_and(|dependency| dependency.source.is_some())
            {
                directs.insert(*target);
            }
        }
    }
    directs
}

/// Every package reachable from `start` through registry packages, `start`
/// included.
fn reachable(start: usize, nodes: &[Node], edges: &[Vec<usize>]) -> BTreeSet<usize> {
    let mut reached = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(position) = pending.pop() {
        if !reached.insert(position) {
            continue;
        }
        for target in edges.get(position).into_iter().flatten() {
            if nodes
                .get(*target)
                .is_some_and(|dependency| dependency.source.is_some())
            {
                pending.push(*target);
            }
        }
    }
    reached
}

#[cfg(test)]
mod sim_via;
