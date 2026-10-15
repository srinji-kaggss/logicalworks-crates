//! Seeded simulation of capability admission against a reference set model.
//!
//! One seed draws grant sets and requirement lists over a pool of names — the
//! shipped four and custom dotted names, with repeats — and every family
//! asserts what [`GrantSet`] and [`Auth`](crate::cap::Auth) report against
//! `BTreeSet` arithmetic written from the module's statement
//! (`required ⊆ granted`):
//!
//! - admission succeeds exactly when every requirement is granted, and a
//!   denial names exactly `required − granted`, in first-declaration order,
//!   each name once;
//! - a proof and the gate name the same shortfall, and only the gate, told a
//!   caller, attributes it;
//! - a minted proof covers the requirement as a sorted, de-duplicated set;
//! - the deficit's grant set, folded into what is held, closes the requirement
//!   and adds nothing already held; granting is idempotent;
//! - a capability granted twice keeps the shorter lifetime, and a proof takes
//!   the shortest lifetime among what it covers, expiring exactly there on a
//!   virtual clock; expiry is reported before coverage by the same-clock
//!   check and after it by the cross-host check;
//! - a borrowed and an owned spelling are one capability;
//! - exactly 401, 403 and 404 are credential statuses;
//! - the same seed replays the same verdicts and distinct seeds diverge.
//!
//! Sources: the confused-deputy problem (Hardy 1988) that sealed proofs exist
//! to close, and RFC 9110 §15.5 for the statuses that refuse a credential.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::cap::{Cap, Demand, is_credential_status};
use crate::clock::Clock;
use crate::error::BotError;
use crate::gate::GrantSet;
use crate::sim_stream::{RECEIPT_START, Stream, fold};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0xCA90_0000_0000_0001,
    0xCA90_0000_0000_0002,
    0xCA90_FFFF_0000_0003,
    0x5EA1_CA90_0000_0004,
];

/// Scenarios each family draws per seed.
const SCENARIOS: usize = 128;

/// The names capabilities are drawn from: the shipped four and custom ones.
const POOL: [&str; 10] = [
    Cap::NET,
    Cap::FS,
    Cap::SYS,
    Cap::NOTIFY,
    "gh.pr.write",
    "gh.pr.read",
    "store.tenant.acme",
    "store.tenant.zeta",
    "model.call",
    "your.domain.cap",
];

/// A family's result.
type Outcome = Result<(), String>;

/// Up to `most` drawn pool names, repeats allowed.
fn names(stream: &mut Stream, most: usize) -> Result<Vec<&'static str>, String> {
    let count = stream.below(most.saturating_add(1))?;
    (0..count).map(|_| stream.choose(&POOL)).collect()
}

/// A drawn lifetime of 1 to 10,000 seconds.
fn ttl(stream: &mut Stream) -> Result<Duration, String> {
    let seconds = u64::try_from(stream.below(10_000)?).map_err(|error| error.to_string())?;
    Ok(Duration::from_secs(seconds.saturating_add(1)))
}

/// One drawn admission: what is granted and what is required.
struct Scenario {
    /// The granted names.
    granted: Vec<&'static str>,
    /// The requirement list, in declaration order, repeats allowed.
    required: Vec<&'static str>,
}

impl Scenario {
    /// A scenario drawn from `stream`.
    fn draw(stream: &mut Stream) -> Result<Self, String> {
        Ok(Self {
            granted: names(stream, 8)?,
            required: names(stream, 8)?,
        })
    }

    /// The grant set the scenario holds.
    fn grants(&self) -> GrantSet {
        self.granted
            .iter()
            .fold(GrantSet::empty(), |set, name| set.grant(Cap::new(*name)))
    }

    /// The granted names as capabilities.
    fn granted_caps(&self) -> Vec<Cap> {
        self.granted.iter().map(|name| Cap::new(*name)).collect()
    }

    /// The requirement as capabilities.
    fn required_caps(&self) -> Vec<Cap> {
        self.required.iter().map(|name| Cap::new(*name)).collect()
    }

    /// The reference shortfall: required names not granted, first-declaration
    /// order, each once.
    fn shortfall(&self) -> Vec<String> {
        let granted: BTreeSet<&str> = self.granted.iter().copied().collect();
        let mut seen = BTreeSet::new();
        self.required
            .iter()
            .copied()
            .filter(|name| !granted.contains(name) && seen.insert(*name))
            .map(str::to_owned)
            .collect()
    }
}

/// Every drawn scenario under each seed.
fn sweep(mut check: impl FnMut(u64, &mut Stream, &Scenario) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..SCENARIOS {
            let scenario = Scenario::draw(&mut stream)?;
            check(seed, &mut stream, &scenario)?;
        }
    }
    Ok(())
}

