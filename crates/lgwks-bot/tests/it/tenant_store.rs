//! Black-box acceptance for the estate-owned keyed run store (#317).
//!
//! The [`TenantStore`](lgwks_bot::tenant_store::TenantStore) is what
//! `logical_ci`'s `src/store.rs` needs without its direct `rusqlite` edge:
//! concurrent coordinators on one host, a dispatch row committed before each
//! lane starts, a transactional close of runs whose coordinator died, and
//! per-tenant keyed reads. Every fact here is proved against the real file,
//! the real lock, and the real `sync_all` — nothing is mocked, and a torn
//! tail is cut with real bytes.
//!
//! - **One hundred tenants at once.** One hundred coordinators share one
//!   directory over one hundred threads, each running a full run lifecycle,
//!   and each tenant afterwards lists exactly its own run with its own
//!   verdict. The wall time is recorded, not printed.
//! - **A killed coordinator is reconciled.** A store dropped with a run open
//!   reopens to that run in `open_runs`; the abandonment closes the run and
//!   every lane in one frame, the dispatched lane reads unknown rather than a
//!   pass, and the closer's record stands against a later finish.
//! - **Reads are keyed and bounded.** Prefix lookup names one run or refuses
//!   both ways, listings cap at 200 newest-first, and policies write once.
//! - **Refusals leave bytes.** A foreign file, a newer version, and a
//!   corrupted frame are refused with the file byte-identical; a torn tail is
//!   trimmed; a held lock is a typed busy refusal.

use std::error::Error;
use std::thread::Builder;
use std::time::{Duration, Instant};

use lgwks_bot::tenant_store::{RunStart, TenantStore, TenantStoreError};

use crate::journal_fixtures as shared;
use crate::scratch::Scratch;

/// The test result: fixtures bubble with `?`, like the rest of the estate.
type TestResult = Result<(), Box<dyn Error>>;

/// How many tenants coordinate at once in the concurrency journey.
const TENANTS: usize = 100;

/// The lanes every journey run plans.
const LANES: [&str; 2] = ["fmt", "clippy"];

/// A tenant name that sorts with its number, so listings order by tenant.
fn tenant_name(tenant_no: usize) -> String {
    format!("tenant-{tenant_no:03}")
}

/// A run id unique to its tenant and sequence number.
fn run_name(tenant_no: usize, seq_no: usize) -> String {
    format!("run-{tenant_no:03}-{seq_no:03}")
}

/// A moment string that orders like the sequence number it carries.
fn moment(seq_no: usize) -> String {
    format!("t-{seq_no:06}")
}

/// One tenant's whole lifecycle: policy, begin, both dispatches, both ends,
/// and the sealed finish.
fn tenant_lifecycle(store: &TenantStore, tenant_no: usize) -> Result<(), String> {
    let tenant = tenant_name(tenant_no);
    let run = run_name(tenant_no, 0);
    let map = |error: TenantStoreError| error.to_string();
    store
        .init_policy(&tenant, "[\"fmt\"]", 1, "2026-10-07T00:00:00Z")
        .map_err(map)?;
    store
        .begin_run(
            &RunStart::new(
                &run,
                &tenant,
                "/repo",
                "host",
                1,
                "lock",
                &moment(tenant_no),
            ),
            &LANES,
        )
        .map_err(map)?;
    for (lane_no, lane_id) in LANES.iter().enumerate() {
        store.lane_dispatched(&run, lane_no).map_err(map)?;
        store
            .lane_ended(&run, lane_no, "pass", lane_id, b"tail")
            .map_err(map)?;
    }
    let finished = store
        .finish_run(&run, "GO", &moment(tenant_no), "record", "seal")
        .map_err(map)?;
    if !finished {
        return Err(format!(
            "{tenant}: a run its coordinator finished reads as already closed"
        ));
    }
    Ok(())
}

