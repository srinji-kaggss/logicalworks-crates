//! A dependency graph: the estate's most common orchestration shape (#694).
//!
//! CI lanes, build systems and data pipelines are all one shape: each node
//! names the nodes it waits for (`needs`), a node starts as soon as those have
//! succeeded, a failed node blocks everything downstream while the rest keeps
//! running, and one bound caps how many run at once. `each` is a flat bounded
//! fan-out and `together` a fixed join; neither says "B starts when A and C
//! have passed". In `script!` this is `graph:` with `node name needs a, b:`
//! lines under it, and it carries the guarantees the other words carry: needs
//! respected, each node once, bounded concurrency, the same declaration
//! reaching the same schedule.
//!
//! The schedule is greedy list scheduling by critical path. Whenever a slot is
//! free and a node is ready, the ready node with the longest chain of work
//! still below it starts, declaration order breaking ties, so the chain that
//! decides the run's length is never left waiting behind short work. Nodes are
//! driven on the awaiting task, as `each` drives its bodies, and a completion
//! re-polls only the node that woke. A greedy schedule on `m` slots finishes
//! within `W / m + C`, where `W` is the total work and `C` the longest chain
//! (Graham, "Bounds on multiprocessing timing anomalies", 1969): no wave
//! barrier holds a ready node back behind a slow sibling.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use super::each::{Wakes, slot_wakers};
use super::trail::Site;
use super::{FlowError, MAX_IN_FLIGHT, POLL_BUDGET, Scope, at_most};

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
    /// A unit cost for the critical-path order.
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
    /// The nodes started together at each scheduling point, in the order the
    /// points came, each in start order.
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

    /// The schedule the run executed: the nodes started together at each
    /// scheduling point, in the order the points came, each in start order.
    /// A point is the start of the run or a node's completion.
    #[must_use]
    pub fn waves(&self) -> &[Vec<&'static str>] {
        &self.schedule
    }

    /// How many scheduling points started a node.
    #[must_use]
    pub fn wave_count(&self) -> usize {
        self.schedule.len()
    }
}

/// Run `nodes` as a dependency graph under `step`, at most `slots` at once.
///
/// A node starts as soon as every node it needs has succeeded and a slot is
/// free, the longest remaining chain first; a failed node blocks everything
/// downstream while independent nodes keep running; each node runs at most
/// once, in a scope of its own (`<step>/<name>`).
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
    run_graph(scope, step, None, Some(slots), nodes).await
}

#[doc(hidden)]
/// [`graph`] for a `graph:` block `script!` wrote at `site`, under `limit`, or
/// under the scope's own fan-out policy when the block wrote none. Called by
/// `script!`, whose node bodies settle themselves.
///
/// # Errors
///
/// As [`graph`].
pub async fn graph_at<'a, T>(
    scope: &Scope,
    step: &str,
    site: &'static Site,
    limit: Option<NonZeroUsize>,
    nodes: Vec<GraphNode<'a, T>>,
) -> Result<GraphReport<T>, FlowError> {
    run_graph(scope, step, Some(site), limit, nodes).await
}

