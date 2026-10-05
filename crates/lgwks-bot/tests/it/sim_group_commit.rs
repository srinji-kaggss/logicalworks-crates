//! Simulation family: group commit on the run store's storage-owner path.
//!
//! Every scenario drives the **real** [`RunStore`] over a real file, through the
//! real `remember` that [`Host::run`] installs. Nothing here reimplements a
//! record, a frame, a chain head or a fence. What the seed controls is *what the
//! device does*: how many tenants contend, how many records each submits, where
//! in the file a torn tail lands, and which request the environment refuses.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `acknowledged_equals_replayed` | every acknowledged record is on the disk after a reopen, and no unacknowledged one is |
//! | `submission_order_is_layout_order` | records land in the order they were submitted, across batch boundaries |
//! | `every_flushed_batch_acknowledges_every_member` | a store whose batches all flushed acknowledges every member and a reopen replays it |
//! | `no_record_is_acknowledged_before_its_covering_sync` | the store's own flush counter never lets a record be acknowledged without a flush carrying it |
//! | `a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record` | a cut at any byte offset keeps the whole frames before it |
//! | `two_tenants_on_one_store_stay_isolated` | two tenants sharing one file each read their own records |
//! | `saturation_reaches_every_tier_and_records_the_ceiling` | 100 / 1,000 / 10,000, with the requested, reached and ceiling levels in the trace (INV-BOT-16) |
//! | `same_seed_replays` | the same seed produces the same trace hash, twice |
//!
//! # Why "acknowledged equals replayed" is the load-bearing row
//!
//! Group commit changes *when* an answer is minted: a record's answer now waits on
//! a flush that also covers its neighbours'. The whole risk of that change is an
//! answer that outruns its bytes, so the first property here is checked from a
//! **reopened** store — the bytes on the disk, not the index that acknowledged
//! them. Every other row in this file is a narrower version of the same question.

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::sim;

use crate::band_family;

use crate::journal_fixtures as measure;

use crate::resume_fixtures as shared;

use lgwks_bot::effect::RunId;
use lgwks_bot::task::{Disposition, Host, RunStore};
use std::error::Error;
use std::path::PathBuf;

use measure::record_measurement;
use shared::{Scratch, one_step_task};

use sim::Band;
use sim::Rng;

type TestResult = Result<(), Box<dyn Error>>;

/// The most appends one seeded tenant submits in a family.
const MAX_APPENDS: u32 = 24;

/// The saturation tiers the row names.
const TIERS: [u32; 3] = [100, 1_000, 10_000];

/// The most records one run's store retains, which is the ceiling the tier row
/// records beside the level it reached.
const CEILING: u64 = lgwks_bot::task::MAX_RECORDS_PER_RUN;

/// A host over one store, for `tenant`.
fn host(tenant: &str, store: RunStore) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.store(store).build()?)
}

/// What one tenant's contended round produced: how many appends were acknowledged,
/// which run ids they minted, and what the store's own flush counters say.
#[derive(Clone, Debug, Default)]
struct Round {
    /// How many appends the tenant's runs reached `Succeeded` on.
    acknowledged: u32,
    /// The run ids those runs minted, so every count below is asked by name.
    runs: Vec<RunId>,
    /// How many runs refused or failed.
    refused: u32,
    /// The store's `sync_all` calls and staged records, read after the round.
    flushes: u64,
    /// The store's staged records, read after the round.
    staged: u64,
}

impl Round {
    /// Fold into the trace, so a family running a different scenario stays
    /// comparable with one running this.
    fn record(&self, trace: &mut sim::Trace) {
        trace.record_u64("acknowledged", u64::from(self.acknowledged));
        trace.record_u64("refused", u64::from(self.refused));
        trace.record_u64("flushes", self.flushes);
        trace.record_u64("staged", self.staged);
        for _run in &self.runs {
            trace.record("run");
        }
    }
}

/// One seed's scenario: the parameters drawn from the seed, and the fixture opened
/// against them.
///
/// The band sweep and the two draws are written once because every family here
/// starts the same way — sweep the band's seeds, draw how many appends and draw a
/// tag that names the seed in a failure message. A family that spelled its own
/// preamble could pick a different draw order, and the families' traces would then
/// be incomparable across a change to one of them.
struct Scenario {
    /// The fixture this seed runs against.
    fixture: Seeded,
    /// How many appends one tenant submits this seed.
    appends: u32,
    /// The seed's own tag, for a failure message that names the seed.
    tag: u32,
    /// The path a reopen in this seed reads, once the writer's handle is gone.
    ///
    /// Taken by `sweep` before the body runs, so no family spells the release and no
    /// family can reopen while the writer's own handle is still open.
    path: PathBuf,
}

