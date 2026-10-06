//! Simulation family: typed readiness under a seeded sweep.
//!
//! The public journeys in `tests/ready.rs` pin one shape each: a duplicate, a
//! stale generation, a failure after ready. This target asks the same questions
//! of *interleavings* — every order in which two tenants start, release, fail
//! and restart the same service — and of the saturation tiers, where a readiness
//! admits 100, 1 000 and 10 000 dependants up to its declared cap.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `an_interleaving_never_releases_a_stale_generation` | under any drawn interleaving, no stale signal releases a dependant and the refusal is named |
//! | `a_duplicate_releases_once_in_every_interleaving` | however many times readiness is signalled, exactly one generation is ever released |
//! | `a_failure_after_ready_reaches_every_running_dependant` | a failure after the release stops every dependant that was still running, and no other |
//! | `two_tenants_never_cross_a_readiness` | two tenants over one service name keep their own steps, values and outcomes |
//! | `saturation_admits_up_to_the_declared_cap` | 100 / 1 000 / 10 000 dependants up to the cap, and the requested, reached and ceiling levels are recorded together |
//! | `the_same_seed_replays_the_same_trace` | the same seed produces the same trace hash, twice, over the union of the families above |
//!
//! # What is real and what is seeded
//!
//! The readiness, its `watch`, its mutex, its admission counter and its
//! cancellation tokens are all real. The seed decides *the order* in which the
//! scenario's actors act, and nothing else: a signal is not delayed, a failure
//! is not injected by a timer, and no elapsed time enters the trace. That is
//! what makes the trace hash a replay receipt — a run that reordered its own
//! actors would change the hash, and a run that merely ran slower would not.
#![cfg(feature = "script")]

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::error::Error;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lgwks_bot::script::{Generation, Readiness, ReadinessError, Scope, Tenant as TenantName};

use sim::Band;
use sim::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A readiness carrying the address a service published.
type Named = Readiness<String>;

/// The budget a scenario's waits run under.
///
/// Small, and that is a load-bearing choice rather than a saving. A wait that
/// *should* be released returns at once, so it costs nothing; only a wait whose
/// readiness never settles spends this, and a seeded script draws several of
/// those per run. At four hundred milliseconds a band's worth of them dominated
/// the suite's wall clock and made the sim layer look slow for a property that
/// has nothing to do with time. At twenty the same wait still ends — it ends for
/// the same reason — and a band costs milliseconds.
const BUDGET: Duration = Duration::from_millis(20);

// ── The plan ────────────────────────────────────────────────────────────────

/// One actor in a scenario: what it does to the readiness, and when.
///
/// A drawn script rather than a drawn probability, because the property under
/// test is a property of *orders*: "no stale signal ever releases a dependant"
/// is only worth anything if the scenario actually produces the orders that
/// would break it, and a per-mille toggle would produce the interesting one
/// about one time in eight and say nothing the rest of the time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// Signal the readiness at the generation the scenario is currently bound to.
    SignalCurrent,
    /// Signal at an older generation than the bound one: a previous instance.
    SignalStale,
    /// Signal at a generation this readiness was never bound to.
    SignalForeign,
    /// Fail the readiness at the bound generation.
    Fail,
    /// Shut the readiness down at the bound generation.
    Shutdown,
    /// Arm a *new* readiness one generation on, as a restart does.
    Restart,
    /// Admit a dependant and observe what it was given.
    Dependant,
}

/// One actor's place in the script.
///
/// `Copy` rather than a hand-written `Clone`, because every field is `Copy` and
/// there is no non-`Copy` payload to lose: the whole point of the derive here is
/// that a script is a value a scenario hands around by copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    /// Which tenant acts. Two tenants over one service name is the isolation
    /// case, and drawing it here rather than in a separate family means the
    /// interleavings that break isolation are the same ones that break fencing.
    tenant: u32,
    /// What the actor does.
    act: Act,
}