/// The names a denial reports, or `None` for an admission.
fn denied_names(result: Result<(), BotError>) -> Result<Option<Vec<String>>, String> {
    match result {
        Ok(()) => Ok(None),
        Err(BotError::CapabilityDenied { deficit }) => Ok(Some(
            deficit
                .shortages()
                .map(|shortage| shortage.required().as_str().to_owned())
                .collect(),
        )),
        Err(other) => Err(format!("expected a capability denial, got {other}")),
    }
}

#[test]
/// Admission succeeds exactly when every requirement is granted, and a
/// denial names exactly the reference shortfall.
fn sim_admission_is_subset_and_a_denial_names_the_whole_shortfall() -> Outcome {
    sweep(|seed, _, scenario| {
        let named = denied_names(scenario.grants().admit(&scenario.required_caps()))?;
        let shortfall = scenario.shortfall();
        match named {
            None => assert!(
                shortfall.is_empty(),
                "seed {seed:#x}: admitted though short {shortfall:?}"
            ),
            Some(names) => assert_eq!(names, shortfall, "seed {seed:#x}: the whole shortfall"),
        }
        Ok(())
    })
}

#[test]
/// A proof minted for the granted names and the gate itself name the same
/// shortfall; only the gate, told a caller, attributes it.
fn sim_the_proof_and_the_gate_name_the_same_shortfall() -> Outcome {
    sweep(|seed, _, scenario| {
        let held = scenario.grants();
        let proof = held
            .issue(&scenario.granted_caps())
            .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let required = scenario.required_caps();
        let from_proof = proof.uncovered(&required);
        let from_gate = held.uncovered(&required, &Demand::new("sim::domain"));
        let proof_names: Vec<&str> = from_proof
            .iter()
            .map(|shortage| shortage.required().as_str())
            .collect();
        let gate_names: Vec<&str> = from_gate
            .iter()
            .map(|shortage| shortage.required().as_str())
            .collect();
        assert_eq!(
            proof_names, gate_names,
            "seed {seed:#x}: one walk, one shortfall"
        );
        assert!(
            from_proof
                .iter()
                .all(|shortage| shortage.demand().is_none()),
            "seed {seed:#x}: a bare proof check names no caller"
        );
        assert!(
            from_gate
                .iter()
                .all(|shortage| shortage.demand().map(Demand::domain) == Some("sim::domain")),
            "seed {seed:#x}: the gate attributes every shortage"
        );
        Ok(())
    })
}

#[test]
/// A minted proof covers its requirement as a sorted, de-duplicated set, and
/// answers membership for every pooled name.
fn sim_a_proof_covers_a_sorted_deduplicated_set() -> Outcome {
    sweep(|seed, _, scenario| {
        let required = scenario.required_caps();
        let proof = required
            .iter()
            .fold(GrantSet::empty(), |set, cap| set.grant(cap.clone()))
            .issue(&required)
            .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let expected: BTreeSet<&str> = scenario.required.iter().copied().collect();
        let covered: Vec<&str> = proof.covers().iter().map(Cap::as_str).collect();
        assert_eq!(
            covered,
            expected.iter().copied().collect::<Vec<_>>(),
            "seed {seed:#x}: name order, each once"
        );
        for name in POOL {
            assert_eq!(
                proof.covers_cap(&Cap::new(name)),
                expected.contains(name),
                "seed {seed:#x}: membership of {name}"
            );
        }
        Ok(())
    })
}

#[test]
/// The deficit's grant set closes the requirement when folded into what is
/// held, and repairs nothing already held.
fn sim_the_deficit_repairs_exactly_the_shortfall() -> Outcome {
    sweep(|seed, _, scenario| {
        let held = scenario.grants();
        let required = scenario.required_caps();
        let Err(BotError::CapabilityDenied { deficit }) = held.admit(&required) else {
            return Ok(());
        };
        let repair = deficit.to_grant_set();
        for name in &scenario.granted {
            assert!(
                !repair.grants(&Cap::new(*name)),
                "seed {seed:#x}: {name} was held and is not repaired"
            );
        }
        let closed = deficit
            .shortages()
            .fold(held, |set, shortage| set.grant(shortage.required().clone()));
        closed
            .admit(&required)
            .map_err(|error| format!("seed {seed:#x}: the repair did not close: {error}"))
    })
}

#[test]
/// Granting is idempotent: a set granted every name twice admits exactly what
/// the set granted once admits.
fn sim_granting_twice_changes_nothing() -> Outcome {
    sweep(|seed, stream, scenario| {
        let once = scenario.grants();
        let twice = scenario
            .granted
            .iter()
            .fold(once.clone(), |set, name| set.grant(Cap::new(*name)));
        for _ in 0..4 {
            let probe: Vec<Cap> = names(stream, 6)?.into_iter().map(Cap::new).collect();
            assert_eq!(
                denied_names(once.admit(&probe))?,
                denied_names(twice.admit(&probe))?,
                "seed {seed:#x}: re-granting changed a verdict"
            );
        }
        Ok(())
    })
}