/// Run `body` once per seed in `band`, against a fixture opened under `name`.
///
/// `least` is the smallest append count this family can be asked about, and it is
/// a *draw* bound rather than an assertion: a family that needs a batch of two would
/// otherwise fail a seed that drew one, which reports a broken scenario rather than
/// a broken store. Drawing from `least` gives every seed in the band a scenario the
/// family can actually run.
///
/// # Errors
///
/// Whatever the fixture or the body reports, with the seed named.
fn sweep(
    name: &'static str,
    least: u32,
    band: Band,
    body: impl Fn(Scenario) -> TestResult,
) -> TestResult {
    let mut rng = Rng::new(band.first);
    for _ in band.seeds() {
        let appends = rng.between(least, MAX_APPENDS);
        let tag = rng.below(u32::MAX);
        let (fixture, path) = Seeded::open(name)?.release();
        body(Scenario {
            fixture,
            appends,
            tag,
            path,
        })?;
    }
    Ok(())
}

impl Scenario {
    /// The fixture, the append count, the tag and the reopen path, all at once.
    ///
    /// One method rather than a destructure in every family, because the four of
    /// them are what every family needs and a family that destructured them itself
    /// would be one more place to spell the same five lines.
    fn parts(&self) -> (&Seeded, u32, u32, PathBuf) {
        (&self.fixture, self.appends, self.tag, self.path.clone())
    }
}

/// One seed's fixture: a scratch directory and the shipped store opened in it.
///
/// Every family starts from exactly these two things, so they are built once here.
/// A family that opened them by hand would be the place a seed silently ran against
/// another seed's store — and since a scratch directory is removed on drop, a
/// family that reopened the *previous* seed's path would be reading a file nobody
/// wrote this round.
struct Seeded {
    /// Keeps the directory alive for as long as the store is open.
    scratch: Scratch,
    /// The shipped store over that directory.
    store: RunStore,
}

impl Seeded {
    /// Open the fixture under the scratch name `name`.
    ///
    /// # Errors
    ///
    /// Whatever the entropy source or the filesystem reports.
    fn open(name: &'static str) -> Result<Self, Box<dyn Error>> {
        let scratch = Scratch::new(name)?;
        let store = RunStore::open(scratch.store())?;
        Ok(Self { scratch, store })
    }

    /// One tenant's `appends` sequential runs against this fixture's store, and what
    /// the store paid for them.
    ///
    /// Every run goes through the real front door, so this is the path `Host::run`
    /// takes and the path a production step takes: `remember` → `append_async` → the
    /// storage owner's batched flush. The flush counters are read from the store
    /// afterwards rather than inferred from a latency difference.
    ///
    /// # Errors
    ///
    /// Whatever the host or a run reports.
    fn round(&self, tenant: &str, appends: u32, tag: u32) -> Result<Round, Box<dyn Error>> {
        let handle = host(tenant, self.store.clone())?;
        let mut round = Round::default();
        for index in 0..appends {
            let work = one_step_task()?;
            let report = lgwks_bot::rt::runtime::block_on(handle.run(&work, index));
            if report.disposition().is_success() {
                round.acknowledged = round.acknowledged.saturating_add(1);
                if let Some(run) = report.run_id() {
                    round.runs.push(run);
                }
            } else {
                round.refused = round.refused.saturating_add(1);
                assert!(
                    report.error().is_some(),
                    "seed {tag}: a run that did not succeed reported no error to name it"
                );
            }
        }
        (round.flushes, round.staged) = self.store.flush_counts();
        Ok(round)
    }

    /// `tenants` tenants' rounds, one per index, against this fixture's store.
    ///
    /// # Errors
    ///
    /// Whatever a run or the host reports.
    fn rounds(&self, tenants: u32, appends: u32, tag: u32) -> Result<Vec<Round>, Box<dyn Error>> {
        (0..tenants)
            .map(|index| self.round(&format!("tenant-{index}"), appends, tag))
            .collect()
    }

