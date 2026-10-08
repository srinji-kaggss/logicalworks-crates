//! Simulated hosts, partitions, and clock skew against the fenced dispatch
//! path (#319).
//!
//! One seed drives three executor hosts against ONE lease authority and ONE
//! durable store — one domain, because two authorities for one domain would be
//! split-brain by construction. The faults injected per step are partitions (a
//! host cut off from the authority), clock skew (a host perceiving a shifted
//! epoch), coordinator loss (a lease dropped with runs open), and store
//! restarts (a second handle proving what the first committed). The real
//! [`AuthorityHandle`](lgwks_bot::dist_lease::AuthorityHandle),
//! [`LoopbackChannel`](lgwks_bot::dist_lease::LoopbackChannel),
//! [`WorkQueue`](lgwks_bot::dist_lease::WorkQueue),
//! [`Coordinator`](lgwks_bot::dist_lease::Coordinator) and
//! [`TenantStore`](lgwks_bot::tenant_store::TenantStore) are driven against an
//! independent model that predicts every validation, every commit, and every
//! refusal.
//!
//! The invariants held every step, under every fault:
//!
//! - **Fencing.** No commit ever lands under a lease the authority deems
//!   stale at that step: each commit records the authority epoch beside the
//!   lease epoch, and the two are equal.
//! - **Partition safety.** A partitioned host's validations all fail closed,
//!   and a partitioned host commits nothing.
//! - **Skew irrelevance.** Hosts perceive a shifted epoch, but perception
//!   never decides a grant: validation does. Whenever perception disagrees
//!   with the authority — behind through lag and skew, or ahead through
//!   positive skew — the grant follows the validation, and the trace counts
//!   the overruling.
//! - **Bounded queues.** Every host queue holds at most its capacity, and
//!   every refused push hands its item back.
//! - **Durable recovery.** A killed holder's open runs abandon with unknown
//!   outcomes, exactly once.
//!
//! One seed, exact replay: the same seed run twice gives the same trace hash,
//! and the first two seeds diverge.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;

use lgwks_std::hash::Hasher;
use lgwks_std::seeded::Seeded;

use lgwks_bot::dist_lease::{
    AuthorityHandle, Coordinator, DispatchError, GrantChannel, HostId, Lease, LoopbackChannel,
    WorkQueue,
};
use lgwks_bot::tenant_store::{RunStart, TenantStore};

use crate::scratch::Scratch;

/// The test result: fixtures bubble with `?`, like the rest of the estate.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// How many seeds the family sweeps.
const SEEDS: u64 = 24;

/// How many steps one seed drives.
const STEPS_PER_SEED: usize = 40;

/// How many executor hosts one seed runs.
const HOSTS: usize = 3;

/// What every host queue holds at most.
const QUEUE_CAPACITY: usize = 4;

/// One executor host: its lease, its perception, its queue, and its counters.
struct SimHost {
    /// The host's name, `host-{id}`.
    name: String,
    /// The lease the host currently holds, if any.
    lease: Option<Lease>,
    /// The highest announced epoch the host received.
    cache: u64,
    /// The host's clock skew in epochs: perception is `cache` shifted by
    /// this. Positive skew perceives ahead, negative behind.
    skew: i64,
    /// The host's dispatch queue of run ids.
    queue: WorkQueue<String>,
    /// Grants that took effect for this host.
    commits: u64,
    /// Grants refused as stale for this host.
    refusals: u64,
    /// Validations lost to partition for this host.
    waits: u64,
    /// Grants that proceeded while perception said otherwise.
    overruled: u64,
}

/// One modeled lane: pending, dispatched, or ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelLane {
    /// Admitted but not yet dispatched.
    Pending,
    /// Dispatched but not yet ended.
    Dispatched,
    /// Ended.
    Ended,
}

/// One modeled run: its host, its lanes, and whether it closed.
#[derive(Debug, Clone)]
struct ModelRun {
    /// Which host began the run.
    host: usize,
    /// The lanes in plan order.
    lanes: Vec<ModelLane>,
    /// Whether the run closed, and with what verdict.
    close: Option<String>,
}

/// The independent model: the epoch, the holder, the queues, and the runs.
#[derive(Debug, Default)]
struct Model {
    /// The epoch the authority must report.
    epoch: u64,
    /// The host the authority must name as holder, if any.
    holder: Option<usize>,
    /// The queue contents each host must hold, oldest first.
    queues: BTreeMap<usize, VecDeque<String>>,
    /// Every run the seed began, by id.
    runs: BTreeMap<String, ModelRun>,
    /// How many runs the seed began, naming the next id.
    begun: usize,
}

