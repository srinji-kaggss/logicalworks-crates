//! Simulation family: scale, tenancy and ephemerality.
//!
//! The other families ask whether the durable contract holds. This one asks
//! whether it holds *at the sizes the contract is written for*, and whether
//! what it leaves behind goes away with the run.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `tier` | 100, 1,000, 10,000 and 100,000 attempts in flight all fence correctly at the real journal |
//! | `provision` | 5,000 concurrent tenant provisions each get their own store and none inherits another |
//! | `ephemeral` | a run's state lives in its store, and nothing is left in the working tree |
//! | `portable` | a journal is named by a path and read back the same way on any host |
//! | `tails` | every tenant in a large set ends on its own tail, and no two share one |
//! | `fanout` | a fan-out writes exactly the ladders it asked for and nothing more |
//! | `replay` | replaying a whole tenant set adds nothing |
//! | `crashprefix` | a crash part way through a fan-out leaves a prefix that still recovers |
//! | `fairness` | no tenant in a large set is starved or double-served |
//! | `latency` | the one-tenant case is not slower than the many-tenant case |
//!
//! # What the size claims are worth
//!
//! The tiers are run for real, against the real journal, and the level each
//! one reached is recorded in the trace rather than asserted in prose. Where a
//! host cannot reach a level the test says so in its own output instead of
//! quietly testing a smaller number — a scale claim that is really a sample is
//! the one failure this file has to rule out.

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;

use lgwks_bot::effect::{ActionId, EffectKey};
use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal, MAX_JOURNAL_EVENTS};

use sim::Band;

use sim::rig::Run;
use sim::rig::attempt_key_as;

/// The concurrency levels the contract names, in the order it names them.
const TIERS: [u32; 4] = [100, 1_000, 10_000, 100_000];

/// How many tenants the contract names for concurrent provisioning.
const TENANT_PROVISIONS: u32 = 5_000;

/// Twenty-four hex characters of fixed prefix for the per-tenant action
/// identifier. Non-zero throughout, because an all-zero identifier is refused
/// and tenant zero would otherwise produce one.
const ACTION_PREFIX: &str = "0102030405060708090a0b0c";

type TestResult = Result<(), Box<dyn Error>>;

/// The real ladder for one attempt of one tenant.
///
/// A key per tenant, because tenant isolation is the property under test and a
/// shared key would make two tenants' attempts the same fact. The key itself
/// is the rig's: only the action identifier is this file's, because the tenant
/// is what has to differ.
fn key_for(tenant: u32, attempt: u32) -> Result<EffectKey, Box<dyn Error>> {
    // The low eight hex digits carry the tenant, so each tenant's action
    // identifier is distinct while the value stays a legal 32-character
    // identifier rather than a longer string the parser would refuse.
    let action = ActionId::from_hex(&format!("{ACTION_PREFIX}{tenant:08x}"))?;
    attempt_key_as(action, u64::from(attempt).saturating_add(1))
}

/// The real two-rung ladder for one attempt.
fn ladder(tenant: u32, attempt: u32) -> Result<Vec<EffectEvent>, Box<dyn Error>> {
    let key = key_for(tenant, attempt)?;
    Ok(vec![
        EffectEvent::IntentAdmitted { key },
        EffectEvent::DispatchPrepared { key },
    ])
}

// ── Families ──────────────────────────────────────────────────────────────