/// A hundred tenants coordinate on one host: one hundred threads over one
/// directory, each running [`tenant_lifecycle`], and each tenant afterwards
/// lists exactly its own run with its own verdict and no other tenant's.
#[test]
fn a_hundred_tenants_coordinate_on_one_host() -> TestResult {
    let scratch = Scratch::new("tenant-store-hundred")?;
    let store = TenantStore::open(scratch.path())?;
    let started = Instant::now();
    let mut handles = Vec::with_capacity(TENANTS);
    for tenant_no in 0..TENANTS {
        let worker = store.clone();
        handles.push(
            Builder::new()
                .name(tenant_name(tenant_no))
                .spawn(move || tenant_lifecycle(&worker, tenant_no))
                .map_err(|error| format!("could not spawn tenant {tenant_no}: {error}"))?,
        );
    }
    for (tenant_no, handle) in handles.into_iter().enumerate() {
        match handle.join() {
            Ok(outcome) => outcome.map_err(|report| format!("tenant {tenant_no}: {report}"))?,
            Err(_) => return Err(format!("tenant {tenant_no}: its thread did not finish").into()),
        }
    }
    let elapsed = started.elapsed();
    for tenant_no in 0..TENANTS {
        let tenant = tenant_name(tenant_no);
        let rows = store.runs(&tenant, 10)?;
        assert_eq!(rows.len(), 1, "{tenant} must list exactly its own run");
        let row = &rows[0];
        assert_eq!(
            row.run_id(),
            run_name(tenant_no, 0),
            "{tenant} must list its own run id"
        );
        assert_eq!(
            row.verdict(),
            Some("GO"),
            "{tenant} must read its own verdict"
        );
        assert_eq!(row.seal(), Some("seal"), "{tenant} must read its own seal");
        let found = store.run(&tenant, &run_name(tenant_no, 0))?;
        assert_eq!(
            found.run_id(),
            run_name(tenant_no, 0),
            "{tenant} must resolve its own full id"
        );
    }
    shared::record_measurement(&format!(
        "tenant_store hundred_tenants_ms={}",
        elapsed.as_millis()
    ))?;
    Ok(())
}

/// A coordinator killed with a run open leaves a record its successor
/// reconciles: the reopened store lists the run as open, the abandonment
/// closes it and every lane in one frame, the dispatched lane reads unknown
/// rather than a pass, and the first closer's record stands.
#[test]
fn a_killed_coordinator_is_reconciled() -> TestResult {
    let scratch = Scratch::new("tenant-store-kill")?;
    let dir = scratch.path();
    let run = "run-kill-001";
    {
        let store = TenantStore::open(dir)?;
        store.init_policy("acme", "[\"fmt\"]", 1, "2026-10-07T00:00:00Z")?;
        store.begin_run(
            &RunStart::new(
                run,
                "acme",
                "/repo",
                "host",
                4242,
                "run-kill-001.lock",
                "t-000001",
            ),
            &LANES,
        )?;
        store.lane_dispatched(run, 0)?;
        assert_eq!(
            store.coordinator_lock(run)?,
            "run-kill-001.lock",
            "the successor must find the dead coordinator's lock name"
        );
    }
    let store = TenantStore::open(dir)?;
    let open = store.open_runs("host")?;
    assert_eq!(open.len(), 1, "the killed run must reopen as open");
    assert_eq!(open[0].run_id(), run, "the open run must be the killed one");
    assert!(store.abandon(run, "t-000002")?, "the open run must abandon");
    assert!(
        !store.abandon(run, "t-000003")?,
        "a second abandon must change nothing"
    );
    assert!(
        !store.finish_run(run, "GO", "t-000004", "record", "seal")?,
        "the abandonment is the closer of record, not the late finish"
    );
    let row = store.run("acme", run)?;
    assert_eq!(
        row.verdict(),
        Some("UNMEASURED"),
        "an abandoned run has no verdict"
    );
    assert!(
        row.reason().is_some(),
        "an abandoned run must say why it closed"
    );
    let lanes = store.lanes(run)?;
    assert_eq!(lanes.len(), 2, "both lanes must survive the abandonment");
    assert_eq!(
        lanes[0].state(),
        "ended",
        "the dispatched lane must be ended"
    );
    assert_eq!(
        lanes[0].outcome.as_deref(),
        Some("outcome-unknown"),
        "a lane that may or may not have run is unknown, never a pass"
    );
    assert_eq!(lanes[1].state(), "ended", "the pending lane must be ended");
    assert_eq!(
        lanes[1].outcome.as_deref(),
        Some("unmeasured"),
        "a lane that never started is unmeasured"
    );
    assert!(
        store.open_runs("host")?.is_empty(),
        "nothing must stay open"
    );
    Ok(())
}

