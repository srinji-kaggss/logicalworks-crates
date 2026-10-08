//! Oracle for the `tenant-cache` task. One test per contract clause.
//!
//! Deterministic: every expected value is derived from the contract in the
//! prompt, and the concurrency test asserts per-call correctness rather than
//! timing. No sleeps anywhere.

use ai_trial::{CacheError, solve};
use lgwks_bot::rt::runtime::Runtime;

/// The runtime every test drives its futures on.
fn runtime() -> Result<Runtime, std::io::Error> {
    Runtime::new()
}

/// The value the contract fixes for `(tenant, resource)`.
fn expected(tenant: &str, resource: &str) -> Vec<u8> {
    format!("value-of-{tenant}-{resource}").into_bytes()
}

/// The token the contract accepts for `tenant`.
fn token(tenant: &str) -> String {
    format!("token-for-{tenant}")
}

#[test]
fn same_request_resolves_the_same_bytes_twice() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let first = runtime.block_on(solve("alpha", "doc", &token("alpha")));
    let second = runtime.block_on(solve("alpha", "doc", &token("alpha")));
    match (first, second) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a, expected("alpha", "doc"), "the contracted value");
            assert_eq!(b, expected("alpha", "doc"), "the same value again");
        }
        (a, b) => return Err(format!("expected Ok twice, got {a:?} then {b:?}").into()),
    }
    Ok(())
}

#[test]
fn two_tenants_on_one_resource_stay_isolated() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    let alpha = runtime.block_on(solve("alpha", "shared", &token("alpha")));
    let beta = runtime.block_on(solve("beta", "shared", &token("beta")));
    match (alpha, beta) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a, expected("alpha", "shared"), "alpha's own bytes");
            assert_eq!(b, expected("beta", "shared"), "beta's own bytes");
            assert_ne!(a, b, "one resource, two tenants: never the same bytes");
        }
        (a, b) => return Err(format!("expected Ok for both tenants, got {a:?} and {b:?}").into()),
    }
    Ok(())
}

#[test]
fn a_wrong_token_is_refused_and_caches_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    for tenant in ["alpha", "beta"] {
        match runtime.block_on(solve(tenant, "doc", "wrong-token")) {
            Err(CacheError::Refused) => {}
            other => return Err(format!("a wrong token for {tenant} is Refused, got {other:?}").into()),
        }
        // The refused call recorded nothing: the authorised call still resolves.
        match runtime.block_on(solve(tenant, "doc", &token(tenant))) {
            Ok(value) => assert_eq!(value, expected(tenant, "doc"), "the authorised value"),
            other => return Err(format!("the refused call must not poison the key, got {other:?}").into()),
        }
    }
    Ok(())
}

#[test]
fn another_tenants_token_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    match runtime.block_on(solve("alpha", "doc", &token("beta"))) {
        Err(CacheError::Refused) => {}
        other => return Err(format!("beta's token for alpha's request is Refused, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_thousand_mixed_calls_all_resolve_their_own_value() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    // Ten resources, each asked by both tenants: the first ten calls are all
    // alpha's, the next ten all beta's, so every resource is read under both
    // tenants and a resource-keyed cache answers beta with alpha's bytes.
    let results = runtime.block_on(futures::future::join_all((0..1000u32).map(|index| {
        let tenant = if index / 10 % 2 == 0 { "alpha" } else { "beta" };
        let resource = format!("r{}", index % 10);
        let token = token(tenant);
        async move {
            let outcome = solve(tenant, &resource, &token).await;
            (tenant, resource, outcome)
        }
    })));
    assert_eq!(results.len(), 1000, "every call answered");
    for (tenant, resource, outcome) in results {
        match outcome {
            Ok(value) => assert_eq!(
                value,
                expected(tenant, &resource),
                "no call ever observed another tenant's bytes"
            ),
            Err(error) => return Err(format!("all authorised calls resolve, got {error:?}").into()),
        }
    }
    Ok(())
}

#[test]
fn dropping_mid_flight_leaves_the_cache_usable() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime()?;
    // Start 200 calls and drop them mid-flight, then prove a later call still
    // resolves: a dropped call poisons nothing it shares with the others.
    runtime.block_on(async {
        let pending = futures::future::join_all((0..200u32).map(|index| {
            let tenant = if index % 2 == 0 { "alpha" } else { "beta" };
            let token = token(tenant);
            async move { solve(tenant, "drop", &token).await }
        }));
        let _ = lgwks_bot::rt::time::timeout(std::time::Duration::from_millis(1), pending).await;
    });
    match runtime.block_on(solve("alpha", "drop", &token("alpha"))) {
        Ok(value) => assert_eq!(value, expected("alpha", "drop"), "the cache still resolves"),
        other => return Err(format!("the cache still resolves after a dropped flight, got {other:?}").into()),
    }
    Ok(())
}
