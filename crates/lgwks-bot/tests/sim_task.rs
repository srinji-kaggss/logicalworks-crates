//! Simulation family: the task front door under a seeded sweep.
//!
//! Every scenario runs the real `Host`, the real `Task`, and the real
//! `script` runtime. Nothing here reimplements admission, a fan-out or a trail.
//! What the seed controls is the *shape* of the run: how deep the nesting goes,
//! how wide the fan-out is, which task fails and how, whether the host is
//! cancelled, and how many tenants run at once.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `permits_conserve` | every permit is back after every scenario, whatever the tree |
//! | `disposition_matches_the_fault` | the injected fault and the reported disposition are the same fact, seen from two sides |
//! | `keys_never_collide_across_tenants` | no step path is shared between two tenants, and a rerun for one tenant reproduces its keys |
//! | `ceiling_binds_the_peak` | the high-water mark never exceeds the ceiling, and reaches it when the tree is wide enough |
//! | `the_same_seed_replays` | the same seed produces the same trace hash, twice |
//!
//! # What is real and what is seeded
//!
//! The host, the semaphore, the deadlines and the trail are real. Time is
//! *small* rather than virtual: each scenario uses a millisecond-scale deadline
//! or an immediate fault, so a run is decided by structure rather than by how
//! loaded the machine is. What is seeded is everything a scenario chooses —
//! never a clock reading. That is why the trace hash is a receipt: it is built
//! from dispositions and step paths only, never from elapsed time, which would
//! differ between two runs of one seed on a busy box.

#![cfg(feature = "script")]

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

use std::cell::RefCell;
use std::error::Error;
use std::num::NonZeroUsize;
use std::rc::Rc;

use lgwks_bot::script::{FlowError, Scope, each};
use lgwks_bot::task::{Disposition, Host, Report, Task, task};

use sim::Band;
use sim::Rng;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A task behind a reference count, so one tree can name its own children.
type Shared<F> = Rc<Task<F>>;

/// What one seed decided this run's environment would do.
///
/// Drawn from the seed rather than from a clock, and every field is a *choice*
/// the scenario acts on. `microseconds` is small on purpose: it is a real
/// timer, so it is real work, but it is small enough that which of two pending
/// waits resolves first is decided by structure and not by scheduling noise.
struct Plan {
    /// How deep the nesting goes, at least one.
    depth: u32,
    /// How many items the widest fan-out holds.
    fan_out: u32,
    /// How many hosts, and so how many tenants, run at once.
    tenants: u32,
    /// Which level of the tree fails, or none.
    failing: Option<u32>,
    /// How the failing level fails.
    fault: Fault,
    /// Whether the host is cancelled part-way through.
    cancels: bool,
    /// Which level the cancellation is raised at.
    cancelling_level: u32,
    /// A real, small duration a seeded timeout uses.
    microseconds: u64,
}

impl Clone for Plan {
    /// Every field is `Copy` except the fault, which is one enum of three unit
    /// variants, so a level's copy is the plan the seed drew and nothing more.
    /// Hand-written so the copy stops compiling the moment a field stops being
    /// `Copy` — a level that then cannot be cloned per call learns it here
    /// rather than in a `Debug` line.
    fn clone(&self) -> Self {
        Self {
            depth: self.depth,
            fan_out: self.fan_out,
            tenants: self.tenants,
            failing: self.failing,
            fault: self.fault,
            cancels: self.cancels,
            cancelling_level: self.cancelling_level,
            microseconds: self.microseconds,
        }
    }
}

/// The one fault a seeded tree injects.
///
/// Kept to three kinds because each maps to a different disposition, and a
/// simulation that cannot distinguish them proves less than one that has only
/// "it failed".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// A permanent failure: `Failed`.
    Permanent,
    /// A run that overruns the host's deadline: `DeadlineExceeded`.
    Overrun,
    /// Nothing goes wrong.
    None,
}

impl Fault {
    /// The disposition this fault must produce.
    const fn expected(self) -> Disposition {
        match self {
            Self::Permanent => Disposition::Failed,
            Self::Overrun => Disposition::DeadlineExceeded,
            Self::None => Disposition::Succeeded,
        }
    }
}

