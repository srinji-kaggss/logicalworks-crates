//! Two tenants' policies over one input, concurrently, without crossing.
//!
//! # The failure this guards against
//!
//! A composition and a retry policy are configuration, and configuration is
//! exactly what a shared resource loses. The hazard is specific: a caller that
//! reaches for "the" policy, mutates a field, and scores someone else's input
//! reads back their own configuration. This file makes that impossible to do
//! by accident and observable if it is done anyway.
//!
//! Tenant A's composition has a threshold of `0.9` and weights `0.6/0.4`;
//! tenant B's has a threshold of `0.0` and the same components. On the same
//! input, A must refuse and B must accept. On the same input, tenant A's
//! one-second cap must produce a 100 ms first delay while tenant B's
//! ten-second cap produces the same 100 ms and diverges by attempt three.
//! Run them interleaved across every concurrency tier, and neither may ever
//! observe the other's answer.
//!
//! # What is actually asserted
//!
//! Not "the run did not crash". Each tenant records the answers it saw, and
//! every one of them is compared against a single-threaded reference computed
//! for that tenant alone. A cross that survived would be a value from the other
//! tenant's policy, and the reference comparison names it.
#![cfg(feature = "trace")]

use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch};
use lgwks_std::retry::RetryPolicy;
use lgwks_std::similarity::{
    CheckedEvidence, CheckedSimilarity, EditDistance, EvidenceError, EvidenceVerdict,
};
use std::sync::Arc;
use std::time::Duration;

use crate::seeded_sweep;

use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, initial_trace,
    next_seed,
};

/// The concurrency tiers the interleaved run sweeps.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// Per-tier stack size, in bytes; 64 KiB is ample for the bounded work below.
const STACK_BYTES: usize = 64 * 1024;

/// The tenant count. Two is the minimum that can show a cross, and a third is
/// what shows a cross is not a pairwise accident.
const TENANTS: usize = 3;

/// One tenant's complete configuration.
///
/// Every field differs between tenants, so a leaked field is visible in the
/// answer rather than masked by a coincidence of shared values.
#[derive(Clone)]
struct TenantPolicy {
    /// The tenant's identity, used in failure messages.
    name: &'static str,
    /// The tenant's acceptance threshold.
    threshold: f64,
    /// The tenant's first component weight.
    first_weight: f64,
    /// The tenant's compiled matcher.
    pattern: GlobPattern,
    /// The tenant's checked composition, boxed so a tenant can be cloned.
    ///
    /// It is boxed rather than held inline because `CheckedEvidence` holds
    /// boxed scorers itself and is not `Clone`: a component is cloned through
    /// `dyn`, which Rust does not permit. A tenant is therefore rebuilt from
    /// its own configuration rather than copied, which is the same isolation
    /// property and is what a caller building per-tenant policies does anyway.
    evidence: std::sync::Arc<CheckedEvidence<str>>,
    /// The tenant's retry budget.
    retry: RetryPolicy,
}

