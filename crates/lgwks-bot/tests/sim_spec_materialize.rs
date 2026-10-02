//! Deterministic simulation of the spec materializer.
//!
//! One seed draws a whole scenario: a grant set, a chain count, and for each
//! chain a source, a target, an action and a condition — every one of them
//! either resolvable or a corruption of the kind admission must report. The
//! model computes the exact `NeedSet` those draws injected, and the run asserts
//! two things the contract names: materialization succeeds **iff** the generated
//! spec had no needs, and the `NeedSet` the materializer returns equals the
//! injected set exactly — no more, no fewer, in the same order.
//!
//! Everything is derived from the seed, so the whole scenario replays. The trace
//! hash is the receipt: `assert_replays` sweeps the band twice and refuses a
//! band whose two hash vectors differ, which catches a nondeterminism the
//! per-seed assertions would miss. The real `Bot`, `DomainRegistry`,
//! `Source`/`Action` handles and `MemoryJournal` are all driven; only the seed
//! is virtual.

mod sim;

use std::error::Error;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{ActionSpec, Bot, BotSpec, ChainSpec, EffectIdentity, EffectScope};
use lgwks_bot::{
    Action, Admission, Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, Need, Observe,
    Source, domains,
};

use sim::{Band, Sim};

/// The flow revision every simulated scope uses.
const FLOW: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// The cause message the counter constructors produce for a target that is not
/// a count, so the model can name it without re-running the constructor.
const TARGET_CAUSE: &str = "incomplete bot spec: missing target";

/// How many seeds the sweep covers. Well over the thousand the contract names,
/// so no band of the space goes untested.
const SEEDS: u64 = 1024;

// ── Domains ────────────────────────────────────────────────────────────────

/// A source that reports the count its target parsed to.
struct SimCounter {
    /// The value to report.
    value: u16,
    /// The capabilities it requires.
    caps: Vec<Cap>,
}

/// Parse a target as a count, or refuse it — the malformed-input case a
/// constructor reports through admission.
fn parse_sim_count(target: &str) -> Result<u16, BotError> {
    target
        .parse::<u16>()
        .map_err(|_| BotError::IncompleteSpec { field: "target" })
}

impl SimCounter {
    /// Build one with the given capabilities directly.
    fn with_caps(value: u16, caps: Vec<Cap>) -> Self {
        Self { value, caps }
    }

    /// Build a cap-free one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Source, BotError> {
        Ok(Source::ordered(Self::with_caps(
            parse_sim_count(target)?,
            Vec::new(),
        )))
    }

    /// Build one that requires `bot.net`.
    fn net_from_target(target: &str) -> Result<Source, BotError> {
        Ok(Source::ordered(Self::with_caps(
            parse_sim_count(target)?,
            vec![Cap::net()],
        )))
    }
}

impl Observe for SimCounter {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        Ok(self.value)
    }

    fn domain_id(&self) -> &str {
        "sim::counter"
    }
}

/// An action that accepts a count.
struct SimPage {
    /// The capabilities it requires.
    caps: Vec<Cap>,
}

impl SimPage {
    /// Build one with the given capabilities directly.
    fn with_caps(caps: Vec<Cap>) -> Self {
        Self { caps }
    }

    /// Build a cap-free one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Action, BotError> {
        parse_sim_count(target)?;
        Ok(Action::new(Self::with_caps(Vec::new())))
    }

    /// Build one that requires `bot.net`.
    fn net_from_target(target: &str) -> Result<Action, BotError> {
        parse_sim_count(target)?;
        Ok(Action::new(Self::with_caps(vec![Cap::net()])))
    }
}

impl Execute for SimPage {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&self.caps)?;
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "sim::page"
    }
}

domains! {
    /// The domains the sweep draws from.
    pub SIM_DOMAINS {
        observe {
            "sim::counter" => SimCounter::from_target,
            "sim::net_counter" => SimCounter::net_from_target,
        }
        execute {
            "sim::page" => SimPage::from_target,
            "sim::net_page" => SimPage::net_from_target,
        }
    }
}

// ── The scenario ───────────────────────────────────────────────────────────

/// A fresh effect scope over an in-memory journal.
fn scope() -> Result<EffectScope, Box<dyn Error>> {
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
            environment,
            FlowRevision::from_tagged("blake3_256", FLOW)?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    ))
}

/// The condition a draw names, and whether it is outside the vocabulary.
fn draw_condition(sim: &mut Sim) -> (String, bool) {
    match sim.rng().below(5) {
        0 => ("always".to_owned(), false),
        1 => ("changed".to_owned(), false),
        2 => (
            format!("threshold::above({})", sim.rng().between(0, 20)),
            false,
        ),
        3 => (
            format!("threshold::below({})", sim.rng().between(0, 20)),
            false,
        ),
        _ => (format!("mystery::gate{}", sim.rng().below(8)), true),
    }
}