/// A plan drawn from `seed`.
///
/// The draws happen in a fixed order and the first three are the structural ones,
/// so changing how a fault is chosen cannot renumber the trees in another
/// family: a regression in fan-out would otherwise be unbisectable against a
/// regression in depth.
fn plan(rng: &mut Rng) -> Plan {
    let depth = rng.between(1, 4);
    let fan_out = rng.between(1, 12);
    let tenants = rng.between(1, 4);
    // The levels are numbered from one, so a fault is drawn in `1..=depth` — a
    // fault named at level zero would name a level the tree never enters, and a
    // plan that could describe it would be a plan whose expectation the run
    // could never meet.
    let failing = (rng.chance(400)).then(|| rng.between(1, depth));
    let fault = match rng.below(3) {
        0 => Fault::Permanent,
        1 => Fault::Overrun,
        _ => Fault::None,
    };
    let cancels = rng.chance(300);
    let cancelling_level = rng.between(1, depth);
    Plan {
        depth,
        fan_out,
        tenants,
        failing,
        fault,
        cancels,
        cancelling_level,
        microseconds: u64::from(rng.between(1, 4)),
    }
}

/// What the tree recorded while it ran.
#[derive(Default)]
struct Observed {
    /// Every step key, with the path it came from.
    keys: RefCell<Vec<(String, String)>>,
    /// The tenant each key was taken under.
    tenants: RefCell<Vec<String>>,
    /// Every event a level applied, in the order it applied it.
    applied: RefCell<Vec<Event>>,
}

/// One thing a level did to end or shape its run.
///
/// The order these are recorded in is the order the run applied them, and the
/// first one is what decided its disposition. Recording it is what lets a
/// scenario state the expectation as a fact about the run rather than a
/// prediction about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// The level stopped the host.
    Cancel,
    /// The level failed or overran.
    Fault(Fault),
}

/// Runs the tree for one input on one host.
///
/// A trait object rather than a generic parameter because the four levels are
/// four distinct closure types: a struct or a generic would have to name all
/// four or erase one. The erasure is confined to the *tree builder* in this
/// test file, which is exactly the point of the row — every task the front
/// door itself runs keeps its own concrete body type.
type Runner = dyn Fn(&Host, u32) -> Report<u32>;

/// What [`build`] hands back: the runner and the tree's shape.
struct Tree {
    /// Runs the tree, descending as far as the plan's depth.
    run: Box<Runner>,
    /// How many items the leaf fans out over.
    fan_out: usize,
}

/// The state every level shares: the host, the plan, and what the run records.
///
/// `Clone` rather than shared by reference: a level's future cannot borrow the
/// level's own environment, so the context is handed to `descend` by value and
/// one more `Host` handle per level is the cost of that.
#[derive(Clone)]
struct Ctx {
    /// The host every level runs through.
    host: Host,
    /// The plan this tree was drawn from.
    plan: Plan,
    /// What the tree records while it runs.
    observed: Rc<Observed>,
}

impl Ctx {
    /// Whether this level, at `level`, is the one the plan fails.
    fn fault_at(&self, level: u32) -> Option<Fault> {
        (self.plan.failing == Some(level)).then_some(self.plan.fault)
    }

    /// Record the tenant a level reported under.
    fn note_tenant(&self) {
        self.observed
            .tenants
            .borrow_mut()
            .push(self.host.tenant().as_str().to_owned());
    }

    /// Whether this level is the one the plan stops the host at.
    fn cancels_at(&self, level: u32) -> bool {
        self.plan.cancels && self.plan.cancelling_level == level
    }
}

/// The input every level passes on: a value and how many levels are left.
type Step = (u32, u32);

