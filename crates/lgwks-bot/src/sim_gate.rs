//! Seeded model check of capability admission and grant lifetimes.
//!
//! One seed draws grant sets (shipped and custom capabilities, some with a
//! lifetime, some granted twice) and requirement lists (duplicates and
//! ungranted names included), and every family compares the shipped
//! [`GrantSet`] with a set-and-map reference written here from its docs:
//!
//! - granting is idempotent, and a set grants exactly what was granted;
//! - a capability granted twice keeps the shorter lifetime;
//! - admission refuses with every ungranted requirement, in declaration
//!   order, each named once, and the total form agrees with it;
//! - the repair a denial derives, folded into the held set, admits the
//!   requirement, and grants nothing the set already held;
//! - a minted proof covers the requirement and carries the shortest lifetime
//!   among it, measured from the minting clock, or none when none was named;
//! - the same seed replays the same verdicts and distinct seeds diverge.
//!
//! Sources: Saltzer and Schroeder 1975 (least privilege, fail-safe defaults),
//! RFC 6749 §4.2.2 and §5.1 (`expires_in` is a duration, not an instant), and
//! INV-BOT-SAME-GATE (shipped and custom domains pass one check).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::cap::{Cap, Demand};
use crate::clock::Clock;
use crate::error::BotError;
use crate::gate::GrantSet;
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x6A7E_0000_0000_0001,
    0x6A7E_0000_0000_0002,
    0x6A7E_FFFF_0000_0003,
    0xCA95_6A7E_0000_0004,
];

/// Scenarios each family draws per seed.
const DRAWS: usize = 128;

/// The capability names drawn: the shipped four and three custom ones.
const NAMES: [&str; 7] = [
    Cap::NET,
    Cap::FS,
    Cap::SYS,
    Cap::NOTIFY,
    "acme.ledger.write",
    "acme.mail.send",
    "tenant.a.read",
];

/// A family's result.
type Outcome = Result<(), String>;

/// Whole seconds drawn below `bound`.
fn seconds(stream: &mut Stream, bound: usize) -> Result<Duration, String> {
    let drawn = u64::try_from(stream.below(bound)?).map_err(|error| error.to_string())?;
    Ok(Duration::from_secs(drawn))
}

/// One drawn scenario: the grants, their reference model, and a requirement.
struct Scenario {
    /// The shipped set.
    grants: GrantSet,
    /// The reference: each granted name and the shortest lifetime it named.
    model: BTreeMap<&'static str, Option<Duration>>,
    /// The requirement, duplicates included.
    required: Vec<Cap>,
}

impl Scenario {
    /// A scenario drawn from `stream`.
    fn from_stream(stream: &mut Stream) -> Result<Self, String> {
        let mut grants = GrantSet::empty();
        let mut model: BTreeMap<&'static str, Option<Duration>> = BTreeMap::new();
        for _ in 0..stream.below(10)? {
            let name = stream.choose(&NAMES)?;
            if stream.below(2)? == 0 {
                grants = grants.grant(Cap::new(name));
                model.entry(name).or_insert(None);
            } else {
                let ttl = seconds(stream, 3_600)?;
                grants = grants.grant_expiring(Cap::new(name), ttl);
                let held = model.entry(name).or_insert(Some(ttl));
                *held = Some(match *held {
                    Some(shorter) => shorter.min(ttl),
                    None => ttl,
                });
            }
        }
        let required = (0..stream.below(6)?)
            .map(|_| stream.choose(&NAMES).map(Cap::new))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            grants,
            model,
            required,
        })
    }

    /// The reference shortfall: ungranted requirements in order, once each.
    fn shortfall(&self) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        self.required
            .iter()
            .map(Cap::as_str)
            .filter(|name| !self.model.contains_key(name) && seen.insert(*name))
            .collect()
    }

    /// The reference lifetime of a proof over the requirement.
    fn lifetime(&self) -> Option<Duration> {
        self.required
            .iter()
            .filter_map(|cap| self.model.get(cap.as_str()).copied().flatten())
            .min()
    }
}

/// Every drawn scenario under each seed.
fn sweep(check: impl FnMut(u64, &mut Stream, &Scenario) -> Outcome) -> Outcome {
    crate::sim_stream::sweep(&SEEDS, DRAWS, Scenario::from_stream, check)
}

/// The names a denial carries (none for an admission), or an error naming
/// what came back instead.
fn denied(outcome: Result<(), BotError>, seed: u64) -> Result<Vec<String>, String> {
    match outcome {
        Err(BotError::CapabilityDenied { deficit }) => Ok(deficit
            .shortages()
            .map(|shortage| shortage.required().as_str().to_owned())
            .collect()),
        Ok(()) => Ok(Vec::new()),
        Err(other) => Err(format!("seed {seed:#x}: {other}")),
    }
}

#[test]
/// A set grants exactly what was granted, and granting again changes nothing.
fn sim_a_set_grants_exactly_what_was_granted() -> Outcome {
    sweep(|seed, stream, scenario| {
        for name in NAMES {
            let held = scenario.grants.grants(&Cap::new(name));
            assert_eq!(
                held,
                scenario.model.contains_key(name),
                "seed {seed:#x}: {name}"
            );
        }
        let again = stream.choose(&NAMES)?;
        let regranted = scenario
            .grants
            .clone()
            .grant(Cap::new(again))
            .grant(Cap::new(again));
        for name in NAMES {
            let expected = scenario.model.contains_key(name) || name == again;
            assert_eq!(
                regranted.grants(&Cap::new(name)),
                expected,
                "seed {seed:#x}: {name}"
            );
        }
        Ok(())
    })
}

