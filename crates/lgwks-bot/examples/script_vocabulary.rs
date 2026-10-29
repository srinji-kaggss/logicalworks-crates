//! script! vocabulary on the real run path: justification, graphs,
//! compensation, streams and typed domains, driven end to end.
//!
//! Each flow below says one of the structured words, `justify`, `saga`,
//! `graph`, `whenever`, `remember`, `admit` and `ready`, and runs it through
//! the public interface: a justification WAF that stops a run with its reason
//! code, a needs-ordered lane graph whose failure blocks only downstream,
//! compensations that undo in reverse order, a bounded live feed, a remembered
//! step, untrusted bytes admitted as a plan, a wait on a service's readiness,
//! and typed observe/act calls against a stub registry. `main` runs every
//! flow, checks what each one reports, and prints the trail.

use std::error::Error;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lgwks_bot::cap::Cap;
use lgwks_bot::proposal::{Decoder, LedgerLimits, PlanBudget, PlanLimits, Surface};
use lgwks_bot::script::{
    FlowError, Gate, Generation, GraphNode, GraphReport, Justify, NodeOutcome, Overflow,
    PumpReport, Readiness, Scope, StreamGate, Tenant, TypedAction, TypedSource, act_typed,
    map_domain, observe_typed,
};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{Action, Auth, BotError, Execute, GrantSet, Observe, Source, domains};

/// A check that fails the run with a readable reason, never a bare panic.
fn check(what: bool, reason: &str) -> Result<(), String> {
    if what { Ok(()) } else { Err(reason.to_owned()) }
}

/// The domain's own error type: what a typed call maps a refusal into.
#[derive(Debug, PartialEq)]
struct DemoFailed(String);

impl std::fmt::Display for DemoFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "demo failed: {}", self.0)
    }
}

impl Error for DemoFailed {}

impl From<FlowError> for DemoFailed {
    fn from(error: FlowError) -> Self {
        Self(error.to_string())
    }
}

/// A stub source: the count its target names.
struct DemoCounter(u16);

impl DemoCounter {
    /// Build from a target that names a count.
    fn from_target(target: &str) -> Result<Source, BotError> {
        match target.parse::<u16>() {
            Ok(count) => Ok(Source::new(Self(count))),
            Err(_) => Err(BotError::IncompleteSpec {
                field: "target",
                cause: String::from("a demo count is a number"),
            }),
        }
    }
}

impl Observe for DemoCounter {
    type Output = u16;

    fn required_caps(&self) -> &[lgwks_bot::Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&[])?;
        Ok(self.0)
    }

    fn domain_id(&self) -> &str {
        "demo::counter"
    }
}

/// A stub action: records nothing, answers `()`.
struct DemoPage;

impl DemoPage {
    /// Build from any target.
    fn from_target(_target: &str) -> Result<Action, BotError> {
        Ok(Action::new(Self))
    }
}

impl Execute for DemoPage {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[lgwks_bot::Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "demo::page"
    }
}

domains! {
    /// What the typed flows name.
    pub(crate) DEMO {
        observe {
            "demo::counter" => DemoCounter::from_target,
        }
        execute {
            "demo::page" => DemoPage::from_target,
        }
    }
}

/// `demo::counter` with a compile-time identity and output type.
struct TypedDemoCounter;

impl TypedSource for TypedDemoCounter {
    const ID: &'static str = "demo::counter";
    type Output = u16;
    type Error = DemoFailed;
}

/// `demo::page` with a compile-time identity, input and output type.
struct TypedDemoPage;

impl TypedAction for TypedDemoPage {
    const ID: &'static str = "demo::page";
    type Input = u16;
    type Output = ();
    type Error = DemoFailed;
}

/// The issue step of a booking: fails when the caller seeds it, logged first.
fn issue(fail: bool) -> Result<&'static str, FlowError> {
    if fail {
        let refusal = Err(FlowError::failed("issue"));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "booking: failing the seeded step");
        return refusal;
    }
    Ok("ticket")
}