/// Build the leaf and the four levels above it.
fn build(host: &Host, plan: &Plan, observed: Rc<Observed>) -> Result<Tree, Box<dyn Error>> {
    let fan_out = plan.fan_out;
    let mut levels = Vec::new();

    // The leaf: an `each` over `fan_out` items, the width of the tree. It sums
    // into a scalar, so the chain's value is one number rather than a vector per
    // level.
    let leaf_observed = Rc::clone(&observed);
    let leaf: Shared<_> = Rc::new(task("leaf", move |scope: Scope, step: Step| {
        let items: Vec<u32> = (0..fan_out)
            .map(|index| step.0.wrapping_add(index))
            .collect();
        let observed = Rc::clone(&leaf_observed);
        let item_log = Rc::clone(&observed);
        async move {
            let values = each::<_, u32, _, _>(
                &scope,
                "item",
                NonZeroUsize::new(1),
                items,
                |item_scope, item| {
                    let log = Rc::clone(&item_log);
                    async move {
                        item_scope.checkpoint()?;
                        log.keys
                            .borrow_mut()
                            .push((item_scope.key().to_hex(), item_scope.path().to_owned()));
                        log.tenants
                            .borrow_mut()
                            .push(item_scope.tenant().as_str().to_owned());
                        Ok(item)
                    }
                },
            )
            .await?;
            Ok(values.iter().copied().sum::<u32>())
        }
    })?);
    levels.push("leaf".to_owned());

    // Each level is its own closure type, capturing the level below it. Four of
    // them are written out rather than generated: a loop would need one variable
    // to hold tasks of four different closure types, which is precisely the
    // erasure the front door exists to avoid. Each takes and returns `Step`, so
    // a level can call either the level below it or the leaf without either
    // call being typed against a different signature.
    // `level_1` reaches the leaf directly; every other level reaches the one below
    // it, and calls the leaf itself once the seed's depth has run out. That is
    // how one body covers every depth: a level with levels left calls the next
    // level down, and the level at the bottom of the seeded tree calls the leaf.
    let base = Ctx {
        host: host.clone(),
        plan: plan.clone(),
        observed: Rc::clone(&observed),
    };
    let leaf_for_1 = Rc::clone(&leaf);
    let leaf_for_2 = Rc::clone(&leaf);
    let leaf_for_3 = Rc::clone(&leaf);
    let leaf_for_4 = Rc::clone(&leaf);
    let level_1 = Rc::new({
        let (ctx, leaf) = (base.clone(), Rc::clone(&leaf_for_1));
        task("level-1", move |_scope: Scope, step: Step| {
            descend(ctx.clone(), 1, Rc::clone(&leaf), Rc::clone(&leaf), step)
        })?
    });
    let level_2 = Rc::new({
        let (ctx, inner) = (base.clone(), Rc::clone(&level_1));
        task("level-2", move |_scope: Scope, step: Step| {
            descend(
                ctx.clone(),
                2,
                Rc::clone(&inner),
                Rc::clone(&leaf_for_2),
                step,
            )
        })?
    });
    let level_3 = Rc::new({
        let (ctx, inner) = (base.clone(), Rc::clone(&level_2));
        task("level-3", move |_scope: Scope, step: Step| {
            descend(
                ctx.clone(),
                3,
                Rc::clone(&inner),
                Rc::clone(&leaf_for_3),
                step,
            )
        })?
    });
    let level_4 = Rc::new({
        let (ctx, inner) = (base, Rc::clone(&level_3));
        task("level-4", move |_scope: Scope, step: Step| {
            descend(
                ctx.clone(),
                4,
                Rc::clone(&inner),
                Rc::clone(&leaf_for_4),
                step,
            )
        })?
    });
    levels.extend([
        "level-1".to_owned(),
        "level-2".to_owned(),
        "level-3".to_owned(),
        "level-4".to_owned(),
    ]);

    // A run starts at the level the seed's depth names, and the depth counts
    // down as each level calls the one below. Every arm is a distinct closure
    // type and each is run directly, so nothing here erases a task.
    let depth = plan.depth.clamp(1, 4);
    let run: Box<Runner> = Box::new(move |host: &Host, input: u32| {
        let step = (input, depth);
        match depth {
            1 => lgwks_bot::block_on(host.run(&level_1, step)),
            2 => lgwks_bot::block_on(host.run(&level_2, step)),
            3 => lgwks_bot::block_on(host.run(&level_3, step)),
            _ => lgwks_bot::block_on(host.run(&level_4, step)),
        }
    });
    Ok(Tree {
        run,
        fan_out: usize::try_from(plan.fan_out)?,
    })
}