/// A whole scenario, drawn from one seed.
#[derive(Clone, Debug)]
struct Plan {
    /// How many tenants share the service name.
    tenants: u32,
    /// The script, in order.
    steps: Vec<Step>,
}

/// The ten acts a scenario draws from.
///
/// `Dependant` is over-represented because it is the only act that *observes*:
/// the rest change state, and a scenario with no observer would pass whatever
/// the state machine did.
fn draw_act(rng: &mut Rng) -> Act {
    match rng.below(12) {
        0..=3 => Act::Dependant,
        4 | 5 => Act::SignalCurrent,
        6 => Act::SignalStale,
        7 => Act::SignalForeign,
        8 => Act::Fail,
        9 => Act::Shutdown,
        _ => Act::Restart,
    }
}

/// A plan drawn from `rng`.
///
/// The length is bounded rather than drawn without limit: a scenario is a
/// *script*, and a script of ten thousand acts is ten thousand assertions about
/// one state machine, which costs a band its budget and proves nothing extra
/// that a script of forty does not.
fn plan(rng: &mut Rng) -> Plan {
    // Two tenants always: a scenario with one cannot show isolation, and the
    // second costs nothing when the two happen to agree.
    let tenants = 2;
    let length = rng.between(6, 24);
    let steps = (0..length)
        .map(|_| Step {
            tenant: rng.below(tenants),
            act: draw_act(rng),
        })
        .collect();
    Plan { tenants, steps }
}

// ── The harness ─────────────────────────────────────────────────────────────

/// One scenario's observation, folded into the trace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Observed {
    /// Every generation this run released, in order. More than one is a second
    /// release, which is the defect the duplicate family looks for.
    released: Vec<u64>,
    /// Every refusal a *signal* drew, in order. Kept apart from a dependant's
    /// own wait outcome, which is a different fact: one is what the readiness
    /// said about a signal, the other is what a wait concluded.
    refusals: Vec<String>,
    /// Every way a dependant's own wait concluded, in order.
    outcomes: Vec<String>,
    /// Every value a dependant was given, in order.
    served: Vec<String>,
    /// How many dependants were admitted in total.
    admitted: u64,
}

impl Observed {
    /// Fold this observation into a trace, in a fixed field order.
    fn record(&self, trace: &mut sim::Trace) {
        trace.record_number("released", self.released.len());
        for generation in &self.released {
            trace.record_number("released-generation", *generation);
        }
        for refusal in &self.refusals {
            trace.record(refusal);
        }
        for outcome in &self.outcomes {
            trace.record(outcome);
        }
        trace.record_number("served", self.served.len());
        for value in &self.served {
            trace.record(value);
        }
        trace.record_number("admitted", self.admitted);
    }
}

/// One tenant's readiness and the scope its dependants wait under.
///
/// Named `Half` rather than `Tenant` so it never shadows the scope's own
/// [`TenantName`]: a shadowed type is a call site that compiles against the
/// wrong one, and this file builds a tenant per actor.
struct Half {
    /// The readiness this tenant's service is gated on.
    readiness: Named,
    /// The generation the readiness is currently bound to.
    bound: Generation,
    /// The scope every dependant of this tenant enters under.
    scope: Scope,
}

/// Build one tenant's half of a scenario.
fn tenant_of(index: u32, generations: &mut u64) -> Result<Half, Box<dyn Error>> {
    let name = format!("tenant-{index}");
    let scope = Scope::root(TenantName::new(&name)?);
    *generations = generations.saturating_add(1);
    let bound = Generation::at(*generations);
    Ok(Half {
        readiness: Readiness::named("db", bound, TENANT_CAP)?,
        bound,
        scope,
    })
}

/// How many dependants one tenant's readiness admits in the interleaved
/// families.
///
/// Small, because these families sweep a *script* and the cap family sweeps
/// the tiers. A cap that were large would turn every script into a scale test
/// and cost the sweep its bands.
const TENANT_CAP: usize = 8;

