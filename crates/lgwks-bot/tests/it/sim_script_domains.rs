//! Simulation family: `observe` and `act` against the `BotSpec` path (#388).
//!
//! Every scenario runs flows the real `script!` macro expanded, on a real
//! `Host` whose registry is `script_domains::SCRIPT_DOMAINS`, and where a spec
//! is the comparison, a real `Bot` materialized from JSON against the same
//! static. The seed draws the inputs: the count a source reads, the threshold
//! a page waits for, how many pages and under which labels, which misspelled
//! flow runs, which tenant runs first. The model is what the spec path does
//! with the same draws, so a script that polled in another order, paged one
//! label too many or ran a step before its admission diverges from it.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `the_script_trace_is_the_spec_trace` | a flow and a spec over one registry poll and execute the same operations in the same order |
//! | `an_unknown_domain_refuses_the_flow_before_any_step` | every undeclared identifier is named at the flow's entry, and no step runs |
//! | `a_domain_short_of_authority_blocks_its_step` | a source's capabilities are checked against each host's own grant |
//! | `an_external_action_is_refused_before_it_runs` | `act` never runs an effect that leaves the process |
//!
//! Each family sweeps its band twice through `sim::assert_replays`, so a
//! nondeterministic run fails on its hash as well as on its oracle.

#![cfg(feature = "script")]

use std::error::Error;

use lgwks_bot::script::Scope;
use lgwks_bot::spec::{Bot, BotSpec};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{BotError, Cap, GrantSet};

use crate::effects::memory_scope;
use crate::script_domains::{
    SCRIPT_DOMAINS, blocked_on, bot_error, host, page_then_misspelled_source, page_when_over, post,
    reach, two_misspellings, undeclared, unknown_in_registry,
};
use crate::sim::{self, Band, Sim};
use crate::spec_materialize::take_trace;

/// A family's result: an error fails it with the seed's text.
type SimResult = Result<(), Box<dyn Error>>;

/// How many seeds each family sweeps.
const SEEDS: u64 = 1024;

/// Two tenants' hosts over [`SCRIPT_DOMAINS`]: `acme` granted nothing,
/// `zenith` granted `bot.net`.
fn two_hosts() -> Result<[Host; 2], Box<dyn Error>> {
    Ok([
        host("acme", GrantSet::empty())?,
        host("zenith", GrantSet::empty().grant(Cap::net()))?,
    ])
}

/// A `u16` below `bound`, from the seed.
fn below(sim: &mut Sim, bound: u32) -> Result<u16, Box<dyn Error>> {
    Ok(u16::try_from(sim.rng().below(bound))?)
}

// ── The trace family ───────────────────────────────────────────────────────

/// The spec a flow's draw corresponds to: one `test::counter` chain whose
/// entries page each label above the threshold, in label order.
fn spec_for(count: u16, over: u16, labels: &[String]) -> String {
    let entries: Vec<String> = labels
        .iter()
        .map(|label| {
            format!(r#"["threshold::above({over})",{{"domain":"test::page","target":"{label}"}}]"#)
        })
        .collect();
    format!(
        r#"{{"version":1,"name":"sim","chains":[{{"source":"test::counter","target":"{count}","on":[{}]}}]}}"#,
        entries.join(",")
    )
}

/// One seed of the trace family on `host`.
fn trace_seed(host: &Host, sim: &mut Sim) -> SimResult {
    let count = below(sim, 10)?;
    let over = below(sim, 10)?;
    let pages = sim.rng().below(6).saturating_add(1);
    let mut labels = Vec::new();
    for _ in 0..pages {
        labels.push(format!("l{}", sim.rng().below(4)));
    }
    let paging = task(
        "paging",
        |scope: Scope, (target, over, labels): (String, u16, Vec<String>)| async move {
            page_when_over(&scope, &target, over, &labels).await
        },
    )?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&paging, (count.to_string(), over, labels.clone())));
    assert_eq!(
        report.result().ok().copied(),
        Some(count),
        "seed {}: {:?}",
        sim.seed,
        report.result()
    );
    let script_trace = take_trace();

    let spec = BotSpec::from_json(&spec_for(count, over, &labels))?;
    let mut bot = Bot::from_spec(&spec, &SCRIPT_DOMAINS, &GrantSet::empty(), memory_scope()?)?;
    bot.tick()?;
    let spec_trace = take_trace();
    assert_eq!(script_trace, spec_trace, "seed {}", sim.seed);

    let pages_sent = if count > over { labels.len() } else { 0 };
    assert_eq!(
        script_trace.len(),
        pages_sent.saturating_add(1),
        "seed {}: one poll, then one page per label over the threshold",
        sim.seed
    );
    sim.record(&format!("{count}>{over}:{}", script_trace.join(",")));
    Ok(())
}

