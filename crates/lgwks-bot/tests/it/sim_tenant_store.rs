//! Seeded histories against the keyed run store (#317).
//!
//! One seed decides the tenants, every run and lane write, abandonments, and
//! process restarts (the store dropped and reopened from its file). The real
//! [`TenantStore`](lgwks_bot::tenant_store::TenantStore) is driven against an
//! independent model: after every step each touched tenant must list exactly
//! its own runs, newest-first, with the model's verdicts, lanes and policy.
//!
//! A restart here is a process ending, not power loss: every commit is synced
//! before it is acknowledged, so dropping the handle and reopening reads back
//! exactly what the model holds. A failing seed is named and replays exactly;
//! the same seed run twice gives the same trace hash, and the first two seeds
//! diverge, so a nondeterministic run fails even when every assertion happens
//! to pass.

use std::collections::BTreeMap;
use std::error::Error;

use lgwks_std::hash::Hasher;
use lgwks_std::seeded::Seeded;

use lgwks_bot::tenant_store::{RunStart, TenantStore, TenantStoreError};

use crate::scratch::Scratch;

/// The test result: fixtures bubble with `?`, like the rest of the estate.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// How many seeds the family sweeps.
const SEEDS: u64 = 40;

/// How many steps one seed drives.
const STEPS_PER_SEED: usize = 60;

/// The tenants a seed draws from.
const TENANTS: [&str; 3] = ["acme", "widget", "soylent"];

/// The verdicts a finished run draws from. Abandonment is never drawn: it is
/// fixed by the store, and drawing it would let the model agree with itself.
const VERDICTS: [&str; 2] = ["GO", "NO-GO"];

/// The lane outcomes an ended lane draws from.
const OUTCOMES: [&str; 3] = ["pass", "fail", "timeout"];

/// One lane as the model holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelLane {
    /// Admitted but not yet dispatched.
    Pending,
    /// Dispatched but not yet ended.
    Dispatched,
    /// Ended with its outcome.
    Ended {
        /// The outcome the lane ended with.
        outcome: String,
    },
}

/// How a modeled run closed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelClose {
    /// The coordinator finished it.
    Finished {
        /// The verdict it closed with.
        verdict: String,
    },
    /// A successor abandoned it.
    Abandoned,
}

/// One run as the model holds it.
#[derive(Debug, Clone)]
struct ModelRun {
    /// Which tenant owns the run.
    tenant: usize,
    /// When the run started, ordering its listing.
    started: String,
    /// The lanes in plan order.
    lanes: Vec<ModelLane>,
    /// How the run closed, if it did.
    close: Option<ModelClose>,
}

/// What the seed must have built: runs, policies, and the moment counters
/// that keep ids and timestamps unique inside the seed.
#[derive(Debug, Default)]
struct Model {
    /// Every run the seed began, by id.
    runs: BTreeMap<String, ModelRun>,
    /// Every tenant that ran init.
    policies: BTreeMap<usize, ()>,
    /// How many runs the seed began, naming the next id.
    begun: usize,
    /// How many moments the seed minted, naming the next one.
    moments: usize,
}

/// A moment string that orders like the counter it carries.
fn moment(counter: usize) -> String {
    format!("s-{counter:06}")
}

/// The next moment, minted once: started and finished moments share one
/// counter, so no two frames in a seed name the same moment and ordering
/// stays total.
fn next_moment(model: &mut Model) -> String {
    let done = moment(model.moments);
    model.moments = model.moments.saturating_add(1);
    done
}

/// The next run id: unique inside the seed, prefixed by tenant so a shared
/// prefix is ambiguous on purpose in later steps.
fn next_run(model: &mut Model, tenant: usize) -> String {
    let id = format!("tenant-{tenant}-run-{begun:04}", begun = model.begun);
    model.begun = model.begun.saturating_add(1);
    id
}

/// The verdict a modeled run carries: its finish verdict, or the fixed
/// abandonment verdict.
fn model_verdict(close: &Option<ModelClose>) -> Option<&str> {
    match *close {
        None => None,
        Some(ModelClose::Finished { ref verdict }) => Some(verdict.as_str()),
        Some(ModelClose::Abandoned) => Some("UNMEASURED"),
    }
}

/// Whether the modeled lane reads ended.
fn lane_ended(lane: &ModelLane) -> bool {
    matches!(*lane, ModelLane::Ended { .. })
}

/// Feed one observation into the trace: the step's tag and what the store
/// answered, never a path or a wall time.
fn observe(hasher: &mut Hasher, tag: &str, text: &str) {
    hasher.write_framed(tag.as_bytes());
    hasher.write_framed(text.as_bytes());
}

