//! Seeded simulations of the world's request system (ADR 0006 R4).
//!
//! Source: the R4 falsifier, "two tenants never read each other's state", and
//! OWASP's broken-object-level-authorization class (API1:2023), where one
//! tenant's request reaches another's record. One seed drives the tenants, the
//! methods, the malformed names, the close point and the threads' streams; the
//! same seed gives the same trace hash, checked by running each seed twice.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use lgwks_std::seeded::Seeded;

use super::{MAX_TENANTS, Request, RequestCounts, RequestFeed, RequestRefused, Response};

/// Seeds each simulation runs.
const SEEDS: u64 = 1_000;

/// A per-tenant counter: `POST /count` adds one, `GET /count` reads it, and a
/// body of `boom` panics inside the handler while it holds the partition.
fn counter() -> RequestFeed {
    RequestFeed::new(|request: &Request<'_>, count: &mut u64| {
        assert!(request.body() != b"boom", "the handler panics on request");
        if request.method() == "POST" {
            *count = count.saturating_add(1);
        }
        Response::new(200, count.to_string().into_bytes())
    })
}

/// FNV-1a folded over `bytes` into `hash`.
fn fold(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// How one seeded run broke the model.
#[derive(Debug)]
enum SeedFailed {
    /// The seeded stream refused a draw.
    Draw(String),
    /// A request was answered after the feed closed.
    ClosedAnswered,
    /// A tenant read a count its own requests did not make.
    ReadAnother(String),
    /// A request was refused with the feed open.
    Refused(RequestRefused),
    /// The feed's counts disagree with the answers the run saw.
    Counts(RequestCounts, u64, u64),
    /// The feed holds a different number of tenants than were served.
    Tenants(usize, usize),
}

impl std::fmt::Display for SeedFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Draw(ref cause) => write!(f, "draw: {cause}"),
            Self::ClosedAnswered => f.write_str("a closed feed answered"),
            Self::ReadAnother(ref tenant) => write!(f, "{tenant} read another tenant's count"),
            Self::Refused(refusal) => write!(f, "unexpected refusal {refusal}"),
            Self::Counts(counts, settled, refused) => {
                write!(f, "counts {counts:?} against {settled}/{refused}")
            }
            Self::Tenants(held, served) => write!(f, "{held} tenants held, {served} served"),
        }
    }
}

/// Log `failure` for `seed` and hand it back, so every failed seed leaves a
/// record.
fn failed<T>(seed: u64, failure: SeedFailed) -> Result<T, SeedFailed> {
    lgwks_std::trace::warn!(seed, %failure, "sim_requests: a seed failed");
    Err(failure)
}

/// One seeded run over one thread: every answer checked against a per-tenant
/// model; the trace hash of every answer in order.
fn run(seed: u64) -> Result<u64, SeedFailed> {
    let mut stream = Seeded::from_seed(seed);
    let draw = |stream: &mut Seeded, bound: u64| {
        stream
            .below(bound)
            .map_err(|error| SeedFailed::Draw(error.to_string()))
    };
    let tenants = draw(&mut stream, 64)?.saturating_add(1);
    let operations = draw(&mut stream, 400)?.saturating_add(1);
    let close_at = draw(&mut stream, operations.saturating_mul(2))?;
    let feed = counter();
    let mut model: BTreeMap<String, u64> = BTreeMap::new();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let (mut settled, mut refused) = (0_u64, 0_u64);
    let mut closed = false;
    for step in 0..operations {
        if step == close_at {
            feed.close();
            closed = true;
        }
        let tenant = format!("tenant-{}", draw(&mut stream, tenants)?);
        // One request in sixteen names a tenant no valid name can be.
        let name = if draw(&mut stream, 16)? == 0 {
            "bad/tenant"
        } else {
            tenant.as_str()
        };
        let method = if draw(&mut stream, 2)? == 0 {
            "POST"
        } else {
            "GET"
        };
        let Ok(request) = Request::new(name, method, "/count", b"") else {
            hash = fold(hash, b"refused-name");
            continue;
        };
        match feed.answer(&request) {
            Ok(_) if closed => return failed(seed, SeedFailed::ClosedAnswered),
            Ok(response) => {
                let count = model.entry(tenant.clone()).or_default();
                if method == "POST" {
                    *count = count.saturating_add(1);
                }
                let body = response.into_body();
                if body != count.to_string().into_bytes() {
                    return failed(seed, SeedFailed::ReadAnother(tenant));
                }
                settled = settled.saturating_add(1);
                hash = fold(hash, &body);
            }
            Err(RequestRefused::Closed) if closed => {
                refused = refused.saturating_add(1);
                hash = fold(hash, b"closed");
            }
            Err(other) => return failed(seed, SeedFailed::Refused(other)),
        }
    }
    let counts = feed.counts();
    if counts.settled != settled || counts.refused != refused {
        return failed(seed, SeedFailed::Counts(counts, settled, refused));
    }
    if counts.tenants != model.len() {
        return failed(seed, SeedFailed::Tenants(counts.tenants, model.len()));
    }
    Ok(hash)
}