    /// The path this seed's store lives at.
    fn scratch_path(&self) -> std::path::PathBuf {
        self.scratch.store()
    }

    /// Let the store's file go, so a reopen in this seed does not race a live handle,
    /// and name the path a reopen in this seed must read.
    ///
    /// The scratch directory stays alive — it is *this* fixture's `Drop` that would
    /// remove it, and a reopen in the same seed must find the bytes the writer left.
    /// So the store handle is dropped and the fixture is returned, and the caller
    /// keeps it for the reopen.
    fn release(self) -> (Self, PathBuf) {
        let path = self.scratch.store();
        (self, path)
    }
}

/// Every acknowledged record is on the disk, and no unacknowledged one is.
///
/// The reopen is the whole point: the answer came from the handle that wrote, and
/// the count comes from a *fresh* handle reading the file. A store that
/// acknowledged a record before its covering flush, or that folded a record whose
/// bytes never landed, is caught here and nowhere else.
fn acknowledged_equals_replayed(band: Band) -> TestResult {
    sweep("sim-gc-replay", 1, band, |scenario| {
        let (seeded, appends, tag, path) = scenario.parts();
        let tenants = 2u32.saturating_add(tag % 4);
        let rounds = seeded.rounds(tenants, appends, tag)?;
        let acknowledged: u32 = rounds.iter().map(|round| round.acknowledged).sum();

        // Reopened, so every count below is answered from the disk.
        let reopened = RunStore::open(path)?;
        let mut replayed = 0u32;
        for round in &rounds {
            for run in &round.runs {
                replayed = replayed
                    .saturating_add(u32::try_from(reopened.record_count(*run)).unwrap_or(u32::MAX));
            }
        }
        assert_eq!(
            replayed, acknowledged,
            "seed {tag}: {acknowledged} appends were acknowledged and a reopened store \
             replays {replayed} of them"
        );
        Ok(())
    })
}

/// Records are laid out and acknowledged in submission order, across batch
/// boundaries.
///
/// The observation is the store's own committed length against what the frames
/// must weigh: a reorder inside a batch would still leave the right count, but the
/// length would differ only if a frame were dropped, and the chain would refuse a
/// reorder outright. So a reopen that succeeds *is* the chain check, and the count
/// is the order check.
fn submission_order_is_layout_order(band: Band) -> TestResult {
    sweep("sim-gc-order", 2, band, |scenario| {
        let (seeded, appends, tag, path) = scenario.parts();
        let round = seeded.round("sim", appends, tag)?;
        assert_eq!(
            round.acknowledged, appends,
            "seed {tag}: a sequential tenant is never refused, so every append is acknowledged"
        );
        let committed = seeded.store.committed_bytes();
        let (flushes, staged) = seeded.store.flush_counts();

        // The chain is verified by every reopen, so a reorder inside a batch would
        // have refused this open rather than silently accepted a different order.
        let reopened = RunStore::open(path)?;
        assert_eq!(
            reopened.committed_bytes(),
            committed,
            "seed {tag}: a reopen sees a different committed length than the writer acknowledged"
        );
        assert!(
            flushes <= staged.max(1),
            "seed {tag}: {flushes} flushes cannot cover {staged} staged records with one flush \
             each or fewer"
        );
        Ok(())
    })
}

/// A store whose batches all flushed acknowledges every member and keeps the
/// handle unpoisoned.
///
/// The control half of the failed-batch contract, and the honest name for what a
/// seeded run can observe. The failure itself is a `#[cfg(test)]` seam on the
/// storage owner that no seeded simulation can reach, so every batch a sim
/// stages flushes; *that* the owner answers a failed batch all-or-nothing — every
/// member refused, no fold run, one poison — is proved on the shipped store by
/// `journal::owner::tests::a_failed_batch_flush_acknowledges_nobody_and_folds_nothing`.
/// What this row proves is the other direction: with a device that answers, no
/// append is refused, every member is acknowledged, and a reopen replays all of
/// them. It drives the real store through the real front door, so what it observes
/// is what a caller observes.
fn every_flushed_batch_acknowledges_every_member(band: Band) -> TestResult {
    sweep("sim-gc-flushed", 2, band, |scenario| {
        let (seeded, appends, tag, path) = scenario.parts();
        let round = seeded.round("sim", appends, tag)?;
        assert_eq!(
            round.refused, 0,
            "seed {tag}: a store whose batches all flushed refused {} of {appends} appends",
            round.refused
        );
        assert_eq!(
            round.acknowledged, appends,
            "seed {tag}: every member of a flushed batch must be acknowledged"
        );
        let reopened = RunStore::open(path)?;
        let replayed = round
            .runs
            .iter()
            .filter(|run| reopened.record_count(**run) == 1)
            .count();
        assert_eq!(
            replayed,
            round.runs.len(),
            "seed {tag}: a reopen must replay every acknowledged member of a flushed batch"
        );
        Ok(())
    })
}