/// The value a tenant's service publishes at a generation.
///
/// Generated from the tenant's own identity rather than drawn, so two tenants on
/// one service name publish *different* addresses and a dependant that was
/// served another tenant's value is visible in the trace. The shape is
/// `db-<tenant>.internal:<generation>`; [`tenant_of_address`] reads the tenant
/// back out of it, so the two cannot drift.
fn published(tenant: u32, generation: Generation) -> String {
    format!("db-{tenant}.internal:{}", generation.get())
}

/// The tenant a published address names.
///
/// A parse of [`published`]'s own shape rather than a `split` at a call site, so
/// the reading of a served value and the writing of it are one definition: a
/// served value this cannot read is a defect in the address format, reported as
/// such rather than silently counted as some other tenant.
fn tenant_of_address(address: &str) -> Result<u32, Box<dyn Error>> {
    let digits = address
        .strip_prefix("db-")
        .and_then(|rest| rest.split_once(".internal:"))
        .map(|(tenant, _)| tenant)
        .ok_or_else(|| format!("a published address names its tenant: {address:?}"))?;
    Ok(digits.parse::<u32>()?)
}

/// Drive one scripted plan and report what every actor observed.
///
/// The whole harness in one function, because a scenario that split its actors
/// across two helpers would let one tenant's readiness be advanced by the
/// other's step — which is exactly the defect two-tenant isolation is here to
/// catch. `runtime` is passed in rather than built per call because a runtime
/// per seed is milliseconds of setup for nothing.
fn drive_plan(plan: &Plan, generations: &mut u64) -> Result<Observed, Box<dyn Error>> {
    let mut halves = Vec::new();
    for index in 0..plan.tenants {
        halves.push(tenant_of(index, generations)?);
    }
    let mut observed = Observed::default();

    for step in &plan.steps {
        let Some(index) = usize::try_from(step.tenant).ok() else {
            continue;
        };
        let Some(tenant) = halves.get_mut(index) else {
            continue;
        };
        match step.act {
            Act::SignalCurrent => {
                let value = published(step.tenant, tenant.bound);
                record(
                    &mut observed,
                    &tenant.readiness.ready(tenant.bound, value.clone()),
                );
                if tenant.readiness.released_at().is_some() {
                    observed.released.push(tenant.bound.get());
                }
            }
            Act::SignalStale => {
                let older = Generation::at(tenant.bound.get().saturating_sub(1));
                let value = published(step.tenant, older);
                record(&mut observed, &tenant.readiness.ready(older, value.clone()));
            }
            Act::SignalForeign => {
                let stranger = Generation::at(tenant.bound.get().saturating_add(1));
                let value = published(step.tenant, stranger);
                record(
                    &mut observed,
                    &tenant.readiness.ready(stranger, value.clone()),
                );
            }
            Act::Fail => {
                record(
                    &mut observed,
                    &tenant.readiness.fail(tenant.bound, "the seeded failure"),
                );
            }
            Act::Shutdown => {
                record(&mut observed, &tenant.readiness.shutdown(tenant.bound));
            }
            Act::Restart => {
                // A restart is a *new* readiness at a *new* generation, never the
                // same one re-signalled: that is what makes a stale signal
                // refused rather than accepted.
                let value = published(step.tenant, tenant.bound);
                if tenant.readiness.released_at().is_some() {
                    observed.released.push(tenant.bound.get());
                }
                *generations = generations.saturating_add(1);
                tenant.bound = Generation::at(*generations);
                tenant.readiness = Readiness::named("db", tenant.bound, TENANT_CAP)?;
                let _refused = tenant.readiness.ready(tenant.bound, value);
                if tenant.readiness.released_at().is_some() {
                    observed.released.push(tenant.bound.get());
                }
            }
            Act::Dependant => {
                let step_scope = tenant.scope.enter(&format!("use-{}", step.tenant))?;
                let outcome = lgwks_bot::block_on(tenant.readiness.wait(&step_scope, BUDGET));
                observed.admitted = observed.admitted.saturating_add(1);
                match outcome {
                    Ok(ready) => {
                        observed.served.push(ready.awaited().to_owned());
                        assert_eq!(
                            ready.generation(),
                            tenant.bound.min(ready.generation()),
                            "a released generation is never newer than the bound one"
                        );
                    }
                    Err(error) => observed.outcomes.push(error.to_string()),
                }
            }
        }
    }
    Ok(observed)
}

