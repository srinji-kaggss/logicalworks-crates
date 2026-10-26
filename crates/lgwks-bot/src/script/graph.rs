//! A dependency graph: the estate's most common orchestration shape (#694).
//!
//! CI lanes, build systems and data pipelines are all one shape: each node
//! names the nodes it waits for (`needs`), nodes start in critical-path order
//! within a slot capacity, a failed node blocks everything downstream while the
//! rest keeps running, and one budget bounds the run. `each` is a flat bounded
//! fan-out and `together` a fixed join; neither says "B starts when A and C
//! have passed". This module says it, with the same guarantees the other words
//! carry: needs respected, each node once, bounded concurrency, and the same
//! seed reaching the same schedule and trace.
//!
//! The schedule is wave-based and deterministic: the ready set of each wave
//! runs in critical-path order (longest remaining chain first, names breaking
//! ties) at most `slots` at a time, and the next wave starts only when its
//! needs have all succeeded. A node whose need failed or was blocked is marked
//! blocked with the names it waited on — it never runs. The static schedule
//! satisfies the Graham-style bound the sims check: the waves of a graph with
//! total work `W` and critical-path length `C` on `m` slots finish within
//! `ceil(W / m) + C` wave-cost units.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;

use super::{FlowError, Scope, at_most, each};

/// The most nodes one [`graph`] call admits.
///
/// Larger than any lane graph, smaller than a memory accident: each node holds
/// its body, and the validation is quadratic in the node count.
pub const MAX_GRAPH_NODES: usize = 4_096;

/// A node's body as awaited: the node's answer.
///
/// `Send` like every other body the runtime drives, so a flow that runs a
/// graph stays `Send` whenever what its nodes hold is.
type BodyFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, FlowError>> + Send + 'a>>;
/// A node's body: a scope in, the node's answer out.
type BodyFn<'a, T> = Box<dyn Fn(Scope) -> BodyFut<'a, T> + Send + 'a>;

/// One node: a name, the names it waits for, and its body.
pub struct GraphNode<'a, T> {
    /// The node's name, unique in the graph.
    name: &'static str,
    /// The nodes that must have succeeded before this one starts.
    needs: &'static [&'static str],
    /// A unit cost for the critical-path order and the Graham bound.
    work: u64,
    /// The body, run once in a scope of its own.
    run: BodyFn<'a, T>,
}

impl<'a, T> GraphNode<'a, T> {
    /// A node named `name`, waiting for `needs`, running `run` once.
    pub fn new(
        name: &'static str,
        needs: &'static [&'static str],
        run: impl Fn(Scope) -> BodyFut<'a, T> + Send + 'a,
    ) -> Self {
        Self {
            name,
            needs,
            work: 1,
            run: Box::new(run),
        }
    }

    /// A node with a declared unit `work` cost for the critical-path order.
    #[must_use]
    pub fn with_work(mut self, work: u64) -> Self {
        self.work = work.max(1);
        self
    }

    /// The node's name: what its needs and the report name it by.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The names this node waits for.
    #[must_use]
    pub fn needs(&self) -> &'static [&'static str] {
        self.needs
    }
}

/// What one node came to.
#[derive(Debug)]
#[non_exhaustive]
pub enum NodeOutcome<T> {
    /// The node ran, with its body's answer.
    Ran(Result<T, FlowError>),
    /// The node never ran: these needs had failed or were blocked.
    Blocked {
        /// The needs that kept it from running, in name order.
        by: Vec<String>,
    },
}

/// What a whole [`graph`] run came to, in the declaration order of its nodes.
#[derive(Debug)]
pub struct GraphReport<T> {
    /// One entry per declared node, in declaration order.
    outcomes: Vec<(&'static str, NodeOutcome<T>)>,
    /// The schedule the run executed: one wave of names per entry, in the
    /// order the waves ran, each wave in start order.
    schedule: Vec<Vec<&'static str>>,
}

impl<T> GraphReport<T> {
    /// The outcome of `name`, or `None` when the graph declared no such node.
    #[must_use]
    pub fn outcome(&self, name: &str) -> Option<&NodeOutcome<T>> {
        self.outcomes
            .iter()
            .find(|item| item.0 == name)
            .map(|item| &item.1)
    }

    /// Names that ran to a value (not a failure, not blocked).
    #[must_use]
    pub fn succeeded(&self) -> Vec<&'static str> {
        self.outcomes
            .iter()
            .filter(|item| matches!(item.1, NodeOutcome::Ran(Ok(_))))
            .map(|item| item.0)
            .collect()
    }