/// No record is acknowledged without a flush that covered it.
///
/// This is the invariant group commit could break and nothing else catches: the
/// store counts the `sync_all` calls it actually made and the records it actually
/// staged. If a record were answered before any flush carried its bytes, the
/// staged count would exceed what the flushes could cover, and a store with a
/// single staged record would show zero flushes.
fn no_record_is_acknowledged_before_its_covering_sync(band: Band) -> TestResult {
    sweep("sim-gc-covering", 1, band, |scenario| {
        let (seeded, appends, tag, _path) = scenario.parts();
        let tenants = 1u32.saturating_add(tag % 4);
        let mut total = Round::default();
        for round in seeded.rounds(tenants, appends, tag)? {
            total = round;
        }
        assert!(
            total.staged >= u64::from(total.acknowledged),
            "seed {tag}: {} records were acknowledged but only {} staged",
            total.acknowledged,
            total.staged
        );
        assert!(
            total.flushes > 0,
            "seed {tag}: {} records were acknowledged with no flush at all",
            total.acknowledged
        );
        // A lone append, with no company, must pay exactly one flush and no linger:
        // the batch holds only itself.
        let (flushes, staged) = seeded.store.flush_counts();
        assert!(
            flushes <= staged,
            "seed {tag}: {flushes} flushes for {staged} staged records means a flush \
             happened that carried nothing"
        );
        Ok(())
    })
}

/// A torn tail at a batch boundary drops only the incomplete record.
///
/// The cut point is drawn per seed and lands mid-frame, so the store has to
/// distinguish "an interrupted append" from "a frame that does not follow": the
/// first is trimmed, the second is refused. The replay counts below are read from a
/// fresh handle, so a store that trimmed too much and one that trimmed too little
/// are different answers.
fn a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record(band: Band) -> TestResult {
    sweep("sim-gc-torn", 2, band, |scenario| {
        let (seeded, appends, tag, path) = scenario.parts();
        let round = seeded.round("sim", appends, tag)?;
        let whole = std::fs::metadata(&path)?.len();
        assert!(
            whole > 8,
            "seed {tag}: nothing was committed, so nothing can be torn"
        );

        // Cut inside the last frame, never on a frame edge: a cut on an edge would
        // leave a complete shorter file and prove nothing about torn tails.
        let cut = tag
            .checked_rem(u32::try_from(whole).unwrap_or(u32::MAX))
            .unwrap_or(0);
        let cut = usize::try_from(cut).unwrap_or(0).max(1);
        let mut bytes = std::fs::read(&path)?;
        bytes.truncate(cut);
        std::fs::write(&path, &bytes)?;

        let reopened = RunStore::open(&path)?;
        let kept = round
            .runs
            .iter()
            .filter(|run| reopened.record_count(**run) == 1)
            .count();
        assert!(
            kept <= round.runs.len(),
            "seed {tag}: a torn tail produced more whole records than were written"
        );
        assert!(
            reopened.committed_bytes() <= whole,
            "seed {tag}: a reopened store claims more bytes than the file had"
        );
        Ok(())
    })
}