/// Draw one action entry and push the needs it injects, in the order the
/// materializer reports them: the action's own need first, then its capability
/// need.
fn draw_action(
    sim: &mut Sim,
    chain_index: usize,
    action_index: usize,
    grants: &GrantSet,
    needs: &mut Vec<Need>,
) -> ActionSpec {
    match sim.rng().below(3) {
        // A target the action constructor rejects.
        2 => {
            let target = format!("bad{}", sim.rng().below(8));
            needs.push(Need::ActionTargetRejected {
                chain: chain_index,
                action: action_index,
                domain: "sim::page".to_owned(),
                cause: TARGET_CAUSE.to_owned(),
            });
            ActionSpec::new("sim::page", target)
        }
        // A cap-free action.
        0 => ActionSpec::new("sim::page", sim.rng().between(0, 20).to_string()),
        // An action that requires `bot.net`.
        _ => {
            if !grants.grants(&Cap::net()) {
                needs.push(Need::MissingCapability {
                    chain: chain_index,
                    action: Some(action_index),
                    domain: "sim::net_page".to_owned(),
                    capability: Cap::net(),
                });
            }
            ActionSpec::new("sim::net_page", sim.rng().between(0, 20).to_string())
        }
    }
}

/// Draw one chain, pushing its needs into `needs` in declaration order.
fn draw_chain(
    sim: &mut Sim,
    chain_index: usize,
    grants: &GrantSet,
    needs: &mut Vec<Need>,
) -> ChainSpec {
    match sim.rng().below(4) {
        // An unknown source identifier: nothing downstream is knowable.
        3 => {
            let domain = format!("unknown::source{}", sim.rng().below(8));
            needs.push(Need::UnknownSource {
                chain: chain_index,
                domain: domain.clone(),
            });
            ChainSpec::new(domain, "x", Vec::new())
        }
        // A source constructor that rejects its target.
        2 => {
            let target = format!("bad{}", sim.rng().below(8));
            needs.push(Need::SourceTargetRejected {
                chain: chain_index,
                domain: "sim::counter".to_owned(),
                cause: TARGET_CAUSE.to_owned(),
            });
            ChainSpec::new("sim::counter", target, Vec::new())
        }
        // A resolvable source, with its capability need and its entries.
        pick => {
            let (domain, network) = if pick == 0 {
                ("sim::counter", false)
            } else {
                ("sim::net_counter", true)
            };
            if network && !grants.grants(&Cap::net()) {
                needs.push(Need::MissingCapability {
                    chain: chain_index,
                    action: None,
                    domain: domain.to_owned(),
                    capability: Cap::net(),
                });
            }
            let value = sim.rng().between(0, 20);
            let entry_count = usize::try_from(sim.rng().between(0, 3)).unwrap_or(0);
            let mut on = Vec::new();
            for action_index in 0..entry_count {
                let (condition, unknown) = draw_condition(sim);
                let action = draw_action(sim, chain_index, action_index, grants, needs);
                if unknown {
                    needs.push(Need::UnknownCondition {
                        chain: chain_index,
                        action: action_index,
                        condition: condition.clone(),
                    });
                }
                // The spec carries the identifier verbatim on both paths; an
                // unknown one is recorded as a need and the entry simply does
                // not build.
                on.push((condition, action));
            }
            ChainSpec::new(domain, value.to_string(), on)
        }
    }
}

/// Draw a whole scenario: a grant set, a spec, and the needs it injects.
fn generate(sim: &mut Sim) -> (GrantSet, BotSpec, Vec<Need>) {
    let grants = if sim.rng().chance(500) {
        GrantSet::empty().grant(Cap::net())
    } else {
        GrantSet::empty()
    };
    let chain_count = sim.rng().between(1, 4);
    let mut chains = Vec::with_capacity(usize::try_from(chain_count).unwrap_or(0));
    let mut needs = Vec::new();
    for chain_index in 0..usize::try_from(chain_count).unwrap_or(0) {
        chains.push(draw_chain(sim, chain_index, &grants, &mut needs));
    }
    (grants, BotSpec::new("sim", chains), needs)
}

/// A stable rendering of an admission outcome, for the trace.
fn describe(outcome: &Result<Bot, Admission>) -> String {
    match *outcome {
        Ok(ref bot) => format!("ok:{}", bot.name()),
        Err(Admission::Needs(ref needs)) => format!("needs:{needs}"),
        // A wildcard covers any future admission arm; `Refused` renders itself.
        Err(ref other) => format!("refused:{other}"),
    }
}

