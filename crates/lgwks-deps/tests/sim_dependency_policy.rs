//! Deterministic simulation of admitted-capability and identity policy
//! (issue #158 A2).
//!
//! One seed drives a family's sequence of approvals and observed edges; the
//! verdicts fold into a trace hash, the same seed must reproduce the same hash,
//! and a failure prints its seed. The invariants under test: an edge is admitted
//! exactly when it matches the approved identity and satisfies every authored
//! policy dimension; an unconstrained (grandfathered) dimension never refuses;
//! and widening a dimension never turns a refusal into an admission.

use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

#[path = "support/sim.rs"]
mod sim;

use sim::Rng;

type TestResult = Result<(), Box<dyn Error>>;

/// A coin from a high bit.
///
/// The LCG's low bit alternates every step, so a low-bit coin would make every
/// derived set phase-locked and a sweep would never exercise a mismatch.
fn coin(rng: &mut Rng) -> bool {
    (rng.next_u64() >> 40) & 1 == 1
}

/// Counts of each verdict seen while folding a family into a trace hash.
#[derive(Default)]
struct Tally {
    /// Edges admitted.
    admitted: usize,
    /// Edges refused.
    refused: usize,
}

impl Tally {
    /// Assert the observed verdict against the model's expectation, then fold it
    /// into the tally and the trace hash.
    fn record(&mut self, code: u8, expected: bool, context: &str, hasher: &mut DefaultHasher) {
        assert_eq!(code, u8::from(!expected), "{context}");
        if code == 0 {
            self.admitted = self.admitted.saturating_add(1);
        } else {
            self.refused = self.refused.saturating_add(1);
        }
        code.hash(hasher);
    }
}

/// One edge with every authored dimension spelled out.
fn edge(
    package: &str,
    features: &[&str],
    uses_default_features: bool,
    optional: bool,
    target: Option<&str>,
    rename: Option<&str>,
) -> Result<DirectEdge, Box<dyn Error>> {
    let features = features
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(",");
    let target = target.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let rename = rename.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"{package}","source":"registry+https://github.com/rust-lang/crates.io-index","req":"1.0","kind":null,"rename":{rename},"optional":{optional},"uses_default_features":{uses_default_features},"features":[{features}],"target":{target},"path":null}}]}}],"workspace_members":["app"]}}"#
    );
    let mut edges = metadata::parse(&document)?;
    edges
        .pop()
        .ok_or_else(|| "the fixture edge must parse".into())
}

/// A register with one approval carrying the given policy lines.
fn register(package: &str, aliases: Option<&str>, policy: &str) -> Result<Contract, Box<dyn Error>> {
    let aliases = aliases.map_or(String::new(), |value| format!("aliases = \"{value}\"\n"));
    let text = format!(
        concat!(
            "[policy]\nschema = 2\nenforce = true\n\n",
            "[[approved]]\n",
            "crate = \"{package}\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.core\"\nsource = \"registry\"\n{aliases}{policy}",
            "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests/sim_dependency_policy.rs\"\n",
        ),
        package = package,
        aliases = aliases,
        policy = policy,
    );
    Ok(Contract::parse(&text)?)
}

/// 0 admitted, 1 any refusal.
fn verdict(refusals: &[Refusal]) -> u8 {
    u8::from(!refusals.is_empty())
}

/// Runs one identity family for `seed`: approved spelling vs observed spelling.
fn identity_family(seed: u64) -> Result<(u64, Tally), Box<dyn Error>> {
    // The observed name is one of two fold-alikes; the approved name is one of
    // the two, and may carry the other as an explicit alias.
    let names = ["engine-core", "engine_core"];
    let mut rng = Rng::new(seed);
    let mut hasher = DefaultHasher::new();
    let mut tally = Tally::default();
    for _ in 0..256 {
        let approved = *rng.pick(&names);
        let observed = *rng.pick(&names);
        let alias: Option<&str> = if coin(&mut rng) {
            names.iter().copied().find(|name| *name != approved)
        } else {
            None
        };
        let register = register(approved, alias, "")?;
        let edge = edge(observed, &[], true, false, None, None)?;
        let code = verdict(&audit_direct(&[edge], &register));
        let admits = approved == observed || alias == Some(observed);
        tally.record(
            code,
            admits,
            &format!("seed {seed}: approved {approved} observed {observed} alias {alias:?}"),
            &mut hasher,
        );
    }
    Ok((hasher.finish(), tally))
}