#[test]
/// A capability granted with several lifetimes keeps the shortest, and a proof
/// takes the shortest among what it covers, expiring exactly there.
fn sim_a_proof_expires_at_its_shortest_lifetime() -> Outcome {
    sweep(|seed, stream, scenario| {
        let mut set = GrantSet::empty();
        let mut shortest: BTreeMap<&str, Duration> = BTreeMap::new();
        for name in &scenario.granted {
            if stream.below(2)? == 0 {
                set = set.grant(Cap::new(*name));
            } else {
                let lifetime = ttl(stream)?;
                set = set.grant_expiring(Cap::new(*name), lifetime);
                let held = shortest.entry(name).or_insert(lifetime);
                *held = (*held).min(lifetime);
            }
        }
        for (name, lifetime) in &shortest {
            assert_eq!(
                set.expiry_of(&Cap::new(*name)),
                Some(*lifetime),
                "seed {seed:#x}: {name} keeps its shortest lifetime"
            );
        }
        let required = scenario.granted_caps();
        let clock = Clock::virtual_at(Duration::ZERO);
        let proof = set
            .issue_at(&required, &clock)
            .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let expected = shortest.values().min().copied();
        assert_eq!(
            proof.expires_at(),
            expected,
            "seed {seed:#x}: the shortest decides"
        );
        let Some(lifetime) = expected else {
            assert!(
                !proof.is_expired(),
                "seed {seed:#x}: no lifetime, no expiry"
            );
            return Ok(());
        };
        clock
            .advance(lifetime.saturating_sub(Duration::from_nanos(1)))
            .map_err(|error| error.to_string())?;
        proof
            .check(&required)
            .map_err(|error| format!("seed {seed:#x}: refused before expiry: {error}"))?;
        clock
            .advance(Duration::from_nanos(1))
            .map_err(|error| error.to_string())?;
        assert!(
            proof.is_expired(),
            "seed {seed:#x}: expired at its deadline"
        );
        assert!(
            matches!(
                proof.check(&required),
                Err(BotError::CredentialExpired { .. })
            ),
            "seed {seed:#x}: the check reports the expiry"
        );
        Ok(())
    })
}

#[test]
/// An expired proof asked for something it never covered: the same-clock
/// check reports the expiry first, the cross-host check the shortfall first.
fn sim_expiry_and_coverage_are_reported_in_their_documented_order() -> Outcome {
    sweep(|seed, stream, scenario| {
        let Some(&first) = scenario.granted.first() else {
            return Ok(());
        };
        let outside: Vec<Cap> = POOL
            .iter()
            .copied()
            .filter(|name| *name != first)
            .map(Cap::new)
            .collect();
        let lifetime = ttl(stream)?;
        let clock = Clock::virtual_at(Duration::ZERO);
        let proof = GrantSet::empty()
            .grant_expiring(Cap::new(first), lifetime)
            .issue_at(&[Cap::new(first)], &clock)
            .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        clock.advance(lifetime).map_err(|error| error.to_string())?;
        assert!(
            matches!(
                proof.check(&outside),
                Err(BotError::CredentialExpired { .. })
            ),
            "seed {seed:#x}: the same-clock check reports expiry first"
        );
        assert!(
            matches!(
                proof.check_remote(&outside, &clock),
                Err(BotError::CapabilityDenied { .. })
            ),
            "seed {seed:#x}: the cross-host check reports the shortfall first"
        );
        Ok(())
    })
}

#[test]
/// A borrowed and an owned spelling of a name are one capability.
fn sim_borrowed_and_owned_spellings_are_one_capability() -> Outcome {
    sweep(|seed, _, scenario| {
        for name in &scenario.required {
            let borrowed = Cap::new(*name);
            let owned = Cap::new((*name).to_owned());
            assert_eq!(borrowed, owned, "seed {seed:#x}: {name} is one capability");
            assert!(
                GrantSet::empty().grant(borrowed.clone()).grants(&owned)
                    && GrantSet::empty().grant(owned).grants(&borrowed),
                "seed {seed:#x}: either spelling's grant covers the other"
            );
        }
        Ok(())
    })
}

#[test]
/// Exactly 401, 403 and 404 refuse the credential.
fn sim_exactly_three_statuses_refuse_the_credential() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..4_096 {
            let status = u16::try_from(stream.below(1_000)?).map_err(|error| error.to_string())?;
            assert_eq!(
                is_credential_status(status),
                [401, 403, 404].contains(&status),
                "seed {seed:#x}: status {status}"
            );
        }
    }
    Ok(())
}

/// The replay receipt of every admission verdict drawn under `seed`.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..SCENARIOS {
        let scenario = Scenario::draw(&mut stream)?;
        let verdict = denied_names(scenario.grants().admit(&scenario.required_caps()))?;
        trace = fold(trace, format!("{verdict:?};").as_bytes());
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