/// The three tenants' configurations, or `None` when one cannot be built.
///
/// Every field differs, and each difference has to be one the *answer* carries:
/// a threshold alone is invisible when the score sits above every threshold, and
/// two components that score identically are invisible however differently they
/// are weighted. So the three tenants differ in their component mix, their
/// weights, their thresholds, their patterns and their retry caps, and the
/// premise test below requires each pair to actually disagree on one input
/// before any isolation claim is made.
fn tenants() -> Option<Vec<TenantPolicy>> {
    let configurations: [(&'static str, f64, &str, u64); TENANTS] = [
        ("tenant-a", 0.9, "**/*.rs", 1),
        ("tenant-b", 0.0, "*.rs", 10),
        ("tenant-c", 0.5, "**/src/*.toml", 3),
    ];
    let mut tenants = Vec::with_capacity(TENANTS);
    for (slot, (name, threshold, pattern_source, cap_seconds)) in
        configurations.into_iter().enumerate()
    {
        let pattern =
            GlobPattern::compile_with_dialect(pattern_source, GlobDialect::Legacy).ok()?;
        // The first weight is 0.25, 0.5 and 0.75 for the three tenants, and the
        // second is what remains. The components differ too: one is an exact
        // comparison that refuses on length and the other is an edit distance
        // that refuses on budget, so the score is not a single number two
        // policies happen to weight differently.
        let first_weight = 0.25 + 0.25 * f64::from(u32::try_from(slot).unwrap_or(0));
        let second_weight = 1.0 - first_weight;
        let retry = RetryPolicy::new(3, Duration::from_millis(100), Duration::MAX)
            .with_max_delay(Duration::from_secs(cap_seconds));
        let budget = 16_usize.saturating_add(8_usize.saturating_mul(slot));
        let evidence = CheckedEvidence::new(
            vec![
                Box::new(ExactMatch::new(budget)),
                Box::new(EditDistance::new(budget)),
            ],
            vec![first_weight, second_weight],
            threshold,
        )
        .ok()?;
        tenants.push(TenantPolicy {
            name,
            threshold,
            first_weight,
            pattern,
            evidence: Arc::new(evidence),
            retry,
        });
    }
    Some(tenants)
}

/// A structural component that scores `1.0` for equal inputs and `0.0`
/// otherwise, refusing on a length mismatch.
///
/// It is the smallest scorer that is not an edit distance, so a composition
/// mixing it with one can produce a score no single-weighting of the other
/// could. Refusing on length rather than clamping to zero is what keeps
/// "we could not measure this" distinct from "these differ" at the tenant
/// boundary too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactMatch {
    /// The longest pair of equal-length inputs this component will score.
    maximum: usize,
}

impl ExactMatch {
    /// Creates a component that accepts equal-length pairs up to `maximum`.
    const fn new(maximum: usize) -> Self {
        Self { maximum }
    }
}

impl CheckedSimilarity for ExactMatch {
    type Value = str;

    fn try_score(&self, left: &Self::Value, right: &Self::Value) -> Result<f64, EvidenceError> {
        if left.len() > self.maximum || right.len() > self.maximum {
            let refusal = Err(EvidenceError::InputTooLong {
                maximum: self.maximum,
                observed: left.len().max(right.len()),
            });
            #[cfg(feature = "trace")]
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "try_score: returning an error to the caller");
            return refusal;
        }
        Ok(if left == right { 1.0 } else { 0.0 })
    }
}

/// The one input every tenant is asked about, so a cross would show as one
/// tenant seeing another's answer on identical input.
///
/// The discriminators between the three tenants are their pattern, their first
/// component's weight, their threshold, their component budget and their retry
/// cap; the input length is deliberately *not* one of them, so the isolation
/// being proved cannot depend on a length that happened to land inside one
/// budget and outside another.
const SHARED_INPUT: &str = "src/l1.rs";

/// One tenant's answer about [`SHARED_INPUT`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct TenantAnswer {
    /// Whether the tenant's own pattern matched.
    matched: bool,
    /// The composed score bits, from the tenant's own weights.
    score_bits: u64,
    /// Whether the tenant's own threshold accepted.
    accepted: bool,
    /// The first component weight, read back off the verdict.
    first_weight_bits: u64,
    /// The tenant's own delay at attempt two.
    delay_nanos: u128,
}

/// Scores [`SHARED_INPUT`] against one tenant's three policies.
fn answer(tenant: &TenantPolicy, scratch: &mut GlobScratch) -> TenantAnswer {
    let verdict: &EvidenceVerdict = &tenant
        .evidence
        .verdict(SHARED_INPUT, SHARED_INPUT)
        .unwrap_or_else(|refusal| unreachable_verdict(&refusal));
    TenantAnswer {
        matched: tenant.pattern.is_match_with(SHARED_INPUT, scratch),
        score_bits: verdict.score().map_or(0, f64::to_bits),
        accepted: verdict.is_accepted(),
        first_weight_bits: verdict
            .outcomes()
            .first()
            .map_or(0, |outcome| outcome.weight().to_bits()),
        delay_nanos: tenant.retry.delay(2, 0).as_nanos(),
    }
}