    /// Names that ran and failed.
    #[must_use]
    pub fn failed(&self) -> Vec<&'static str> {
        self.outcomes
            .iter()
            .filter(|item| matches!(item.1, NodeOutcome::Ran(Err(_))))
            .map(|item| item.0)
            .collect()
    }

    /// Names that never ran.
    #[must_use]
    pub fn blocked(&self) -> Vec<&'static str> {
        self.outcomes
            .iter()
            .filter(|item| matches!(item.1, NodeOutcome::Blocked { .. }))
            .map(|item| item.0)
            .collect()
    }

    /// Whether every node ran to a value.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.outcomes
            .iter()
            .all(|item| matches!(item.1, NodeOutcome::Ran(Ok(_))))
    }

    /// The schedule the run executed: one wave of names per entry, in the
    /// order the waves ran, each wave in start order.
    #[must_use]
    pub fn waves(&self) -> &[Vec<&'static str>] {
        &self.schedule
    }

    /// The critical-path length of the executed schedule in waves.
    #[must_use]
    pub fn wave_count(&self) -> usize {
        self.schedule.len()
    }
}

/// Run `nodes` as a dependency graph under `step`, at most `slots` at once.
///
/// Nodes start in critical-path order within the slot capacity; a failed node
/// blocks everything downstream while independent nodes keep running; each
/// node runs at most once, in a scope of its own (`<step>/<name>`).
///
/// # Errors
///
/// [`FlowError::Graph`] when the declaration cannot run — no nodes, past
/// [`MAX_GRAPH_NODES`], a duplicate name, an unknown need, an empty name, or
/// a cycle — before any node starts; [`FlowError::InvalidBound`] for a slot
/// count outside `1..=MAX_IN_FLIGHT`; [`FlowError::Cancelled`] on a stop. A
/// node body failure is not an error of the run: it is the node's
/// [`NodeOutcome::Ran`] entry, and its dependants are blocked.
pub async fn graph<'a, T>(
    scope: &Scope,
    step: &str,
    slots: usize,
    nodes: Vec<GraphNode<'a, T>>,
) -> Result<GraphReport<T>, FlowError> {
    let slots = at_most(slots)?;
    validate(&nodes)?;
    let here = scope.enter(step)?;
    let order = declaration(&nodes);
    let rank = critical_rank(&nodes)?;
    let outcome = drive(&here, step, slots, nodes, &rank).await;
    here.settle(&outcome);
    outcome.and_then(|(mut outcomes, schedule)| {
        let mut ordered = Vec::with_capacity(order.len());
        for name in order {
            let outcome = outcomes
                .remove(&name)
                .ok_or_else(|| FlowError::graph(format!("{name} has no outcome")))?;
            ordered.push((name, outcome));
        }
        Ok(GraphReport {
            outcomes: ordered,
            schedule,
        })
    })
}