#[test]
fn sim_tenants_read_only_their_own_state_and_replay_by_seed() -> Result<(), String> {
    for seed in 0..SEEDS {
        let first = run(seed).map_err(|failure| format!("seed {seed}: {failure}"))?;
        let again = run(seed).map_err(|failure| format!("seed {seed}: {failure}"))?;
        if first != again {
            return Err(format!("seed {seed}: trace {first:x} then {again:x}"));
        }
    }
    Ok(())
}

#[test]
fn sim_threads_on_shared_tenants_lose_no_update() -> Result<(), String> {
    // 16 threads is the host's cap on real parallelism here; each sends
    // 6,250 posts, 100,000 in all, over tenants the threads share.
    const THREADS: u64 = 16;
    const EACH: u64 = 6_250;
    for seed in 0..8_u64 {
        let feed = counter();
        let tenants = Seeded::from_seed(seed)
            .below(32)
            .map_err(|error| error.to_string())?
            .saturating_add(1);
        let sent: Vec<BTreeMap<u64, u64>> = std::thread::scope(|scope| {
            // Every thread is started before any is joined, so they run at once.
            let mut workers = Vec::new();
            for thread in 0..THREADS {
                let feed = feed.clone();
                workers.push(scope.spawn(move || {
                    let mut stream =
                        Seeded::from_seed(seed.wrapping_mul(THREADS).wrapping_add(thread));
                    let mut sent: BTreeMap<u64, u64> = BTreeMap::new();
                    for _ in 0..EACH {
                        let Ok(tenant) = stream.below(tenants) else {
                            break;
                        };
                        let name = format!("t{tenant}");
                        if let Ok(request) = Request::new(&name, "POST", "/count", b"")
                            && feed.answer(&request).is_ok()
                        {
                            let count = sent.entry(tenant).or_default();
                            *count = count.saturating_add(1);
                        }
                    }
                    sent
                }));
            }
            workers
                .into_iter()
                .filter_map(|worker| worker.join().ok())
                .collect()
        });
        let mut expected: BTreeMap<u64, u64> = BTreeMap::new();
        for one in &sent {
            for (&tenant, &count) in one {
                let total = expected.entry(tenant).or_default();
                *total = total.saturating_add(count);
            }
        }
        for (tenant, count) in expected {
            let name = format!("t{tenant}");
            let request =
                Request::new(&name, "GET", "/count", b"").map_err(|error| error.to_string())?;
            let answer = feed.answer(&request).map_err(|error| error.to_string())?;
            let body = answer.into_body();
            if body != count.to_string().into_bytes() {
                return Err(format!(
                    "seed {seed}: {name} counted {body:?}, sent {count}"
                ));
            }
        }
        let counts = feed.counts();
        let reads = u64::try_from(counts.tenants).map_err(|error| error.to_string())?;
        if counts.settled != THREADS.saturating_mul(EACH).saturating_add(reads) {
            return Err(format!("seed {seed}: settled {:?}", feed.counts()));
        }
    }
    Ok(())
}

#[test]
fn sim_a_panicking_handler_leaves_its_partition_serving() -> Result<(), String> {
    for seed in 0..SEEDS {
        let feed = counter();
        let mut stream = Seeded::from_seed(seed);
        let before = stream.below(8).map_err(|error| error.to_string())?;
        let post =
            Request::new("acme", "POST", "/count", b"").map_err(|error| error.to_string())?;
        for _ in 0..before {
            feed.answer(&post).map_err(|error| error.to_string())?;
        }
        let boom =
            Request::new("acme", "POST", "/count", b"boom").map_err(|error| error.to_string())?;
        if catch_unwind(AssertUnwindSafe(|| feed.answer(&boom))).is_ok() {
            return Err(format!("seed {seed}: the handler did not panic"));
        }
        let after = feed.answer(&post).map_err(|error| error.to_string())?;
        let body = after.into_body();
        if body != before.saturating_add(1).to_string().into_bytes() {
            return Err(format!("seed {seed}: acme read {body:?} after the panic"));
        }
    }
    Ok(())
}

#[test]
fn sim_a_tenant_past_the_ceiling_is_answered_503_and_the_rest_still_serve() -> Result<(), String> {
    let feed = counter();
    for tenant in 0..MAX_TENANTS {
        let name = format!("t{tenant}");
        let request =
            Request::new(&name, "POST", "/count", b"").map_err(|error| error.to_string())?;
        feed.answer(&request).map_err(|error| error.to_string())?;
    }
    let late =
        Request::new("one-more", "POST", "/count", b"").map_err(|error| error.to_string())?;
    let refused = feed.answer(&late).map_err(|error| error.to_string())?;
    if refused.status() != 503 || feed.counts().refused != 1 {
        return Err(format!(
            "the tenant past the ceiling got {} ({:?})",
            refused.status(),
            feed.counts()
        ));
    }
    let known = Request::new("t0", "GET", "/count", b"").map_err(|error| error.to_string())?;
    let served = feed
        .answer(&known)
        .map_err(|error| error.to_string())?
        .into_body();
    if served != b"1" || feed.counts().tenants != MAX_TENANTS {
        return Err(format!("t0 read {served:?} at the ceiling"));
    }
    Ok(())
}