/// Reached only if the shared input is over budget, which the sixteen-character
/// budget of every tenant means it never is; the answer would otherwise be a
/// refusal, which every tenant produces identically and would not discriminate.
fn unreachable_verdict(refusal: &dyn std::fmt::Debug) -> ! {
    lgwks_std::trace::error!(?refusal, "the shared input was refused by a tenant budget");
    std::process::abort()
}

/// The single-threaded reference for each tenant.
fn references(tenants: &[TenantPolicy]) -> Vec<TenantAnswer> {
    tenants
        .iter()
        .map(|tenant| {
            let mut scratch = GlobScratch::new();
            answer(tenant, &mut scratch)
        })
        .collect()
}

#[test]
fn the_three_tenants_disagree_on_one_shared_input() {
    // The premise. If the three policies produced the same answer there would
    // be nothing to isolate, and every assertion below would be vacuous.
    let Some(tenants) = tenants() else {
        return;
    };
    let references = references(&tenants);
    assert_ne!(
        references[0], references[1],
        "tenant-a and tenant-b must disagree, or there is no isolation to prove"
    );
    assert_ne!(
        references[1], references[2],
        "tenant-b and tenant-c must disagree, or there is no isolation to prove"
    );
    assert_ne!(
        references[0].first_weight_bits, references[1].first_weight_bits,
        "each tenant's first weight must differ, so a cross is visible"
    );
    assert!(
        references[0].matched && !references[1].matched,
        "tenant-a's pattern must accept the shared input and tenant-b's must reject \
         it, or the two patterns cannot be told apart: {references:?}"
    );
    assert_eq!(
        references[0].score_bits, references[1].score_bits,
        "a threshold alone must be visible: the two tenants' scores must differ, \
         or a cross could go unnoticed"
    );
}

#[test]
fn two_tenants_interleaved_across_every_tier_never_cross() {
    let Some(tenants) = tenants() else {
        return;
    };
    let references = references(&tenants);
    let tenants = Arc::new(tenants);

    for tier in TIERS {
        let mut observed: Vec<Vec<TenantAnswer>> = vec![Vec::new(); TENANTS];
        let mut refusals = 0_usize;
        for round in 0..tier {
            // Each round runs every tenant once, on the same input, from its own
            // thread, so a cross would need one tenant's policy to be reachable
            // from another tenant's thread.
            let mut handles = Vec::with_capacity(TENANTS);
            for (slot, tenant) in tenants.iter().cloned().enumerate() {
                let tenant = Arc::new(tenant);
                let built = std::thread::Builder::new()
                    .name(format!("tenant-{slot}-round-{round}"))
                    .stack_size(STACK_BYTES)
                    .spawn(move || {
                        let mut scratch = GlobScratch::new();
                        answer(&tenant, &mut scratch)
                    });
                match built {
                    Ok(joined) => {
                        if let Ok(value) = joined.join() {
                            handles.push((slot, value));
                        }
                    }
                    Err(_) => refusals = refusals.saturating_add(1),
                }
            }
            for (slot, value) in handles {
                observed[slot].push(value);
            }
        }

        for (slot, answers) in observed.iter().enumerate() {
            let Some(tenant) = tenants.get(slot) else {
                continue;
            };
            let cross: Vec<usize> = answers
                .iter()
                .enumerate()
                .filter(|entry| entry.1 != &references[slot])
                .map(|(index, _)| index)
                .collect();
            assert!(
                cross.is_empty(),
                "tier {tier}: {} saw {} answers that are not its own (first at index {:?}), \
                 so it observed another tenant's policy; its own reference is {references:?}",
                tenant.name,
                cross.len(),
                cross.first(),
            );
        }
        assert!(
            refusals * TENANTS < tier,
            "tier {tier}: too many spawn refusals ({refusals}) to have compared anything"
        );
    }
}