#[test]
/// A capability granted twice keeps the shorter lifetime.
fn sim_a_regrant_keeps_the_shorter_lifetime() -> Outcome {
    sweep(|seed, _, scenario| {
        for name in NAMES {
            let expected = scenario.model.get(name).copied().flatten();
            let held = scenario.grants.expiry_of(&Cap::new(name));
            assert_eq!(held, expected, "seed {seed:#x}: {name}");
        }
        Ok(())
    })
}

#[test]
/// Admission names every ungranted requirement in order, once each.
fn sim_admission_names_the_whole_shortfall_in_order() -> Outcome {
    sweep(|seed, _, scenario| {
        let named = denied(scenario.grants.admit(&scenario.required), seed)?;
        assert_eq!(
            named,
            scenario.shortfall(),
            "seed {seed:#x}: {:?}",
            scenario.required
        );
        Ok(())
    })
}

#[test]
/// The total form names the shortfall the refusing form does.
fn sim_the_total_form_agrees_with_admission() -> Outcome {
    sweep(|seed, _, scenario| {
        let total: Vec<String> = scenario
            .grants
            .uncovered(&scenario.required, &Demand::new("sim::gate"))
            .iter()
            .map(|shortage| shortage.required().as_str().to_owned())
            .collect();
        let refused = denied(scenario.grants.admit(&scenario.required), seed)?;
        assert_eq!(total, refused, "seed {seed:#x}");
        Ok(())
    })
}

#[test]
/// The repair a denial derives closes the requirement and holds nothing the
/// set already granted.
fn sim_the_derived_repair_closes_the_requirement() -> Outcome {
    sweep(|seed, _, scenario| {
        let Err(BotError::CapabilityDenied { deficit }) = scenario.grants.admit(&scenario.required)
        else {
            return Ok(());
        };
        let repair = deficit.to_grant_set();
        for name in scenario.model.keys() {
            assert!(
                !repair.grants(&Cap::new(*name)),
                "seed {seed:#x}: {name} was already held"
            );
        }
        let closed = deficit
            .shortages()
            .fold(scenario.grants.clone(), |set, shortage| {
                set.grant(shortage.required().clone())
            });
        assert!(closed.admit(&scenario.required).is_ok(), "seed {seed:#x}");
        Ok(())
    })
}

#[test]
/// A minted proof covers the requirement and carries the shortest lifetime
/// among it, measured from the minting clock.
fn sim_a_proof_carries_the_shortest_lifetime() -> Outcome {
    sweep(|seed, stream, scenario| {
        let started = seconds(stream, 10_000)?;
        let clock = Clock::virtual_at(started);
        match scenario.grants.issue_at(&scenario.required, &clock) {
            Ok(proof) => {
                assert!(
                    scenario.shortfall().is_empty(),
                    "seed {seed:#x}: a short set minted"
                );
                for cap in &scenario.required {
                    assert!(proof.covers_cap(cap), "seed {seed:#x}: {cap:?}");
                }
                let expected = scenario.lifetime().map(|ttl| started.saturating_add(ttl));
                assert_eq!(proof.expires_at(), expected, "seed {seed:#x}");
                assert_eq!(proof.remaining(), scenario.lifetime(), "seed {seed:#x}");
                Ok(())
            }
            Err(BotError::CapabilityDenied { .. }) => {
                assert!(
                    !scenario.shortfall().is_empty(),
                    "seed {seed:#x}: a full set refused"
                );
                Ok(())
            }
            Err(other) => Err(format!("seed {seed:#x}: {other}")),
        }
    })
}

#[test]
/// The shipped four are granted together and nothing else is.
fn sim_the_shipped_grant_is_the_four_and_nothing_else() -> Outcome {
    sweep(|seed, _, scenario| {
        let named = denied(GrantSet::all_shipped().admit(&scenario.required), seed)?;
        let mut seen = BTreeSet::new();
        let custom: Vec<&str> = scenario
            .required
            .iter()
            .map(Cap::as_str)
            .filter(|name| name.starts_with("acme.") || name.starts_with("tenant."))
            .filter(|name| seen.insert(*name))
            .collect();
        assert_eq!(named, custom, "seed {seed:#x}: only custom names are short");
        Ok(())
    })
}

#[test]
/// An empty set refuses every non-empty requirement and admits the empty one.
fn sim_an_empty_set_refuses_everything_it_is_asked_for() -> Outcome {
    sweep(|seed, _, scenario| {
        let outcome = GrantSet::empty().admit(&scenario.required);
        assert_eq!(
            outcome.is_ok(),
            scenario.required.is_empty(),
            "seed {seed:#x}"
        );
        Ok(())
    })
}

/// The replay receipt of every verdict drawn under `seed`.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..DRAWS {
        let scenario = Scenario::from_stream(&mut stream)?;
        let named = denied(scenario.grants.admit(&scenario.required), seed)?;
        trace = fold(
            trace,
            format!("{named:?}{:?};", scenario.lifetime()).as_bytes(),
        );
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one verdict trace.
fn sim_the_same_seed_replays_the_same_verdicts() -> Outcome {
    for seed in SEEDS {
        assert_eq!(
            verdict_trace(seed)?,
            verdict_trace(seed)?,
            "seed {seed:#x} replays"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_verdicts() -> Outcome {
    assert_ne!(
        verdict_trace(SEEDS[0])?,
        verdict_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