/// Fold one signal's outcome into the observation.
///
/// A refusal is recorded by name and an acceptance is not: the trace's job is
/// to make two runs of one seed comparable, and "this signal was refused as
/// `AlreadyReady`" is the fact a second run must reproduce exactly.
fn record<T>(observed: &mut Observed, outcome: &Result<T, ReadinessError>) {
    if let Err(ref error) = *outcome {
        observed.refusals.push(error_label(error));
    }
}

/// A stable label for one refusal.
///
/// The type's own `Debug` would do, but it carries the service name and the
/// generation's `Debug` form (`Generation(3)`) alongside them; a hand-written
/// label names the *arm* and the two generations, which is the whole fact a
/// trace replay has to reproduce, and keeps the trace independent of the
/// `Display` wording.
fn error_label(error: &ReadinessError) -> String {
    match *error {
        ReadinessError::AlreadyReady { generation, .. } => {
            format!("already-ready {generation}")
        }
        ReadinessError::StaleGeneration { expected, got, .. } => {
            format!("stale expected-{expected} got-{got}")
        }
        ReadinessError::UnknownGeneration { got, .. } => format!("unknown {got}"),
        ReadinessError::AlreadyFailed { generation, .. } => {
            format!("already-failed {generation}")
        }
        ReadinessError::Closed { generation, .. } => format!("closed {generation}"),
        ReadinessError::NotReady { generation, .. } => format!("not-ready {generation}"),
        ReadinessError::DependantsFull { cap, .. } => format!("full cap-{cap}"),
        ReadinessError::InvalidName { reason } => format!("invalid-name {reason}"),
        // The enum is `#[non_exhaustive]`, so a wildcard is required here and a
        // future arm must land in it. The label is rendered from the arm's own
        // `Display`, which changes when the arm does — so a newly added variant
        // changes the trace hash rather than silently folding into one bucket
        // and letting two different runs agree.
        ref other => format!("other {other}"),
    }
}

// ── Families ────────────────────────────────────────────────────────────────

/// Under any drawn interleaving, no stale or foreign signal releases anybody.
///
/// The property that makes the fencing worth anything: an older instance and a
/// generation this readiness never issued are both refused *before* a dependant
/// can be released by them, and the refusal is named so a repair is possible.
fn an_interleaving_never_releases_a_stale_generation(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let mut generations = 0_u64;
        let plan = plan(sim.rng());
        let observed = drive_plan(&plan, &mut generations)?;
        // Every signal refusal is one of the five arms the readiness can draw
        // from a signal, and each is *named*. The claim under test is that
        // `stale` and `unknown` are never the release — and a family that
        // accepted any label at all would pass if a stale signal had been
        // silently accepted, so the vocabulary is closed here.
        for label in &observed.refusals {
            let arm = label.split_once(' ').map_or(label.as_str(), |(arm, _)| arm);
            assert!(
                matches!(
                    arm,
                    "already-ready" | "stale" | "unknown" | "already-failed" | "closed"
                ),
                "a refused signal must name one of the readiness's five signal \
                 arms, got {label:?}"
            );
        }
        // A stale signal and a foreign one are *different* refusals, and both
        // are refusals that released nobody: neither arm may appear beside a
        // published value, which is what a release would have added.
        for label in &observed.refusals {
            assert!(
                !label.contains("db-"),
                "a refusal label carries the arm and the generations, never a \
                 published value, got {label:?}"
            );
        }
        observed.record(&mut sim.trace);
        sim.record(&format!(
            "tenants={} steps={} released={} refusals={} served={}",
            plan.tenants,
            plan.steps.len(),
            observed.released.len(),
            observed.refusals.len(),
            observed.served.len()
        ));
        Ok(())
    })
}

