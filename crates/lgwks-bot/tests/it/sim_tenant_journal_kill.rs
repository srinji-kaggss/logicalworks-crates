//! Simulation family: two tenants drive effects through one journal directory, a
//! kill lands mid-run, and the restart loses nothing, duplicates nothing and
//! crosses nothing (#268 item 4).
//!
//! Every scenario drives the **real** [`FileJournal`]: real storage-owner
//! threads, real frames, the real torn-tail repair at open. The seed decides how
//! many attempts each tenant runs, how their appends interleave, where the kill
//! lands, and what the kill left of each tenant's in-flight append:
//!
//! - **between** appends: the file holds exactly the acknowledged events;
//! - **torn**: the in-flight frame is cut at a seeded byte, so the restart must
//!   take it back and the retry must land it;
//! - **landed**: the frame reached the file but its acknowledgment was lost, so
//!   the restart must keep it and the retry, presented at the tail the
//!   controller last saw, must be refused rather than written twice.
//!
//! The model is each tenant's own append plan. After the restart a tenant's
//! journal is exactly its acknowledged prefix plus a landed frame, never an
//! event from the other tenant's plan; after the retry every event of the
//! interrupted append is present exactly once; and after the remaining plan
//! runs, a second reopen reads back the whole plan and nothing else.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `kill_band_00..07` | the three kill shapes over 1,000 seeds, each band swept twice for one trace hash |
//! | `the_kill_bands_cover_every_seed_exactly_once` | the bands partition the seed space |

use crate::sim;

use std::error::Error;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, JournalPosition};

use sim::Band;
use sim::rig::ladder;

/// What a scenario reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this family sweeps, and how many bands it is cut into.
const SEEDS: u64 = 1_000;
const PARTS: u64 = 8;

/// Each tenant numbers its attempts from its own base, so every key a tenant
/// writes is one the other tenant's plan never contains.
const ATTEMPT_BASES: [u64; 2] = [1, 1_000_001];

/// The most attempts one tenant runs in a scenario. Each attempt is a two-rung
/// ladder and each rung before the kill is its own synced append.
const MAX_ATTEMPTS: u32 = 3;

/// What the kill left of a tenant's in-flight append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kill {
    /// No append was in flight.
    Between,
    /// The frame was cut short at a byte offset into it.
    Torn,
    /// The frame is whole on the file; only the acknowledgment was lost.
    Landed,
}

impl Kill {
    /// The name the trace records.
    const fn name(self) -> &'static str {
        match self {
            Self::Between => "between",
            Self::Torn => "torn",
            Self::Landed => "landed",
        }
    }
}

/// One tenant's journal, its plan, and what the kill did to it.
struct Tenant {
    /// The tenant's journal, in the shared directory.
    path: PathBuf,
    /// Every event this tenant appends over the whole run, in order.
    plan: Vec<EffectEvent>,
    /// How many events of `plan` were acknowledged before the kill.
    acknowledged: usize,
    /// The tail the tenant's controller last saw acknowledged.
    seen_tail: JournalPosition,
    /// What the kill left of the next append.
    kill: Kill,
    /// The file length before the in-flight append, and the cut inside it.
    cut: Option<(u64, u64)>,
}

/// The one band of seeds a declared test sweeps.
fn band(index: usize) -> Band {
    sim::bands(SEEDS, PARTS).swap_remove(index)
}

/// A tenant's plan: `attempts` two-rung ladders numbered from its own base.
fn plan(base: u64, attempts: u32) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    let mut events = Vec::new();
    for offset in 0..u64::from(attempts) {
        events.extend(ladder(base.saturating_add(offset))?);
    }
    Ok(events)
}

/// Append one event of `tenant`'s plan through `journal` and acknowledge it.
fn acknowledge(tenant: &mut Tenant, journal: &mut FileJournal) -> TestResult {
    let event = *tenant
        .plan
        .get(tenant.acknowledged)
        .ok_or("an acknowledged append is drawn only while the plan has events left")?;
    journal.compare_and_append_all(&[event])?;
    tenant.acknowledged = tenant.acknowledged.saturating_add(1);
    tenant.seen_tail = journal.tail();
    Ok(())
}