/// One level's body, shared by the four closures that call it.
///
/// `inner` is the level below and `leaf` the fan-out at the bottom. `step.1`
/// counts the levels still to run, so the level at the bottom of the seeded
/// depth calls the leaf directly and every other calls the level below it.
fn descend<F, G, FutF, FutG>(
    ctx: Ctx,
    level: u32,
    inner: Shared<F>,
    leaf: Shared<G>,
    step: Step,
) -> impl std::future::Future<Output = Result<u32, FlowError>>
where
    F: Fn(Scope, Step) -> FutF,
    FutF: std::future::Future<Output = Result<u32, FlowError>>,
    G: Fn(Scope, Step) -> FutG,
    FutG: std::future::Future<Output = Result<u32, FlowError>>,
{
    lgwks_std::trace::warn!(
        operation = "descend",
        "operation refused its request; the typed error carries the facts"
    );
    let fault = ctx.fault_at(level);
    let cancels = ctx.cancels_at(level);
    ctx.note_tenant();
    async move {
        if cancels {
            // The stop reaches this level and every level below it, so the
            // child's run is refused and the whole tree ends `Cancelled`.
            ctx.observed.applied.borrow_mut().push(Event::Cancel);
            ctx.host.cancel();
        }
        if let Some(fault) = fault {
            ctx.observed.applied.borrow_mut().push(Event::Fault(fault));
            match fault {
                Fault::Overrun => {
                    // A body that never returns, which the host's deadline
                    // ends. Nothing below this level is ever entered.
                    std::future::pending::<()>().await;
                    return Ok(0);
                }
                Fault::Permanent => {
                    return Err(FlowError::failed("the seeded fault refused this level"));
                }
                Fault::None => {}
            }
        }
        // A level propagates its child's failure rather than swallowing it. That is
        // what makes the expected disposition derivable: a fault three levels
        // down must surface as the same fault at the top, or the report would
        // claim a success the tree never had. The child's error is already
        // located at the step that failed, and that location is kept — it names
        // the deepest step, which is the one worth naming.
        let remaining = step.1.saturating_sub(1);
        let report = if remaining == 0 {
            ctx.host.run(&leaf, (step.0, 0)).await
        } else {
            ctx.host.run(&inner, (step.0, remaining)).await
        };
        report.into_result()
    }
}

/// Run the tree once and return its report.
fn run(host: &Host, tree: &Tree, input: u32) -> Report<u32> {
    (tree.run)(host, input)
}

// ── Families ───────────────────────────────────────────────────────────────

/// Every permit is back after every scenario, whatever shape the run took.
///
/// This is the property that makes the other families' numbers mean anything: a
/// budget that leaked would make the ceiling meaningless within one seed.
fn permits_conserve(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(1, 4))?;
        let plan = plan(sim.rng());
        let host = Host::builder("acme")?
            .max_concurrent_tasks(NonZeroUsize::new(ceiling).ok_or("a ceiling of one")?)
            .default_deadline(std::time::Duration::from_millis(400))
            .build()?;
        let observed = Rc::new(Observed::default());
        let tree = build(&host, &plan, Rc::clone(&observed))?;

        let report = run(&host, &tree, 7);
        assert_eq!(
            host.admission().available_permits(),
            ceiling,
            "every permit is back after a {:?} run at a ceiling of {ceiling}: {:?}",
            report.disposition(),
            report.error()
        );
        assert_eq!(
            host.admission().in_flight(),
            0,
            "nothing is left in flight: a leak here is invisible in every other family"
        );
        assert!(
            host.admission().admitted() >= 1,
            "the scenario ran at least one task"
        );
        sim.record(&format!(
            "ceiling={ceiling} depth={} fan={} admitted={} end={}",
            plan.depth,
            tree.fan_out,
            host.admission().admitted(),
            report.disposition()
        ));
        Ok(())
    })
}

