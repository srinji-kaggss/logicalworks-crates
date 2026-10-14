//! The settlement family: a settle the store refuses is reported and leaves the request unsettled, one reported test per seed.

use super::*;

/// A failure reason too large for one record to carry.
///
/// *Above* the store's per-record byte ceiling, not a fraction of it: the
/// property under test is that a verdict the store cannot archive is refused,
/// and only a reason past the bound produces that. The bound is the ceiling plus
/// a margin, because the archived record also carries the disposition code, the
/// step path and the framing head, and a reason just under the ceiling would fit
/// and the write would succeed.
fn over_ceiling_reason() -> String {
    let over = lgwks_bot::task::MAX_RECORD_BYTES.saturating_add(1024);
    "r".repeat(over)
}

/// A body whose only act is to fail with a reason too large for one record.
///
/// The refusal under test is produced by the store's declared per-record byte
/// ceiling, so the size has to be a property of the body rather than of the
/// store: a store configured to refuse would be a second store, and a second
/// store is a second definition of what a terminal record is.
fn oversize_failure_task() -> Result<
    lgwks_bot::task::Task<impl Fn(lgwks_bot::script::Scope, u32) -> request::BodyFuture>,
    FlowError,
> {
    let reason = over_ceiling_reason();
    let body = move |_scope: lgwks_bot::script::Scope, _: u32| -> request::BodyFuture {
        let reason = reason.clone();
        Box::pin(async move { Err::<u32, FlowError>(FlowError::failed(reason)) })
    };
    task("oversize", body)
}