/// The run before the kill: both tenants' journals open at once, their appends
/// interleaved in a seeded order, and the kill drawn at a seeded append.
fn run_until_killed(sim: &mut sim::Sim, dir: &Path) -> Result<Vec<Tenant>, Box<dyn Error>> {
    let mut tenants = Vec::new();
    let mut journals = Vec::new();
    for (index, base) in (0_u32..).zip(ATTEMPT_BASES) {
        let attempts = sim.rng().between(1, MAX_ATTEMPTS);
        let path = sim.journal_path(dir, index);
        let journal = FileJournal::open(&path)?;
        tenants.push(Tenant {
            path,
            plan: plan(base, attempts)?,
            acknowledged: 0,
            seen_tail: journal.tail(),
            kill: Kill::Between,
            cut: None,
        });
        journals.push(journal);
    }
    let total: usize = tenants.iter().map(|tenant| tenant.plan.len()).sum();
    let kill_at = sim.rng().below(u32::try_from(total)?);
    for _ in 0..kill_at {
        let open: Vec<usize> = tenants
            .iter()
            .enumerate()
            .filter(|&(_, tenant)| tenant.acknowledged < tenant.plan.len())
            .map(|(index, _)| index)
            .collect();
        let pick = *open
            .get(usize::try_from(
                sim.rng().below(u32::try_from(open.len())?),
            )?)
            .ok_or("a tenant with appends left is drawn while one exists")?;
        let (Some(tenant), Some(journal)) = (tenants.get_mut(pick), journals.get_mut(pick)) else {
            return Err("every tenant has a journal".into());
        };
        acknowledge(tenant, journal)?;
        sim.trace.record_number("append-tenant", pick);
    }
    for (tenant, journal) in tenants.iter_mut().zip(journals.iter_mut()) {
        if tenant.acknowledged == tenant.plan.len() {
            continue;
        }
        tenant.kill = match sim.rng().below(3) {
            0 => Kill::Between,
            1 => Kill::Torn,
            _ => Kill::Landed,
        };
        if tenant.kill != Kill::Between {
            let before = std::fs::metadata(&tenant.path)?.len();
            let event = *tenant
                .plan
                .get(tenant.acknowledged)
                .ok_or("an in-flight append has an event")?;
            journal.compare_and_append_all(&[event])?;
            let after = std::fs::metadata(&tenant.path)?.len();
            let frame = u32::try_from(after.saturating_sub(before))?;
            if tenant.kill == Kill::Torn {
                let into = u64::from(sim.rng().between(1, frame.saturating_sub(1)));
                tenant.cut = Some((before, into));
            }
        }
    }
    drop(journals);
    for tenant in &tenants {
        if let Some((before, into)) = tenant.cut {
            OpenOptions::new()
                .write(true)
                .open(&tenant.path)?
                .set_len(before.saturating_add(into))?;
        }
        sim.trace.record(tenant.kill.name());
        sim.trace.record_number("acknowledged", tenant.acknowledged);
        sim.trace
            .record_number("cut", tenant.cut.map_or(0, |(_, into)| into));
    }
    Ok(tenants)
}

/// The restart for one tenant: what reopened, the retry of the interrupted
/// append, the rest of the plan, and a second reopen.
fn restart(tenant: &Tenant, other: &[EffectEvent], sim: &mut sim::Sim) -> TestResult {
    let mut journal = FileJournal::open(&tenant.path)?;
    let in_flight = tenant.plan.get(tenant.acknowledged).copied();
    let mut expected: Vec<EffectEvent> = tenant
        .plan
        .iter()
        .take(tenant.acknowledged)
        .copied()
        .collect();
    if tenant.kill == Kill::Landed {
        expected.extend(in_flight);
    }
    let held: Vec<EffectEvent> = journal.events().copied().collect();
    assert_eq!(
        held, expected,
        "after a {:?} kill the journal is the acknowledged prefix plus a landed frame",
        tenant.kill
    );
    assert_eq!(
        journal.torn_tail_repaired(),
        tenant.kill == Kill::Torn,
        "only a torn append is taken back at the restart"
    );
    assert!(
        held.iter().all(|event| !other.contains(event)),
        "a tenant's journal never holds the other tenant's keys"
    );
    let Some(event) = in_flight else {
        sim.record("plan-complete-before-the-kill");
        return Ok(());
    };
    if tenant.kill != Kill::Between {
        let retry = journal.compare_and_append(tenant.seen_tail, &event);
        assert_eq!(
            retry.is_ok(),
            tenant.kill == Kill::Torn,
            "a retry at the tail the controller saw lands a torn append and is refused for a landed one"
        );
        let current = journal.tail();
        assert!(
            journal.compare_and_append(current, &event).is_err(),
            "the interrupted append is never written a second time"
        );
        let copies = journal
            .events()
            .filter(|&&written| written == event)
            .count();
        assert_eq!(copies, 1, "the interrupted append is present exactly once");
    }
    let resume_from = tenant
        .acknowledged
        .saturating_add(usize::from(tenant.kill != Kill::Between));
    let rest = tenant
        .plan
        .get(resume_from..)
        .ok_or("the resume point is inside the plan")?;
    if !rest.is_empty() {
        journal.compare_and_append_all(rest)?;
    }
    drop(journal);
    let reopened = FileJournal::open(&tenant.path)?;
    let whole: Vec<EffectEvent> = reopened.events().copied().collect();
    assert_eq!(
        whole, tenant.plan,
        "a second reopen reads the whole plan, once, and nothing else"
    );
    sim.trace.record_number("final-events", whole.len());
    Ok(())
}

/// One seed: run, kill, restart both tenants.
fn kill_case(sim: &mut sim::Sim) -> TestResult {
    let dir = sim.scratch("tenant-kill")?;
    let tenants = run_until_killed(sim, &dir)?;
    for (index, tenant) in tenants.iter().enumerate() {
        let other: Vec<EffectEvent> = tenants
            .iter()
            .enumerate()
            .filter(|&(position, _)| position != index)
            .flat_map(|(_, neighbour)| neighbour.plan.iter().copied())
            .collect();
        restart(tenant, &other, sim)?;
    }
    sim.record("two-tenants-restarted");
    Ok(())
}

macro_rules! kill_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded band of two-tenant kills, swept twice for one trace hash.
            #[test]
            fn $name() -> TestResult {
                sim::assert_replays(band($index), kill_case)
            }
        )+
    };
}

kill_family!(
    kill_band_00 => 0; kill_band_01 => 1; kill_band_02 => 2; kill_band_03 => 3;
    kill_band_04 => 4; kill_band_05 => 5; kill_band_06 => 6; kill_band_07 => 7;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_kill_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(SEEDS, PARTS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}