/// Runs one feature-policy family for `seed`: allowed set vs enabled set.
fn feature_family(seed: u64) -> Result<(u64, Tally), Box<dyn Error>> {
    let universe = ["a", "b", "c", "d"];
    let mut rng = Rng::new(seed);
    let mut hasher = DefaultHasher::new();
    let mut tally = Tally::default();
    for _ in 0..256 {
        let allowed: Vec<&str> = universe.iter().copied().filter(|_| coin(&mut rng)).collect();
        let enabled: Vec<&str> = universe.iter().copied().filter(|_| coin(&mut rng)).collect();
        let constrained = !allowed.is_empty();
        let policy = if constrained {
            format!("features = \"{}\"\n", allowed.join(","))
        } else {
            String::new()
        };
        let register = register("engine", None, &policy)?;
        let edge = edge("engine", &enabled, true, false, None, None)?;
        let code = verdict(&audit_direct(&[edge], &register));
        let expected = !constrained || enabled.iter().all(|feature| allowed.contains(feature));
        tally.record(
            code,
            expected,
            &format!("seed {seed}: allowed {allowed:?} enabled {enabled:?}"),
            &mut hasher,
        );
    }
    Ok((hasher.finish(), tally))
}

/// Runs one dimension family for `seed`: default-features, optionality and
/// target each toggled independently, with the policy either authored or
/// grandfathered.
fn dimension_family(seed: u64) -> Result<(u64, Tally), Box<dyn Error>> {
    let targets = [None, Some("cfg(unix)")];
    let mut rng = Rng::new(seed);
    let mut hasher = DefaultHasher::new();
    let mut tally = Tally::default();
    for _ in 0..256 {
        // The edge's authored bits and the policy's authored bits are chosen
        // independently, so a mismatch is a real mismatch.
        let def_edge = coin(&mut rng);
        let def_policy = coin(&mut rng);
        let def_value = coin(&mut rng);
        let opt_edge = coin(&mut rng);
        let opt_policy = coin(&mut rng);
        let opt_value = coin(&mut rng);
        let target_edge = *rng.pick(&targets);
        let target_value = *rng.pick(&targets);
        let target_policy = coin(&mut rng);
        let mut policy = String::new();
        if def_policy {
            writeln!(policy, "uses_default_features = \"{def_value}\"")?;
        }
        if opt_policy {
            writeln!(policy, "optional = \"{opt_value}\"")?;
        }
        if target_policy {
            writeln!(policy, "target = \"{}\"", target_value.unwrap_or(""))?;
        }
        let register = register("engine", None, &policy)?;
        let edge = edge("engine", &[], def_edge, opt_edge, target_edge, None)?;
        let code = verdict(&audit_direct(&[edge], &register));
        let expected = (!def_policy || def_edge == def_value)
            && (!opt_policy || opt_edge == opt_value)
            && (!target_policy || target_edge.unwrap_or("") == target_value.unwrap_or(""));
        tally.record(
            code,
            expected,
            &format!(
                "seed {seed}: def {def_edge}/{def_policy}:{def_value} opt {opt_edge}/{opt_policy}:{opt_value} target {target_edge:?}/{target_policy}:{target_value:?}"
            ),
            &mut hasher,
        );
    }
    Ok((hasher.finish(), tally))
}

/// A seeded family: `(trace hash, tally)` for one seed.
type Family = fn(u64) -> Result<(u64, Tally), Box<dyn Error>>;

/// Fold a family over 64 seeds and assert determinism plus both verdicts seen.
fn sweep(name: &str, family: Family) -> TestResult {
    let mut total = Tally::default();
    for seed in 0..64_u64 {
        let (first, tally) = family(seed)?;
        let (second, _) = family(seed)?;
        assert_eq!(first, second, "{name}: seed {seed} gave two trace hashes");
        total.admitted = total.admitted.saturating_add(tally.admitted);
        total.refused = total.refused.saturating_add(tally.refused);
    }
    assert!(
        total.admitted > 0 && total.refused > 0,
        "{name}: the sweep must exercise both verdicts ({} admitted, {} refused)",
        total.admitted,
        total.refused
    );
    Ok(())
}

#[test]
fn identity_admission_is_deterministic() -> TestResult {
    sweep("identity", identity_family)
}

#[test]
fn feature_policy_is_deterministic() -> TestResult {
    sweep("feature", feature_family)
}

#[test]
fn dimension_policy_is_deterministic() -> TestResult {
    sweep("dimension", dimension_family)
}