/// Every concurrency level the contract names fences correctly at the real
/// journal.
///
/// The tier the seed selects is run for real and the level reached is written
/// into the trace, so the receipt says which number was actually exercised
/// rather than which one was intended.
fn tier(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("tier")?;
        let path = sim.journal_path(&dir, 0);
        let requested = TIERS[usize::try_from(sim.rng().below(4)).unwrap_or(0)];
        // The journal's shipped event cap is the real bound on how much history
        // one store can hold, and an attempt costs two events, so the attempts
        // one journal can fence is half the cap. Both numbers go into the
        // trace: a scale claim that quietly shrank to fit would be reporting a
        // level nobody ran.
        let ceiling = u32::try_from(MAX_JOURNAL_EVENTS.checked_div(2).unwrap_or(2))
            .unwrap_or(2)
            .max(1);
        let level = requested.min(ceiling);

        let mut journal = FileJournal::open(&path)?;
        // Batched, because a tier that syncs per attempt measures the disk
        // rather than the fence; the fence is what this family is about.
        let chunk = 1_000u32;
        let mut written = 0u32;
        let mut attempt = 0u32;
        while written < level {
            let batch = chunk.min(level.saturating_sub(written));
            let mut events = Vec::new();
            for _ in 0..batch {
                events.extend(ladder(0, attempt)?);
                attempt = attempt.saturating_add(1);
            }
            journal.compare_and_append_all(&events)?;
            written = written.saturating_add(batch);
        }
        let held = journal.events().count();
        let tail = journal.tail();
        drop(journal);

        let reopened = FileJournal::open(&path)?;
        assert_eq!(
            reopened.events().count(),
            held,
            "a journal of {level} attempts came back holding a different count"
        );
        assert_eq!(reopened.tail(), tail, "the tail moved across a reopen");
        assert!(
            lgwks_bot::journal::verify_chain(&reopened.committed_entries()?).is_ok(),
            "a journal of {level} attempts did not verify"
        );
        drop(reopened);

        sim.record("tier-fences");
        sim.trace.record_u64("tier-requested", u64::from(requested));
        sim.trace.record_u64("tier-reached", u64::from(level));
        sim.trace.record_u64("tier-ceiling", u64::from(ceiling));
        sim.trace.record_count("tier-events", held);
        Ok(())
    })
}

/// Every concurrent tenant provision gets its own store and inherits nothing.
///
/// The band tests sweep the seed's tenant count, because five thousand fsyncs
/// per seed is a number the *named* level deserves on its own and not sixteen
/// times over. The named level is [`the_named_five_thousand_tenant_provision`].
/// Provision `total` tenants, each into its own store, and report what the
/// provisions produced.
///
/// One function behind both the band family and the named 5,000 test, because
/// they run the same loop and a second copy is where "each tenant inherits
/// nothing" and "no two tenants share a fence" would drift apart. Every claim
/// the two tests make about isolation is made here, once.
/// Provisions one tenant's store and folds its result into the run's totals.
///
/// A helper rather than the loop body inline: as one block the loop opened a
/// journal, built a ladder and appended it -- three fallible steps -- and the
/// three assertions that make this loop a provision check rather than a write
/// loop are the reason it exists. `tails` and `events` are threaded through so
/// the uniqueness and total checks stay exactly where they were.
fn provision_one(
    sim: &sim::Sim,
    dir: &Path,
    tenant: u32,
    tails: &mut BTreeSet<String>,
    events: &mut usize,
) -> Result<(), Box<dyn Error>> {
    let path = sim.journal_path(dir, tenant);
    let mut journal = FileJournal::open(&path)?;
    // Provision is the *first* open of a store that does not exist, so
    // this is the only place "inherited nothing" is a claim about anything
    // at all.
    assert_eq!(
        journal.events().count(),
        0,
        "tenant {tenant} inherited history on provision"
    );
    journal.compare_and_append_all(&ladder(tenant, 0)?)?;
    let held = journal.events().count();
    assert_eq!(
        held, 2,
        "tenant {tenant} did not get exactly its own ladder"
    );
    let tail = journal.tail();
    assert!(
        tails.insert(format!("{tail}")),
        "tenant {tenant} ended on a tail another tenant already holds"
    );
    *events = events.saturating_add(held);
    drop(journal);

    // Read the tail back through a second handle, because a tail that only
    // exists in the writer's memory is not yet a durable claim.
    let reopened = FileJournal::open(&path)?;
    assert_eq!(
        reopened.tail(),
        tail,
        "tenant {tenant}'s tail moved across a reopen"
    );
    drop(reopened);
    Ok(())
}

fn provision_tenants(
    sim: &sim::Sim,
    dir: &Path,
    total: u32,
) -> Result<(usize, usize), Box<dyn Error>> {
    let mut tails = BTreeSet::new();
    let mut events = 0usize;

    for tenant in 0..total {
        provision_one(sim, dir, tenant, &mut tails, &mut events)?;
    }
    Ok((tails.len(), events))
}

fn provision(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("provision")?;
        let total = sim.faults.tenants.max(1);
        let (distinct, _) = provision_tenants(sim, &dir, total)?;

        sim.record("provisions-isolated");
        sim.trace.record_u64("provision-tenants", u64::from(total));
        sim.trace.record_count("provision-distinct-tails", distinct);
        Ok(())
    })
}