#[test]
fn the_script_trace_is_the_spec_trace() -> SimResult {
    let host = host("acme", GrantSet::empty())?;
    sim::assert_replays(Band::new(0, SEEDS), |sim| trace_seed(&host, sim))
}

// ── The admission family ───────────────────────────────────────────────────

/// One seed of the admission family on whichever of `hosts` the seed draws.
fn admission_seed(hosts: &[Host; 2], sim: &mut Sim) -> SimResult {
    let tenant = usize::from(below(sim, 2)?);
    let target = below(sim, 10)?.to_string();
    let host = hosts.get(tenant).ok_or("two hosts")?;
    let _ = take_trace();
    let (flow, report) = if sim.rng().below(2) == 0 {
        let checking = task("checking", |scope: Scope, target: String| async move {
            page_then_misspelled_source(&scope, &target).await
        })?;
        (
            "page-then-misspelled",
            lgwks_bot::block_on(host.run(&checking, target)),
        )
    } else {
        let checking = task("checking", |scope: Scope, target: String| async move {
            two_misspellings(&scope, &target).await
        })?;
        (
            "two-misspellings",
            lgwks_bot::block_on(host.run(&checking, target)),
        )
    };
    let source = unknown_in_registry("source", "test::countr", "test::counter");
    let expected = if flow == "two-misspellings" {
        vec![
            source,
            unknown_in_registry("action", "test::pager", "test::page"),
        ]
    } else {
        vec![source]
    };
    assert_eq!(undeclared(&report)?, expected, "seed {}: {flow}", sim.seed);
    let trace = take_trace();
    assert!(
        trace.is_empty(),
        "seed {}: {flow} ran {trace:?} before its admission",
        sim.seed
    );
    sim.record(&format!("{tenant}:{flow}:{}", expected.len()));
    Ok(())
}

#[test]
fn an_unknown_domain_refuses_the_flow_before_any_step() -> SimResult {
    let hosts = two_hosts()?;
    sim::assert_replays(Band::new(0, SEEDS), |sim| admission_seed(&hosts, sim))
}

// ── The authority family ───────────────────────────────────────────────────

/// One seed of the authority family: both tenants reach the same identifier
/// in a seeded order, and each is judged by its own host's grant.
fn authority_seed(bare: &Host, granted: &Host, sim: &mut Sim) -> SimResult {
    let reaching = task("reaching", |scope: Scope, target: String| async move {
        reach(&scope, &target).await
    })?;
    let granted_first = sim.rng().below(2) == 0;
    let order: [(&Host, bool); 2] = if granted_first {
        [(granted, true), (bare, false)]
    } else {
        [(bare, false), (granted, true)]
    };
    for (host, holds_net) in order {
        let count = below(sim, 10)?;
        let _ = take_trace();
        let report = lgwks_bot::block_on(host.run(&reaching, count.to_string()));
        let trace = take_trace();
        if holds_net {
            assert_eq!(
                report.result().ok().copied(),
                Some(count),
                "seed {}",
                sim.seed
            );
            assert_eq!(trace, ["poll:test::counter"], "seed {}", sim.seed);
        } else {
            assert_eq!(blocked_on(&report)?, [Cap::net()], "seed {}", sim.seed);
            assert!(trace.is_empty(), "seed {}: polled while blocked", sim.seed);
        }
        sim.record(&format!("{holds_net}:{count}"));
    }
    Ok(())
}

#[test]
fn a_domain_short_of_authority_blocks_its_step() -> SimResult {
    let [bare, granted] = two_hosts()?;
    sim::assert_replays(Band::new(0, SEEDS), |sim| {
        authority_seed(&bare, &granted, sim)
    })
}

// ── The effect family ──────────────────────────────────────────────────────

/// One seed of the effect family: an external action under any grant, any
/// target and any value is refused before it runs.
fn effect_seed(hosts: &[Host; 2], sim: &mut Sim) -> SimResult {
    let host = hosts.get(usize::from(below(sim, 2)?)).ok_or("two hosts")?;
    let target = format!("channel-{}", sim.rng().below(16));
    let value = below(sim, 1000)?;
    let posting = task(
        "posting",
        |scope: Scope, (target, value): (String, u16)| async move { post(&scope, &target, value).await },
    )?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&posting, (target.clone(), value)));
    assert!(
        matches!(
            *bot_error(&report)?,
            BotError::UnjournaledEffect { ref domain } if domain == "test::post"
        ),
        "seed {}: {:?}",
        sim.seed,
        report.result()
    );
    let trace = take_trace();
    assert!(
        trace.is_empty(),
        "seed {}: the action ran: {trace:?}",
        sim.seed
    );
    sim.record(&format!("{target}:{value}"));
    Ok(())
}

#[test]
fn an_external_action_is_refused_before_it_runs() -> SimResult {
    let hosts = two_hosts()?;
    sim::assert_replays(Band::new(0, SEEDS), |sim| effect_seed(&hosts, sim))
}