/// The body of [`graph`] and [`graph_at`].
async fn run_graph<'a, T>(
    scope: &Scope,
    step: &str,
    site: Option<&'static Site>,
    limit: Option<NonZeroUsize>,
    nodes: Vec<GraphNode<'a, T>>,
) -> Result<GraphReport<T>, FlowError> {
    validate(&nodes)?;
    let plan = Plan::of(&nodes)?;
    let here = scope.enter_from(step, site)?;
    // An absent limit is the scope's own policy, as for `each`; either way no
    // more slots than nodes, and never past the crate's ceiling.
    let declared = match limit {
        Some(written) => written,
        None => here.policy().fan_out(),
    };
    let width = declared.get().min(MAX_IN_FLIGHT).min(nodes.len()).max(1);
    let (wakes, wakers) = slot_wakers(width);
    let mut run = Run::new(plan, nodes, width);
    let mut stopped = Box::pin(here.token().cancelled_owned());
    let at = &here;
    let finished = std::future::poll_fn(move |context: &mut Context<'_>| {
        wakes.set_parent(context.waker());
        let mut polled: usize = 0;
        loop {
            if stopped.as_mut().poll(context).is_ready() {
                run.clear();
                return Poll::Ready(Err(FlowError::Cancelled {
                    at: Arc::clone(at.shared_path()),
                }));
            }
            if let Err(error) = run.start(at, &wakes) {
                run.clear();
                return Poll::Ready(Err(error));
            }
            let ready = wakes.take_ready();
            if ready.is_empty() {
                break;
            }
            for position in ready {
                polled = polled.saturating_add(1);
                run.poll_slot(position, &wakers);
            }
            if polled >= POLL_BUDGET {
                context.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
        // Nothing running after a start round means nothing is ready either:
        // every node has run or is blocked, since cycles were refused.
        if run.running() == 0 {
            Poll::Ready(run.report())
        } else {
            Poll::Pending
        }
    })
    .await;
    here.settle(&finished);
    finished
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
    // Kahn's pass over names: what never reaches zero waits on a cycle.
    let mut outstanding: BTreeMap<&'static str, usize> = nodes
        .iter()
        .map(|node| (node.name, node.needs.len()))
        .collect();
    let mut ready: Vec<&'static str> = outstanding
        .iter()
        .filter(|item| *item.1 == 0)
        .map(|item| *item.0)
        .collect();
    while let Some(name) = ready.pop() {
        outstanding.remove(name);
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
    outstanding
        .keys()
        .next()
        .map(|name| format!("the needs cycle through {name}: nothing can start it"))
}

/// A validated declaration as indices: each node's needs, its dependants, and
/// its height, the longest chain of work from it to the end of the graph.
struct Plan {
    /// The names, in declaration order.
    names: Vec<&'static str>,
    /// Each node's needs, as declaration indices.
    needs: Vec<Vec<usize>>,
    /// The nodes that need each node, as declaration indices.
    dependants: Vec<Vec<usize>>,
    /// Each node's own work plus the heaviest chain of dependants below it.
    height: Vec<u64>,
}

impl Plan {
    /// The plan of a declaration [`validate`] accepted.
    fn of<T>(nodes: &[GraphNode<'_, T>]) -> Result<Self, FlowError> {
        let index: BTreeMap<&'static str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(at, node)| (node.name, at))
            .collect();
        let mut needs = Vec::with_capacity(nodes.len());
        let mut dependants: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        for (at, node) in nodes.iter().enumerate() {
            let mut resolved = Vec::with_capacity(node.needs.len());
            for need in node.needs {
                let of = *index
                    .get(need)
                    .ok_or_else(|| FlowError::graph(format!("{need} is not a declared node")))?;
                resolved.push(of);
                if let Some(below) = dependants.get_mut(of) {
                    below.push(at);
                }
            }
            needs.push(resolved);
        }
        // A topological order, then heights from the leaves up.
        let mut unmet: Vec<usize> = needs.iter().map(Vec::len).collect();
        let mut queue: Vec<usize> = unmet
            .iter()
            .enumerate()
            .filter(|entry| *entry.1 == 0)
            .map(|entry| entry.0)
            .collect();
        let mut order = Vec::with_capacity(nodes.len());
        while let Some(at) = queue.pop() {
            order.push(at);
            if let Some(below) = dependants.get(at) {
                for &next in below {
                    if let Some(count) = unmet.get_mut(next) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            queue.push(next);
                        }
                    }
                }
            }
        }
        let mut height: Vec<u64> = nodes.iter().map(|node| node.work).collect();
        for &at in order.iter().rev() {
            let below = match dependants.get(at) {
                Some(list) => list
                    .iter()
                    .filter_map(|next| height.get(*next))
                    .fold(0_u64, |heaviest, chain| heaviest.max(*chain)),
                None => 0,
            };
            if let Some(own) = height.get_mut(at) {
                *own = own.saturating_add(below);
            }
        }
        Ok(Self {
            names: nodes.iter().map(|node| node.name).collect(),
            needs,
            dependants,
            height,
        })
    }
}

/// A node running in a slot.
struct Running<'a, T> {
    /// The node's declaration index.
    index: usize,
    /// The node's own scope, `<graph>/<name>`.
    scope: Scope,
    /// The node's body.
    future: BodyFut<'a, T>,
}