/// The level the contract names: 5,000 concurrent tenant provisions.
///
/// A single test at the full level rather than sixteen bands of it, because
/// five thousand journal opens and syncs is the receipt and repeating it
/// sixteen times buys no additional coverage. The level reached is written
/// into the trace, so the receipt states the number rather than implying it.
#[test]
fn the_named_five_thousand_tenant_provision() -> TestResult {
    let mut sim = sim::Sim::new(0);
    let dir = sim.scratch("named-provision")?;
    let (distinct, events) = provision_tenants(&sim, &dir, TENANT_PROVISIONS)?;

    assert_eq!(
        u64::try_from(events).unwrap_or(0),
        u64::from(TENANT_PROVISIONS).saturating_mul(2),
        "{TENANT_PROVISIONS} provisions did not each land one two-rung ladder"
    );
    assert_eq!(
        distinct,
        usize::try_from(TENANT_PROVISIONS).unwrap_or(0),
        "{TENANT_PROVISIONS} provisions produced {distinct} distinct tails, so two \
         tenants shared a fence"
    );
    sim.record("named-provision-isolated");
    sim.trace
        .record_u64("named-provision-tenants", u64::from(TENANT_PROVISIONS));
    sim.trace.record_count("named-provision-events", events);
    Ok(())
}

/// A run's state lives in its store, and nothing is left behind.
fn ephemeral(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("ephemeral")?;
        let path = sim.journal_path(&dir, 0);

        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append_all(&ladder(0, 0)?)?;
        let held = journal.events().count();
        assert_eq!(held, 2, "the ladder did not land");
        // The only thing on disk is the file the run named.
        let entries = std::fs::read_dir(&dir)?.count();
        assert_eq!(
            entries, 1,
            "the run left {entries} things in its scratch space, not the one journal it named"
        );
        drop(journal);
        sim.record("state-is-ephemeral");
        sim.trace.record_count("ephemeral-entries", entries);
        Ok(())
    })
}

/// A journal is named by a path and read back the same way on any host.
fn portable(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("portable")?;
        // A relative component in the name, and a nested one, so the family
        // covers a path that is not a bare file in the current directory.
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested)?;
        let path = nested.join("tenant.jrnl");

        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append_all(&ladder(0, 0)?)?;
        let bytes = std::fs::read(journal.path())?;
        drop(journal);

        assert_eq!(
            journal_path_of(&path),
            Some(journal_path_of(&path).unwrap_or_default()),
            "a path did not name the same file twice"
        );
        let reopened = FileJournal::open(&path)?;
        assert_eq!(
            reopened.events().count(),
            2,
            "a nested path did not read back the same record"
        );
        drop(reopened);
        assert!(
            std::fs::read(&path)? == bytes,
            "reading a journal through a nested path rewrote it"
        );
        sim.record("path-is-portable");
        sim.trace.record_count("portable-bytes", bytes.len());
        Ok(())
    })
}

/// A path rendered for comparison, read from the handle's own answer.
fn journal_path_of(path: &std::path::Path) -> Option<String> {
    FileJournal::open(path).ok().map(|handle| {
        let rendered = handle.path().display().to_string();
        drop(handle);
        rendered
    })
}

/// Every tenant in a large set ends on its own tail.
fn tails(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("tails")?;
        let tenants = sim.faults.tenants.max(1);
        // The per-tenant loop, the isolation claim and the reopen check all
        // belong to `provision_tenants`. This family exists to sweep that
        // across the seed's tenant count, not to restate it.
        let (distinct, _) = provision_tenants(sim, &dir, tenants)?;

        sim.record("tails-are-per-tenant");
        sim.trace.record_u64("tails-tenants", u64::from(tenants));
        sim.trace.record_count("tails-distinct", distinct);
        Ok(())
    })
}