/// The feed's next batch; `None` once it is drained.
fn next_batch(feed: &Mutex<Vec<Vec<u32>>>) -> Option<Vec<u32>> {
    feed.lock().ok()?.pop()
}

lgwks_bot::script! {
    /// Poll under justification: an allowance of two evaluations, so the
    /// block's entry and the first attempt pass and the second attempt stops
    /// the run with its reason code.
    flow capped_polls(rules: &Justify) -> u16:
        let seen = justify with (rules):
            retry up to 6 times, waiting (Duration::ZERO):
                fail transiently with "the upstream is down"
        give back seen

    /// Book a seat, charging the card: the finished steps are undone, newest
    /// first, when issuing fails.
    flow booking(fail: bool) -> &'static str:
        let ticket = saga booking:
            step reserve:
                1_u8
            compensate reserve:
                lgwks_std::trace::info!("booking: releasing the seat")
            step charge:
                2_u8
            compensate charge:
                lgwks_std::trace::info!("booking: refunding the card")
            step issue:
                issue(fail)?
        give back ticket

    /// Run the lanes as a dependency graph: fetch twice, merge, publish.
    flow lanes() -> GraphReport<u64>:
        let report = graph, at most (2) at once:
            node fetch_a:
                1_u64
            node fetch_b:
                2_u64
            node merge needs fetch_a, fetch_b:
                3_u64
            node publish needs merge:
                4_u64
        give back report

    /// React to a seeded feed through a bounded buffer.
    flow prices(feed: &Mutex<Vec<Vec<u32>>>) -> PumpReport:
        let report = whenever price in next_batch(feed), buffering 4, drop oldest:
            price
        give back report

    /// Quote once per run: a resumed run reads the quote back.
    flow quote(asked: &AtomicU32) -> u32:
        let price = remember quote:
            asked.fetch_add(1, Ordering::SeqCst).saturating_add(40)
        give back price

    /// Admit a model's reply as a plan before anything acts on it.
    flow proposal(gate: &Gate, reply: &[u8]) -> usize:
        let plan = admit from model through (gate):
            reply
        give back plan.wanted().len()

    /// Wait for the pool, under a deadline, then size it.
    flow pooled(db: &Readiness<u32>) -> u32:
        let size = ready pool from (db) within 30s:
            pool.awaited().saturating_add(0)
        give back size
}

/// A gate over a surface with one read operation, for the `admit` flow.
fn proposal_gate() -> Result<Gate, Box<dyn Error>> {
    let surface = Surface::builder("demo")?
        .operation("read-report", &[Cap::fs()])?
        .holding(&[Cap::fs()])
        .build();
    Ok(Gate::new(
        "demo",
        surface,
        Decoder::new(PlanLimits::default()),
        PlanBudget::new(4),
        LedgerLimits::new(3, 8),
    ))
}