/// The name of one executor host.
fn host_name(id: usize) -> String {
    format!("host-{id}")
}

/// The tenant one host's runs belong to.
fn host_tenant(id: usize) -> String {
    format!("sim-tenant-{id}")
}

/// What the host perceives: its cached epoch shifted by its skew.
///
/// Perception is read-only display state: it never decides a grant, which is
/// what makes skew a fault the fence absorbs rather than a second fence.
fn perceived(host: &SimHost) -> u64 {
    host.cache.saturating_add_signed(host.skew)
}

/// Feed one observation into the trace: the step's tag and the domain facts,
/// never a path or a wall time.
fn observe(hasher: &mut Hasher, tag: &str, text: &str) {
    hasher.write_framed(tag.as_bytes());
    hasher.write_framed(text.as_bytes());
}

/// The skew a step draws: minus two through two, named without a cast.
fn draw_skew(drawn: u64) -> i64 {
    match drawn {
        0 => -2,
        1 => -1,
        2 => 0,
        3 => 1,
        _ => 2,
    }
}

/// Announce the current epoch to every connected host: the broadcast an
/// acquire and a revoke both emit, dropped on partitioned links.
fn announce(hosts: &mut [SimHost], cut: &BTreeSet<usize>, epoch: u64) {
    for (id, host) in hosts.iter_mut().enumerate() {
        if !cut.contains(&id) && host.cache < epoch {
            host.cache = epoch;
        }
    }
}

/// The authority must report exactly what the model holds: the epoch and the
/// holder, after every mint and every revoke.
fn check_authority(authority: &AuthorityHandle, model: &Model, seed: u64) -> TestResult {
    assert_eq!(
        authority.epoch(),
        model.epoch,
        "seed {seed}: the authority epoch must be the modeled one"
    );
    let holder = authority.holder();
    match (holder, model.holder) {
        (None, None) => Ok::<(), Box<dyn Error>>(()),
        (Some(found), Some(wanted)) => {
            assert_eq!(
                found,
                HostId::new(&host_name(wanted)),
                "seed {seed}: the authority holder must be the modeled one"
            );
            Ok(())
        }
        (found, wanted) => Err(format!(
            "seed {seed}: the authority holder {found:?} must be the modeled {wanted:?}"
        )
        .into()),
    }
}

/// The verdict the trace names: the recorded one, or the open fact when the
/// run has none.
fn trace_verdict(verdict: Option<&str>) -> &str {
    match verdict {
        Some(found) => found,
        None => "open",
    }
}

/// The per-host drivers one seed step reaches: the hosts, their channels to
/// the authority, and their coordinators over the shared store.
///
/// Grouped because the three travel together to every step, and threading
/// three parallel vectors through every step signature is three chances to
/// hand one host's channel to another host's coordinator.
struct HostSet {
    /// The executor hosts, by id.
    hosts: Vec<SimHost>,
    /// One channel per host over the shared authority.
    channels: Vec<LoopbackChannel>,
    /// One coordinator per host over the shared store and authority.
    coordinators: Vec<Coordinator>,
}