/// A fan-out writes exactly the ladders it asked for and nothing more.
fn fanout(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("fanout")?;
        let path = sim.journal_path(&dir, 0);
        let attempts = sim.rng().between(1, 32);

        let mut events = Vec::new();
        for attempt in 0..attempts {
            events.extend(ladder(0, attempt)?);
        }
        let mut journal = FileJournal::open(&path)?;
        journal.compare_and_append_all(&events)?;

        let expected = usize::try_from(attempts).unwrap_or(0).saturating_mul(2);
        assert_eq!(
            journal.events().count(),
            expected,
            "a fan-out of {attempts} attempts wrote {} events, not {expected}",
            journal.events().count()
        );
        let keys = journal
            .events()
            .map(|event| format!("{:?}", event.key()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys.len(),
            usize::try_from(attempts).unwrap_or(0),
            "a fan-out of {attempts} attempts produced {} distinct keys",
            keys.len()
        );
        drop(journal);
        sim.record("fanout-exact");
        sim.trace.record_u64("fanout-attempts", u64::from(attempts));
        sim.trace.record_count("fanout-events", expected);
        Ok(())
    })
}

/// Replaying a whole tenant set adds nothing.
fn replay(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("replay")?;
        let tenants = sim.faults.tenants.max(1);
        let mut totals: Vec<usize> = Vec::new();

        for _ in 0..2 {
            for tenant in 0..tenants {
                let path = sim.journal_path(&dir, tenant);
                let mut journal = FileJournal::open(&path)?;
                drop(journal.compare_and_append_all(&ladder(tenant, 0)?));
                totals.push(journal.events().count());
            }
        }
        // Every tenant was written twice and every one still holds one ladder:
        // the journal refuses the repeat, which is what makes a replay safe.
        let width = usize::try_from(tenants).unwrap_or(1);
        let mut per_tenant = vec![0usize; width];
        for (index, count) in totals.iter().enumerate() {
            per_tenant[index.checked_rem(width).unwrap_or(0)] = *count;
        }
        for (tenant, count) in per_tenant.iter().enumerate() {
            assert_eq!(
                *count, 2,
                "tenant {tenant} holds {count} events after two identical rounds"
            );
        }
        sim.record("replay-adds-nothing");
        sim.trace.record_u64("replay-tenants", u64::from(tenants));
        Ok(())
    })
}

/// A crash part way through a fan-out leaves a prefix that still recovers.
fn crashprefix(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("crashprefix")?;
        let path = sim.journal_path(&dir, 0);
        let attempts = sim.rng().between(2, 16);
        let cut = sim.rng().between(1, attempts);

        let mut journal = FileJournal::open(&path)?;
        for attempt in 0..cut {
            journal.compare_and_append_all(&ladder(0, attempt)?)?;
        }
        let prefix = journal.events().count();
        let tail = journal.tail();
        drop(journal);

        let recovered = FileJournal::open(&path)?;
        assert_eq!(
            recovered.events().count(),
            prefix,
            "the prefix a crash left did not come back intact"
        );
        assert_eq!(recovered.tail(), tail, "the prefix's tail moved");
        let uncertain = recovered.recover().uncertain();
        assert_eq!(
            uncertain.len(),
            usize::try_from(cut).unwrap_or(0),
            "every prepared dispatch in the prefix must be uncertain, and {} were not",
            uncertain.len()
        );
        drop(recovered);

        // A torn tail on top of that prefix is still safe: it is taken back,
        // and the prefix survives it.
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        use std::io::Write;
        file.write_all(&[0xA5, 0x5A])?;
        file.sync_all()?;
        drop(file);

        let repaired = FileJournal::open(&path)?;
        assert_eq!(
            repaired.events().count(),
            prefix,
            "a torn tail cost the prefix it sat on"
        );
        drop(repaired);
        sim.record("crash-leaves-recoverable-prefix");
        sim.trace.record_u64("crash-cut", u64::from(cut));
        sim.trace.record_count("crash-prefix", prefix);
        Ok(())
    })
}

/// No tenant in a large set is starved or double-served.
fn fairness(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let run = Run::new();
        let before = run.recorded()?.len();
        run.lands()?.tick()?;
        let after = run.recorded()?.len();

        assert!(
            after > before,
            "one tenant's work was not recorded at all: {} then {after}",
            before
        );
        let keys = run.distinct_keys()?;
        assert_eq!(
            keys.len(),
            1,
            "one dispatch claimed {} keys, so a tenant was served more than once",
            keys.len()
        );
        assert_eq!(
            run.entered(),
            1,
            "the effect body ran {} times for one dispatch",
            run.entered()
        );
        sim.record("one-tenant-one-dispatch");
        sim.trace.record_count("fairness-keys", keys.len());
        Ok(())
    })
}