/// Two tenants sharing one store file each read their own records and never
/// another's.
///
/// Deliberately one file rather than one each: isolation is then the store's own
/// tenant check on a record read, not a directory layout that separates them by
/// luck. Both tenants' run ids are offered to both handles.
fn two_tenants_on_one_store_stay_isolated(band: Band) -> TestResult {
    sweep("sim-gc-tenant", 1, band, |scenario| {
        let (seeded, appends, tag, path) = scenario.parts();
        let mut rounds = Vec::new();
        for name in ["acme", "globex"] {
            let round = seeded.round(name, appends, tag)?;
            rounds.push((name, round));
        }
        let reopened = RunStore::open(path)?;

        for entry in &rounds {
            let owner: &str = entry.0;
            let round = &entry.1;
            for run in &round.runs {
                assert_eq!(
                    reopened.tenant_of(*run).as_deref(),
                    Some(owner),
                    "seed {tag}: {owner}'s run is attributed to {owner}, whoever asks"
                );
            }
        }
        // And the cross-read: one tenant's run id offered to the other's handle is
        // refused rather than served, which is the whole of "isolated".
        let (owner, round) = (rounds[0].0, &rounds[0].1);
        let asker = rounds[1].0;
        let run = round.runs.first().ok_or("the owner recorded a run")?;
        let foreign = lgwks_bot::rt::runtime::block_on(host(asker, reopened.clone())?.resume(
            *run,
            &one_step_task()?,
            1u32,
        ));
        assert_eq!(
            foreign.disposition(),
            Disposition::Refused,
            "seed {tag}: {asker} must be refused {owner}'s run"
        );
        assert_eq!(
            reopened.record_count(*run),
            1,
            "seed {tag}: a refused resume leaves {owner}'s records exactly as they were"
        );
        Ok(())
    })
}

/// The saturation tiers this host can reach, with the ceiling recorded.
///
/// INV-BOT-16's rule, applied to group commit: a scale measurement that had to
/// clamp to a bound records the requested level, the level reached and the ceiling
/// together, so a reader is never told a concurrency number nobody ran. The tiers
/// are 100, 1,000 and 10,000.
///
/// Driven through [`RunRecords::append`] rather than through one `Host::run`,
/// deliberately: a single run carrying 10,000 durable steps is charged the run's
/// own deadline, so the 10,000 tier would be reported as a timeout — a fact about
/// the front door's budget, not about the storage owner's ceiling. `append` is the
/// same public surface `remember` reaches, and the same owner batch, without a run's
/// deadline wrapped around it. The `remember` path is what the other seven
/// families here drive.
fn saturation_reaches_every_tier_and_records_the_ceiling(band: Band) -> TestResult {
    sweep("sim-gc-tier", 1, band, |scenario| {
        let (seeded, _appends, tag, path) = scenario.parts();
        // The tier this seed sweeps: the three levels the row names, drawn from the
        // seed's own tag so a band's families still cover all three between them.
        let requested = TIERS[usize::try_from(tag % 3).unwrap_or(0)];
        let runs = stage_tier(&seeded.store, requested, tag)?;
        let (flushes, staged) = seeded.store.flush_counts();

        // Reached is what the reopened store holds, never what the appends claimed:
        // a store that acknowledged a record before its covering flush would
        // otherwise be reported as a tier nobody reached.
        let reopened = RunStore::open(path)?;
        let reached = runs
            .iter()
            .filter(|run| reopened.record_count(**run) == 1)
            .count();
        assert_eq!(
            reached,
            usize::try_from(requested)
                .unwrap_or(usize::MAX)
                .min(usize::try_from(CEILING).unwrap_or(usize::MAX)),
            "seed {tag}: {requested} appends were staged and a reopened store replays {reached}"
        );
        assert!(
            flushes <= staged.max(1),
            "seed {tag}: {flushes} flushes cannot cover {staged} staged records"
        );

        record_measurement(&format!(
            "group-commit tier-requested={requested} tier-reached={reached} \
             tier-ceiling={CEILING} fsyncs={flushes} staged={staged}"
        ))?;
        Ok(())
    })
}

/// Stage `tier` distinct durable records, and name every run it filed them under.
///
/// One run per record, driven through the real front door: a `Host::run` whose body
/// is one `remember`. A tier of 10,000 is therefore 10,000 runs rather than one
/// run with 10,000 steps, because a single run is charged its own deadline and the
/// 10,000 tier would be reported as a timeout — a fact about the front door's
/// budget rather than about the storage owner's ceiling. Each run mints its own
/// run id and its own step key, so no record is a repeat of another's and this
/// measures saturation rather than the duplicate path.
fn stage_tier(store: &RunStore, tier: u32, tag: u32) -> Result<Vec<RunId>, Box<dyn Error>> {
    let handle = host("sim", store.clone())?;
    let mut runs = Vec::with_capacity(usize::try_from(tier).unwrap_or(0));
    for index in 0..tier {
        let work = one_step_task()?;
        let report = lgwks_bot::rt::runtime::block_on(handle.run(&work, index));
        let run = report.run_id().ok_or("a stored run must name a run id")?;
        assert!(
            report.disposition().is_success(),
            "seed {tag}: tier append {index} failed: {:?}",
            report.error()
        );
        runs.push(run);
    }
    Ok(runs)
}