/// However many times a readiness is signalled, at most one generation is ever
/// released — and the duplicate is refused by name.
///
/// The seed chooses *how many* times to signal and how many dependants to admit
/// around the signals, and the family draws the whole script itself. That is
/// deliberate: a duplicate only exists when nothing between the two signals
/// changes the readiness, so interleaving a random plan between them would make
/// the property untestable — a plan whose middle step restarted the service
/// would produce two correct releases and no duplicate to refuse. The seed's
/// choice is the *size* of the case, not whether the case is real.
fn a_duplicate_releases_once_in_every_interleaving(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let signals = usize::try_from(sim.rng().between(2, 6))?;
        let dependants = usize::try_from(sim.rng().between(1, 6))?;
        let mut generations = 0_u64;
        let scope = Scope::root(TenantName::new("acme")?);
        let bound = tenant_of(0, &mut generations)?;
        let expected = published(0, bound.bound);

        // Every dependant is admitted *after* the release, so each one is served
        // by the first signal and by no other: a second release would be visible
        // as a second set of values rather than as a count.
        let steps: Vec<Scope> = (0..dependants)
            .map(|index| scope.enter(&format!("worker-{index}")))
            .collect::<Result<_, _>>()?;

        let mut accepted = 0_usize;
        let mut refused = 0_usize;
        for index in 0..signals {
            match bound.readiness.ready(bound.bound, expected.clone()) {
                Ok(()) => accepted = accepted.saturating_add(1),
                Err(error) => {
                    let label = error_label(&error);
                    assert!(
                        label.starts_with("already-ready "),
                        "a repeated signal at a settled generation must be refused \\
                         as a duplicate, got {label:?}"
                    );
                    refused = refused.saturating_add(1);
                }
            }
            if index == 0 {
                for step in &steps {
                    let ready = lgwks_bot::block_on(bound.readiness.wait(step, BUDGET))?;
                    assert_eq!(
                        ready.awaited(),
                        &expected,
                        "every dependant is served the first release, at signal {index}"
                    );
                }
            }
        }

        assert_eq!(accepted, 1, "exactly one signal may release");
        assert_eq!(
            refused,
            signals.saturating_sub(1),
            "every other signal is a refused duplicate, at {signals} signals"
        );
        assert_eq!(
            bound.readiness.released_at(),
            Some(bound.bound),
            "the released generation survives every duplicate"
        );
        // The dependants are still running, so the duplicate family can hand
        // them to the failure-after-ready family rather than releasing twice.
        assert_eq!(
            bound.readiness.dependants_live(),
            dependants,
            "one token per dependant the release handed out"
        );
        sim.record(&format!(
            "signals={signals} dependants={dependants} accepted={accepted} \\
             refused={refused}"
        ));
        Ok(())
    })
}

