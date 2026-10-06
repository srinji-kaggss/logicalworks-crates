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

use crate::deps_sim;
use crate::sim::declared_scope;

use deps_sim::{REGISTRY, Rng, TestResult, alias_line, code_for, coin, edge, register};

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

/// The verdict for one identity draw, through the public API the audit uses.
///
/// The approval and the observed edge are built here so the family loop states
/// one fallible step: a loop that built both itself was a chain of four `?`
/// where a reader could not see which draw had refused.
fn identity_verdict(
    approved: &str,
    observed: &str,
    alias: Option<&str>,
) -> Result<u8, Box<dyn Error>> {
    let approval = register(approved, "registry", &alias_line(alias))?;
    let observed_edge = edge(observed, Some(REGISTRY), &[], true, false, None, None)?;
    Ok(code_for(&approval, observed_edge))
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
        let approved = rng.pick_named("identity names", &names)?;
        let observed = rng.pick_named("identity names", &names)?;
        let approved = *approved;
        let observed = *observed;
        let alias: Option<&str> = if coin(&mut rng) {
            names.iter().copied().find(|name| *name != approved)
        } else {
            None
        };
        let code = identity_verdict(approved, observed, alias)?;
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
        let allowed: Vec<&str> = universe
            .iter()
            .copied()
            .filter(|_| coin(&mut rng))
            .collect();
        let enabled: Vec<&str> = universe
            .iter()
            .copied()
            .filter(|_| coin(&mut rng))
            .collect();
        let constrained = !allowed.is_empty();
        let policy = if constrained {
            format!("features = \"{}\"\n", allowed.join(","))
        } else {
            String::new()
        };
        let approval = register("engine", "registry", &policy)?;
        let observed = edge("engine", Some(REGISTRY), &enabled, true, false, None, None)?;
        let code = code_for(&approval, observed);
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

/// One draw of the dimension family: toggles each authored bit independently
/// and records the verdict against what the policy says it should be.
fn dimension_draw(
    seed: u64,
    rng: &mut Rng,
    hasher: &mut DefaultHasher,
    tally: &mut Tally,
) -> Result<(), Box<dyn Error>> {
    let targets = [None, Some("cfg(unix)")];
    // The edge's authored bits and the policy's authored bits are chosen
    // independently, so a mismatch is a real mismatch.
    let def_edge = coin(rng);
    let def_policy = coin(rng);
    let def_value = coin(rng);
    let opt_edge = coin(rng);
    let opt_policy = coin(rng);
    let opt_value = coin(rng);
    let target_edge = *rng.pick_named("target scopes", &targets)?;
    let target_value = *rng.pick_named("target scopes", &targets)?;
    let target_policy = coin(rng);
    let mut policy = String::new();
    if def_policy {
        writeln!(policy, "uses_default_features = \"{def_value}\"")?;
    }
    if opt_policy {
        writeln!(policy, "optional = \"{opt_value}\"")?;
    }
    if target_policy {
        writeln!(policy, "target = \"{}\"", declared_scope(target_value))?;
    }
    let approval = register("engine", "registry", &policy)?;
    let observed = edge(
        "engine",
        Some(REGISTRY),
        &[],
        def_edge,
        opt_edge,
        target_edge,
        None,
    )?;
    let code = code_for(&approval, observed);
    let expected = (!def_policy || def_edge == def_value)
        && (!opt_policy || opt_edge == opt_value)
        && (!target_policy || declared_scope(target_edge) == declared_scope(target_value));
    tally.record(
        code,
        expected,
        &format!(
            "seed {seed}: def {def_edge}/{def_policy}:{def_value} opt {opt_edge}/{opt_policy}:{opt_value} target {target_edge:?}/{target_policy}:{target_value:?}"
        ),
        hasher,
    );
    Ok(())
}

/// Runs one dimension family for `seed`: default-features, optionality and
/// target each toggled independently, with the policy either authored or
/// grandfathered.
fn dimension_family(seed: u64) -> Result<(u64, Tally), Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut hasher = DefaultHasher::new();
    let mut tally = Tally::default();
    for _ in 0..256 {
        dimension_draw(seed, &mut rng, &mut hasher, &mut tally)?;
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