/// The scheduler's state: what waits, what is ready, what runs, what came of
/// each node.
struct Run<'a, T> {
    /// The validated declaration.
    plan: Plan,
    /// Each node until it starts or is blocked.
    nodes: Vec<Option<GraphNode<'a, T>>>,
    /// How many of each node's needs have not yet succeeded.
    unmet: Vec<usize>,
    /// The ready nodes, tallest chain first, declaration order breaking ties.
    ready: BTreeSet<(Reverse<u64>, usize)>,
    /// What each node came to, once it has.
    outcomes: Vec<Option<NodeOutcome<T>>>,
    /// The slots; `None` is free.
    slots: Vec<Option<Running<'a, T>>>,
    /// Free slot positions.
    free: Vec<usize>,
    /// The nodes started at each scheduling point.
    schedule: Vec<Vec<&'static str>>,
}

impl<'a, T> Run<'a, T> {
    /// A run of `nodes` on `width` slots, with every root ready.
    fn new(plan: Plan, nodes: Vec<GraphNode<'a, T>>, width: usize) -> Self {
        let unmet: Vec<usize> = plan.needs.iter().map(Vec::len).collect();
        let ready = unmet
            .iter()
            .enumerate()
            .filter(|entry| *entry.1 == 0)
            .filter_map(|entry| {
                plan.height
                    .get(entry.0)
                    .map(|height| (Reverse(*height), entry.0))
            })
            .collect();
        let count = nodes.len();
        Self {
            plan,
            nodes: nodes.into_iter().map(Some).collect(),
            unmet,
            ready,
            outcomes: (0..count).map(|_| None).collect(),
            slots: (0..width).map(|_| None).collect(),
            free: (0..width).rev().collect(),
            schedule: Vec::new(),
        }
    }

    /// Start the tallest ready nodes while a slot is free.
    fn start(&mut self, here: &Scope, wakes: &Wakes) -> Result<(), FlowError> {
        let mut started = Vec::new();
        while !self.ready.is_empty() {
            let Some(position) = self.free.pop() else {
                break;
            };
            let Some((_, index)) = self.ready.pop_first() else {
                self.free.push(position);
                break;
            };
            let node = self
                .nodes
                .get_mut(index)
                .and_then(Option::take)
                .ok_or_else(|| FlowError::graph("a ready node had already started"))?;
            let scope = here.enter(node.name)?;
            let future = (node.run)(scope.clone());
            started.push(node.name);
            if let Some(slot) = self.slots.get_mut(position) {
                *slot = Some(Running {
                    index,
                    scope,
                    future,
                });
            }
            wakes.mark(position);
        }
        if !started.is_empty() {
            self.schedule.push(started);
        }
        Ok(())
    }

    /// Poll the node in `position` with its own waker; on its answer, settle
    /// its scope, free the slot, and release or block what needs it.
    fn poll_slot(&mut self, position: usize, wakers: &[Waker]) {
        let (Some(entry), Some(waker)) = (self.slots.get_mut(position), wakers.get(position))
        else {
            return;
        };
        let Some(running) = entry.as_mut() else {
            return;
        };
        let mut context = Context::from_waker(waker);
        let Poll::Ready(outcome) = running.future.as_mut().poll(&mut context) else {
            return;
        };
        running.scope.settle(&outcome);
        let index = running.index;
        let outcome = outcome.map_err(|error| error.located(&running.scope));
        *entry = None;
        self.free.push(position);
        self.finish(index, outcome);
    }

    /// Record `index`'s answer, then ready each dependant whose needs have all
    /// succeeded, or block every dependant when it failed.
    fn finish(&mut self, index: usize, outcome: Result<T, FlowError>) {
        let succeeded = outcome.is_ok();
        self.record(index, NodeOutcome::Ran(outcome));
        let below = match self.plan.dependants.get(index) {
            Some(list) => list.clone(),
            None => Vec::new(),
        };
        for next in below {
            if !succeeded {
                self.block(next);
                continue;
            }
            let open = self.is_open(next);
            if let Some(count) = self.unmet.get_mut(next) {
                *count = count.saturating_sub(1);
                if *count == 0
                    && open
                    && let Some(height) = self.plan.height.get(next)
                {
                    self.ready.insert((Reverse(*height), next));
                }
            }
        }
    }