/// Prefix lookup names exactly one run: a full id resolves, a shared prefix
/// is ambiguous, and a miss names its tenant.
#[test]
fn prefix_lookup_names_one_run_or_refuses() -> TestResult {
    let scratch = Scratch::new("tenant-store-prefix")?;
    let store = TenantStore::open(scratch.path())?;
    for seq_no in 0..3 {
        let run = format!("run-abc-{seq_no}");
        store.begin_run(
            &RunStart::new(&run, "acme", "/repo", "host", 1, "lock", &moment(seq_no)),
            &["only"],
        )?;
    }
    let found = store.run("acme", "run-abc-1")?;
    assert_eq!(found.run_id(), "run-abc-1", "a full id must resolve");
    match store.run("acme", "run-abc-") {
        Err(TenantStoreError::Ambiguous { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("a shared prefix must be ambiguous, got {other}").into()),
        Ok(_) => return Err("a shared prefix must not resolve".into()),
    }?;
    match store.run("acme", "run-zzz-") {
        Err(TenantStoreError::NoSuchRun { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("a miss must be NoSuchRun, got {other}").into()),
        Ok(_) => return Err("a miss must not resolve".into()),
    }?;
    match store.run("other", "run-abc-1") {
        Err(TenantStoreError::NoSuchRun { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => Err(format!("another tenant must not name the run, got {other}").into()),
        Ok(_) => Err("another tenant must not name the run".into()),
    }
}

/// A policy is written once: the second init is refused, the first stands,
/// and a tenant that never ran init has no policy.
#[test]
fn a_policy_is_written_once() -> TestResult {
    let scratch = Scratch::new("tenant-store-policy")?;
    let store = TenantStore::open(scratch.path())?;
    store.init_policy("acme", "[\"fmt\"]", 2, "2026-10-07T00:00:00Z")?;
    match store.init_policy("acme", "[\"clippy\"]", 3, "2026-10-08T00:00:00Z") {
        Err(TenantStoreError::AlreadyInitialised { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("a second init must be refused, got {other}").into()),
        Ok(()) => return Err("a second init must not rewrite the policy".into()),
    }?;
    let policy = store
        .policy("acme")?
        .ok_or_else(|| "the first policy must survive its successor's refusal".to_owned())?;
    assert_eq!(
        policy.gates_json(),
        "[\"fmt\"]",
        "the first gates must stand"
    );
    assert_eq!(policy.hardness, 2, "the first hardness must stand");
    assert!(
        store.policy("nobody")?.is_none(),
        "a tenant that never ran init has no policy"
    );
    Ok(())
}

/// A foreign file, a newer version, and a corrupted frame are refused with
/// the file byte-identical: refusals never move evidence.
#[test]
fn refusals_leave_the_bytes_untouched() -> TestResult {
    let scratch = Scratch::new("tenant-store-refusals")?;
    let dir = scratch.path();
    let foreign = dir.join("tenant.store");
    std::fs::write(&foreign, b"this is not a tenant store")?;
    match TenantStore::open(dir) {
        Err(TenantStoreError::NotAStore) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("a foreign file must be NotAStore, got {other}").into()),
        Ok(_) => return Err("a foreign file must not open".into()),
    }?;
    assert_eq!(
        std::fs::read(&foreign)?,
        b"this is not a tenant store",
        "a refused foreign file must not move"
    );
    let mut newer = *b"lgwks-tstore\x00\x00\x00\x00";
    newer[15] = 9;
    std::fs::write(&foreign, newer)?;
    match TenantStore::open(dir) {
        Err(TenantStoreError::FormatVersion { found, expected }) => {
            assert_eq!(found, 9, "the refusal must name the version it met");
            assert_eq!(expected, 1, "the refusal must name the version it reads");
            Ok::<(), Box<dyn Error>>(())
        }
        Err(other) => {
            return Err(format!("a newer version must be FormatVersion, got {other}").into());
        }
        Ok(_) => return Err("a newer version must not open".into()),
    }?;
    assert_eq!(
        std::fs::read(&foreign)?,
        newer,
        "a refused version must not move"
    );
    std::fs::remove_file(&foreign)?;
    let store = TenantStore::open(dir)?;
    store.begin_run(
        &RunStart::new(
            "run-rot-001",
            "acme",
            "/repo",
            "host",
            1,
            "lock",
            "t-000001",
        ),
        &["only"],
    )?;
    drop(store);
    let before = std::fs::read(&foreign)?;
    let mut rotted = before;
    let byte = rotted[16];
    rotted[16] = byte.wrapping_add(1);
    std::fs::write(&foreign, &rotted)?;
    match TenantStore::open(dir) {
        Err(TenantStoreError::Corrupt { at }) => {
            assert_eq!(at, 0, "the rot is in the first frame");
            Ok::<(), Box<dyn Error>>(())
        }
        Err(other) => return Err(format!("rot must be Corrupt, got {other}").into()),
        Ok(_) => return Err("rot must not open".into()),
    }?;
    assert_eq!(
        std::fs::read(&foreign)?,
        rotted,
        "refused rot must not move"
    );
    Ok(())
}

/// An append cut mid-frame is an interrupted append, not a record: the next
/// open trims it and keeps every acknowledged frame.
#[test]
fn a_cut_append_is_trimmed_and_kept_frames_survive() -> TestResult {
    let scratch = Scratch::new("tenant-store-cut")?;
    let dir = scratch.path();
    let store = TenantStore::open(dir)?;
    for seq_no in 0..2 {
        let run = format!("run-cut-{seq_no}");
        store.begin_run(
            &RunStart::new(&run, "acme", "/repo", "host", 1, "lock", &moment(seq_no)),
            &["only"],
        )?;
    }
    drop(store);
    let path = dir.join("tenant.store");
    let whole = std::fs::read(&path)?;
    let cut = whole.len().saturating_sub(5);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)?
        .set_len(
            u64::try_from(cut).map_err(|error| format!("the cut is past the file: {error}"))?,
        )?;
    let store = TenantStore::open(dir)?;
    assert_eq!(
        store.lanes("run-cut-0")?.len(),
        1,
        "the acknowledged run must survive the cut"
    );
    match store.lanes("run-cut-1") {
        Err(TenantStoreError::NoSuchRun { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("the cut run was never acknowledged, got {other}").into()),
        Ok(_) => return Err("the cut run must not exist".into()),
    }?;
    let whole_len = u64::try_from(whole.len())
        .map_err(|error| format!("the whole file does not fit: {error}"))?;
    let trimmed = std::fs::metadata(&path)?.len();
    assert!(
        trimmed < whole_len,
        "the trim must remove the torn tail rather than keep the cut"
    );
    drop(store);
    let store = TenantStore::open(dir)?;
    assert_eq!(
        store.lanes("run-cut-0")?.len(),
        1,
        "the trim must be stable across a second reopen"
    );
    assert_eq!(
        std::fs::metadata(&path)?.len(),
        trimmed,
        "a second reopen must move nothing"
    );
    Ok(())
}

/// A lock held past the bound is a typed busy refusal, and the store opens
/// once the lock is gone: contention waits, then tells.
#[test]
fn a_held_lock_is_a_busy_refusal() -> TestResult {
    let scratch = Scratch::new("tenant-store-busy")?;
    let dir = scratch.path();
    std::fs::write(dir.join("tenant.lock"), b"someone-else")?;
    let waited = match TenantStore::open_with_busy_timeout(dir, Duration::from_millis(120)) {
        Err(TenantStoreError::Busy { waited }) => waited,
        Err(other) => return Err(format!("a held lock must be Busy, got {other}").into()),
        Ok(_) => return Err("a held lock must not open".into()),
    };
    assert!(
        waited >= Duration::from_millis(120),
        "the busy refusal must come after the bound, not before it"
    );
    std::fs::remove_file(dir.join("tenant.lock"))?;
    TenantStore::open(dir)?;
    Ok(())
}

/// The row preconditions refuse exactly: a second begin, a second dispatch,
/// a second end, and updates for runs that were never begun.
#[test]
fn row_preconditions_refuse_exactly() -> TestResult {
    let scratch = Scratch::new("tenant-store-rows")?;
    let store = TenantStore::open(scratch.path())?;
    store.begin_run(
        &RunStart::new(
            "run-row-001",
            "acme",
            "/repo",
            "host",
            1,
            "lock",
            "t-000001",
        ),
        &["only"],
    )?;
    match store.begin_run(
        &RunStart::new(
            "run-row-001",
            "acme",
            "/repo",
            "host",
            1,
            "lock",
            "t-000002",
        ),
        &["only"],
    ) {
        Err(TenantStoreError::DuplicateRun { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => {
            return Err(format!("a second begin must be DuplicateRun, got {other}").into());
        }
        Ok(()) => return Err("a second begin must not commit".into()),
    }?;
    store.lane_dispatched("run-row-001", 0)?;
    match store.lane_dispatched("run-row-001", 0) {
        Err(TenantStoreError::LaneLost { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("a second dispatch must be LaneLost, got {other}").into()),
        Ok(()) => return Err("a second dispatch must not commit".into()),
    }?;
    match store.lane_dispatched("run-row-001", 7) {
        Err(TenantStoreError::LaneLost { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => {
            return Err(format!("a dispatch past the plan must be LaneLost, got {other}").into());
        }
        Ok(()) => return Err("a dispatch past the plan must not commit".into()),
    }?;
    store.lane_ended("run-row-001", 0, "pass", "done", b"tail")?;
    match store.lane_ended("run-row-001", 0, "pass", "done", b"tail") {
        Err(TenantStoreError::LaneLost { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => return Err(format!("a second end must be LaneLost, got {other}").into()),
        Ok(()) => return Err("a second end must not overwrite".into()),
    }?;
    for missing in ["lane_dispatched", "lane_ended", "finish_run", "abandon"] {
        let refused = match missing {
            "lane_dispatched" => store.lane_dispatched("run-missing", 0).is_err(),
            "lane_ended" => store
                .lane_ended("run-missing", 0, "pass", "done", b"")
                .is_err(),
            "finish_run" => store
                .finish_run("run-missing", "GO", "t-1", "r", "s")
                .is_err(),
            _ => store.abandon("run-missing", "t-1").is_err(),
        };
        assert!(refused, "{missing} must refuse a run that was never begun");
    }
    match store.lanes("run-missing") {
        Err(TenantStoreError::NoSuchRun { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(other) => Err(format!("lanes for a missing run must be NoSuchRun, got {other}").into()),
        Ok(_) => Err("lanes for a missing run must not list".into()),
    }
}

/// Listings cap at 200 newest-first: the 201st run pages out, and order is by
/// moment then id.
#[test]
fn listings_cap_at_two_hundred_newest_first() -> TestResult {
    let scratch = Scratch::new("tenant-store-cap")?;
    let store = TenantStore::open(scratch.path())?;
    for seq_no in 0..205 {
        let run = format!("run-cap-{seq_no:03}");
        store.begin_run(
            &RunStart::new(&run, "acme", "/repo", "host", 1, "lock", &moment(seq_no)),
            &["only"],
        )?;
    }
    let rows = store.runs("acme", 5_000)?;
    assert_eq!(rows.len(), 200, "the listing must cap at 200");
    assert_eq!(
        rows[0].run_id(),
        "run-cap-204",
        "the newest run must list first"
    );
    assert_eq!(
        rows[199].run_id(),
        "run-cap-005",
        "the cap must page out the oldest"
    );
    let two = store.runs("acme", 2)?;
    assert_eq!(two.len(), 2, "a smaller limit must be honored");
    Ok(())
}
