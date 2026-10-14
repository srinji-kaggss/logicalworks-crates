//! The store-fault families of the drift sweep: a pre-version store refused naming both versions, and an unreadable store refused as itself.

use super::*;

/// The format-version byte at the end of the header, the byte that differs
/// between formats.
const VERSION_BYTE: usize = 15;
/// The version byte a pre-version store carries, and the version this build reads.
///
/// Spelled out rather than imported: the claim under test is that the two are
/// *not* the same, so a check that read both from the crate would agree with a
/// revert. The two are literals here for the same reason `CURRENT_FORMAT` is in
/// `task_resume.rs` — the same fact, one owner each for the two questions that
/// read it (a store handle, and a host installing over a directory).
const PRE_VERSION: u8 = 1;
/// The version this build writes and reads.
const CURRENT_FORMAT: u8 = 2;

/// One tenant of the refused-resume sweep.
fn refused_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let input = sim.rng().below(64);
    let dir = Scratch(sim.scratch("format")?);
    let path = store_path(&dir.0, tenant);

    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let identity = first_identity(&host, input, FIRST_CODEC)?;
    let first = lgwks_bot::block_on(host.run_under(
        &identity,
        &work,
        Job {
            input,
            dir: dir.0.clone(),
        },
    ));
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "{tenant}: the first attempt must succeed so there are real frames to downgrade"
    );
    drop(first);
    drop(host);

    // The shipped file becomes a `\x01` file: real committed frames under
    // a pre-version header, which is exactly the artifact a deployment on
    // the earlier format holds.
    let mut bytes = std::fs::read(&path)?;
    assert_eq!(
        bytes[VERSION_BYTE], CURRENT_FORMAT,
        "{tenant}: the store must be written at the current version before it is downgraded"
    );
    bytes[VERSION_BYTE] = PRE_VERSION;
    std::fs::write(&path, &bytes)?;
    let before = bytes.clone();

    let refusal = RunStore::open(&path)
        .err()
        .ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}: a pre-version store must not open").into()
        })?;
    let (found, expected) = shared::format_version(&refusal).ok_or_else(|| -> Box<dyn Error> {
        format!("{tenant}: a \\x01 store must be a version refusal, got: {refusal}").into()
    })?;
    assert_eq!(
        found, PRE_VERSION,
        "{tenant}: the refusal must name the version found"
    );
    assert_eq!(
        expected, CURRENT_FORMAT,
        "{tenant}: the refusal must name the version this build reads"
    );
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "{tenant}: the refusal moved bytes in a store it could not read"
    );

    sim.record(tenant);
    sim.trace.record_number("found", u64::from(PRE_VERSION));
    sim.trace
        .record_number("expected", u64::from(CURRENT_FORMAT));
    sim.trace
        .record_number("store-bytes", u64::try_from(before.len())?);
    Ok(())
}

/// A store in an older format is refused naming both versions, and its bytes do
/// not move.
///
/// The file half of T15's "before any new effect". A record written without a
/// definition identity cannot be shown to be this build's own work, so the only
/// honest answer is a refusal — and the refusal has to distinguish "this is an
/// older version of *my* format" from "this was never my format at all", because
/// the second message is what makes an operator delete a file their system is
/// still relying on. This family writes a real store, rewrites its version byte
/// and reopens it, sweeping the tenant per seed so both stores are exercised, and
/// asserts the typed `FormatVersion { found, expected }` payload *and* that the
/// file is byte-identical afterwards.
pub(super) fn a_pre_version_store_is_refused_naming_both_versions(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            refused_tenant(sim, tenant)?;
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}

/// One tenant of the same-seed replay sweep.
fn replay_tenant(sim: &mut sim::Sim, tenant: &str) -> TestResult {
    let input = sim.rng().below(64);
    // Drawn before anything else, so the trace's first fact is the one that
    // selects the scenario rather than a value derived later.
    let armed = sim.rng().chance(500);
    let dir = Scratch(sim.scratch("read-failure")?);

    let host = host_for(tenant, &dir.0)?;
    let work = drift_task()?;
    let identity = first_identity(&host, input, FIRST_CODEC)?;
    let job = Job {
        input,
        dir: dir.0.clone(),
    };
    let first = lgwks_bot::block_on(host.run_under(&identity, &work, job.clone()));
    assert_eq!(
        first.disposition(),
        Disposition::Succeeded,
        "{tenant}: the first attempt must succeed before a read can fail"
    );
    let run = first.run_id().ok_or("a stored run must name its run id")?;
    drop(first);
    drop(host);

    // A fresh handle over a fresh replay of the file, so the resume below is
    // a restart rather than a map the first attempt left warm.
    let reopened = host_for(tenant, &dir.0)?;
    if armed {
        reopened
            .run_store()
            .ok_or("a stored host keeps a store")?
            .fail_next_index_read();
    }

    let before = constructions();
    let refused = lgwks_bot::block_on(reopened.resume_under(run, &identity, &work, job));

    if armed {
        let error = refused.error().ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}: a store that cannot be read must not succeed").into()
        })?;
        assert!(
            !matches!(error, FlowError::Incompatible { .. }),
            "{tenant}: a read failure must not be reported as a definition drift, got: \
             {error}"
        );
        // The typed half of INV-BOT-7: the refusal is the store's own,
        // reached by its variant and its wrapped kind, not by reading the
        // rendered text. A rendering that merely mentioned a device would
        // satisfy a `contains` while the payload was another arm.
        let refusal = shared::store_refusal(error).ok_or_else(|| -> Box<dyn Error> {
            format!("{tenant}: a read failure must reach the caller as FlowError::Store").into()
        })?;
        assert!(
            matches!(refusal, StoreError::Storage { .. }),
            "{tenant}: the wrapped refusal must be the device's own storage error, got: \
             {refusal:?}"
        );
        assert_eq!(
            constructions().saturating_sub(before),
            0,
            "{tenant}: a refused step constructed its body, so the fault did not stop it"
        );
    } else {
        assert_eq!(
            refused.disposition(),
            Disposition::Succeeded,
            "{tenant}: the control arm must replay, got: {:?}",
            refused.error()
        );
        assert_eq!(
            constructions().saturating_sub(before),
            0,
            "{tenant}: the control arm re-ran a durable step"
        );
    }

    sim.record(tenant);
    sim.trace.record_number("armed", u64::from(armed));
    sim.trace.record_number(
        "disposition",
        shared::disposition_code(refused.disposition()),
    );
    Ok(())
}

/// A store that cannot be read is refused as itself, never as a drift.
///
/// The seed draws whether the read fault is armed at all, so the sweep carries the
/// control beside the fault rather than only the fault. That matters here more
/// than in most families: the property INV-BOT-7 names — a read failure reaches the
/// caller as itself — is only meaningful against a store that would otherwise have
/// succeeded, and a sweep of nothing but refusals would be satisfied by a store
/// that refuses everything.
///
/// Every armed arm must agree on two things. The refusal is never
/// [`FlowError::Incompatible`], which is the defect this family exists to catch:
/// the old check turned a device error into `false` and then rendered that as a
/// claim about a definition. And the step's body was never constructed, because a
/// store that cannot say whether it holds a record must not run the work as though
/// it held none.
pub(super) fn an_unreadable_store_is_refused_as_itself(band: sim::Band) -> TestResult {
    let body = |sim: &mut sim::Sim| -> TestResult {
        for tenant in TENANTS {
            replay_tenant(sim, tenant)?;
        }
        Ok(())
    };
    sim::assert_replays(band, body)
}