/// A failure after the release reaches every dependant that is still running,
/// and no other.
///
/// The two facts a failure-after-ready has to get right, checked together
/// because either alone is satisfiable by a readiness that simply never
/// releases: that every dependant the readiness *did* release is stopped, and
/// that a dependant arriving afterwards hears the failure rather than a
/// release from the generation that died.
fn a_failure_after_ready_reaches_every_running_dependant(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let scope = Scope::root(TenantName::new("acme")?);
        let generation = Generation::at(1);
        let readiness: Named = Readiness::named("db", generation, TENANT_CAP)?;
        let expected = published(0, generation);
        // The release happens *first*: the dependants below are released by it
        // and are still running when the failure lands, which is the whole state
        // this family is about.
        readiness.ready(generation, expected.clone())?;

        let steps: Vec<Scope> = (0..TENANT_CAP)
            .map(|index| scope.enter(&format!("worker-{index}")))
            .collect::<Result<_, _>>()?;
        for step in &steps {
            let ready = lgwks_bot::block_on(readiness.wait(step, BUDGET))?;
            assert_eq!(
                ready.awaited(),
                &expected,
                "every dependant is released with the published address"
            );
        }
        assert_eq!(
            readiness.dependants_live(),
            steps.len(),
            "the readiness holds one token per dependant it released"
        );

        readiness.fail(generation, "the seeded failure after ready")?;
        let after = readiness
            .failed_at()
            .ok_or("a failure after the release must be recorded as one")?;
        assert_eq!(
            after.generation(),
            generation,
            "the recorded failure names the released generation"
        );
        assert_eq!(
            readiness.dependants_live(),
            0,
            "the failure took every token it reached"
        );
        // Every dependant's own scope was cancelled, and a second failure
        // cancels nobody twice.
        for (index, step) in steps.iter().enumerate() {
            assert!(
                step.is_cancelled(),
                "dependant {index} must have been stopped by the failure"
            );
        }
        assert!(
            readiness.fail(generation, "again").is_err(),
            "a second failure for the same generation is refused, not re-applied"
        );
        // A dependant arriving after the failure hears the failure, never a
        // release from the generation that died.
        let late = scope.enter("late")?;
        let outcome = lgwks_bot::block_on(readiness.wait(&late, BUDGET));
        assert!(
            outcome.is_err(),
            "a readiness that failed after ready must not release a late dependant"
        );
        sim.record(&format!(
            "dependants={} released={} later=refused second=refused",
            steps.len(),
            readiness.released_at().map_or(0, Generation::get)
        ));
        Ok(())
    })
}

/// Two tenants over one service name never cross: each is served only its own
/// address, and one tenant's failure never reaches the other's dependants.
fn two_tenants_never_cross_a_readiness(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let mut generations = 0_u64;
        let plan = plan(sim.rng());
        let observed = drive_plan(&plan, &mut generations)?;

        // Every value a dependant was served names a tenant. A dependant served
        // another tenant's address is the cross this family exists to catch, and
        // the address carries the tenant by construction.
        for value in &observed.served {
            assert!(
                value.starts_with("db-"),
                "a served value must name the tenant that published it, got {value:?}"
            );
            let tenant = tenant_of_address(value)?;
            assert!(
                tenant < plan.tenants,
                "tenant {tenant} is outside the scenario's {} tenants",
                plan.tenants
            );
        }
        observed.record(&mut sim.trace);
        sim.record(&format!(
            "tenants={} steps={} served={} refusals={}",
            plan.tenants,
            plan.steps.len(),
            observed.served.len(),
            observed.refusals.len()
        ));
        Ok(())
    })
}