/// The verdict the trace names for one row: the recorded verdict, or the
/// open fact when the run has none.
fn trace_verdict(verdict: Option<&str>) -> &str {
    match verdict {
        Some(found) => found,
        None => "open",
    }
}

/// The store's answer for `tenant` must be exactly the model's: the same run
/// ids in the same newest-first order, with the same verdicts, and the same
/// lanes for every listed run.
fn check_tenant(
    store: &TenantStore,
    model: &Model,
    tenant_no: usize,
    hasher: &mut Hasher,
) -> TestResult {
    let tenant = TENANTS[tenant_no];
    let mut expected: Vec<(&String, &ModelRun)> = model
        .runs
        .iter()
        .filter(|&(_, run)| run.tenant == tenant_no)
        .collect();
    expected.sort_by(|left, right| {
        right
            .1
            .started
            .cmp(&left.1.started)
            .then_with(|| right.0.cmp(left.0))
    });
    let rows = store.runs(tenant, 500)?;
    assert_eq!(
        rows.len(),
        expected.len(),
        "tenant {tenant} must list exactly its own runs"
    );
    for (&(id, run), row) in expected.iter().zip(rows.iter()) {
        assert_eq!(
            row.run_id(),
            id.as_str(),
            "tenant {tenant} must list its own ids"
        );
        assert_eq!(
            row.verdict(),
            model_verdict(&run.close),
            "tenant {tenant} must read the modeled verdict for {id}"
        );
        let lanes = store.lanes(id)?;
        assert_eq!(
            lanes.len(),
            run.lanes.len(),
            "run {id} must hold its modeled lane count"
        );
        for (slot, modeled) in lanes.iter().zip(run.lanes.iter()) {
            let state = match *modeled {
                ModelLane::Pending => "pending",
                ModelLane::Dispatched => "dispatched",
                ModelLane::Ended { .. } => "ended",
            };
            assert_eq!(
                slot.state(),
                state,
                "run {id} must hold its modeled lane states"
            );
            observe(&mut *hasher, "lane", &format!("{id}/{}/{state}", slot.id()));
        }
        observe(
            &mut *hasher,
            "run",
            &format!("{id}/{}", trace_verdict(row.verdict())),
        );
    }
    observe(&mut *hasher, "tenant", &format!("{tenant}/{}", rows.len()));
    Ok(())
}

/// The store's finish of one run: the verdict the seed drew, committed when
/// the run is open and refused exactly as the model predicts.
fn close_finished(
    store: &TenantStore,
    model: &mut Model,
    seed: u64,
    rng: &mut Seeded,
    run: &str,
    hasher: &mut Hasher,
) -> TestResult {
    let picked = rng.index(VERDICTS.len())?;
    let finished = next_moment(&mut *model);
    let open = close_model(
        &mut *model,
        run,
        ModelClose::Finished {
            verdict: VERDICTS[picked].to_owned(),
        },
    )?;
    match store.finish_run(run, VERDICTS[picked], &finished, "record", "seal") {
        Ok(closed) => assert_eq!(closed, open, "seed {seed}: finish must match the model"),
        Err(TenantStoreError::NoSuchRun { .. }) => {
            assert!(!open, "seed {seed}: a refused finish must be modeled")
        }
        Err(other) => return Err(format!("seed {seed}: finish refused: {other}").into()),
    }
    observe(hasher, "finish", &format!("{run}/{open}"));
    Ok(())
}

/// The store's abandonment of one run: the fixed unknown close, committed
/// when the run is open and refused exactly as the model predicts.
fn close_abandoned(
    store: &TenantStore,
    model: &mut Model,
    seed: u64,
    run: &str,
    hasher: &mut Hasher,
) -> TestResult {
    let done = next_moment(&mut *model);
    let open = abandon_model(&mut *model, run)?;
    match store.abandon(run, &done) {
        Ok(closed) => assert_eq!(closed, open, "seed {seed}: abandon must match the model"),
        Err(TenantStoreError::NoSuchRun { .. }) => {
            assert!(!open, "seed {seed}: a refused abandon must be modeled")
        }
        Err(other) => return Err(format!("seed {seed}: abandon refused: {other}").into()),
    }
    observe(hasher, "abandon", &format!("{run}/{open}"));
    Ok(())
}