/// Run every vocabulary flow, check what each one reports, and print the trail.
fn main() -> Result<(), Box<dyn Error>> {
    use std::io::Write as _;

    let mut out = std::io::stdout().lock();
    let scope = Scope::root(Tenant::new("demo").map_err(|error| error.to_string())?);
    let budget = Justify::new(vec![lgwks_bot::script::Rule::allowance(
        "polls",
        Arc::new(lgwks_bot::script::Allowance::new(2)),
        "poll-budget-spent",
    )]);
    let blocked = lgwks_bot::block_on(capped_polls(&scope, &budget));
    check(
        matches!(blocked, Err(FlowError::Justified { .. })),
        "a stop must be justified",
    )?;
    if let Err(FlowError::Justified { rule, reason, .. }) = blocked {
        check(rule == "polls", "the refusal names the deciding rule")?;
        check(
            reason == "poll-budget-spent",
            "the refusal carries its reason code",
        )?;
        writeln!(out, "justification: blocked by {rule} with reason {reason}")
            .map_err(|error| format!("the report was not written: {error}"))?;
    }

    let ticket = lgwks_bot::block_on(booking(&scope, false)).map_err(|error| format!("{error}"))?;
    check(
        ticket == "ticket",
        "a booking that succeeds issues its ticket",
    )?;
    writeln!(out, "compensation: success runs no undo, ticket issued")
        .map_err(|error| format!("the report was not written: {error}"))?;
    let refused = lgwks_bot::block_on(booking(&scope, true));
    check(refused.is_err(), "issuing must fail this run")?;
    if let Err(error) = refused {
        writeln!(out, "compensation: issuing failed the run as {error}")
            .map_err(|error| format!("the report was not written: {error}"))?;
    }

    let report = lgwks_bot::block_on(lanes(&scope)).map_err(|error| format!("{error}"))?;
    check(report.is_ok(), "every lane ran to a value")?;
    check(
        report.succeeded() == vec!["fetch_a", "fetch_b", "merge", "publish"],
        "every lane ran in needs order",
    )?;
    check(report.failed().is_empty(), "no lane failed")?;
    check(report.blocked().is_empty(), "no lane was blocked")?;
    check(
        report.wave_count() == report.waves().len(),
        "the wave count is the schedule length",
    )?;
    match report.outcome("merge") {
        Some(outcome) => check(
            matches!(*outcome, NodeOutcome::Ran(_)),
            "merge ran, so its outcome is a run",
        )?,
        None => check(false, "merge has no outcome")?,
    }
    writeln!(out, "graph: lanes ran {:?}", report.succeeded())
        .map_err(|error| format!("the report was not written: {error}"))?;

    let pending = Mutex::new(vec![vec![4, 5], vec![1, 2, 3]]);
    let feed = lgwks_bot::block_on(prices(&scope, &pending)).map_err(|error| format!("{error}"))?;
    check(feed.handled == 5, "every feed event was handled")?;
    check(feed.dropped == 0, "a feed inside the bound sheds nothing")?;
    check(feed.refused == 0, "a feed inside the bound refuses nothing")?;
    writeln!(
        out,
        "stream: handled {} dropped {} refused {}",
        feed.handled, feed.dropped, feed.refused
    )
    .map_err(|error| format!("the report was not written: {error}"))?;

    let asked = AtomicU32::new(0);
    let price = lgwks_bot::block_on(quote(&scope, &asked)).map_err(|error| format!("{error}"))?;
    check(price == 40, "the remembered step gives its value")?;
    check(asked.load(Ordering::SeqCst) == 1, "the block ran once")?;
    let gate = proposal_gate()?;
    let wanted = lgwks_bot::block_on(proposal(&scope, &gate, b"op=read-report\n"))
        .map_err(|error| format!("{error}"))?;
    check(wanted == 1, "the admitted plan wants its one operation")?;
    let refused = lgwks_bot::block_on(proposal(&scope, &gate, b"op=install-dependency\n"));
    check(
        matches!(refused, Err(FlowError::Refused { .. })),
        "an unregistered operation is refused",
    )?;
    let db: Readiness<u32> = Readiness::new("pool")?;
    db.ready(Generation::FIRST, 8)?;
    let size = lgwks_bot::block_on(pooled(&scope, &db)).map_err(|error| format!("{error}"))?;
    check(size == 8, "the wait is released with the pool")?;
    writeln!(
        out,
        "durable: remembered {price}, admitted {wanted} operation, pool of {size}"
    )
    .map_err(|error| format!("the report was not written: {error}"))?;

    let host = Host::builder("demo")
        .map_err(|error| error.to_string())?
        .domains(&DEMO)
        .grants(GrantSet::empty())
        .build()
        .map_err(|error| format!("{error}"))?;
    let reading = task("reading", |scope: Scope, target: String| async move {
        observe_typed::<TypedDemoCounter>(&scope, &target)
            .await
            .map_err(FlowError::failed)
    })
    .map_err(|error| format!("{error}"))?;
    let paging = task(
        "paging",
        |scope: Scope, (label, value): (String, u16)| async move {
            act_typed::<TypedDemoPage>(&scope, &label, value)
                .await
                .map_err(FlowError::failed)
        },
    )
    .map_err(|error| format!("{error}"))?;
    let count = lgwks_bot::block_on(host.run(&reading, String::from("7")))
        .result()
        .ok()
        .copied();
    check(count == Some(7), "the typed source polls its count")?;
    let paged = lgwks_bot::block_on(host.run(&paging, (String::from("oncall"), 2)));
    check(paged.result().is_ok(), "the typed action runs")?;
    writeln!(out, "typed: counter polled 7, page acted once")
        .map_err(|error| format!("the report was not written: {error}"))?;

    let allowance = Arc::new(lgwks_bot::script::Allowance::new(1));
    check(allowance.remaining() == 1, "an allowance holds its tokens")?;
    check(allowance.take(), "a token spends")?;
    check(!allowance.take(), "an empty allowance refuses")?;
    allowance.refill(2);
    check(allowance.remaining() == 2, "a refill lands")?;
    let probe = lgwks_bot::script::Rule::new("probe", |_| lgwks_bot::script::Verdict::Allow);
    check(probe.name() == "probe", "a rule carries its name")?;
    let paced = lgwks_bot::script::Rule::pace("paced", 1, Duration::ZERO);
    let suspended = lgwks_bot::script::Rule::suspend("quiet", 10, "quiet-hours");
    let rules = lgwks_bot::script::Justify::new(vec![paced, suspended]);
    check(!rules.is_empty(), "installed rules are not empty")?;
    check(rules.len() == 2, "every rule is installed")?;
    check(
        lgwks_bot::script::Justify::empty().is_empty(),
        "no rules is empty",
    )?;
    let decision = rules.decide(&lgwks_bot::script::JustifyCtx::new("demo", 0));
    let word = match decision.verdict {
        lgwks_bot::script::Verdict::Allow => "allow",
        lgwks_bot::script::Verdict::Slow { .. } => "slow",
        lgwks_bot::script::Verdict::Park { .. } => "park",
        lgwks_bot::script::Verdict::Block { .. } => "block",
        _ => "unknown",
    };
    check(decision.rule == "paced", "the first rule decides first")?;
    writeln!(out, "justify: paced rule answers {word}")
        .map_err(|error| format!("the report was not written: {error}"))?;

    let node = GraphNode::new("probe", &["fetch_a"], move |_scope| {
        Box::pin(async move { Ok::<u64, FlowError>(0) })
    })
    .with_work(2);
    check(node.name() == "probe", "a node carries its name")?;
    check(node.needs() == ["fetch_a"], "a node carries its needs")?;
    check(
        lgwks_bot::script::MAX_GRAPH_NODES == 4_096,
        "the graph admits more lanes than any lane graph needs",
    )?;

    let mut gate = StreamGate::new(2, Overflow::Refuse)?;
    let policies = [Overflow::DropOldest, Overflow::DropNewest, Overflow::Refuse];
    check(
        policies.len() == 3,
        "the gate names its three overflow policies",
    )?;
    check(gate.is_empty(), "a new gate holds nothing")?;
    check(gate.capacity() == 2, "a gate reports its bound")?;
    check(gate.is_empty(), "a new gate retains nothing")?;
    check(gate.push(1_u32).is_ok(), "the first push fits")?;
    check(gate.len() == 1, "a push retains its event")?;
    check(gate.pop() == Some(1_u32), "a retained event pops back")?;
    check(gate.dropped() == 0, "an unpressured gate sheds nothing")?;
    check(gate.refused() == 0, "an unpressured gate refuses nothing")?;
    check(
        map_domain::<u16, DemoFailed>(Ok(7)) == Ok(7),
        "a success crosses the domain boundary unchanged",
    )?;
    writeln!(
        out,
        "vocabulary: every runtime answered through the public interface"
    )
    .map_err(|error| format!("the report was not written: {error}"))?;
    Ok(())
}