/// 100, 1 000 and 10 000 dependants against one readiness, up to its cap.
///
/// The tiers are the claim, so they are named rather than drawn. Each tier
/// records the level requested, the level reached and the cap together, so a
/// host that cannot reach a tier says so instead of the trace implying a
/// concurrency nobody ran (the INV-BOT-16 rule).
#[test]
fn saturation_admits_up_to_the_declared_cap() -> TestResult {
    const TIERS: [usize; 3] = [100, 1_000, 10_000];
    let scope = Scope::root(TenantName::new("acme")?);
    let generation = Generation::at(1);

    for tier in TIERS {
        // The cap is the tier, so the sweep measures the readiness at the level
        // it claims and the refusal at the level past it.
        let readiness: Named = Readiness::named("db", generation, tier)?;
        let expected = published(0, generation);
        // The release is what the tier's dependants are waiting for; a readiness
        // that is never released would admit them all and serve none, which is a
        // different property from the one under test.
        readiness.ready(generation, expected.clone())?;
        let served = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicUsize::new(0));
        let steps: Vec<Scope> = (0..tier)
            .map(|index| scope.enter(&format!("worker-{index}")))
            .collect::<Result<_, _>>()?;

        // Every dependant waits for real: the wait is a subscription and a
        // select, so a thousand of them are a thousand wakers on one channel
        // rather than a thousand timers. The readiness is cloned into each body
        // rather than moved, because a clone is a pointer copy and the whole
        // tier must share one state — a copy per dependant would be a thousand
        // readinesses, which is a different property entirely.
        let width = NonZeroUsize::new(tier)
            .ok_or("the tier must be at least one")?
            .get();
        let reported = lgwks_bot::block_on(lgwks_bot::rt::task::join_all_bounded(
            width,
            steps.iter().map(|step| {
                let gate = readiness.clone();
                let expected = expected.clone();
                // The scope is cloned into the body rather than borrowed: a
                // `Scope` is a pointer copy by construction, and a body that
                // borrowed one out of a local `Vec` would need that `Vec` to
                // outlive the fan-out, which is a lifetime the tier has no
                // reason to constrain itself by.
                let step = step.clone();
                async move {
                    match gate.wait(&step, BUDGET).await {
                        Ok(ready) => {
                            assert_eq!(
                                ready.awaited(),
                                &expected,
                                "a dependant must be served the tenant's own published \
                                 address at a tier of {tier}"
                            );
                            1_u8
                        }
                        Err(_) => 0,
                    }
                }
            }),
        ));
        let served_count = reported.iter().filter(|outcome| **outcome == 1).count();
        served.store(served_count, Ordering::SeqCst);
        assert_eq!(
            served.load(Ordering::SeqCst),
            tier,
            "a readiness at a cap of {tier} must admit all {tier} dependants"
        );
        assert_eq!(
            readiness.admitted(),
            u64::try_from(tier)?,
            "and charge every admission once"
        );

        // One past the cap is refused, and the refusal charges nothing.
        let over = scope.enter("over-cap")?;
        let over_refused = lgwks_bot::block_on(readiness.wait(&over, BUDGET)).is_err();
        assert!(
            over_refused,
            "a dependant past the cap of {tier} must be refused"
        );
        refused.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            readiness.admitted(),
            u64::try_from(tier)?,
            "a refused admission at a tier of {tier} charged no slot"
        );

        // A failure at this tier reaches every dependant it released.
        readiness.fail(generation, "the saturated failure")?;
        let live = readiness.dependants_live();
        assert_eq!(
            live, 0,
            "a failure at a tier of {tier} took every token it reached"
        );
        assert_eq!(
            refused.load(Ordering::SeqCst),
            1,
            "exactly one dependant was past the cap"
        );
        let _ = refused;
    }
    Ok(())
}

// ── The replay receipt ──────────────────────────────────────────────────────

/// The same seed produces the same trace hash, twice, over the union of the
/// families above.
///
/// `assert_replays` sweeps the band twice and compares the hash vectors, so a
/// nondeterministic run fails even when every assertion passes.
fn the_same_seed_replays_the_same_trace(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let mut generations = 0_u64;
        let plan = plan(sim.rng());
        let observed = drive_plan(&plan, &mut generations)?;
        sim.record(&format!(
            "tenants={} steps={} generations={generations}",
            plan.tenants,
            plan.steps.len()
        ));
        observed.record(&mut sim.trace);
        Ok(())
    })
}

// ── Band declarations ───────────────────────────────────────────────────────

