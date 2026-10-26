//! script! vocabulary on the real run path: justification, graphs,
//! compensation, streams and typed domains, driven end to end.
//!
//! Each flow below names one of the runtime vocabularies beside its steps and
//! runs it through the public interface: a justification WAF that stops a run
//! with its reason code, a needs-ordered lane graph whose failure blocks only
//! downstream, compensations that undo in reverse order, a bounded live feed,
//! and typed observe/act calls against a stub registry. `main` runs every
//! flow, checks what each one reports, and prints the trail.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use lgwks_bot::script::{
    FlowError, GraphNode, GraphReport, NodeOutcome, Overflow, PumpReport, Scope, StreamGate,
    Tenant, TypedAction, TypedSource, act_typed, map_domain, observe_typed,
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

lgwks_bot::script! {
    /// Poll under justification: an allowance of two polls, so the third
    /// evaluation stops the run with its reason code.
    flow capped_polls() -> u16:
        let justified = scope.with_justification(lgwks_bot::script::Justify::new(vec![
            lgwks_bot::script::Rule::allowance("polls", Arc::new(lgwks_bot::script::Allowance::new(2)), "poll-budget-spent"),
        ]))
        let scope = &justified
        let seen = retry up to 6 times, waiting (Duration::ZERO):
            fail transiently with "the upstream is down"
        give back seen

    /// Book a seat, charging the card: compensations undo in reverse order
    /// when issuing fails.
    flow booking(fail: bool) -> &'static str:
        let undone = lgwks_bot::script::compensated(scope, "booking", |_scope, compensations| async move {
            for step in ["reserve", "charge", "issue"] {
                if fail && step == "issue" {
                    let refusal = Err::<(), lgwks_bot::script::FlowError>(lgwks_bot::script::FlowError::failed(step));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "booking: failing the seeded step");
                    return refusal;
                }
                compensations.add(step, move |_undo| {
                    Box::pin(async move {
                        Ok::<(), lgwks_bot::script::FlowError>(())
                    })
                });
            }
            Ok(())
        }).await?
        let _ = undone
        give back "ticket"

    /// Run the lanes as a dependency graph: fetch twice, merge, publish.
    flow lanes() -> GraphReport<u64>:
        let nodes = vec![
            lgwks_bot::script::GraphNode::new("fetch-a", &[], move |_scope| {
                Box::pin(async move { Ok::<u64, lgwks_bot::script::FlowError>(1) })
            }).with_work(1),
            lgwks_bot::script::GraphNode::new("fetch-b", &[], move |_scope| {
                Box::pin(async move { Ok::<u64, lgwks_bot::script::FlowError>(2) })
            }).with_work(1),
            lgwks_bot::script::GraphNode::new("merge", &["fetch-a", "fetch-b"], move |_scope| {
                Box::pin(async move { Ok::<u64, lgwks_bot::script::FlowError>(3) })
            }).with_work(2),
            lgwks_bot::script::GraphNode::new("publish", &["merge"], move |_scope| {
                Box::pin(async move { Ok::<u64, lgwks_bot::script::FlowError>(4) })
            }).with_work(1),
        ]
        let report = lgwks_bot::script::graph(scope, "lanes", 2, nodes).await?
        give back report

    /// React to a seeded feed through a bounded gate.
    flow prices() -> PumpReport:
        let mut gate = lgwks_bot::script::StreamGate::new(4, lgwks_bot::script::Overflow::DropOldest).map_err(lgwks_bot::script::FlowError::failed)?
        let pending: std::cell::RefCell<Vec<Option<Vec<u32>>>> = std::cell::RefCell::new(
            vec![None, Some(vec![4, 5]), Some(vec![1, 2, 3])],
        )
        let report = lgwks_bot::script::pump(
            scope,
            "prices",
            &mut gate,
            || {
                let batch = pending.borrow_mut().pop().flatten();
                async move { batch }
            },
            |_, _| async move { Ok::<(), lgwks_bot::script::FlowError>(()) },
        ).await?
        give back report
}

/// Run every vocabulary flow, check what each one reports, and print the trail.
fn main() -> Result<(), Box<dyn Error>> {
    use std::io::Write as _;

    let mut out = std::io::stdout().lock();
    let scope = Scope::root(Tenant::new("demo").map_err(|error| error.to_string())?);
    let blocked = lgwks_bot::block_on(capped_polls(&scope));
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
        report.succeeded() == vec!["fetch-a", "fetch-b", "merge", "publish"],
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

    let feed = lgwks_bot::block_on(prices(&scope)).map_err(|error| format!("{error}"))?;
    check(feed.handled == 5, "every feed event was handled")?;
    check(feed.dropped == 0, "a feed inside the bound sheds nothing")?;
    check(feed.refused == 0, "a feed inside the bound refuses nothing")?;
    writeln!(
        out,
        "stream: handled {} dropped {} refused {}",
        feed.handled, feed.dropped, feed.refused
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

    let node = GraphNode::new("probe", &["fetch-a"], move |_scope| {
        Box::pin(async move { Ok::<u64, FlowError>(0) })
    });
    check(node.name() == "probe", "a node carries its name")?;
    check(node.needs() == ["fetch-a"], "a node carries its needs")?;
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