/// Drive one seed and return its trace hash.
fn run_seed(seed: u64) -> TestResult<String> {
    let scratch = Scratch::new("sim-dist-lease")?;
    let dir = scratch.path();
    let store = TenantStore::open(dir)?;
    let authority = AuthorityHandle::new();
    let mut set = HostSet {
        hosts: Vec::new(),
        channels: Vec::new(),
        coordinators: Vec::new(),
    };
    for id in 0..HOSTS {
        set.hosts.push(SimHost {
            name: host_name(id),
            lease: None,
            cache: 0,
            skew: 0,
            queue: WorkQueue::new(QUEUE_CAPACITY)
                .map_err(|error| format!("the host queue must build: {error}"))?,
            commits: 0,
            refusals: 0,
            waits: 0,
            overruled: 0,
        });
        set.channels.push(LoopbackChannel::new(authority.clone()));
        set.coordinators.push(Coordinator::with_authority(
            store.clone(),
            &host_name(id),
            authority.clone(),
        ));
    }
    let mut model = Model::default();
    let mut cut: BTreeSet<usize> = BTreeSet::new();
    let mut rng = Seeded::from_seed(seed);
    let mut hasher = Hasher::new();
    hasher.write_framed(&seed.to_le_bytes());
    let mut step_no = 0usize;
    while step_no < STEPS_PER_SEED {
        step_no = step_no.saturating_add(1);
        let choice = rng.below(100)?;
        let hid = rng.index(HOSTS)?;
        if choice < 20 {
            let lease = authority.acquire(&host_name(hid));
            announce(&mut set.hosts, &cut, lease.epoch());
            set.hosts[hid].lease = Some(lease);
            model.epoch = model.epoch.saturating_add(1);
            model.holder = Some(hid);
            check_authority(&authority, &model, seed)?;
            observe(
                &mut hasher,
                "acquire",
                &format!("host-{hid}/{}", model.epoch),
            );
        } else if choice < 40 {
            grant_step(&mut set, &cut, &mut model, &mut rng, seed, hid, &mut hasher)?;
        } else if choice < 50 {
            if cut.contains(&hid) {
                cut.remove(&hid);
                set.channels[hid].set_partitioned(false);
                announce(&mut set.hosts, &cut, authority.epoch());
                observe(&mut hasher, "heal", &format!("host-{hid}"));
            } else {
                cut.insert(hid);
                set.channels[hid].set_partitioned(true);
                observe(&mut hasher, "partition", &format!("host-{hid}"));
            }
        } else if choice < 58 {
            set.hosts[hid].skew = draw_skew(rng.below(5)?);
            observe(
                &mut hasher,
                "skew",
                &format!("host-{hid}/{}", set.hosts[hid].skew),
            );
        } else if choice < 68 {
            let item = format!("work-{begun:04}", begun = model.begun);
            model.begun = model.begun.saturating_add(1);
            let queue = model.queues.entry(hid).or_default();
            match set.hosts[hid].queue.try_push(item.clone()) {
                Ok(()) => {
                    queue.push_back(item.clone());
                    observe(&mut hasher, "push", &format!("host-{hid}/ok"));
                }
                Err(refused) => {
                    assert_eq!(
                        refused.into_item(),
                        item,
                        "seed {seed}: a refused push must hand its item back"
                    );
                    observe(&mut hasher, "push", &format!("host-{hid}/full"));
                }
            }
            assert!(
                set.hosts[hid].queue.len() <= QUEUE_CAPACITY,
                "seed {seed}: host-{hid} must never exceed its bound"
            );
            assert_eq!(
                set.hosts[hid].queue.len(),
                queue.len(),
                "seed {seed}: host-{hid} must hold its modeled queue"
            );
        } else if choice < 76 {
            let queue = model.queues.entry(hid).or_default();
            let got = set.hosts[hid].queue.pop();
            let wanted = queue.pop_front();
            assert_eq!(
                got, wanted,
                "seed {seed}: host-{hid} must pop its modeled queue"
            );
            observe(&mut hasher, "pop", &format!("host-{hid}/{}", got.is_some()));
        } else if choice < 84 {
            set.hosts[hid].lease = None;
            observe(&mut hasher, "kill", &format!("host-{hid}"));
            let rid = rng.index(HOSTS)?;
            let partitioned = cut.contains(&rid);
            if rid != hid && partitioned {
                set.hosts[rid].waits = set.hosts[rid].waits.saturating_add(1);
            } else if rid != hid
                && let Some(lease) = set.hosts[rid].lease.clone()
            {
                if !(set.channels[rid].validate(&lease)?) {
                    set.hosts[rid].refusals = set.hosts[rid].refusals.saturating_add(1);
                } else {
                    let dead = host_name(hid);
                    let closed = set.coordinators[rid].recover(&lease, &dead, "t-recover")?;
                    let mut wanted = 0usize;
                    for run in model.runs.values_mut() {
                        if run.host == hid && run.close.is_none() {
                            run.close = Some("UNMEASURED".to_owned());
                            for slot in &mut run.lanes {
                                if *slot != ModelLane::Ended {
                                    *slot = ModelLane::Ended;
                                }
                            }
                            wanted = wanted.saturating_add(1);
                        }
                    }
                    assert_eq!(
                        closed, wanted,
                        "seed {seed}: recovery must close exactly the open runs"
                    );
                    assert_eq!(
                        set.coordinators[rid].recover(&lease, &dead, "t-recover-again")?,
                        0,
                        "seed {seed}: a second recovery must close nothing"
                    );
                }
            }
        } else if choice < 92 {
            if let Some(lease) = set.hosts[hid].lease.clone() {
                if cut.contains(&hid) {
                    set.hosts[hid].waits = set.hosts[hid].waits.saturating_add(1);
                    observe(&mut hasher, "begin", &format!("host-{hid}/wait"));
                } else {
                    let run = format!("seed-{seed}-host-{hid}-{begun:04}", begun = model.begun);
                    model.begun = model.begun.saturating_add(1);
                    let tenant = host_tenant(hid);
                    let hostname = set.hosts[hid].name.clone();
                    let start =
                        RunStart::new(&run, &tenant, "/repo", &hostname, 1, "lock", "t-begin");
                    if !set.channels[hid].validate(&lease)? {
                        set.hosts[hid].refusals = set.hosts[hid].refusals.saturating_add(1);
                        match set.coordinators[hid].begin_run(&lease, &start, &["fmt", "clippy"]) {
                            Err(DispatchError::Lease(_)) => Ok::<(), Box<dyn Error>>(()),
                            Err(other) => {
                                return Err(format!(
                                    "seed {seed}: a stale begin must fence, got {other}"
                                )
                                .into());
                            }
                            Ok(()) => {
                                return Err(
                                    format!("seed {seed}: a stale begin must not commit").into()
                                );
                            }
                        }?;
                        observe(&mut hasher, "begin", &format!("host-{hid}/stale"));
                    } else {
                        set.coordinators[hid].begin_run(&lease, &start, &["fmt", "clippy"])?;
                        model.runs.insert(
                            run.clone(),
                            ModelRun {
                                host: hid,
                                lanes: vec![ModelLane::Pending, ModelLane::Pending],
                                close: None,
                            },
                        );
                        set.hosts[hid].commits = set.hosts[hid].commits.saturating_add(1);
                        observe(&mut hasher, "begin", &format!("host-{hid}/ok"));
                    }
                }
            } else {
                observe(&mut hasher, "begin", &format!("host-{hid}/no-lease"));
            }
        } else {
            let probe = TenantStore::open(dir)?;
            for tenant_no in 0..HOSTS {
                let rows = probe.runs(&host_tenant(tenant_no), 500)?;
                let modeled = model
                    .runs
                    .iter()
                    .filter(|&(_, run)| run.host == tenant_no)
                    .count();
                assert_eq!(
                    rows.len(),
                    modeled,
                    "seed {seed}: a reopened handle must read the modeled runs"
                );
            }
            observe(&mut hasher, "reopen", "ok");
        }
    }
    for tenant_no in 0..HOSTS {
        let rows = store.runs(&host_tenant(tenant_no), 500)?;
        for row in &rows {
            let held = model
                .runs
                .get(row.run_id())
                .ok_or_else(|| format!("seed {seed}: the model lost {}", row.run_id()))?;
            assert_eq!(
                row.verdict(),
                held.close.as_deref(),
                "seed {seed}: {} must read its modeled verdict",
                row.run_id()
            );
            observe(
                &mut hasher,
                "final",
                &format!("{}/{}", row.run_id(), trace_verdict(row.verdict())),
            );
        }
    }
    for (hid, host) in set.hosts.iter().enumerate() {
        observe(
            &mut hasher,
            "counters",
            &format!(
                "host-{hid}/{}/{}/{}/{}",
                host.commits, host.refusals, host.waits, host.overruled
            ),
        );
    }
    Ok(hasher.finalize().to_hex())
}

