#![cfg(all(feature = "inspect", feature = "script"))]
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

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::domain::inspect::{InspectionJob, Inspector, Subject, inspection_task};
use lgwks_bot::inspect::{InspectRequest, Inspection, inspect};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{Bot, BotSpec, EffectScope};
use lgwks_bot::task::Host;
use lgwks_bot::verb::Query;
use lgwks_bot::{
    Action, Admission, Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, Need, domains,
};

mod sim;

/// A test result that may carry a `BotError`, an id error or an I/O error.
type TestResult<T> = Result<T, Box<dyn Error>>;

thread_local! {
    /// The last report an action received on this thread.
    ///
    /// A thread-local rather than shared state: `Bot::tick` runs the action on
    /// the calling thread's ECS schedule, and this is the read-what-the-action-
    /// saw oracle the identity assertions need.
    static CAPTURED: RefCell<Option<Inspection>> = const { RefCell::new(None) };
}

/// Read and clear the captured report.
fn take_captured() -> TestResult<Inspection> {
    CAPTURED
        .with(|slot| slot.borrow_mut().take())
        .ok_or_else(|| "the capture action did not run".into())
}

/// An action that records the inspection it was handed.
struct Capture;

impl Capture {
    /// Build one from the `target` its spec names (ignored).
    fn from_target(target: &str) -> Result<Action, BotError> {
        let _ = target;
        Ok(Action::new(Self))
    }
}

impl Execute for Capture {
    type Input = Inspection;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &Inspection)) -> Result<(), BotError> {
        call.0.check(&[])?;
        CAPTURED.with(|slot| *slot.borrow_mut() = Some(call.1.clone()));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::capture"
    }
}

domains! {
    /// The registry these tests run: the shipped inspection source and a
    /// capture action.
    pub INSPECT_DOMAINS {
        observe {
            "inspect::subject" => Subject::from_target,
        }
        execute {
            "test::capture" => Capture::from_target,
        }
    }
}

/// A fresh effect scope over an in-memory journal.
///
/// The run identity and the probe journal come from the shared simulation rig,
/// so this file adds no second copy of the estate's bot-scope construction.
fn scope() -> TestResult<EffectScope> {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    sim::rig::scope(sim::rig::identity()?, store)
}

/// Escape `text` as a JSON string body.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A one-chain spec naming the inspection source and the capture action.
fn spec_for(artifact: &str) -> TestResult<BotSpec> {
    let json = format!(
        r#"{{"version":1,"name":"wired","chains":[
            {{"source":"inspect::subject","target":{target},
             "on":[["always",{{"domain":"test::capture","target":""}}]]}}]}}"#,
        target = json_string(artifact)
    );
    Ok(BotSpec::from_json(&json)?)
}

/// Write `subject` to a unique temp artifact and return its path string.
fn artifact_for(label: &str, subject: &str) -> TestResult<String> {
    let path = std::env::temp_dir().join(format!(
        "lgwks-inspect-wiring-{label}-{}",
        std::process::id()
    ));
    std::fs::write(&path, subject)?;
    Ok(path.to_string_lossy().into_owned())
}

#[test]
fn both_entry_points_produce_the_identical_inspection() -> TestResult<()> {
    let subject = "fn f() { g().unwrap(); }\n";
    let artifact = artifact_for("identity", subject)?;
    let direct = inspect(&InspectRequest::new(&artifact, subject));

    // (A) The registry path, native: a bot observes the shipped source.
    let grants = GrantSet::empty().grant(Cap::fs());
    let mut native = Bot::builder("native")
        .observe(Subject::at(&artifact))
        .on(|_: &Inspection| true, Capture)
        .with_effects(scope()?)
        .build(&grants)?;
    CAPTURED.with(|slot| *slot.borrow_mut() = None);
    assert_eq!(native.tick()?, 1, "the one chain fires once");
    let native_report = take_captured()?;

    // (A') The registry path, from a document: the same source and action,
    // resolved through the same `assemble`/`build` path.
    let spec = spec_for(&artifact)?;
    let mut made = Bot::from_spec(&spec, &INSPECT_DOMAINS, &grants, scope()?)?;
    CAPTURED.with(|slot| *slot.borrow_mut() = None);
    assert_eq!(made.tick()?, 1, "the materialized chain fires once");
    let spec_report = take_captured()?;

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
            alpha_job.join().map_err(|_| "alpha thread panicked".to_owned()),
            beta_job.join().map_err(|_| "beta thread panicked".to_owned()),
        )
    });
    let alpha_report = alpha_run.map_err(std::convert::Into::<Box<dyn Error>>::into)??;
    let beta_report = beta_run.map_err(std::convert::Into::<Box<dyn Error>>::into)??;

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