/// Drive one seed against a fresh directory and return its trace hash.
fn run_seed(seed: u64) -> TestResult<String> {
    let scratch = Scratch::new("sim-tenant-store")?;
    let dir = scratch.path();
    let mut store = TenantStore::open(dir)?;
    let mut model = Model::default();
    let mut rng = Seeded::from_seed(seed);
    let mut hasher = Hasher::new();
    hasher.write_framed(&seed.to_le_bytes());
    let mut step_no = 0usize;
    while step_no < STEPS_PER_SEED {
        step_no = step_no.saturating_add(1);
        let choice = rng.below(100)?;
        let tenant_no = rng.index(TENANTS.len())?;
        let tenant = TENANTS[tenant_no];
        if choice < 8 {
            let done = model.policies.insert(tenant_no, ()).is_some();
            match store.init_policy(tenant, "[\"fmt\"]", 1, "2026-10-07T00:00:00Z") {
                Ok(()) => assert!(!done, "seed {seed}: a first init must be novel"),
                Err(TenantStoreError::AlreadyInitialised { .. }) => {
                    assert!(done, "seed {seed}: a refused init must be modeled")
                }
                Err(other) => return Err(format!("seed {seed}: init refused: {other}").into()),
            }
            observe(&mut hasher, "init", &format!("{tenant}/{done}"));
        } else if choice < 38 {
            let run = next_run(&mut model, tenant_no);
            let lane_count = 1u64.saturating_add(rng.below(3)?);
            let mut ids: Vec<String> = Vec::new();
            let mut lane_no = 0u64;
            while lane_no < lane_count {
                ids.push(format!("lane-{lane_no}"));
                lane_no = lane_no.saturating_add(1);
            }
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let started = next_moment(&mut model);
            let duplicate = model.runs.contains_key(&run);
            match store.begin_run(
                &RunStart::new(&run, tenant, "/repo", "host", 1, "lock", &started),
                &refs,
            ) {
                Ok(()) => {
                    assert!(!duplicate, "seed {seed}: a begun run must be novel");
                    model.runs.insert(
                        run.clone(),
                        ModelRun {
                            tenant: tenant_no,
                            started,
                            lanes: ids.iter().map(|_| ModelLane::Pending).collect(),
                            close: None,
                        },
                    );
                }
                Err(TenantStoreError::DuplicateRun { .. }) => {
                    assert!(duplicate, "seed {seed}: a refused begin must be modeled")
                }
                Err(other) => return Err(format!("seed {seed}: begin refused: {other}").into()),
            }
            observe(&mut hasher, "begin", &format!("{run}/{duplicate}"));
            check_tenant(&store, &model, tenant_no, &mut hasher)?;
        } else if choice < 52 {
            let Some((run, lane)) = pick_lane(&mut rng, &model)? else {
                continue;
            };
            let outcome = step_lane(&mut model, &run, lane, true)?;
            match store.lane_dispatched(&run, lane) {
                Ok(()) => assert!(outcome, "seed {seed}: a committed dispatch must be modeled"),
                Err(TenantStoreError::LaneLost { .. } | TenantStoreError::NoSuchRun { .. }) => {
                    assert!(!outcome, "seed {seed}: a refused dispatch must be modeled")
                }
                Err(other) => return Err(format!("seed {seed}: dispatch refused: {other}").into()),
            }
            observe(&mut hasher, "dispatch", &format!("{run}/{lane}/{outcome}"));
            check_tenant(&store, &model, model_tenant(&model, &run)?, &mut hasher)?;
        } else if choice < 66 {
            let Some((run, lane)) = pick_lane(&mut rng, &model)? else {
                continue;
            };
            let picked = rng.index(OUTCOMES.len())?;
            let outcome_text = OUTCOMES[picked];
            let done = step_lane(&mut model, &run, lane, false)?;
            match store.lane_ended(&run, lane, outcome_text, "detail", b"tail") {
                Ok(()) => {
                    assert!(done, "seed {seed}: a committed end must be modeled");
                    end_model_lane(&mut model, &run, lane, outcome_text)?;
                }
                Err(TenantStoreError::LaneLost { .. } | TenantStoreError::NoSuchRun { .. }) => {
                    assert!(!done, "seed {seed}: a refused end must be modeled")
                }
                Err(other) => return Err(format!("seed {seed}: end refused: {other}").into()),
            }
            observe(&mut hasher, "end", &format!("{run}/{lane}/{done}"));
            check_tenant(&store, &model, model_tenant(&model, &run)?, &mut hasher)?;
        } else if choice < 86 {
            let Some((run, _)) = pick_run(&mut rng, &model)? else {
                continue;
            };
            if rng.below(2)? == 0 {
                close_finished(&store, &mut model, seed, &mut rng, &run, &mut hasher)?;
            } else {
                close_abandoned(&store, &mut model, seed, &run, &mut hasher)?;
            }
            check_tenant(&store, &model, model_tenant(&model, &run)?, &mut hasher)?;
        } else {
            drop(store);
            store = TenantStore::open(dir)?;
            observe(&mut hasher, "reopen", "ok");
            for tenant_no in 0..TENANTS.len() {
                check_tenant(&store, &model, tenant_no, &mut hasher)?;
            }
        }
    }
    Ok(hasher.finalize().to_hex())
}

