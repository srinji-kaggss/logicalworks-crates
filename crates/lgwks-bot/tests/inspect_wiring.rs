//! Wiring acceptance for #150/R8: the inspection operation reached through the
//! same registry/admission path every other domain uses, and through a
//! [`Host`]-run task, producing the *identical* [`Inspection`].
//!
//! The point these tests hold is that there is one operation and several
//! doors: a native bot, a `BotSpec` materialized through a `DomainRegistry`, a
//! `Query` called with an `Auth` proof, and a `Task` a host runs all return the
//! operation's own report. A second scanner, a second entry point with its own
//! rules, or a divergence between the domain read and the supplied bytes would
//! fail these assertions.
#![cfg(all(feature = "inspect", feature = "script"))]

mod inspect_support;

use std::error::Error;

use inspect_support::{
    INSPECT_DOMAINS, TestResult, artifact_for, observe_once, scope, spec_for, tick_once,
};
use lgwks_bot::domain::inspect::{InspectionJob, Inspector, inspection_task};
use lgwks_bot::inspect::{InspectRequest, Inspection, inspect};
use lgwks_bot::spec::Bot;
use lgwks_bot::task::Host;
use lgwks_bot::verb::Query;
use lgwks_bot::{Admission, Cap, GrantSet, Need};

#[test]
fn both_entry_points_produce_the_identical_inspection() -> TestResult<()> {
    let subject = "fn f() { g().unwrap(); }\n";
    let artifact = artifact_for("identity", subject)?;
    let direct = inspect(&InspectRequest::new(&artifact, subject));

    // (A) The registry path, native: a bot observes the shipped source.
    let grants = GrantSet::empty().grant(Cap::fs());
    let native_report = observe_once(&artifact, "native")?;

    // (A') The registry path, from a document: the same source and action,
    // resolved through the same `assemble`/`build` path.
    let spec = spec_for(&artifact)?;
    let mut made = Bot::from_spec(&spec, &INSPECT_DOMAINS, &grants, scope()?)?;
    let spec_report = tick_once(&mut made)?;

    // (A'') The Query verb, with an `Auth` proof, over the same bytes.
    let auth = GrantSet::empty().issue(&[])?;
    let job = InspectionJob::new(&artifact, subject);
    let query_report = lgwks_std::task::block_on(Inspector::new().query((auth, &job)))?;

    // (B) The script-step entry point: a host runs the inspection task.
    let host = Host::builder("wiring")?.build()?;
    let task = inspection_task("inspect")?;
    let report = host.block_on(&task, InspectionJob::new(&artifact, subject))?;
    let task_report = report.into_result()?;

    assert_eq!(
        native_report, direct,
        "the native Observe source must return the operation's own report"
    );
    assert_eq!(
        spec_report, direct,
        "the materialized bot must return the operation's own report"
    );
    assert_eq!(
        query_report, direct,
        "the Query verb must return the operation's own report"
    );
    assert_eq!(
        task_report, direct,
        "the Host task must return the identical Inspection as the domain path"
    );
    Ok(())
}

#[test]
fn a_spec_naming_the_inspection_source_without_bot_fs_is_an_admission_need() -> TestResult<()> {
    let spec = spec_for("src/lib.rs")?;
    let refusal = Bot::from_spec(&spec, &INSPECT_DOMAINS, &GrantSet::empty(), scope()?);
    let Err(Admission::Needs(needs)) = refusal else {
        return Err(format!("expected a capability need, got {refusal:?}").into());
    };
    assert_eq!(needs.len(), 1, "the source is the only capped link");
    let need = needs
        .needs()
        .first()
        .cloned()
        .ok_or("the need set must carry the missing capability")?;
    match need {
        Need::MissingCapability { capability, .. } => assert_eq!(
            capability,
            Cap::fs(),
            "reading the artifact is a `bot.fs` read, refused at admission"
        ),
        other => return Err(format!("expected MissingCapability, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn two_tenants_inspecting_the_same_artifact_stay_isolated() -> TestResult<()> {
    // The same artifact *name* for both tenants, with different bytes behind
    // it. Each tenant's report must bind to its own bytes: a shared operation
    // must not leak one tenant's subject into the other's report.
    let artifact = "shared/name.rs";
    let alpha = "fn alpha() { a().unwrap(); }\n";
    let beta = "fn beta() { b().unwrap(); }\n";

    let alpha_host = Host::builder("alpha")?.build()?;
    let beta_host = Host::builder("beta")?.build()?;
    let task = inspection_task("inspect")?;

    let run = |host: &Host, source: &str| -> Result<Inspection, String> {
        let report = host
            .block_on(&task, InspectionJob::new(artifact, source))
            .map_err(|error| error.to_string())?;
        report.into_result().map_err(|error| error.to_string())
    };

    let (alpha_run, beta_run) = std::thread::scope(|scope| {
        let alpha_job = scope.spawn(|| run(&alpha_host, alpha));
        let beta_job = scope.spawn(|| run(&beta_host, beta));
        (
            alpha_job
                .join()
                .map_err(|_| "alpha thread panicked".to_owned()),
            beta_job
                .join()
                .map_err(|_| "beta thread panicked".to_owned()),
        )
    });
    let alpha_report = alpha_run.map_err(|cause| -> Box<dyn Error> { cause.into() })??;
    let beta_report = beta_run.map_err(|cause| -> Box<dyn Error> { cause.into() })??;

    assert_eq!(
        alpha_report,
        inspect(&InspectRequest::new(artifact, alpha)),
        "tenant alpha's report must be a report about alpha's bytes"
    );
    assert_eq!(
        beta_report,
        inspect(&InspectRequest::new(artifact, beta)),
        "tenant beta's report must be a report about beta's bytes"
    );
    assert_ne!(
        alpha_report.subject_digest(),
        beta_report.subject_digest(),
        "the same artifact name over different bytes must not collide"
    );
    assert_ne!(
        alpha_host.tenant().as_str(),
        beta_host.tenant().as_str(),
        "the two hosts are two tenants"
    );
    Ok(())
}