/// One request of the settle-refusal sweep: leave it in flight, then refuse to settle it.
fn settle_request(
    sim_run: &mut sim::Sim,
    scratch: &Scratch,
    index: u32,
    refused: &mut u32,
) -> TestResult {
    let tenant = tenant_for(index)?;
    let key = RequestKey::new(&format!("settle-{tenant}-{index}"))?;
    let payload = sim_run.rng().between(0, 1000);
    // One signal shared by the parked body and the dropping waiter: the
    // waiter must observe the *same* flag the body sets, or the drop would
    // fire at an arbitrary point and the family would be measuring luck.
    let recorded = Arc::new(AtomicBool::new(false));
    let parked = parking_task(Arc::clone(&recorded))?;

    // Leave the request in flight: a receipt and one durable record, and no
    // verdict yet. That is the only state a settle can be asked to settle.
    //
    // Its own host, and its own store handle: walking away from an append
    // latches that handle's poison (INV-BOT-50), because the bytes may be on
    // the disk under no acknowledgment. Every later step in this iteration
    // therefore *reopens* the file rather than sharing the handle, which is
    // also what a restart is.
    drop_after_first_step(
        Box::pin(host_on(tenant, reopen(scratch)?)?.submit(&key, &parked, payload)),
        &recorded,
    );
    let run = match lgwks_bot::block_on(
        host_on(tenant, reopen(scratch)?)?.submit(&key, &parked, payload),
    )? {
        Submission::InFlight(in_flight) => in_flight.run(),
        other => {
            let refusal = Err(format!(
                "seed {}: a dropped waiter must leave the request in flight; got {other:?}",
                sim_run.seed
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "settle_refusal_scenario: returning an error to the caller");
            return refusal;
        }
    };

    // The settle: a resume whose verdict is too large for one record.
    let settler = host_on(tenant, reopen(scratch)?)?;
    let settled = lgwks_bot::block_on(settler.resume(run, &oversize_failure_task()?, payload));
    assert_eq!(
        settled.disposition(),
        Disposition::Failed,
        "seed {}: a run that reached a verdict the store refused is Failed, not Succeeded",
        sim_run.seed
    );
    let rendered = settled
        .error()
        .ok_or("the refusal names its failure")?
        .to_string();
    assert!(
        rendered.contains("outcome was not recorded"),
        "seed {}: the report says the outcome was not recorded: {rendered}",
        sim_run.seed
    );
    assert!(
        rendered.contains("record's bytes"),
        "seed {}: the report names the ceiling that refused it: {rendered}",
        sim_run.seed
    );
    sim_run.trace.record(&format!("settle-refused {tenant}"));

    // Nothing was recorded, so a reopen still sees the request unsettled.
    let reopened = host_on(tenant, reopen(scratch)?)?;
    let after = lgwks_bot::block_on(reopened.submit(&key, &oversize_failure_task()?, payload))?;
    match after {
        Submission::InFlight(_) => {}
        other => {
            let refusal = Err(format!(
                "seed {}: a request whose verdict the store refused must stay unsettled, not \
             reattach to {other:?}",
                sim_run.seed
            )
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "settle_refusal_scenario: returning an error to the caller");
            return refusal;
        }
    }
    *refused = refused.saturating_add(1);
    sim_run.trace.record(&format!("still-unsettled {tenant}"));
    Ok(())
}

/// A seeded sweep of the settlement path when the store refuses the verdict.
///
/// The complement of every other family here, and the only one about the
/// *absence* of a record. Those families prove a verdict lands; this proves that
/// when the store refuses it, the resume says so and nothing pretends otherwise.
/// Two facts per drawn request, both read through public doors:
///
/// 1. the resume reports `Failed` naming that the outcome went unrecorded, so a
///    run that reached a verdict the store would not take is never reported as a
///    success nobody can find later;
/// 2. a reopen of the same store reports the request `InFlight`, so no later
///    client is handed a verdict that was never written.
///
/// The size of the refusal is drawn per seed so the sweep is not one fixed
/// accident, and each seed's verdict is hashed into the trace so the whole
/// family replays exactly.
fn settle_refusal_scenario(sim_run: &mut sim::Sim) -> TestResult {
    let requests = sim_run.rng().between(1, MAX_SETTLE_REQUESTS);
    let scratch = Scratch::new("sim-req-settle")?;
    let mut refused = 0_u32;

    for index in 0..requests {
        settle_request(sim_run, &scratch, index, &mut refused)?;
    }
    assert_eq!(
        refused, requests,
        "every drawn request's refused settlement left it unsettled"
    );
    sim_run
        .trace
        .record_number("settle-refused", u64::from(refused));
    sim_run.trace.record_number("requests", u64::from(requests));
    Ok(())
}

/// How many requests one seeded settlement sweep drives.
const MAX_SETTLE_REQUESTS: u32 = 2;

/// A fresh store handle over the sweep's shared file.
///
/// Opened per host rather than cloned once, because a handle whose waiter walked
/// away from an append is poisoned until it is reopened (INV-BOT-50) and because
/// two handles over one file are two writers, which the store's own fence
/// refuses. A reopen is what a restart does, so the sweep's later steps take one.
///
/// # Errors
///
/// Whatever opening the store reports.
fn reopen(scratch: &Scratch) -> Result<RunStore, Box<dyn Error>> {
    RunStore::open(scratch.path().join("shared.runstore")).map_err(Into::into)
}

/// One seeded settlement sweep, over the seeds this test's band declares.
///
/// Written per seed rather than through the band macro so each seed's refusal
/// and its exact-replay receipt are separate reported tests: a family whose seeds
/// all hide behind one declaration cannot be read seed by seed when one of them
/// fails.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_a() -> TestResult {
    assert_settle_sweep(sim::Band::new(1000, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_b() -> TestResult {
    assert_settle_sweep(sim::Band::new(1001, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_c() -> TestResult {
    assert_settle_sweep(sim::Band::new(1002, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_d() -> TestResult {
    assert_settle_sweep(sim::Band::new(1003, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_e() -> TestResult {
    assert_settle_sweep(sim::Band::new(1004, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_f() -> TestResult {
    assert_settle_sweep(sim::Band::new(1005, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_g() -> TestResult {
    assert_settle_sweep(sim::Band::new(1006, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_h() -> TestResult {
    assert_settle_sweep(sim::Band::new(1007, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_i() -> TestResult {
    assert_settle_sweep(sim::Band::new(1008, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_j() -> TestResult {
    assert_settle_sweep(sim::Band::new(1009, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_k() -> TestResult {
    assert_settle_sweep(sim::Band::new(1010, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_l() -> TestResult {
    assert_settle_sweep(sim::Band::new(1011, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_m() -> TestResult {
    assert_settle_sweep(sim::Band::new(1012, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_n() -> TestResult {
    assert_settle_sweep(sim::Band::new(1013, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_o() -> TestResult {
    assert_settle_sweep(sim::Band::new(1014, 1))
}

/// The same property over the next seed.
#[test]
fn settle_refusals_are_reported_and_leave_the_request_unsettled_seed_p() -> TestResult {
    assert_settle_sweep(sim::Band::new(1015, 1))
}

/// Run the settlement sweep over `band`, twice, and require the same trace hash.
fn assert_settle_sweep(band: sim::Band) -> TestResult {
    sim::assert_replays(band, settle_refusal_scenario)?;
    Ok(())
}