/// One modeled run id drawn from the model, or `None` when the seed began
/// none yet.
fn pick_run(rng: &mut Seeded, model: &Model) -> TestResult<Option<(String, usize)>> {
    let ids: Vec<String> = model.runs.keys().cloned().collect();
    if ids.is_empty() {
        return Ok(None);
    }
    let picked = rng.index(ids.len())?;
    let run = ids[picked].clone();
    let tenant = model
        .runs
        .get(&run)
        .map(|held| held.tenant)
        .ok_or_else(|| format!("the model lost {run}"))?;
    Ok(Some((run, tenant)))
}

/// One modeled run and lane drawn for a lane step, or `None` when the seed
/// began no run yet.
///
/// One draw for the two lane steps — dispatch and end name different rows
/// but draw the same shape of scenario — so the two cannot drift into
/// drawing different populations.
fn pick_lane(rng: &mut Seeded, model: &Model) -> TestResult<Option<(String, usize)>> {
    let Some((run, _)) = pick_run(rng, model)? else {
        return Ok(None);
    };
    let drawn = rng.below(4)?;
    let lane =
        usize::try_from(drawn).map_err(|error| format!("the lane draw does not fit: {error}"))?;
    Ok(Some((run, lane)))
}

/// The tenant that owns `run` in the model.
fn model_tenant(model: &Model, run: &str) -> TestResult<usize> {
    model
        .runs
        .get(run)
        .map(|held| held.tenant)
        .ok_or_else(|| format!("the model lost {run}").into())
}

/// Move one modeled lane toward its end: dispatch advances pending to
/// dispatched, and the end half advances anything unended. Returns whether
/// the store must commit the step.
fn step_lane(model: &mut Model, run: &str, lane: usize, dispatch: bool) -> TestResult<bool> {
    let held = model
        .runs
        .get_mut(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    let Some(slot) = held.lanes.get_mut(lane) else {
        return Ok(false);
    };
    if dispatch {
        if *slot == ModelLane::Pending {
            *slot = ModelLane::Dispatched;
            return Ok(true);
        }
        return Ok(false);
    }
    if lane_ended(slot) {
        return Ok(false);
    }
    Ok(true)
}

/// Record the modeled end of one lane.
fn end_model_lane(model: &mut Model, run: &str, lane: usize, outcome: &str) -> TestResult<()> {
    let held = model
        .runs
        .get_mut(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    let Some(slot) = held.lanes.get_mut(lane) else {
        return Err(format!("the model lost lane {lane} of {run}").into());
    };
    *slot = ModelLane::Ended {
        outcome: outcome.to_owned(),
    };
    Ok(())
}

/// Close one modeled run with `close`. Returns whether the run was open.
fn close_model(model: &mut Model, run: &str, close: ModelClose) -> TestResult<bool> {
    let held = model
        .runs
        .get_mut(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    if held.close.is_some() {
        return Ok(false);
    }
    held.close = Some(close);
    Ok(true)
}

/// Abandon one modeled run: dispatched and pending lanes read ended.
/// Returns whether the run was open.
fn abandon_model(model: &mut Model, run: &str) -> TestResult<bool> {
    let held = model
        .runs
        .get_mut(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    if held.close.is_some() {
        return Ok(false);
    }
    for slot in &mut held.lanes {
        if *slot == ModelLane::Dispatched || *slot == ModelLane::Pending {
            *slot = ModelLane::Ended {
                outcome: "abandoned".to_owned(),
            };
        }
    }
    held.close = Some(ModelClose::Abandoned);
    Ok(true)
}

/// Every seed replays to the same trace hash, and the first two seeds
/// diverge: one seed, exact replay, and a nondeterministic run fails even
/// when every assertion happens to pass.
#[test]
fn seeded_histories_match_the_model_and_replay() -> TestResult {
    let mut hashes: Vec<String> = Vec::new();
    let mut seed = 0u64;
    while seed < SEEDS {
        let first = run_seed(seed)?;
        let second = run_seed(seed)?;
        assert_eq!(
            first, second,
            "seed {seed} must replay to the same trace hash"
        );
        hashes.push(first);
        seed = seed.saturating_add(1);
    }
    let count = u64::try_from(hashes.len())
        .map_err(|error| format!("the seed count does not fit: {error}"))?;
    assert_eq!(count, SEEDS, "every seed must leave a hash");
    let diverge = match (hashes.first(), hashes.get(1)) {
        (Some(first), Some(second)) => first != second,
        _ => false,
    };
    assert!(diverge, "distinct seeds must drive distinct histories");
    Ok(())
}
