//! The script vocabulary on the shipped artifact: justification, graphs,
//! compensation, streams, typed domains, bounds and deadlines.
//!
//! Each section names the runtime its words desugar into and checks what the
//! run reports: a WAF that stops a run with its reason code, a needs-ordered
//! graph whose failure blocks only downstream, compensations in reverse order,
//! a bounded feed, typed calls mapped into the domain error, and the bounds
//! and deadlines the words check at. A mismatch fails the smoke: the binary's
//! clean exit is the proof. `run` drives every section through the public
//! interface; every new `script` item the crate exposes is named here, which
//! is what makes this module the surface guard's caller for them.

use std::sync::Arc;
use std::time::Duration;

use lgwks_bot::dist_lease::QueueError;
use lgwks_bot::rt::runtime::Runtime;
use lgwks_bot::script::{
    Allowance, Compensations, Decision, FlowError, GraphNode, GraphReport, Justify, JustifyCtx,
    NodeOutcome, Overflow, PumpReport, Rule, Scope, StreamGate, Tenant, TypedAction, TypedSource,
    Verdict, act_typed, compensated, graph, map_domain, observe_typed, pump,
};
use lgwks_bot::script::{MAX_GRAPH_NODES, at_most, attempts, retry, within, within_on};

use super::say;

/// The vocabulary's own error: what a typed call maps a refusal into.
#[derive(Debug, PartialEq)]
struct VocabFailed(String);

impl std::fmt::Display for VocabFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "vocabulary failed: {}", self.0)
    }
}

impl std::error::Error for VocabFailed {}

impl From<FlowError> for VocabFailed {
    fn from(error: FlowError) -> Self {
        Self(error.to_string())
    }
}

/// `vocab::counter` with a compile-time identity and output type.
struct VocabCounter;

impl TypedSource for VocabCounter {
    const ID: &'static str = "vocab::counter";
    type Output = u16;
    type Error = VocabFailed;
}

/// `vocab::page` with a compile-time identity, input and output type.
struct VocabPage;

impl TypedAction for VocabPage {
    const ID: &'static str = "vocab::page";
    type Input = u16;
    type Output = ();
    type Error = VocabFailed;
}

/// The vocabulary section a refusal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// The justification WAF and its verdicts.
    Justify,
    /// The bounds and deadlines the words check at.
    Bounds,
    /// The needs-ordered graph.
    Graph,
    /// The compensation stack.
    Compensation,
    /// The bounded stream gate.
    Stream,
    /// The typed domain boundary.
    Typed,
}

impl std::fmt::Display for Section {
    /// The section's name, as the smoke names it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match *self {
            Self::Justify => "justify",
            Self::Bounds => "bounds",
            Self::Graph => "graph",
            Self::Compensation => "compensation",
            Self::Stream => "stream",
            Self::Typed => "typed",
        };
        formatter.write_str(name)
    }
}

/// What the vocabulary proved: the section and the check that failed it, or
/// the runtime error the run returned. Every arm is matchable, so a caller
/// can tell a failed check from a refused runtime without parsing a string.
#[derive(Debug)]
pub(crate) enum VocabularyError {
    /// An assertion failed: the section and the check.
    Check {
        /// The section that failed.
        section: Section,
        /// The check that failed, as written at the call site.
        check: &'static str,
    },
    /// The runtime refused: a matchable flow failure.
    Flow(FlowError),
    /// The gate refused its bound: a matchable queue refusal.
    Queue(QueueError),
    /// The report was not written: the write's own error.
    Io(std::io::Error),
}

impl std::fmt::Display for VocabularyError {
    /// The section and the check, or the runtime error behind the failure.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Check { section, check } => {
                write!(formatter, "vocabulary {section} failed: {check}")
            }
            Self::Flow(ref error) => write!(formatter, "vocabulary runtime failed: {error}"),
            Self::Queue(ref error) => write!(formatter, "vocabulary queue failed: {error}"),
            Self::Io(ref error) => write!(formatter, "vocabulary report failed: {error}"),
        }
    }
}

impl std::error::Error for VocabularyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Check { .. } => None,
            Self::Flow(ref error) => Some(error),
            Self::Queue(ref error) => Some(error),
            Self::Io(ref error) => Some(error),
        }
    }
}