/// One seed's whole scenario.
fn one_seed(sim: &mut Sim) -> Result<(), Box<dyn Error>> {
    lgwks_std::trace::warn!(
        operation = "one_seed",
        "operation refused its request; the typed error carries the facts"
    );
    let (grants, spec, expected) = generate(sim);

    // The in-memory spec: succeed iff the model injected no need, and report
    // exactly the injected set.
    let outcome = Bot::from_spec(&spec, &SIM_DOMAINS, &grants, scope()?);
    let described = describe(&outcome);
    match outcome {
        Ok(bot) => assert!(
            expected.is_empty(),
            "seed {}: materialization of {} succeeded with {} injected need(s)",
            sim.seed,
            bot.name(),
            expected.len()
        ),
        Err(Admission::Needs(needs)) => {
            assert!(
                !expected.is_empty(),
                "seed {}: a clean spec was refused with {} need(s)",
                sim.seed,
                needs.len()
            );
            assert_eq!(
                needs.needs(),
                expected.as_slice(),
                "seed {}: the NeedSet must equal exactly the injected set",
                sim.seed
            );
        }
        Err(Admission::Refused(cause)) => {
            return Err(format!(
                "seed {}: a well-formed spec was refused structurally: {cause}",
                sim.seed
            )
            .into());
        }
        Err(_) => {
            return Err(format!("seed {}: unexpected admission variant", sim.seed).into());
        }
    }
    sim.record(&described);

    // The wire round trip: the same spec through JSON must reach the same
    // outcome, because the materializer's input is the same document.
    let json = spec.to_json()?;
    let reparsed = BotSpec::from_json(&json)?;
    let replayed = Bot::from_spec(&reparsed, &SIM_DOMAINS, &grants, scope()?);
    assert_eq!(
        describe(&replayed),
        described,
        "seed {}: the JSON round trip changed the admission outcome",
        sim.seed
    );

    // A mutated document must never panic the parser; only the outcome is
    // recorded, because a random byte may land validly.
    let mut glyphs: Vec<char> = json.chars().collect();
    if !glyphs.is_empty() {
        let last = glyphs.len().saturating_sub(1);
        let at =
            usize::try_from(sim.rng().between(0, u32::try_from(last).unwrap_or(0))).unwrap_or(0);
        if let Some(slot) = glyphs.get_mut(at) {
            *slot = '@';
        }
    }
    let mutated: String = glyphs.into_iter().collect();
    let accepted = BotSpec::from_json(&mutated).is_ok();
    sim.record(if accepted {
        "mutated-accepted"
    } else {
        "mutated-refused"
    });

    // An unsupported version is a typed refusal, whatever else the seed drew.
    assert!(
        matches!(
            BotSpec::from_json(r#"{"version":2,"name":"x","chains":[]}"#),
            Err(BotError::UnsupportedSpecVersion { found: 2, .. })
        ),
        "seed {}: an unsupported version must be refused",
        sim.seed
    );
    sim.record("version-boundary-checked");

    Ok(())
}

// ── Declared cases ─────────────────────────────────────────────────────────

#[test]
fn generated_specs_admit_exactly_their_injected_needs() -> Result<(), Box<dyn Error>> {
    sim::assert_replays(Band::new(0, SEEDS), one_seed)
}

/// Runs one seed's scenario under the replay check, for a source-visible case.
fn replay_seed(seed: u64) -> Result<(), Box<dyn Error>> {
    sim::assert_replays(Band::new(seed, 1), one_seed)
}

/// Declares source-visible `#[test]` attributes over the seed space.
macro_rules! source_visible_cases {
    ($(#[$attr:meta] $name:ident => $seed:literal;)+) => {
        $(
            #[doc = "Source-visible deterministic materialization replay case."]
            #[$attr]
            fn $name() -> Result<(), Box<dyn Error>> {
                replay_seed($seed)
            }
        )+
    };
}

source_visible_cases!(
    #[test] spec_replay_0000 => 0;
    #[test] spec_replay_0001 => 1;
    #[test] spec_replay_0002 => 2;
    #[test] spec_replay_0003 => 3;
    #[test] spec_replay_0004 => 4;
    #[test] spec_replay_0005 => 5;
    #[test] spec_replay_0006 => 6;
    #[test] spec_replay_0007 => 7;
    #[test] spec_replay_0008 => 8;
    #[test] spec_replay_0009 => 9;
    #[test] spec_replay_0010 => 10;
    #[test] spec_replay_0011 => 11;
    #[test] spec_replay_0012 => 12;
    #[test] spec_replay_0013 => 13;
    #[test] spec_replay_0014 => 14;
    #[test] spec_replay_0015 => 15;
);
