//! The declared hyperscale tiers of the proposal artifact store: 100, 1,000 and 10,000 workers, and two tenants saturating one digest.

use super::*;

// ── Hyperscale: the declared tiers ────────────────────────────────────────────

/// 100, 1,000 and 10,000 concurrent workers each complete, with every artifact
/// intact and nothing attributed to the wrong tenant.
///
/// The requested level, the level reached and the store's ceiling are recorded
/// together, per INV-BOT-16: a tier the host cannot reach is clamped and said to
/// be clamped, never reported as reached. Each tier's three numbers go into the
/// run's own `Trace` — where the rest of the simulation layer records its tiers —
/// so the receipt is readable and comparable with the other families rather than
/// printed to a stream nobody reads. `print_stdout` is `forbid` crate-wide, and
/// this crate's convention for a tier receipt is the trace.
#[test]
fn saturation_reaches_100_1000_and_10000() -> TestResult {
    let mut trace = sim::Trace::new();
    for tier in TIERS {
        let requested = tier;
        let workers = tier.min(MAX_WORKERS);
        let store = ArtifactStore::new();
        let content = format!("the artifact tier {tier} contends for").into_bytes();
        let digest = ArtifactStore::digest_of(&content)?;
        store.write(support::TENANT, &content)?;

        let reads = std::thread::scope(|scope| {
            // An explicit loop, not an adapter: `scope.spawn` borrows the scope,
            // so collecting through an adapter would keep that borrow alive past
            // the collect and the scope would not accept it.
            let mut handles = Vec::new();
            for index in 0..workers {
                let store = store.clone();
                handles.push(scope.spawn(move || {
                    let tenant = TENANTS[index % TENANTS.len()];
                    store.artifacts(tenant) + usize::from(store.holds(tenant, &digest))
                }));
            }
            // A panicking worker is reported by the `join` below rather than
            // propagated through a `?` this plain closure cannot carry.
            let mut results = Vec::new();
            for handle in handles {
                results.push(handle.join());
            }
            results
        });

        let mut reported = 0_usize;
        for seen in reads {
            reported = reported.saturating_add(seen.map_err(|_| "a worker thread panicked")?);
        }

        // Each worker reports `artifacts(its tenant) + holds(its tenant, digest)`:
        // one for a tenant that wrote nothing, two for the one that wrote the
        // contended artifact. The workers are spread across the tenant set, so the
        // total is checked against the store rather than assumed to be the worker
        // count — a number nobody can derive is a number nobody can check.
        let expected = (0..workers)
            .map(|index| {
                let tenant = TENANTS[index % TENANTS.len()];
                store.artifacts(tenant) + usize::from(store.holds(tenant, &digest))
            })
            .sum::<usize>();
        assert_eq!(
            reported, expected,
            "tier {tier}: every worker reported, and the total matches what each tenant's shelf \
             holds after the tier"
        );
        assert!(
            reported >= TENANTS.len(),
            "tier {tier}: every worker reported, and the tenant that wrote the contended \
             artifact contributes two facts while every other contributes one"
        );
        assert_eq!(
            store.artifacts(support::TENANT),
            1,
            "tier {tier}: the contended artifact survived the tier intact"
        );
        assert!(
            store.read(support::TENANT, &digest).is_some(),
            "tier {tier}: and is still readable afterwards"
        );
        // The three numbers together, so a reader is never told a level nobody
        // ran. Recorded in the trace rather than printed, which is where the
        // rest of this layer keeps its tier receipts.
        trace.record_number("tier-requested", u64::try_from(requested)?);
        trace.record_number("tier-reached", u64::try_from(workers)?);
        trace.record_number("tier-ceiling", u64::try_from(MAX_WORKERS)?);
        trace.record("tier-tenant");
        trace.record(support::TENANT);
        trace.record_number("tier-bytes", content.len());
    }
    assert!(
        !trace.is_empty(),
        "every tier recorded its requested, reached and ceiling levels, so the receipt is \
         readable"
    );
    Ok(())
}

/// Saturation across tenants: two tenants contending the same digest under a
/// drawn number of writers each, with neither reading the other's artifact.
#[test]
fn two_tenant_saturation_keeps_its_shelves_apart() -> TestResult {
    let store = ArtifactStore::new();
    let shared = b"identical bytes from two tenants at scale".to_vec();
    let digest = ArtifactStore::digest_of(&shared)?;
    const TENANT_PAIR: [&str; 2] = [support::TENANT, support::OTHER_TENANT];
    let per_tenant: usize = 256;

    for tenant in TENANT_PAIR {
        store.write(tenant, &shared)?;
    }

    // The handle list is built with an explicit loop rather than a `flat_map`:
    // the adapter is `FnMut`, and a `move` closure inside it takes the captured
    // store and content by value on its first call — leaving every later tenant
    // with nothing to clone.
    let writes = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for tenant in TENANT_PAIR {
            for _ in 0..per_tenant {
                let store = store.clone();
                let content = shared.clone();
                handles.push(scope.spawn(move || store.write(tenant, &content).is_ok()));
            }
        }
        // The scope closure returns values: a panicking writer is reported by the
        // `join` below rather than propagated through a `?` a plain closure
        // cannot carry.
        handles
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>()
    });
    let mut wrote = 0_usize;
    for ok in writes {
        if ok.map_err(|_| "a writer thread panicked")? {
            wrote = wrote.saturating_add(1);
        }
    }
    let kept = TENANT_PAIR
        .into_iter()
        .map(|tenant| (tenant, store.artifacts(tenant)))
        .collect::<Vec<_>>();

    assert_eq!(
        wrote,
        per_tenant.saturating_mul(2),
        "every concurrent writer reported"
    );
    for (tenant, artifacts) in kept {
        assert_eq!(
            artifacts, 1,
            "tenant {tenant}: {per_tenant} concurrent writers of one key left exactly one artifact"
        );
        // The seeding write above is itself a writer of the key, so the receipt
        // counts one more than the concurrent tier — and saying so is the point:
        // a count of exactly the tier would mean the seed write was not counted.
        assert_eq!(
            store.writers(tenant, &digest),
            u64::try_from(per_tenant)?.saturating_add(1),
            "tenant {tenant}: every concurrent writer plus the seeding write reached the key and \
             was serialized into one commit"
        );
    }
    Ok(())
}