    /// Block `first` and everything below it that has not come to anything,
    /// each naming the needs that failed or were blocked.
    fn block(&mut self, first: usize) {
        let mut stack = vec![first];
        while let Some(at) = stack.pop() {
            if !self.is_open(at) {
                continue;
            }
            if let Some(node) = self.nodes.get_mut(at) {
                *node = None;
            }
            let mut by: Vec<String> = self
                .plan
                .needs
                .get(at)
                .into_iter()
                .flatten()
                .filter(|need| {
                    matches!(
                        self.outcomes.get(**need),
                        Some(Some(NodeOutcome::Ran(Err(_)) | NodeOutcome::Blocked { .. }))
                    )
                })
                .filter_map(|need| self.plan.names.get(*need))
                .map(|name| (*name).to_owned())
                .collect();
            by.sort_unstable();
            self.record(at, NodeOutcome::Blocked { by });
            if let Some(below) = self.plan.dependants.get(at) {
                stack.extend(below.iter().copied());
            }
        }
    }

    /// Whether `at` has neither started nor come to anything.
    fn is_open(&self, at: usize) -> bool {
        matches!(self.outcomes.get(at), Some(None)) && matches!(self.nodes.get(at), Some(Some(_)))
    }

    /// Keep what `at` came to.
    fn record(&mut self, at: usize, outcome: NodeOutcome<T>) {
        if let Some(entry) = self.outcomes.get_mut(at) {
            *entry = Some(outcome);
        }
    }

    /// Nodes in flight now: the slots not on the free list.
    fn running(&self) -> usize {
        self.slots.len().saturating_sub(self.free.len())
    }

    /// Drop every node still running and start nothing more.
    fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.ready.clear();
    }

    /// The report, in declaration order, once every node has come to
    /// something.
    fn report(&mut self) -> Result<GraphReport<T>, FlowError> {
        let mut outcomes = Vec::with_capacity(self.outcomes.len());
        for (name, outcome) in self.plan.names.iter().zip(self.outcomes.drain(..)) {
            let Some(outcome) = outcome else {
                let refusal = Err(FlowError::graph(format!("{name} has no outcome")));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "report: returning an error to the caller");
                return refusal;
            };
            outcomes.push((*name, outcome));
        }
        Ok(GraphReport {
            outcomes,
            schedule: std::mem::take(&mut self.schedule),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphNode, Plan, graph, validate};
    use crate::script::{FlowError, Scope, Tenant};

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
            "a chain must validate"
        );
        Ok(())
    }

    /// Heights count the heaviest chain from a node to the end of the graph.
    #[test]
    fn heights_count_the_longest_chain_below() -> TestResult {
        let nodes = vec![
            named("root", &[]),
            named("left", &["root"]),
            named("right", &["root"]),
            named("join", &["left", "right"]),
        ];
        let plan = Plan::of(&nodes).map_err(|error| error.to_string())?;
        assert_eq!(plan.height, vec![3, 2, 2, 1], "root, left, right, join");
        Ok(())
    }

    /// A failed node blocks only what needs it; the rest still run.
    #[test]
    fn a_failure_blocks_only_downstream() -> TestResult {
        let nodes: Vec<GraphNode<'static, ()>> = vec![
            GraphNode::new("broken", &[], |_| {
                Box::pin(async { Err(FlowError::failed("broken")) })
            }),
            GraphNode::new("below", &["broken"], |_| Box::pin(async { Ok(()) })),
            GraphNode::new("beside", &[], |_| Box::pin(async { Ok(()) })),
        ];
        let scope = Scope::root(Tenant::new("acme").map_err(|error| error.to_string())?);
        let report =
            crate::block_on(graph(&scope, "ship", 4, nodes)).map_err(|error| error.to_string())?;
        assert_eq!(
            report.failed(),
            vec!["broken"],
            "the failure is the node's own"
        );
        assert_eq!(
            report.blocked(),
            vec!["below"],
            "only what needs it is blocked"
        );
        assert_eq!(report.succeeded(), vec!["beside"], "the rest still ran");
        Ok(())
    }
}
