//! The scaffolding both repair test files need, written once.
//!
//! `repair.rs` and `sim_repair.rs` drive the same journey — an analysis step that
//! is remembered, then a publication step that reaches for authority this host
//! does not grant — and they need the same four things to do it: that body, a host
//! wired with both a run store and a repair ledger, a scratch directory two runs
//! cannot share, and the assertions the journey makes about itself.
//!
//! The counters are the point of the whole file. T23 asks that prior analysis is
//! not rerun, and a step that *returns* a recorded value does not prove its body
//! was not polled unless the body can be seen to have been skipped. So the bodies
//! here count their own polls, and the tests assert on the count rather than on
//! the value: a replayed analysis returns the right number whether or not it ran,
//! so only the counter distinguishes them.
//!
//! Each including test target uses a different subset of this harness, so a name
//! unused in one is not dead. The lint is real per target and the allowance is
//! inherent to sharing one harness across both of them.
#![allow(
    dead_code,
    reason = "each including test target uses a different subset of the shared repair harness"
)]

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::cap::Cap;
use lgwks_bot::effect::RunId;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Host, RepairTicket, RunLedger, RunStore, Task, task};

/// A test's result: an error fails it with the error's text.
pub type TestResult = Result<(), Box<dyn Error>>;

/// The failure a test reports when its precondition did not hold.
///
/// A function rather than a bare `&str`, because `&str` is not an
/// [`Error`](std::error::Error) and a test whose assertion helper reports a
/// missing precondition would not compile into the failure it is meant to be.
pub fn missing(what: &'static str) -> Box<dyn Error> {
    what.into()
}

/// How many times each step's body has been polled.
///
/// One counter per step rather than one total: an analysis that ran once and a
/// publication that ran once look identical in a total, and the two claims are
/// different — the analysis must run once *across both attempts*, while the
/// publication must run once and only after the repair.
#[derive(Debug, Default)]
pub struct Polls {
    /// The analysis step's body polls.
    analysis: AtomicU64,
    /// The publication step's body polls.
    publish: AtomicU64,
}

impl Polls {
    /// A fresh pair of counters, shared by every attempt of a run.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record one poll of the publication body.
    ///
    /// A method rather than the raw counter because the counters are private to
    /// this harness, and a variant journey that reaches *before* its analysis — the
    /// admission-boundary form — has to count from a body declared in the test
    /// target rather than from the one above.
    pub fn published(&self) {
        self.publish.fetch_add(1, Ordering::SeqCst);
    }

    /// How many times the analysis body was polled.
    pub fn analysis(&self) -> u64 {
        self.analysis.load(Ordering::SeqCst)
    }

    /// How many times the publication body was polled.
    pub fn publish(&self) -> u64 {
        self.publish.load(Ordering::SeqCst)
    }
}

/// The named future a body returns, so one body has one type every call site can
/// pass to a [`Task`].
///
/// `Pin<Box<dyn Future>>` rather than the concrete anonymous future: the point of
/// these files is to prove what the report and the ledger do, and a boxed future
/// is the only spelling of "a task body" that can be named in a signature.
pub type BodyFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// The task type the journey body builds.
pub type Journey = Task<fn(Scope, JourneyInput) -> BodyFuture>;

/// What the journey's body takes: the counters, and what it will publish.
#[derive(Clone)]
pub struct JourneyInput {
    /// How many times each step's body has been polled.
    polls: Arc<Polls>,
    /// What the analysis step computes, and the publication step publishes.
    analysis: u32,
    /// The capabilities the publication step reaches for.
    ///
    /// A parameter rather than a constant so one seeded family can sweep how many
    /// capabilities a run is short of at once, which is T23's "multiple missing
    /// permissions yield one complete NeedSet" rather than the one-capability case.
    needs: Vec<Cap>,
}

impl JourneyInput {
    /// The counters this attempt shares with the run's other attempts.
    #[must_use]
    pub fn polls(&self) -> &Arc<Polls> {
        &self.polls
    }

    /// The capabilities this attempt reaches for at its publication step.
    #[must_use]
    pub fn needs(&self) -> &[Cap] {
        &self.needs
    }

    /// What the analysis step computes and the publication step publishes.
    #[must_use]
    pub const fn analysis(&self) -> u32 {
        self.analysis
    }
}

/// The publication capability the journey usually reaches for.
///
/// The split is the whole of T23's "repair only publication authority": the
/// analysis reaches nothing, so a repair that hands out `bot.net` must not change
/// what the analysis is able to do and must not re-run it.
pub const PUBLISH_CAP: &str = Cap::NET;

/// The capabilities a run is short of when the caller asks for the default.
pub fn default_needs() -> Vec<Cap> {
    vec![Cap::new(PUBLISH_CAP)]
}