/// One grant attempt by one host: validate through the channel, then commit
/// through the coordinator only past a current lease.
fn grant_step(
    set: &mut HostSet,
    cut: &BTreeSet<usize>,
    model: &mut Model,
    rng: &mut Seeded,
    seed: u64,
    hid: usize,
    hasher: &mut Hasher,
) -> TestResult {
    let Some(lease) = set.hosts[hid].lease.clone() else {
        observe(&mut *hasher, "grant", &format!("host-{hid}/no-lease"));
        return Ok(());
    };
    if set.channels[hid].partitioned() {
        assert!(
            cut.contains(&hid),
            "seed {seed}: a partitioned channel must be a cut host"
        );
        match set.channels[hid].validate(&lease) {
            Err(_) => Ok::<(), Box<dyn Error>>(()),
            Ok(_) => {
                return Err(format!("seed {seed}: a partitioned channel must not answer").into());
            }
        }?;
        set.hosts[hid].waits = set.hosts[hid].waits.saturating_add(1);
        observe(&mut *hasher, "grant", &format!("host-{hid}/wait"));
        return Ok(());
    }
    let validated = set.channels[hid].validate(&lease)?;
    let predicted = lease.epoch() == model.epoch;
    assert_eq!(
        validated, predicted,
        "seed {seed}: host-{hid} must validate exactly as the model predicts"
    );
    if perceived(&set.hosts[hid]) != model.epoch {
        set.hosts[hid].overruled = set.hosts[hid].overruled.saturating_add(1);
    }
    observe(
        hasher,
        "grant",
        &format!("host-{hid}/{validated}/saw-{}", perceived(&set.hosts[hid])),
    );
    let Some((run, lane, end)) = pick_grant(rng, model, hid)? else {
        return Ok(());
    };
    if !validated {
        set.hosts[hid].refusals = set.hosts[hid].refusals.saturating_add(1);
        let refused = if end {
            set.coordinators[hid].end_lane(&lease, &run, lane, "pass", "detail", b"tail")
        } else {
            set.coordinators[hid].dispatch_lane(&lease, &run, lane)
        };
        match refused {
            Err(DispatchError::Lease(_)) => Ok::<(), Box<dyn Error>>(()),
            Err(other) => {
                return Err(format!("seed {seed}: a stale grant must fence, got {other}").into());
            }
            Ok(()) => return Err(format!("seed {seed}: a stale grant must not commit").into()),
        }?;
        return Ok(());
    }
    let at = set.coordinators[hid].handle().epoch();
    if end {
        let must_commit = lane_must_end(model, &run, lane)?;
        match set.coordinators[hid].end_lane(&lease, &run, lane, "pass", "detail", b"tail") {
            Ok(()) => {
                assert!(must_commit, "seed {seed}: a committed end must be modeled");
                end_model_lane(model, &run, lane)?;
                assert_eq!(
                    at,
                    lease.epoch(),
                    "seed {seed}: no commit under a stale epoch"
                );
                set.hosts[hid].commits = set.hosts[hid].commits.saturating_add(1);
            }
            Err(_) => assert!(!must_commit, "seed {seed}: a refused end must be modeled"),
        }
    } else {
        let must_commit = lane_must_dispatch(model, &run, lane)?;
        match set.coordinators[hid].dispatch_lane(&lease, &run, lane) {
            Ok(()) => {
                assert!(
                    must_commit,
                    "seed {seed}: a committed dispatch must be modeled"
                );
                dispatch_model_lane(model, &run, lane)?;
                assert_eq!(
                    at,
                    lease.epoch(),
                    "seed {seed}: no commit under a stale epoch"
                );
                set.hosts[hid].commits = set.hosts[hid].commits.saturating_add(1);
            }
            Err(_) => assert!(
                !must_commit,
                "seed {seed}: a refused dispatch must be modeled"
            ),
        }
    }
    Ok(())
}

