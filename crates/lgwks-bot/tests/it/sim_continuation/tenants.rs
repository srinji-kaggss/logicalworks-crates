//! The tenant family: two tenants continue their own journal chains in one directory, and no key crosses between them.

use super::*;

/// Drive one tenant's journal for `rounds` attempts in `dir`.
///
/// Answers what the tenant meant to do, how many times its chain continued, and
/// where its live generation is, with the handle dropped so a reopen is a real one.
fn run_tenant(
    dir: &std::path::Path,
    tenant: u64,
    rounds: u64,
) -> Result<(Oracle, u64, PathBuf), Box<dyn Error>> {
    let action = u64::MAX.saturating_sub(tenant);
    let path = dir.join(format!("tenant-{tenant}.jrnl"));
    let mut journal = FileJournal::open_continuing_with(&path, policy()?)?;
    let mut oracle = Oracle::default();
    let mut continued = 0_u64;
    let mut until_unknown = 6_u64;
    let mut pending = Pending::new(action);
    for number in 1..=rounds {
        let fate = if until_unknown == 0 {
            until_unknown = 6;
            Fate::Unknown
        } else {
            until_unknown = until_unknown.saturating_sub(1);
            Fate::Applied
        };
        if !pending.push(&mut journal, &mut oracle, number, fate, number == rounds)? {
            continue;
        }
        if let Some(successor) = continue_if_due(&mut journal)? {
            journal = successor;
            continued = continued.saturating_add(1);
        }
    }
    assert_fold_agrees(&journal, &oracle, tenant)?;
    assert!(
        continued > 1,
        "tenant {tenant} continued {continued} times, which is not enough to \
         say a chain of files works"
    );
    Ok((oracle, continued, journal.path().to_path_buf()))
}

/// Two tenants over one directory continue independently and lose nothing.
pub(super) fn tenant_continuation(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("tenants")?;
        let rounds = 90 + u64::from(sim.rng().below(40));
        let mut oracles: BTreeMap<u64, Oracle> = BTreeMap::new();
        let mut lives: BTreeMap<u64, PathBuf> = BTreeMap::new();
        let mut continuations: BTreeMap<u64, u64> = BTreeMap::new();

        for tenant in 1_u64..=2 {
            let (oracle, continued, live) = run_tenant(&dir, tenant, rounds)?;
            oracles.insert(tenant, oracle);
            continuations.insert(tenant, continued);
            lives.insert(tenant, live);
        }

        assert_ne!(lives[&1], lives[&2], "the two tenants share a journal");
        for tenant in 1_u64..=2 {
            let mine = u64::MAX.saturating_sub(tenant);
            let theirs = u64::MAX.saturating_sub(3_u64.saturating_sub(tenant));
            let handle = FileJournal::open(&lives[&tenant])?;
            for held in handle.recover().attempts() {
                assert_eq!(
                    held.key().action(),
                    action_of(mine)?,
                    "tenant {tenant} recovered an attempt belonging to another tenant"
                );
                assert_ne!(
                    held.key().action(),
                    action_of(theirs)?,
                    "no key crosses tenants"
                );
            }
            assert_eq!(
                oracles[&tenant].dispatched, rounds,
                "tenant {tenant} dispatched every attempt exactly once"
            );
        }
        sim.record("tenants-continue-independently");
        sim.trace.record_number("tenant-rounds", rounds);
        for (tenant, continued) in &continuations {
            sim.trace
                .record_number(&format!("tenant-{tenant}-continuations"), *continued);
        }
        Ok(())
    })
}
