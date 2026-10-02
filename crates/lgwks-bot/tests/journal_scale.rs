//! Append-path scale: latency tails and concurrent tenant appends.
//!
//! Two measurements against the real [`FileJournal`] on a real file: the
//! p50/p95/p99 of one acknowledged append (the shipped path, where the write
//! and `sync_all` run on the storage owner thread, not the caller's), and a
//! sweep of concurrent tenant appends at 100. The tail numbers are written to
//! `LGWKS_SCALE_OUT` when that variable names a path, so the report can carry
//! a measured value rather than an assertion that merely passed.

use std::error::Error;
use std::time::Instant;

use lgwks_bot::journal::{DurabilityPromise, EffectEvent, EffectJournal, FileJournal};

#[path = "support/journal.rs"]
mod shared;

use shared::{TempGuard, key, scratch};

/// How many acknowledged appends the latency family times.
const SAMPLES: usize = 1_024;
/// Tenant journals in the concurrent sweep. Bounded because each `FileJournal`
/// owns one storage-owner OS thread; the sweep is a concurrency claim, not a
/// thread-count stress, and 100 is the level the report claims.
const TENANTS: u64 = 100;

type TestResult = Result<(), Box<dyn Error>>;

/// The nearest-rank percentile of an already-sorted slice, in microseconds.
fn percentile(sorted: &[u128], percent: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted
        .len()
        .saturating_mul(percent)
        .saturating_add(99)
        .checked_div(100)
        .unwrap_or(1)
        .saturating_sub(1);
    sorted[rank.min(sorted.len().saturating_sub(1))]
}

/// One acknowledged append's latency tails on the real path.
///
/// Every sample is a full `compare_and_append`: fence, frame, write and
/// `sync_all` on the storage owner, then the acknowledgment. The bounds are
/// generous — the assertion is that the path is not pathological on this host,
/// and the exact tails are the measured value the report carries.
#[test]
fn append_latency_tails_are_bounded() -> TestResult {
    let dir = scratch("latency");
    std::fs::create_dir_all(&dir)?;
    let _guard = TempGuard(dir.clone());
    let path = dir.join("journal.log");
    let mut journal = FileJournal::open(&path)?;

    let mut samples: Vec<u128> = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let attempt = u64::try_from(index)?.saturating_add(1);
        let event = EffectEvent::IntentAdmitted { key: key(attempt)? };
        let start = Instant::now();
        let ack = journal.compare_and_append(journal.tail(), &event)?;
        samples.push(start.elapsed().as_micros());
        assert_eq!(
            ack.promise(),
            DurabilityPromise::ProcessCrash,
            "every sampled append must earn the durable promise"
        );
    }
    samples.sort_unstable();
    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    let p99 = percentile(&samples, 99);
    let max = samples.last().copied().unwrap_or(0);
    if let Some(out) = std::env::var_os("LGWKS_SCALE_OUT") {
        std::fs::write(
            out,
            format!("p50_us={p50} p95_us={p95} p99_us={p99} max_us={max}\n"),
        )?;
    }
    assert_eq!(journal.events().count(), SAMPLES, "every append committed");
    assert!(
        p99 < 250_000,
        "append p99 {p99}us is pathological on this host (p50={p50} p95={p95} max={max})"
    );
    Ok(())
}

/// Concurrent tenant appends at 100 stay isolated and all earn the durable
/// promise.
///
/// Each tenant owns its own file and its own storage-owner thread, so the
/// appends genuinely overlap. No tenant sees another's events.
#[test]
fn concurrent_tenant_appends_at_100_stay_isolated() -> TestResult {
    let dir = scratch("concurrent");
    std::fs::create_dir_all(&dir)?;
    let _guard = TempGuard(dir.clone());

    let mut handles = Vec::with_capacity(usize::try_from(TENANTS)?);
    for tenant in 0..TENANTS {
        let path = dir.join(format!("tenant-{tenant}.jrnl"));
        let handle = std::thread::Builder::new()
            .name(format!("tenant-{tenant}"))
            .spawn(move || -> Result<usize, String> {
                let mut journal = FileJournal::open(&path).map_err(|error| error.to_string())?;
                for step in 1..=2u64 {
                    let attempt = tenant.saturating_mul(1_000).saturating_add(step);
                    let key = key(attempt).map_err(|error| error.to_string())?;
                    let tail = journal.tail();
                    journal
                        .compare_and_append(tail, &EffectEvent::IntentAdmitted { key })
                        .map_err(|error| error.to_string())?;
                }
                Ok(journal.events().count())
            })
            .map_err(|error| format!("failed to spawn a tenant thread: {error}"))?;
        handles.push(handle);
    }

    for handle in handles {
        let count = handle.join().map_err(|_| "a tenant thread panicked")??;
        assert_eq!(count, 2, "a tenant journal held another tenant's events");
    }
    Ok(())
}