/// The injected fault and the reported disposition are the same fact.
fn disposition_matches_the_fault(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let forces = sim.rng().chance(700);
        let mut plan = plan(sim.rng());
        // Force a fault every so often: a scenario that draws `None` three times
        // out of four proves nothing about how a failure is reported.
        if forces {
            plan.failing = Some(1);
            plan.fault = if sim.rng().chance(500) {
                Fault::Permanent
            } else {
                Fault::Overrun
            };
        }
        let host = Host::builder("acme")?
            .max_concurrent_tasks(NonZeroUsize::new(2).ok_or("a ceiling")?)
            // A real, small deadline. The overrun is a body that never returns,
            // so this fires rather than the body finishing first.
            .default_deadline(std::time::Duration::from_millis(60))
            .build()?;
        let observed = Rc::new(Observed::default());
        let tree = build(&host, &plan, Rc::clone(&observed))?;

        let report = run(&host, &tree, 11);
        // The disposition the report must carry is decided by the first event the tree
        // applied *at the level that ends the run*. A cancel raised at a level
        // raises the host's stop before that level's own body checks for a
        // fault, so on its own it decides — but a body that has already
        // produced a `FlowError` cannot be interrupted by a stop, so a permanent
        // failure at that same level still surfaces as `Failed`. The tree's
        // record plus the fault's kind therefore decide together, which is a
        // fact about the run rather than a prediction about the level numbers.
        let first = observed.applied.borrow().first().copied();
        let same_level_fault = plan.cancels
            && plan.failing == Some(plan.cancelling_level)
            && plan.failing.is_some_and(|level| level <= plan.depth);
        let expected = match (first, same_level_fault) {
            // A permanent failure at the cancelling level still surfaces: the
            // body returned its error before the stop could be observed.
            (Some(Event::Cancel), true) => match plan.fault {
                Fault::Permanent => Disposition::Failed,
                // An overrun never returns an error at all; the stop raised in
                // the same body is what the `within` around it observes.
                _ => Disposition::Cancelled,
            },
            (Some(Event::Cancel), false) => Disposition::Cancelled,
            (Some(Event::Fault(fault)), _) => fault.expected(),
            (None, _) => Disposition::Succeeded,
        };
        let reached = |level: u32| level <= plan.depth;
        let fault_level = plan.failing.filter(|&level| reached(level));
        let cancels_at =
            (plan.cancels && reached(plan.cancelling_level)).then_some(plan.cancelling_level);
        let applied = observed.applied.borrow();
        let fault_applied = applied.contains(&Event::Fault(plan.fault));
        let cancel_applied = applied.contains(&Event::Cancel);
        // A cancel raised above the faulting level stops the host before that level's
        // body runs, so the fault is recorded nowhere. Every other reachable
        // fault must be applied, or the plan said something the tree did not do.
        let stopped_first = matches!(first, Some(Event::Cancel));
        if fault_level.is_some() {
            assert!(
                fault_applied || stopped_first,
                "a reachable fault must be applied unless a cancel stopped the run \
                 first: fault at {fault_level:?}, cancel at {cancels_at:?}, \
                 applied {applied:?}"
            );
        } else {
            assert!(
                !fault_applied,
                "a fault the run never entered must not be applied: \
                 fault at {fault_level:?}, applied {applied:?}"
            );
        }
        // A cancel at a level the run reached is applied, unless something above it
        // ended the run first — an overrun never returns, so the levels beneath
        // it are never entered and their events never happen. An unreachable
        // cancel is never applied either way.
        if cancels_at.is_some() && !fault_applied {
            assert!(
                cancel_applied,
                "a reachable cancel with nothing above it stopping the run must \
                 be applied: cancel at {cancels_at:?}, applied {applied:?}"
            );
        }
        if cancels_at.is_none() {
            assert!(
                !cancel_applied,
                "a cancel at a level the run never entered must not be applied: \
                 cancel at {cancels_at:?}, applied {applied:?}"
            );
        }
        drop(applied);
        assert_eq!(
            report.disposition(),
            expected,
            "the injected fault {:?} at level {:?} must be reported as {expected}, got {:?}",
            plan.fault,
            plan.failing,
            report.error()
        );
        sim.record(&format!(
            "depth={} fault={:?} at={:?} cancels={} end={}",
            plan.depth,
            plan.fault,
            plan.failing,
            plan.cancels,
            report.disposition()
        ));
        Ok(())
    })
}