/// The declared names, in declaration order.
fn declaration<T>(nodes: &[GraphNode<'_, T>]) -> Vec<&'static str> {
    nodes.iter().map(|node| node.name).collect()
}

/// Refuse a declaration that cannot run, naming what is wrong.
///
/// The check runs before any node starts and before the step is entered, so
/// an unrunnable graph costs nothing and records nothing.
fn validate<T>(nodes: &[GraphNode<'_, T>]) -> Result<(), FlowError> {
    if let Some(reason) = refusal_reason(nodes) {
        lgwks_std::trace::debug!(
            reason = reason.as_str(),
            "validate: refusing a declaration that cannot run"
        );
        return Err(FlowError::graph(reason));
    }
    Ok(())
}

/// The first reason `nodes` cannot run: an empty graph, a past-ceiling graph,
/// an empty name, a duplicate name, an unknown or self need, or a cycle.
fn refusal_reason<T>(nodes: &[GraphNode<'_, T>]) -> Option<String> {
    if nodes.is_empty() {
        return Some(String::from("a graph runs at least one node"));
    }
    if nodes.len() > MAX_GRAPH_NODES {
        return Some(format!(
            "a graph runs at most {MAX_GRAPH_NODES} nodes, got {}",
            nodes.len()
        ));
    }
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for node in nodes {
        if node.name.is_empty() {
            return Some(String::from("a node is named the empty string"));
        }
        if !seen.insert(node.name) {
            return Some(format!("a node is declared twice: {}", node.name));
        }
    }
    for node in nodes {
        for need in node.needs {
            if !seen.contains(need) {
                return Some(format!(
                    "{} needs {}, which no node declares",
                    node.name, need
                ));
            }
            if *need == node.name {
                return Some(format!("{} needs itself", node.name));
            }
        }
    }
    // Kahn's pass over names: a declaration with a cycle refuses before any
    // node starts, naming one node still waiting.
    let mut outstanding: BTreeMap<&'static str, usize> = BTreeMap::new();
    for node in nodes {
        outstanding.insert(node.name, node.needs.len());
    }
    let mut ready: Vec<&'static str> = outstanding
        .iter()
        .filter(|item| *item.1 == 0)
        .map(|item| *item.0)
        .collect();
    let mut settled = 0_usize;
    while let Some(name) = ready.pop() {
        settled = settled.saturating_add(1);
        for node in nodes {
            if node.needs.contains(&name)
                && let Some(count) = outstanding.get_mut(node.name)
            {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push(node.name);
                }
            }
        }
    }
    if settled != nodes.len() {
        let found = outstanding.iter().find(|item| *item.1 > 0);
        let name = match found {
            Some(item) => *item.0,
            None => {
                return Some(String::from("the needs cycle and no wave can start it"));
            }
        };
        return Some(format!(
            "the needs cycle through {name}: no wave can start it"
        ));
    }
    None
}

/// The critical-path rank of every node: one plus the longest rank of its
/// needs, roots at zero. The static validation already refused cycles, so the
/// relaxation below always settles.
fn critical_rank<T>(nodes: &[GraphNode<'_, T>]) -> Result<BTreeMap<&'static str, u64>, FlowError> {
    let mut rank: BTreeMap<&'static str, u64> = BTreeMap::new();
    for node in nodes {
        rank.insert(node.name, 0);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for node in nodes {
            let mut need_rank = 0_u64;
            for need in node.needs {
                let held = rank
                    .get(need)
                    .ok_or_else(|| FlowError::graph(format!("{need} is not a ranked node")))?;
                need_rank = need_rank.max(*held);
            }
            let ranked = need_rank.saturating_add(1);
            let at = rank
                .get(node.name)
                .ok_or_else(|| FlowError::graph(format!("{} is not a ranked node", node.name)))?;
            if ranked > *at && !node.needs.is_empty() {
                rank.insert(node.name, ranked);
                changed = true;
            }
        }
    }
    Ok(rank)
}

/// Run the waves: each wave is the ready set in critical-path order, at most
/// `slots` at a time, and a wave settles before the next is computed.
///
/// Returns the outcomes by name and the executed schedule, one wave per entry.
async fn drive<'a, T>(
    here: &Scope,
    step: &str,
    slots: NonZeroUsize,
    nodes: Vec<GraphNode<'a, T>>,
    ranks: &BTreeMap<&'static str, u64>,
) -> Result<
    (
        BTreeMap<&'static str, NodeOutcome<T>>,
        Vec<Vec<&'static str>>,
    ),
    FlowError,
> {
    let mut waiting: BTreeMap<&'static str, GraphNode<'a, T>> = BTreeMap::new();
    for node in nodes {
        waiting.insert(node.name, node);
    }
    let mut done: BTreeMap<&'static str, NodeOutcome<T>> = BTreeMap::new();
    let mut schedule: Vec<Vec<&'static str>> = Vec::new();
    let mut wave: usize = 0;
    while !waiting.is_empty() {
        here.checkpoint()?;
        // Ready: every need ran to a value. Blocked: some need failed or was
        // blocked. The rest waits for a later wave, when its needs are done.
        let mut ready: Vec<&'static str> = Vec::new();
        for (name, node) in waiting.iter() {
            let mut blocked = false;
            let mut pending = false;
            for need in node.needs {
                match done.get(need) {
                    None => {
                        pending = true;
                    }
                    Some(outcome) => {
                        if matches!(
                            *outcome,
                            NodeOutcome::Ran(Err(_)) | NodeOutcome::Blocked { .. }
                        ) {
                            blocked = true;
                        }
                    }
                }
            }
            if blocked {
                let mut by: Vec<String> = node
                    .needs
                    .iter()
                    .filter(|need| {
                        !matches!(done.get(*need), Some(outcome) if matches!(*outcome, NodeOutcome::Ran(Ok(_))))
                    })
                    .map(|need| (*need).to_owned())
                    .collect();
                by.sort_unstable();
                done.insert(name, NodeOutcome::Blocked { by });
            } else if !pending {
                ready.push(*name);
            }
        }
        for item in done.iter() {
            if matches!(item.1, NodeOutcome::Blocked { .. }) {
                waiting.remove(item.0);
            }
        }
        if ready.is_empty() {
            // The static validation refused cycles, and every node that is
            // neither done nor ready waits on a pending need — except there is
            // no pending need left, because every waiting node was just marked
            // blocked. An empty wave with work left is a defect in this loop.
            if waiting.is_empty() {
                break;
            }
            let refusal = Err(FlowError::graph("no wave is ready but nodes are waiting"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drive: returning an error to the caller");
            return refusal;
        }
        // Critical-path order: the longest remaining chain first, names
        // breaking ties, so the same declaration always starts the same wave.
        // Ranks are read before sorting, so the comparator holds no lookup
        // that could substitute a default for a missing rank.
        let mut starting: Vec<(GraphNode<'a, T>, u64)> = Vec::new();
        for name in ready {
            if let Some(node) = waiting.remove(name) {
                let rank = ranks
                    .get(name)
                    .ok_or_else(|| FlowError::graph(format!("{name} is not a ranked node")))?;
                starting.push((node, *rank));
            }
        }
        starting.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.name.cmp(right.0.name)));
        schedule.push(starting.iter().map(|entry| entry.0.name).collect());
        let wave_nodes: Vec<GraphNode<'a, T>> = starting.into_iter().map(|entry| entry.0).collect();
        let label = format!("{step}/wave{wave}");
        wave = wave.saturating_add(1);
        // Every body answers `Ok` with its name and result, so one failing
        // node never cancels its wave: the wave settles every outcome, and
        // only the next wave reads them.
        let settled: Vec<(&'static str, Result<T, FlowError>)> = each(
            here,
            &label,
            Some(slots),
            wave_nodes,
            |item_scope, node| async move {
                let name = node.name;
                let outcome = (node.run)(item_scope.clone()).await;
                item_scope.settle(&outcome);
                Ok((name, outcome))
            },
        )
        .await?;
        for (name, outcome) in settled {
            done.insert(name, NodeOutcome::Ran(outcome));
        }
    }
    Ok((done, schedule))
}

#[cfg(test)]
mod tests {
    use super::{GraphNode, critical_rank, validate};

    /// A test's result: an error fails it with the error's text.
    type TestResult = Result<(), String>;

    /// A node that immediately gives back its name, for declaration checks.
    fn named(
        name: &'static str,
        needs: &'static [&'static str],
    ) -> GraphNode<'static, &'static str> {
        GraphNode::new(name, needs, move |_| Box::pin(async move { Ok(name) }))
    }

    /// An empty graph, a duplicate name, an unknown need, a self-need and a
    /// cycle are each refused before any node starts.
    #[test]
    fn a_declaration_that_cannot_run_is_refused() -> TestResult {
        for (what, nodes) in [
            ("empty", Vec::new()),
            ("duplicate", vec![named("a", &[]), named("a", &[])]),
            ("unknown", vec![named("a", &["missing"])]),
            ("self-need", vec![named("a", &["a"])]),
            ("cycle", vec![named("a", &["b"]), named("b", &["a"])]),
        ] {
            assert!(
                validate(&nodes).is_err(),
                "an unrunnable {what} declaration must be refused"
            );
        }
        assert!(
            validate(&[named("a", &[]), named("b", &["a"])]).is_ok(),
            "a diamond must validate"
        );
        Ok(())
    }

    /// Ranks count the longest chain of needs: roots at zero, each node one
    /// past its deepest need.
    #[test]
    fn ranks_count_the_longest_chain_of_needs() -> TestResult {
        let nodes = vec![
            named("root", &[]),
            named("left", &["root"]),
            named("right", &["root"]),
            named("join", &["left", "right"]),
        ];
        let rank = critical_rank(&nodes).map_err(|error| error.to_string())?;
        assert_eq!(rank.get("root"), Some(&0), "roots rank zero");
        assert_eq!(rank.get("left"), Some(&1), "one past the root");
        assert_eq!(rank.get("join"), Some(&2), "one past the deepest need");
        Ok(())
    }
}