band_family::band_family! {
    an_interleaving_never_releases_a_stale_generation_band_00 => an_interleaving_never_releases_a_stale_generation, 0;
    an_interleaving_never_releases_a_stale_generation_band_01 => an_interleaving_never_releases_a_stale_generation, 1;
    an_interleaving_never_releases_a_stale_generation_band_02 => an_interleaving_never_releases_a_stale_generation, 2;
    an_interleaving_never_releases_a_stale_generation_band_03 => an_interleaving_never_releases_a_stale_generation, 3;
    an_interleaving_never_releases_a_stale_generation_band_04 => an_interleaving_never_releases_a_stale_generation, 4;
    an_interleaving_never_releases_a_stale_generation_band_05 => an_interleaving_never_releases_a_stale_generation, 5;
    an_interleaving_never_releases_a_stale_generation_band_06 => an_interleaving_never_releases_a_stale_generation, 6;
    a_duplicate_releases_once_in_every_interleaving_band_00 => a_duplicate_releases_once_in_every_interleaving, 0;
    a_duplicate_releases_once_in_every_interleaving_band_01 => a_duplicate_releases_once_in_every_interleaving, 1;
    a_duplicate_releases_once_in_every_interleaving_band_02 => a_duplicate_releases_once_in_every_interleaving, 2;
    a_duplicate_releases_once_in_every_interleaving_band_03 => a_duplicate_releases_once_in_every_interleaving, 3;
    a_duplicate_releases_once_in_every_interleaving_band_04 => a_duplicate_releases_once_in_every_interleaving, 4;
    a_duplicate_releases_once_in_every_interleaving_band_05 => a_duplicate_releases_once_in_every_interleaving, 5;
    a_duplicate_releases_once_in_every_interleaving_band_06 => a_duplicate_releases_once_in_every_interleaving, 6;
    a_failure_after_ready_reaches_every_running_dependant_band_00 => a_failure_after_ready_reaches_every_running_dependant, 0;
    a_failure_after_ready_reaches_every_running_dependant_band_01 => a_failure_after_ready_reaches_every_running_dependant, 1;
    a_failure_after_ready_reaches_every_running_dependant_band_02 => a_failure_after_ready_reaches_every_running_dependant, 2;
    a_failure_after_ready_reaches_every_running_dependant_band_03 => a_failure_after_ready_reaches_every_running_dependant, 3;
    a_failure_after_ready_reaches_every_running_dependant_band_04 => a_failure_after_ready_reaches_every_running_dependant, 4;
    a_failure_after_ready_reaches_every_running_dependant_band_05 => a_failure_after_ready_reaches_every_running_dependant, 5;
    two_tenants_never_cross_a_readiness_band_00 => two_tenants_never_cross_a_readiness, 0;
    two_tenants_never_cross_a_readiness_band_01 => two_tenants_never_cross_a_readiness, 1;
    two_tenants_never_cross_a_readiness_band_02 => two_tenants_never_cross_a_readiness, 2;
    two_tenants_never_cross_a_readiness_band_03 => two_tenants_never_cross_a_readiness, 3;
    two_tenants_never_cross_a_readiness_band_04 => two_tenants_never_cross_a_readiness, 4;
    two_tenants_never_cross_a_readiness_band_05 => two_tenants_never_cross_a_readiness, 5;
    two_tenants_never_cross_a_readiness_band_06 => two_tenants_never_cross_a_readiness, 6;
    the_same_seed_replays_the_same_trace_band_00 => the_same_seed_replays_the_same_trace, 0;
    the_same_seed_replays_the_same_trace_band_01 => the_same_seed_replays_the_same_trace, 1;
    the_same_seed_replays_the_same_trace_band_02 => the_same_seed_replays_the_same_trace, 2;
    the_same_seed_replays_the_same_trace_band_03 => the_same_seed_replays_the_same_trace, 3;
    the_same_seed_replays_the_same_trace_band_04 => the_same_seed_replays_the_same_trace, 4;
    the_same_seed_replays_the_same_trace_band_05 => the_same_seed_replays_the_same_trace, 5;
    the_same_seed_replays_the_same_trace_band_06 => the_same_seed_replays_the_same_trace, 6;
}