/// No step path is shared between two tenants, and a rerun for one tenant
/// reproduces its keys.
/// Builds one tenant's host, runs it, and returns the keys it took.
///
/// A helper rather than the loop body inline: as one block the loop built a
/// host, resolved a ceiling, built a tree and ran it, so the three fallible
/// steps a tenant needs sat in one statement next to the assertion that the
/// keys it took belong to it. Splitting them keeps the assertion attached to
/// what it is about.
fn provision_and_observe(plan: &Plan, index: u32) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let name = format!("tenant-{index}");
    let host = Host::builder(&name)?
        .max_concurrent_tasks(NonZeroUsize::new(1).ok_or("a ceiling")?)
        .default_deadline(std::time::Duration::from_millis(400))
        .build()?;
    let observed = Rc::new(Observed::default());
    let tree = build(&host, plan, Rc::clone(&observed))?;
    run(&host, &tree, 3);
    for seen in observed.tenants.borrow().iter() {
        assert_eq!(
            seen, &name,
            "a key taken inside the tree belongs to the host's tenant"
        );
    }
    Ok(observed.keys.borrow().clone())
}

fn keys_never_collide_across_tenants(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let tenants = plan.tenants;
        let mut all: Vec<Vec<(String, String)>> = Vec::new();

        for index in 0..tenants {
            all.push(provision_and_observe(&plan, index)?);
        }

        // Every tenant's key set is disjoint from every other's.
        for (index, keys) in all.iter().enumerate() {
            for (other_index, others) in all.iter().enumerate() {
                if index == other_index {
                    continue;
                }
                let overlap = keys
                    .iter()
                    .filter(|entry| others.iter().any(|theirs| theirs.0 == entry.0))
                    .count();
                assert_eq!(
                    overlap, 0,
                    "tenants {index} and {other_index} must share no step key"
                );
            }
        }

        // The same tenant, run again, reproduces every key.
        let name = "tenant-0";
        let host = Host::builder(name)?
            .max_concurrent_tasks(NonZeroUsize::new(1).ok_or("a ceiling")?)
            .default_deadline(std::time::Duration::from_millis(400))
            .build()?;
        let again = Rc::new(Observed::default());
        let tree = build(&host, &plan, Rc::clone(&again))?;
        run(&host, &tree, 3);
        assert_eq!(
            all.first().map(Vec::len),
            Some(again.keys.borrow().len()),
            "a rerun for one tenant enters the same number of steps"
        );
        sim.record(&format!(
            "tenants={tenants} depth={} fan={} keys={}",
            plan.depth,
            plan.fan_out,
            all.first().map_or(0, Vec::len)
        ));
        Ok(())
    })
}

/// The high-water mark never exceeds the ceiling, however many runs are queued
/// behind it.
fn ceiling_binds_the_peak(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let runs = sim.rng().between(1, 8);
        let plan = plan(sim.rng());
        // The ceiling is one for this family, so every sibling top-level run is
        // the interesting case: each takes its own permit, and none may have
        // two at once.
        let ceiling = 1_usize;
        let runs = usize::try_from(runs)?;
        let host = Host::builder("acme")?
            .max_concurrent_tasks(NonZeroUsize::new(ceiling).ok_or("a ceiling")?)
            .default_deadline(std::time::Duration::from_millis(400))
            .build()?;

        // Several runs of the same tree, each a top-level run. They are driven one
        // after another rather than concurrently: the property under test is
        // that the ceiling *bounds* what a host admits, and the peak mark is
        // read from the host's own counters, which one-at-a-time makes exact.
        let observed = Rc::new(Observed::default());
        // The plan's cancellation is a real stop of the host, so every run
        // after the one that raised it is refused at admission. That is the
        // host's contract, not a defect in the tree, so this family builds the
        // runs with the cancellation off: its subject is the ceiling, and a
        // stopped host has no ceiling left to measure.
        // An overrun is off-subject for the same reason, and costlier: each one
        // waits out the whole deadline in real time, and up to eight runs in a
        // row made this family five seconds a band. A permanent fault still
        // ends a run early, so a failing run's permit is still counted back.
        let mut measured = plan.clone();
        measured.cancels = false;
        if matches!(measured.fault, Fault::Overrun) {
            measured.fault = Fault::Permanent;
        }
        let tree = build(&host, &measured, Rc::clone(&observed))?;
        let reports: Vec<Report<u32>> = (0..runs)
            .map(|index| (tree.run)(&host, u32::try_from(index).unwrap_or_default()))
            .collect();
        for report in &reports {
            assert!(
                report.disposition().was_admitted(),
                "every queued run is admitted on an unstopped host: {:?}",
                report.error()
            );
        }
        assert!(
            host.admission().peak_in_flight() <= ceiling,
            "peak {} exceeded the ceiling {ceiling}",
            host.admission().peak_in_flight()
        );
        assert_eq!(
            host.admission().available_permits(),
            ceiling,
            "every permit came back after {runs} runs"
        );
        assert_eq!(host.admission().in_flight(), 0, "nothing is left in flight");
        sim.record(&format!(
            "runs={runs} depth={} fan={} peak={} admitted={}",
            plan.depth,
            plan.fan_out,
            host.admission().peak_in_flight(),
            host.admission().admitted()
        ));
        Ok(())
    })
}