/// The journey task: a remembered analysis, then a publication that reaches for
/// whatever `input.needs` names.
///
/// Two `remember` steps, because that is the only way a run can block *after* doing
/// work. The analysis commits its record; the publication's step then calls
/// [`Scope::require`] and finds no authority, so the run is `Blocked` with the
/// analysis's record already on the disk. A repair resumes under the run's id, so
/// the analysis replays from that record — its body never polled again — and only
/// the publication runs.
///
/// The task declares no whole-task needs: the reach happens at the step that
/// reaches, which is what leaves the analysis's work replayable.
pub fn journey_task() -> Result<Journey, FlowError> {
    task("publish", boxed_body)
}

/// The journey body: analyze durably, then publish.
fn boxed_body(scope: Scope, input: JourneyInput) -> BodyFuture {
    Box::pin(async move {
        let analyzed = remember(&scope, "analysis", || async {
            input.polls.analysis.fetch_add(1, Ordering::SeqCst);
            Ok::<_, FlowError>(input.analysis)
        })
        .await?;
        publish(&scope, &input, analyzed).await
    })
}

/// The publication step of the journey: reach for the needs, then send.
///
/// The reach is here, at the step that is about to leave the process, so the
/// analysis before it is already recorded and a repair replays it rather than
/// paying for it twice.
async fn publish(scope: &Scope, input: &JourneyInput, analyzed: u32) -> Result<u32, FlowError> {
    let publication = scope.enter("publish")?;
    publication.require(input.needs())?;
    remember(&publication, "send", || async {
        input.polls.publish.fetch_add(1, Ordering::SeqCst);
        Ok::<_, FlowError>(analyzed)
    })
    .await
}

/// The input a journey attempt is given, reaching for the default need.
#[must_use]
pub fn input(polls: Arc<Polls>, analysis: u32) -> JourneyInput {
    JourneyInput {
        polls,
        analysis,
        needs: default_needs(),
    }
}

/// The input a journey attempt is given, reaching for `needs`.
#[must_use]
pub fn input_needing(polls: Arc<Polls>, analysis: u32, needs: Vec<Cap>) -> JourneyInput {
    JourneyInput {
        polls,
        analysis,
        needs,
    }
}

/// A host for `tenant` with a run store and a repair ledger under `dir`, granting
/// `grants`.
///
/// Both stores are installed because neither alone is enough: the step store is
/// what makes the analysis replayable, and the ledger is what makes a repair
/// decidable (epoch, root budget, applied tickets). A host with only one of them
/// refuses the other half, and the tests that need one without the other build it
/// explicitly.
pub fn repairable_host(tenant: &str, dir: &Path, grants: GrantSet) -> Result<Host, Box<dyn Error>> {
    let stored = Host::builder(tenant)?.grants(grants).run_store(dir)?;
    Ok(stored.repair_ledger(dir)?.build()?)
}

/// A host with a run store but no repair ledger: it can replay, and it cannot be
/// repaired, because there is nothing to decide a repair against.
pub fn unrepairable_host(
    tenant: &str,
    dir: &Path,
    grants: GrantSet,
) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?
        .grants(grants)
        .run_store(dir)?
        .build()?)
}

/// The ledger a [`repairable_host`] installed.
pub fn ledger_of(host: &Host) -> Result<&RunLedger, Box<dyn Error>> {
    host.run_ledger()
        .ok_or_else(|| missing("a repairable host keeps a repair ledger"))
}

/// The step store a [`repairable_host`] installed.
pub fn store_of(host: &Host) -> Result<&RunStore, Box<dyn Error>> {
    host.run_store()
        .ok_or_else(|| missing("a repairable host keeps a run store"))
}

/// A repair ticket read off a blocked report, or an error naming why there is
/// none.
///
/// A `Blocked` report with no ticket is the state a host with no ledger produces,
/// so this accessor is a place where that difference is visible rather than a
/// silent `Option` a test would unwrap away.
pub fn ticket_of<O>(report: &lgwks_bot::task::Report<O>) -> Result<&RepairTicket, Box<dyn Error>> {
    report
        .repair()
        .ok_or_else(|| missing("a blocked run on a repairable host must carry a repair ticket"))
}

/// The run a report names, or an error naming why it does not.
pub fn run_of<O>(report: &lgwks_bot::task::Report<O>) -> Result<RunId, Box<dyn Error>> {
    report
        .run_id()
        .ok_or_else(|| missing("a run on a host with a store must name its run id"))
}

/// A scratch directory, removed when the guard ends.
pub struct Scratch {
    /// The directory itself.
    path: PathBuf,
}

impl Scratch {
    /// A scratch directory named by random bytes, so two runs never collide.
    ///
    /// # Errors
    ///
    /// Whatever the entropy source or the filesystem reports.
    pub fn new(tag: &'static str) -> Result<Self, Box<dyn Error>> {
        let unique = lgwks_std::random::bytes::<8>()?;
        let hex: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("lgwks-{tag}-{hex}"));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The directory, borrowed.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A path inside the directory.
    #[must_use]
    pub fn join(&self, tail: &str) -> PathBuf {
        self.path.join(tail)
    }
}

impl Drop for Scratch {
    /// Remove the directory.
    ///
    /// Best effort: every assertion has already been made, and a leaked temp
    /// directory is a nuisance rather than a wrong answer.
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}