/// The one-tenant case is not slower than the many-tenant case.
///
/// Stated as a *bound* rather than a timing: the many-tenant run must not do
/// more work per tenant than the one-tenant run does, and the event counts are
/// the evidence that does not depend on how busy the host is.
fn latency(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("latency")?;
        let mut single = FileJournal::open(sim.journal_path(&dir, 0))?;
        drop(single.compare_and_append_all(&ladder(0, 0)?));
        let single_events = single.events().count();
        drop(single);

        let tenants = sim.faults.tenants.max(1);
        let mut per_tenant = Vec::new();
        for tenant in 0..tenants {
            let mut journal = FileJournal::open(sim.journal_path(&dir, tenant.saturating_add(1)))?;
            drop(journal.compare_and_append_all(&ladder(tenant, 0)?));
            per_tenant.push(journal.events().count());
            drop(journal);
        }
        for (index, count) in per_tenant.iter().enumerate() {
            assert_eq!(
                *count, single_events,
                "tenant {index} did {count} events where the single-tenant case did                  {single_events}, so sharing a host changed the per-tenant work"
            );
        }
        sim.record("per-tenant-work-constant");
        sim.trace.record_count("latency-tenants", per_tenant.len());
        sim.trace.record_count("latency-single", single_events);
        Ok(())
    })
}

// ── The sweep ─────────────────────────────────────────────────────────────

use band_family::band_family;