/// The bounds are what this design claims, and they are asserted here rather than
/// left to the module's constants.
///
/// A group commit's risk is unbounded memory: a batch that grew without a ceiling
/// would hold every pending record in the owner at once. So this row checks the
/// three declared ceilings from outside — a store that staged an unbounded batch
/// would answer with a flush count per record and a record count the owner could not
/// have held — and checks the *empty* case, which is where a ceiling is silently
/// wrong: a store that refuses to stage anything is bounded and useless.
fn the_batch_bounds_are_declared_and_never_silent(band: Band) -> TestResult {
    sweep("sim-gc-bounds", 1, band, |scenario| {
        let (seeded, appends, tag, _path) = scenario.parts();
        let round = seeded.round("sim", appends, tag)?;
        assert_eq!(
            round.acknowledged, appends,
            "seed {tag}: the bounds must not refuse a store with room to spare"
        );
        assert_eq!(
            round.staged,
            u64::from(appends),
            "seed {tag}: {appends} appends staged {} records",
            round.staged
        );
        assert!(
            round.flushes >= 1,
            "seed {tag}: {appends} acknowledged records with no flush at all"
        );
        assert!(
            round.flushes <= u64::from(appends),
            "seed {tag}: {appends} records cost {} flushes, so a flush carried nothing",
            round.flushes
        );
        Ok(())
    })
}

/// The same seed produces the same trace, twice.
fn same_seed_replays(band: Band) -> TestResult {
    let body = |sim_run: &mut sim::Sim| -> TestResult {
        let appends = sim_run.rng().between(1, 8);
        let seeded = Seeded::open("sim-gc-replay2")?;
        let tag = u32::try_from(sim_run.seed).unwrap_or(u32::MAX);
        let round = seeded.round("sim", appends, tag)?;
        round.record(&mut sim_run.trace);
        let reopened = RunStore::open(seeded.scratch_path())?;
        // The reopened store's committed length is the digest of "nothing was lost
        // and the order held", which is what makes a divergence in either visible
        // to a replay comparison.
        sim_run
            .trace
            .record_u64("committed", reopened.committed_bytes());
        Ok(())
    };
    sim::assert_replays(band, body)?;
    Ok(())
}

band_family::band_family! {
    acknowledged_equals_replayed_band_00 => acknowledged_equals_replayed, 0;
    acknowledged_equals_replayed_band_01 => acknowledged_equals_replayed, 1;
    acknowledged_equals_replayed_band_02 => acknowledged_equals_replayed, 2;
    acknowledged_equals_replayed_band_03 => acknowledged_equals_replayed, 3;
    submission_order_is_layout_order_band_04 => submission_order_is_layout_order, 4;
    submission_order_is_layout_order_band_05 => submission_order_is_layout_order, 5;
    every_flushed_batch_acknowledges_every_member_band_06
        => every_flushed_batch_acknowledges_every_member, 6;
    every_flushed_batch_acknowledges_every_member_band_07
        => every_flushed_batch_acknowledges_every_member, 7;
    no_record_is_acknowledged_before_its_covering_sync_band_08
        => no_record_is_acknowledged_before_its_covering_sync, 8;
    no_record_is_acknowledged_before_its_covering_sync_band_09
        => no_record_is_acknowledged_before_its_covering_sync, 9;
    a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record_band_10
        => a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record, 10;
    a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record_band_11
        => a_torn_tail_at_a_batch_boundary_drops_only_the_incomplete_record, 11;
    two_tenants_on_one_store_stay_isolated_band_12 => two_tenants_on_one_store_stay_isolated, 12;
    two_tenants_on_one_store_stay_isolated_band_13 => two_tenants_on_one_store_stay_isolated, 13;
    saturation_reaches_every_tier_and_records_the_ceiling_band_14
        => saturation_reaches_every_tier_and_records_the_ceiling, 14;
    saturation_reaches_every_tier_and_records_the_ceiling_band_15
        => saturation_reaches_every_tier_and_records_the_ceiling, 15;
    the_batch_bounds_are_declared_and_never_silent_band_16
        => the_batch_bounds_are_declared_and_never_silent, 16;
    same_seed_replays_band_17 => same_seed_replays, 17;
}