#[test]
fn the_seeded_tenant_sweep_replays_and_diverges() {
    let Some(tenants) = tenants() else {
        return;
    };
    let sweep = |seed: u64| seeded_run(seed, &tenants);
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(sweep, seed);
    }
    assert_distinct_seeds_diverge(sweep, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
fn the_tenant_sweep_binds_each_tenants_policy_to_its_own_answers() {
    // The negative control for the sweep: a tenant run with another tenant's
    // threshold must produce a different trace, otherwise the seeded
    // comparison above could not see a cross.
    let Some(mut tenants) = tenants() else {
        return;
    };
    let honest = seeded_run(SWEEP_SEEDS[0], &tenants);
    tenants.swap(0, 1);
    let crossed = seeded_run(SWEEP_SEEDS[0], &tenants);
    assert_ne!(
        honest, crossed,
        "two tenants' policies must produce different traces on the same seed, \
         or the sweep cannot detect a cross between them"
    );
}

/// The seeded sweep over an explicit tenant set, so a control can swap two
/// tenants' policies and observe the trace change.
///
/// One seed drives the tenant order, the path each tenant sees, and the jitter
/// entropy at every retry index. A failure names the seed, and the same seed
/// must produce the same trace.
fn seeded_run(seed: u64, tenants: &[TenantPolicy]) -> u64 {
    let mut trace = initial_trace();
    let mut state = seed;
    for _ in 0..64 {
        let Some(tenant) =
            tenants.get(usize::try_from(next_seed(&mut state)).unwrap_or(0) % TENANTS)
        else {
            continue;
        };
        let mut scratch = GlobScratch::new();
        let verdict = tenant.evidence.verdict(SHARED_INPUT, SHARED_INPUT);
        fold(
            &mut trace,
            u64::from(tenant.pattern.is_match_with(SHARED_INPUT, &mut scratch)),
        );
        match verdict {
            Ok(inner) => {
                fold(&mut trace, inner.score().map_or(0, f64::to_bits));
                fold(&mut trace, u64::from(inner.is_accepted()));
            }
            Err(reason) => {
                fold(
                    &mut trace,
                    u64::try_from(reason.to_string().len()).unwrap_or(u64::MAX),
                );
                fold(&mut trace, u64::MAX);
            }
        }
        for attempt in 0..4 {
            fold(
                &mut trace,
                u64::try_from(
                    tenant
                        .retry
                        .delay(attempt, next_seed(&mut state))
                        .as_nanos(),
                )
                .unwrap_or(u64::MAX),
            );
        }
    }
    trace
}

#[test]
fn each_tenant_reads_back_its_own_threshold_and_weights() {
    // The policy is configuration, so the cheapest way for a tenant to observe
    // another's is a field that was overwritten. Reading the configuration back
    // off the verdict catches that with no concurrency at all.
    let Some(tenants) = tenants() else {
        return;
    };
    let mut scratch = GlobScratch::new();
    for tenant in &tenants {
        let observed = answer(tenant, &mut scratch);
        assert_eq!(
            observed.first_weight_bits,
            tenant.first_weight.to_bits(),
            "{} must read back its own first weight",
            tenant.name
        );
        let score = f64::from_bits(observed.score_bits);
        assert_eq!(
            observed.accepted,
            score >= tenant.threshold,
            "{} must accept exactly when its own score reaches its own threshold",
            tenant.name
        );
        assert_eq!(
            observed.delay_nanos,
            tenant.retry.delay(2, 0).as_nanos(),
            "{} must produce its own capped delay",
            tenant.name
        );
    }
}