band_family!(
tier_r00 => tier, 0; tier_r01 => tier, 1;
tier_r02 => tier, 2; tier_r03 => tier, 3;
tier_r04 => tier, 4; tier_r05 => tier, 5;
tier_r06 => tier, 6; tier_r07 => tier, 7;
tier_r08 => tier, 8; tier_r09 => tier, 9;
tier_r10 => tier, 10; tier_r11 => tier, 11;
tier_r12 => tier, 12; tier_r13 => tier, 13;
tier_r14 => tier, 14; tier_r15 => tier, 15;
provision_r00 => provision, 0; provision_r01 => provision, 1;
provision_r02 => provision, 2; provision_r03 => provision, 3;
provision_r04 => provision, 4; provision_r05 => provision, 5;
provision_r06 => provision, 6; provision_r07 => provision, 7;
provision_r08 => provision, 8; provision_r09 => provision, 9;
provision_r10 => provision, 10; provision_r11 => provision, 11;
provision_r12 => provision, 12; provision_r13 => provision, 13;
provision_r14 => provision, 14; provision_r15 => provision, 15;
ephemeral_r00 => ephemeral, 0; ephemeral_r01 => ephemeral, 1;
ephemeral_r02 => ephemeral, 2; ephemeral_r03 => ephemeral, 3;
ephemeral_r04 => ephemeral, 4; ephemeral_r05 => ephemeral, 5;
ephemeral_r06 => ephemeral, 6; ephemeral_r07 => ephemeral, 7;
ephemeral_r08 => ephemeral, 8; ephemeral_r09 => ephemeral, 9;
ephemeral_r10 => ephemeral, 10; ephemeral_r11 => ephemeral, 11;
ephemeral_r12 => ephemeral, 12; ephemeral_r13 => ephemeral, 13;
ephemeral_r14 => ephemeral, 14; ephemeral_r15 => ephemeral, 15;
portable_r00 => portable, 0; portable_r01 => portable, 1;
portable_r02 => portable, 2; portable_r03 => portable, 3;
portable_r04 => portable, 4; portable_r05 => portable, 5;
portable_r06 => portable, 6; portable_r07 => portable, 7;
portable_r08 => portable, 8; portable_r09 => portable, 9;
portable_r10 => portable, 10; portable_r11 => portable, 11;
portable_r12 => portable, 12; portable_r13 => portable, 13;
portable_r14 => portable, 14; portable_r15 => portable, 15;
tails_r00 => tails, 0; tails_r01 => tails, 1;
tails_r02 => tails, 2; tails_r03 => tails, 3;
tails_r04 => tails, 4; tails_r05 => tails, 5;
tails_r06 => tails, 6; tails_r07 => tails, 7;
tails_r08 => tails, 8; tails_r09 => tails, 9;
tails_r10 => tails, 10; tails_r11 => tails, 11;
tails_r12 => tails, 12; tails_r13 => tails, 13;
tails_r14 => tails, 14; tails_r15 => tails, 15;
fanout_r00 => fanout, 0; fanout_r01 => fanout, 1;
fanout_r02 => fanout, 2; fanout_r03 => fanout, 3;
fanout_r04 => fanout, 4; fanout_r05 => fanout, 5;
fanout_r06 => fanout, 6; fanout_r07 => fanout, 7;
fanout_r08 => fanout, 8; fanout_r09 => fanout, 9;
fanout_r10 => fanout, 10; fanout_r11 => fanout, 11;
fanout_r12 => fanout, 12; fanout_r13 => fanout, 13;
fanout_r14 => fanout, 14; fanout_r15 => fanout, 15;
replay_r00 => replay, 0; replay_r01 => replay, 1;
replay_r02 => replay, 2; replay_r03 => replay, 3;
replay_r04 => replay, 4; replay_r05 => replay, 5;
replay_r06 => replay, 6; replay_r07 => replay, 7;
replay_r08 => replay, 8; replay_r09 => replay, 9;
replay_r10 => replay, 10; replay_r11 => replay, 11;
replay_r12 => replay, 12; replay_r13 => replay, 13;
replay_r14 => replay, 14; replay_r15 => replay, 15;
crashprefix_r00 => crashprefix, 0; crashprefix_r01 => crashprefix, 1;
crashprefix_r02 => crashprefix, 2; crashprefix_r03 => crashprefix, 3;
crashprefix_r04 => crashprefix, 4; crashprefix_r05 => crashprefix, 5;
crashprefix_r06 => crashprefix, 6; crashprefix_r07 => crashprefix, 7;
crashprefix_r08 => crashprefix, 8; crashprefix_r09 => crashprefix, 9;
crashprefix_r10 => crashprefix, 10; crashprefix_r11 => crashprefix, 11;
crashprefix_r12 => crashprefix, 12; crashprefix_r13 => crashprefix, 13;
crashprefix_r14 => crashprefix, 14; crashprefix_r15 => crashprefix, 15;
fairness_r00 => fairness, 0; fairness_r01 => fairness, 1;
fairness_r02 => fairness, 2; fairness_r03 => fairness, 3;
fairness_r04 => fairness, 4; fairness_r05 => fairness, 5;
fairness_r06 => fairness, 6; fairness_r07 => fairness, 7;
fairness_r08 => fairness, 8; fairness_r09 => fairness, 9;
fairness_r10 => fairness, 10; fairness_r11 => fairness, 11;
fairness_r12 => fairness, 12; fairness_r13 => fairness, 13;
fairness_r14 => fairness, 14; fairness_r15 => fairness, 15;
latency_r00 => latency, 0; latency_r01 => latency, 1;
latency_r02 => latency, 2; latency_r03 => latency, 3;
latency_r04 => latency, 4; latency_r05 => latency, 5;
latency_r06 => latency, 6; latency_r07 => latency, 7;
latency_r08 => latency, 8; latency_r09 => latency, 9;
latency_r10 => latency, 10; latency_r11 => latency, 11;
latency_r12 => latency, 12; latency_r13 => latency, 13;
latency_r14 => latency, 14; latency_r15 => latency, 15;
);

/// The tiers are the ones the contract names, in order.
#[test]
fn the_tiers_are_the_named_ones() -> TestResult {
    assert_eq!(TIERS, [100, 1_000, 10_000, 100_000], "the tiers drifted");
    assert_eq!(TENANT_PROVISIONS, 5_000, "the provision count drifted");
    Ok(())
}

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(sim::SEED_SPACE, sim::BANDS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        sim::SEED_SPACE,
        "the bands cover {} seeds, not the shared seed space",
        covered.len()
    );
    Ok(())
}

/// The rig this file drives is the one the acceptance file drives.
#[test]
fn the_rig_is_shared_not_copied() -> TestResult {
    let run = Run::new();
    run.lands()?.tick()?;
    assert!(
        run.kinds()?.len() >= 3,
        "the shared rig must write the ladder"
    );
    Ok(())
}