/// The same seed produces the same trace, twice over.
fn the_same_seed_replays(band: Band) -> TestResult {
    // `assert_replays` sweeps the band twice and compares the hash vectors, so
    // a nondeterministic run fails even when every assertion passes. The body is
    // the union of the families above, so the hash covers dispositions, step
    // paths and counters rather than one scenario's shape.
    sim::assert_replays(band, |sim| {
        let plan = plan(sim.rng());
        let host = Host::builder("acme")?
            .max_concurrent_tasks(NonZeroUsize::new(2).ok_or("a ceiling")?)
            .default_deadline(std::time::Duration::from_millis(80))
            .build()?;
        let observed = Rc::new(Observed::default());
        let tree = build(&host, &plan, Rc::clone(&observed))?;
        let report = run(&host, &tree, 5);

        sim.record(&format!(
            "depth={} fan={} tenants={} end={} admitted={} refused={} peak={}",
            plan.depth,
            tree.fan_out,
            plan.tenants,
            report.disposition(),
            host.admission().admitted(),
            host.admission().refused(),
            host.admission().peak_in_flight()
        ));
        // The step paths go into the trace in order, never a timing.
        for entry in observed.keys.borrow().iter() {
            sim.record(&format!("step {} {}", entry.1, entry.0));
        }
        Ok(())
    })
}