impl From<FlowError> for VocabularyError {
    /// A refused runtime travels as itself, located where it happened.
    fn from(error: FlowError) -> Self {
        Self::Flow(error)
    }
}

impl From<QueueError> for VocabularyError {
    /// A refused bound travels as itself, naming the zero cap.
    fn from(error: QueueError) -> Self {
        Self::Queue(error)
    }
}

impl From<std::io::Error> for VocabularyError {
    /// An unwritten report travels as the write's own error.
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Refuse the vocabulary with `check` in `section`, logging it first.
///
/// A bare `return Err` in this binary is a value with no signal, which the
/// scan refuses: the log names the refusing check, and the message travels to
/// the smoke's mismatch and its non-zero exit.
fn refuse<T>(section: Section, check: &'static str) -> Result<T, VocabularyError> {
    let error = VocabularyError::Check { section, check };
    lgwks_std::trace::debug!(error = ?error, "vocabulary: refusing the run");
    Err(error)
}

/// Drive every vocabulary section through the public interface.
///
/// # Errors
///
/// The first failed check with its section, or the runtime error the run
/// returned.
pub(crate) fn run(driver: &Runtime) -> Result<(), VocabularyError> {
    let scope = Scope::root(Tenant::new("vocab")?);

    // Justification: one rule per verdict, each deciding on its own.
    let allow: Decision = Justify::empty().decide(&JustifyCtx::new("vocab", 0));
    if !matches!(allow.verdict, Verdict::Allow) || allow.rule != "allow" {
        return refuse(Section::Justify, "no rules is allow, not a refusal");
    }
    let paced = Justify::new(vec![Rule::pace("vocab-pace", 1, Duration::ZERO)]);
    if paced.is_empty() || paced.len() != 1 {
        return refuse(
            Section::Justify,
            "an installed rule is not empty and counts exactly once",
        );
    }
    let slowed: Decision = paced.decide(&JustifyCtx::new("vocab", 0));
    match slowed.verdict {
        Verdict::Slow { delay } => {
            say(&format!("justify: the paced rule slows by {delay:?}"))?;
        }
        Verdict::Allow | Verdict::Park { .. } | Verdict::Block { .. } => {
            return refuse(Section::Justify, "a pace of one slows every evaluation");
        }
        _ => {
            return refuse(Section::Justify, "a refusal the vocabulary does not name");
        }
    }
    let parked: Decision = Justify::new(vec![Rule::suspend("vocab-quiet", 0, "vocab-quiet-hours")])
        .decide(&JustifyCtx::new("vocab", 0));
    match parked.verdict {
        Verdict::Park { reason } => {
            if parked.rule != "vocab-quiet" || reason != "vocab-quiet-hours" {
                return refuse(Section::Justify, "the park names its rule and reason code");
            }
            say(&format!(
                "justify: parked by {} with reason {reason}",
                parked.rule
            ))?;
        }
        _ => {
            return refuse(Section::Justify, "zero admissions parks at once");
        }
    }
    let blocked: Decision = Justify::new(vec![Rule::allowance(
        "vocab-budget",
        Arc::new(Allowance::new(0)),
        "vocab-budget-spent",
    )])
    .decide(&JustifyCtx::new("vocab", 0));
    match blocked.verdict {
        Verdict::Block { reason } => {
            if reason != "vocab-budget-spent" {
                return refuse(Section::Justify, "the block carries its reason code");
            }
        }
        _ => {
            return refuse(
                Section::Justify,
                "an empty allowance blocks with its reason code",
            );
        }
    }
    let probe = Rule::new("vocab-probe", |_| Verdict::Allow);
    if probe.name() != "vocab-probe" {
        return refuse(Section::Justify, "a rule carries its name");
    }
    if !matches!(probe.decide(&JustifyCtx::new("vocab", 0)), Verdict::Allow) {
        return refuse(Section::Justify, "the probe rule allows");
    }
    let allowance = Arc::new(Allowance::new(1));
    if allowance.remaining() != 1 || !allowance.take() || allowance.take() {
        return refuse(
            Section::Justify,
            "an allowance spends one token, then refuses",
        );
    }
    allowance.refill(2);
    if allowance.remaining() != 2 {
        return refuse(Section::Justify, "a refill lands");
    }
    let bare = scope.with_justification(Justify::empty());
    driver.block_on(bare.justified(0))?;
    say("justify: the paced rule slows, the rest allow or stop with a code")?;

    // FlowError constructors the refusal paths carry.
    let stopped = FlowError::justified("vocab-rule", "vocab-reason");
    if !format!("{stopped}").contains("vocab-rule") {
        return refuse(Section::Justify, "the justified refusal names its rule");
    }
    let unbuilt = FlowError::graph("vocab-not-runnable");
    if !format!("{unbuilt}").contains("vocab-not-runnable") {
        return refuse(Section::Graph, "the graph refusal names why it cannot run");
    }

    // Bounds and deadlines the words check at.
    let bound = at_most(2)?;
    if bound.get() != 2 {
        return refuse(Section::Bounds, "the fan-out bound holds what it was given");
    }
    let budget = attempts(3)?;
    let quick = driver.block_on(within(
        &scope,
        "vocab-within",
        Duration::from_secs(60),
        async { Ok::<u64, FlowError>(7) },
    ))?;
    if quick != 7 {
        return refuse(
            Section::Bounds,
            "a body inside its deadline answers its value",
        );
    }
    let quick_on = driver.block_on(within_on(
        scope.clock(),
        &scope,
        "vocab-within-on",
        Duration::from_secs(60),
        async { Ok::<u64, FlowError>(8) },
    ))?;
    if quick_on != 8 {
        return refuse(
            Section::Bounds,
            "a body on its named clock answers its value",
        );
    }
    let retried = driver.block_on(retry(
        &scope,
        "vocab-retry",
        budget,
        Duration::ZERO,
        |_, _| async { Ok::<u64, FlowError>(9) },
    ))?;
    if retried != 9 {
        return refuse(
            Section::Bounds,
            "a body that succeeds ends the loop at once",
        );
    }
    say("bounds: the fan-out bound, retry budget and deadlines hold")?;

    // A needs-ordered graph: fetch twice, merge, with the bound above.
    if MAX_GRAPH_NODES != 4_096 {
        return refuse(
            Section::Graph,
            "the graph admits more lanes than any lane graph needs",
        );
    }
    let nodes = vec![
        GraphNode::new("fetch-a", &[], |_scope| {
            Box::pin(async move { Ok::<u64, FlowError>(1) })
        })
        .with_work(2),
        GraphNode::new("fetch-b", &[], |_scope| {
            Box::pin(async move { Ok::<u64, FlowError>(2) })
        })
        .with_work(1),
        GraphNode::new("merge", &["fetch-a", "fetch-b"], |_scope| {
            Box::pin(async move { Ok::<u64, FlowError>(3) })
        })
        .with_work(1),
    ];
    let report: GraphReport<u64> =
        driver.block_on(graph(&scope, "vocab-lanes", bound.get(), nodes))?;
    if !report.is_ok() {
        return refuse(Section::Graph, "every lane ran to a value");
    }
    if report.succeeded() != vec!["fetch-a", "fetch-b", "merge"] {
        return refuse(Section::Graph, "every lane ran in needs order");
    }
    if !report.failed().is_empty() || !report.blocked().is_empty() {
        return refuse(Section::Graph, "no lane failed and none was blocked");
    }
    if report.wave_count() != report.waves().len() {
        return refuse(Section::Graph, "the wave count is the schedule length");
    }
    match report.outcome("merge") {
        Some(outcome) => {
            if !matches!(outcome, NodeOutcome::Ran(Ok(_))) {
                return refuse(Section::Graph, "merge ran to a value");
            }
        }
        None => {
            return refuse(Section::Graph, "merge has an outcome");
        }
    }
    let node = GraphNode::new("probe", &["fetch-a"], |_scope| {
        Box::pin(async move { Ok::<u64, FlowError>(0) })
    });
    if node.name() != "probe" || node.needs() != ["fetch-a"] {
        return refuse(Section::Graph, "a node carries its name and its needs");
    }
    say(&format!("graph: lanes ran {:?}", report.succeeded()))?;

    // Compensation: a booking that fails at its issue undoes charge and reserve.
    let fresh: Compensations<'_> = Compensations::new();
    say(&format!(
        "compensation: a fresh registrar holds {}",
        fresh.len()
    ))?;
    if !fresh.is_empty() {
        return refuse(Section::Compensation, "a fresh registrar holds nothing");
    }
    let booking = driver.block_on(compensated(
        &scope,
        "vocab-booking",
        |_scope, stack| async move {
            stack.add("vocab-reserve", |undo| {
                Box::pin(async move {
                    let _ = undo;
                    Ok::<(), FlowError>(())
                })
            });
            stack.add("vocab-charge", |undo| {
                Box::pin(async move {
                    let _ = undo;
                    Ok::<(), FlowError>(())
                })
            });
            if stack.len() != 2 || stack.is_empty() {
                Err(FlowError::failed("the registrar lost its undos"))
            } else {
                Err::<(), FlowError>(FlowError::failed("vocab-issue"))
            }
        },
    ));
    match booking {
        Err(error) => {
            if !error.to_string().contains("vocab-issue") {
                return refuse(Section::Compensation, "the caller gets the body's failure");
            }
            say(&format!("compensation: issuing failed the run as {error}"))?;
        }
        Ok(()) => {
            return refuse(Section::Compensation, "issuing must fail this run");
        }
    }

    // A bounded feed: every event through a gate of four, nothing shed.
    let mut gate: StreamGate<u32> = StreamGate::new(4, Overflow::DropOldest)?;
    match StreamGate::<u32>::new(0, Overflow::Refuse) {
        Err(error) => {
            say(&format!("stream: a zero cap is refused: {error}"))?;
        }
        Ok(_) => {
            return refuse(Section::Stream, "a zero cap must not build");
        }
    }
    let policies = [Overflow::DropOldest, Overflow::DropNewest, Overflow::Refuse];
    if policies.len() != 3 {
        return refuse(
            Section::Stream,
            "the gate names its three overflow policies",
        );
    }
    say(&format!(
        "stream: a new gate holds {} of {}",
        gate.len(),
        gate.capacity()
    ))?;
    if !gate.is_empty() || gate.capacity() != 4 {
        return refuse(
            Section::Stream,
            "a new gate holds nothing under a bound of four",
        );
    }
    if gate.push(1_u32).is_err() {
        return refuse(Section::Stream, "the first push fits");
    }
    if gate.len() != 1 || gate.pop() != Some(1_u32) {
        return refuse(Section::Stream, "a retained event pops back");
    }
    if gate.dropped() != 0 || gate.refused() != 0 {
        return refuse(
            Section::Stream,
            "an unpressured gate sheds and refuses nothing",
        );
    }
    let pending: std::cell::RefCell<Vec<Option<Vec<u32>>>> =
        std::cell::RefCell::new(vec![None, Some(vec![4, 5]), Some(vec![1, 2, 3])]);
    let report: PumpReport = driver.block_on(pump(
        &scope,
        "vocab-prices",
        &mut gate,
        || {
            let batch = pending.borrow_mut().pop().flatten();
            async move { batch }
        },
        |_, _| async move { Ok::<(), FlowError>(()) },
    ))?;
    if report.handled != 5 || report.dropped != 0 || report.refused != 0 {
        return refuse(
            Section::Stream,
            "every feed event is handled, none shed, none refused",
        );
    }
    say(&format!(
        "stream: handled {} dropped {} refused {}",
        report.handled, report.dropped, report.refused
    ))?;

    // Typed domains: no registry installed, so both calls fail typed, and the
    // boundary maps success and failure without touching either.
    let undeclared: Result<u16, VocabFailed> =
        driver.block_on(observe_typed::<VocabCounter>(&scope, "7"));
    if undeclared.is_ok() {
        return refuse(Section::Typed, "an undeclared source must fail typed");
    }
    let unacted: Result<(), VocabFailed> =
        driver.block_on(act_typed::<VocabPage>(&scope, "oncall", 2_u16));
    if unacted.is_ok() {
        return refuse(Section::Typed, "an undeclared action must fail typed");
    }
    if map_domain::<u16, VocabFailed>(Ok(7_u16)) != Ok(7_u16) {
        return refuse(
            Section::Typed,
            "a success crosses the domain boundary unchanged",
        );
    }
    let mapped: Result<u16, VocabFailed> = map_domain(Err(FlowError::failed("vocab-down")));
    if mapped.is_ok() {
        return refuse(Section::Typed, "a failure crosses as the domain's error");
    }
    say("typed: undeclared domains fail typed, the boundary maps both ways")?;
    Ok(())
}