/// One grant's target drawn from the host's open modeled runs: a run and a
/// lane, ending or dispatching by draw.
fn pick_grant(
    rng: &mut Seeded,
    model: &Model,
    hid: usize,
) -> TestResult<Option<(String, usize, bool)>> {
    let ids: Vec<String> = model
        .runs
        .iter()
        .filter(|&(_, run)| run.host == hid && run.close.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() {
        return Ok(None);
    }
    let picked = rng.index(ids.len())?;
    let drawn = rng.below(3)?;
    let lane =
        usize::try_from(drawn).map_err(|error| format!("the lane draw does not fit: {error}"))?;
    let end = rng.below(2)? == 0;
    Ok(Some((ids[picked].clone(), lane, end)))
}

/// Whether the modeled lane must dispatch: present and pending.
fn lane_must_dispatch(model: &Model, run: &str, lane: usize) -> TestResult<bool> {
    let held = model
        .runs
        .get(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    Ok(held.lanes.get(lane) == Some(&ModelLane::Pending))
}

/// Whether the modeled lane must end: present and unended.
fn lane_must_end(model: &Model, run: &str, lane: usize) -> TestResult<bool> {
    let held = model
        .runs
        .get(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    Ok(match held.lanes.get(lane) {
        None => false,
        Some(&ModelLane::Ended) => false,
        Some(_) => true,
    })
}

/// Record the modeled dispatch of one lane.
fn dispatch_model_lane(model: &mut Model, run: &str, lane: usize) -> TestResult<()> {
    let held = model
        .runs
        .get_mut(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    let Some(slot) = held.lanes.get_mut(lane) else {
        return Err(format!("the model lost lane {lane} of {run}").into());
    };
    *slot = ModelLane::Dispatched;
    Ok(())
}

/// Record the modeled end of one lane.
fn end_model_lane(model: &mut Model, run: &str, lane: usize) -> TestResult<()> {
    let held = model
        .runs
        .get_mut(run)
        .ok_or_else(|| format!("the model lost {run}"))?;
    let Some(slot) = held.lanes.get_mut(lane) else {
        return Err(format!("the model lost lane {lane} of {run}").into());
    };
    *slot = ModelLane::Ended;
    Ok(())
}

/// Every seed replays to the same trace hash, and the first two seeds
/// diverge: one seed, exact replay, and a nondeterministic run fails even
/// when every assertion happens to pass.
#[test]
fn partitions_and_skew_never_break_the_fence() -> TestResult {
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