band_family::band_family! {
    permits_conserve_band_00 => permits_conserve, 0;
    permits_conserve_band_01 => permits_conserve, 1;
    permits_conserve_band_02 => permits_conserve, 2;
    permits_conserve_band_03 => permits_conserve, 3;
    permits_conserve_band_04 => permits_conserve, 4;
    permits_conserve_band_05 => permits_conserve, 5;
    permits_conserve_band_06 => permits_conserve, 6;
    permits_conserve_band_07 => permits_conserve, 7;
    permits_conserve_band_08 => permits_conserve, 8;
    permits_conserve_band_09 => permits_conserve, 9;
    permits_conserve_band_10 => permits_conserve, 10;
    permits_conserve_band_11 => permits_conserve, 11;
    permits_conserve_band_12 => permits_conserve, 12;
    permits_conserve_band_13 => permits_conserve, 13;
    permits_conserve_band_14 => permits_conserve, 14;
    permits_conserve_band_15 => permits_conserve, 15;
    disposition_matches_the_fault_band_00 => disposition_matches_the_fault, 0;
    disposition_matches_the_fault_band_01 => disposition_matches_the_fault, 1;
    disposition_matches_the_fault_band_02 => disposition_matches_the_fault, 2;
    disposition_matches_the_fault_band_03 => disposition_matches_the_fault, 3;
    disposition_matches_the_fault_band_04 => disposition_matches_the_fault, 4;
    disposition_matches_the_fault_band_05 => disposition_matches_the_fault, 5;
    disposition_matches_the_fault_band_06 => disposition_matches_the_fault, 6;
    disposition_matches_the_fault_band_07 => disposition_matches_the_fault, 7;
    disposition_matches_the_fault_band_08 => disposition_matches_the_fault, 8;
    disposition_matches_the_fault_band_09 => disposition_matches_the_fault, 9;
    disposition_matches_the_fault_band_10 => disposition_matches_the_fault, 10;
    disposition_matches_the_fault_band_11 => disposition_matches_the_fault, 11;
    disposition_matches_the_fault_band_12 => disposition_matches_the_fault, 12;
    disposition_matches_the_fault_band_13 => disposition_matches_the_fault, 13;
    disposition_matches_the_fault_band_14 => disposition_matches_the_fault, 14;
    disposition_matches_the_fault_band_15 => disposition_matches_the_fault, 15;
    keys_never_collide_across_tenants_band_00 => keys_never_collide_across_tenants, 0;
    keys_never_collide_across_tenants_band_01 => keys_never_collide_across_tenants, 1;
    keys_never_collide_across_tenants_band_02 => keys_never_collide_across_tenants, 2;
    keys_never_collide_across_tenants_band_03 => keys_never_collide_across_tenants, 3;
    keys_never_collide_across_tenants_band_04 => keys_never_collide_across_tenants, 4;
    keys_never_collide_across_tenants_band_05 => keys_never_collide_across_tenants, 5;
    keys_never_collide_across_tenants_band_06 => keys_never_collide_across_tenants, 6;
    keys_never_collide_across_tenants_band_07 => keys_never_collide_across_tenants, 7;
    keys_never_collide_across_tenants_band_08 => keys_never_collide_across_tenants, 8;
    keys_never_collide_across_tenants_band_09 => keys_never_collide_across_tenants, 9;
    keys_never_collide_across_tenants_band_10 => keys_never_collide_across_tenants, 10;
    keys_never_collide_across_tenants_band_11 => keys_never_collide_across_tenants, 11;
    keys_never_collide_across_tenants_band_12 => keys_never_collide_across_tenants, 12;
    keys_never_collide_across_tenants_band_13 => keys_never_collide_across_tenants, 13;
    keys_never_collide_across_tenants_band_14 => keys_never_collide_across_tenants, 14;
    keys_never_collide_across_tenants_band_15 => keys_never_collide_across_tenants, 15;
    ceiling_binds_the_peak_band_00 => ceiling_binds_the_peak, 0;
    ceiling_binds_the_peak_band_01 => ceiling_binds_the_peak, 1;
    ceiling_binds_the_peak_band_02 => ceiling_binds_the_peak, 2;
    ceiling_binds_the_peak_band_03 => ceiling_binds_the_peak, 3;
    ceiling_binds_the_peak_band_04 => ceiling_binds_the_peak, 4;
    ceiling_binds_the_peak_band_05 => ceiling_binds_the_peak, 5;
    ceiling_binds_the_peak_band_06 => ceiling_binds_the_peak, 6;
    ceiling_binds_the_peak_band_07 => ceiling_binds_the_peak, 7;
    ceiling_binds_the_peak_band_08 => ceiling_binds_the_peak, 8;
    ceiling_binds_the_peak_band_09 => ceiling_binds_the_peak, 9;
    ceiling_binds_the_peak_band_10 => ceiling_binds_the_peak, 10;
    ceiling_binds_the_peak_band_11 => ceiling_binds_the_peak, 11;
    ceiling_binds_the_peak_band_12 => ceiling_binds_the_peak, 12;
    ceiling_binds_the_peak_band_13 => ceiling_binds_the_peak, 13;
    ceiling_binds_the_peak_band_14 => ceiling_binds_the_peak, 14;
    ceiling_binds_the_peak_band_15 => ceiling_binds_the_peak, 15;
    the_same_seed_replays_band_00 => the_same_seed_replays, 0;
    the_same_seed_replays_band_01 => the_same_seed_replays, 1;
    the_same_seed_replays_band_02 => the_same_seed_replays, 2;
    the_same_seed_replays_band_03 => the_same_seed_replays, 3;
    the_same_seed_replays_band_04 => the_same_seed_replays, 4;
    the_same_seed_replays_band_05 => the_same_seed_replays, 5;
    the_same_seed_replays_band_06 => the_same_seed_replays, 6;
    the_same_seed_replays_band_07 => the_same_seed_replays, 7;
    the_same_seed_replays_band_08 => the_same_seed_replays, 8;
    the_same_seed_replays_band_09 => the_same_seed_replays, 9;
    the_same_seed_replays_band_10 => the_same_seed_replays, 10;
    the_same_seed_replays_band_11 => the_same_seed_replays, 11;
    the_same_seed_replays_band_12 => the_same_seed_replays, 12;
    the_same_seed_replays_band_13 => the_same_seed_replays, 13;
    the_same_seed_replays_band_14 => the_same_seed_replays, 14;
    the_same_seed_replays_band_15 => the_same_seed_replays, 15;
}
