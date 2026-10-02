//! Host-held resumable runs, driven through the public task interface.
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_run_without_a_store_claims_no_durability` | no store: no run id, no ticket, `EffectKnowledge::None`, and `remember` re-runs |
//! | `a_recorded_step_returns_without_polling_its_future` | a completed record answers with the future never polled |
//! | `a_killed_process_resumes_without_rerunning_finished_steps` | real `SIGKILL` mid-run: finished steps replay, the stopped step runs once, the output matches |
//! | `two_tenants_resuming_one_run_id_stay_isolated` | tenant B is refused tenant A's run id, and A's records are untouched |
//! | `a_torn_final_record_is_dropped_and_earlier_ones_survive` | a truncated tail loses only its own record |
//! | `a_store_corrupt_before_the_tail_is_refused_not_trimmed` | a committed frame that does not follow is refused |
//! | `an_oversized_record_is_refused_naming_the_ceiling` | the per-record cap is a typed refusal that writes nothing |
//! | `a_recorded_step_replays_under_a_changed_body` | a resume answers from the record, not from the body |
//! | `a_stopped_run_carries_a_ticket_that_names_where_it_stopped` | the ticket names run, tenant, disposition and step path |
//! | `a_foreign_ticket_is_refused_before_any_step_runs` | cross-tenant resume never reaches a body |
//! | `a_duplicate_append_of_the_same_record_is_a_no_op` | the same fact recorded twice appends no byte |
//! | `the_store_is_opened_at_installation_not_at_the_first_step` | a foreign file is refused while building the host |
//!
//! # How the kill test discriminates
//!
//! The child records a step by calling [`remember`], which commits and `sync_all`s
//! before it returns, and *then* writes a marker file. A marker therefore proves
//! the record was acknowledged before the process died, so a marker of `1` for a
//! finished step after the `SIGKILL` means the record really was on the disk and
//! the resume really replayed it rather than re-running the body.

#![cfg(all(feature = "script", feature = "ephemeral"))]

use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lgwks_bot::effect::RunId;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Disposition, EffectKnowledge, Host, StoreError, Task, task};

type TestResult = Result<(), Box<dyn Error>>;

/// The body every test resumes: three durable steps whose bodies each append to
/// a marker file, so "did this body run?" is answered by the filesystem rather
/// than by a counter that died with the process.
///
/// Named rather than written out per call site because `Task<F>` stores its
/// body's own future type, so two separately written closures are two types and
/// a `Task` built by one cannot be passed where the other is expected. One body,
/// one type, every call site.
async fn three_step_body(scope: Scope, dir: PathBuf) -> Result<u32, FlowError> {
    let alpha = remember(&scope, "alpha", || async {
        record_run(&dir, "alpha")?;
        Ok::<_, FlowError>(2u32)
    })
    .await?;
    let beta = remember(&scope, "beta", || async {
        record_run(&dir, "beta")?;
        Ok::<_, FlowError>(3u32)
    })
    .await?;
    let gamma = remember(&scope, "gamma", || async {
        record_run(&dir, "gamma")?;
        Ok::<_, FlowError>(5u32)
    })
    .await?;
    alpha
        .checked_add(beta)
        .and_then(|sum| sum.checked_add(gamma))
        .ok_or_else(|| FlowError::failed("the three step values do not fit a u32"))
}

/// The task those three steps make, built fresh per call because a run reads its
/// tenant and its store from the host, not from the task.
fn three_step_task() -> Result<Task<fn(Scope, PathBuf) -> ThreeFuture>, FlowError> {
    task("three", boxed_three_step_body)
}

/// The same body behind a nameable return type.
fn boxed_three_step_body(scope: Scope, dir: PathBuf) -> ThreeFuture {
    Box::pin(three_step_body(scope, dir))
}

/// The future [`three_step_body`] returns, named because the task's type
/// parameter is the function pointer and its output is the `async` block's own
/// type.
///
/// `Pin<Box<dyn Future>>` rather than the concrete anonymous future: the point of
/// this file is to prove what the store and the report do, and a boxed future is
/// the only spelling of "a task body" that can be named in a signature. The
/// front door's own test proves the unboxed form.
type ThreeFuture = Pin<Box<dyn Future<Output = Result<u32, FlowError>>>>;

/// A host for `tenant` with a durable store under `dir`.
fn stored_host(tenant: &str, dir: &Path) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.run_store(dir)?.build()?)
}

/// A unique scratch directory under the system temp dir, removed on drop.
///
/// Named by a process-wide counter and the process id rather than by the clock,
/// so two tests never collide and a failure leaves a directory a reader can
/// open. Nothing here writes inside the repository, which is what keeps the suite
/// portable across hosts and CI checkouts.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Result<Self, Box<dyn Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lgwks-resume-{tag}-{}-{unique}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, tail: &str) -> PathBuf {
        self.0.join(tail)
    }
}

impl Drop for Scratch {
    /// Remove the directory. A failure to remove is not a test failure: every
    /// assertion has already been made, and a leaked temp directory is a
    /// nuisance rather than a wrong answer.
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// A plain-process pause, for the two places this file waits without a runtime:
/// the child parking until the parent kills it, and the parent polling for the
/// child's evidence.
///
/// The workspace's ban on `std::thread::sleep` exists because blocking an
/// executor thread stalls every task on it. Neither place here has an executor:
/// this file drives its runs through `block_on`, which returns before the wait,
/// and the parked child and the marker poll *are* the observation's subject
/// rather than its scaffolding.
#[expect(
    clippy::disallowed_methods,
    reason = "the kill harness is a plain process with no async runtime and no reactor to stall; \
              the parked child and the marker poll are the observation's shape, and \
              `rt::time::sleep` cannot be awaited here"
)]
fn pause(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

/// The marker naming one step's body.
fn marker(dir: &Path, step: &str) -> PathBuf {
    dir.join(format!("{step}.ran"))
}

/// How many times a step's body has run, as the filesystem records it.
fn ran(dir: &Path, step: &str) -> Result<u32, Box<dyn Error>> {
    match std::fs::read_to_string(marker(dir, step)) {
        Ok(text) => Ok(text.trim().parse::<u32>().unwrap_or_default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

/// Append one to a step's marker. The file is the count, so it survives the kill.
fn record_run(dir: &Path, step: &str) -> Result<(), FlowError> {
    let next = ran(dir, step).map_err(FlowError::failed)?.saturating_add(1);
    std::fs::write(marker(dir, step), next.to_string()).map_err(FlowError::failed)?;
    Ok(())
}

/// Without a store the run claims nothing, and a durable step is just a step.
#[test]
fn a_run_without_a_store_claims_no_durability() -> TestResult {
    let scratch = Scratch::new("nostore")?;
    let host = Host::builder("acme")?.build()?;
    let input = scratch.path().to_path_buf();

    let report = lgwks_bot::block_on(host.run(&three_step_task()?, input.clone()));
    assert_eq!(report.disposition(), Disposition::Succeeded);
    assert_eq!(report.output(), Some(&10));
    assert_eq!(
        report.run_id(),
        None,
        "a host with no store has nothing to key a record by, so it names no run"
    );
    assert_eq!(
        report.ticket(),
        None,
        "there is nothing to resume from, so there is no ticket"
    );
    assert_eq!(
        report.effects(),
        EffectKnowledge::None,
        "no in-memory sink may be reported as durability"
    );
    assert!(host.run_store().is_none());
    assert_eq!(ran(scratch.path(), "alpha")?, 1);

    // The same work again re-runs every step: at-least-once is the whole of the
    // claim when nothing is recorded.
    let again = lgwks_bot::block_on(host.run(&three_step_task()?, input));
    assert_eq!(again.output(), Some(&10));
    assert_eq!(ran(scratch.path(), "alpha")?, 2);
    Ok(())
}

/// A completed record answers without the future being polled.
#[test]
fn a_recorded_step_returns_without_polling_its_future() -> TestResult {
    let scratch = Scratch::new("replay")?;
    let store = scratch.join("store");
    let host = stored_host("acme", &store)?;
    let input = scratch.path().to_path_buf();

    let one = lgwks_bot::block_on(host.run(&three_step_task()?, input.clone()));
    assert_eq!(one.output(), Some(&10));
    let run = one.run_id().ok_or("a stored run must name its run id")?;

    let two = lgwks_bot::block_on(host.resume(run, &three_step_task()?, input.clone()));
    assert_eq!(two.output(), Some(&10));
    assert_eq!(
        ran(scratch.path(), "gamma")?,
        1,
        "a recorded step must not be polled again on a resume"
    );

    // A store reopened over the same file replays them too, which is the claim a
    // restart actually rests on.
    let reopened = stored_host("acme", &store)?;
    let three = lgwks_bot::block_on(reopened.resume(run, &three_step_task()?, input));
    assert_eq!(three.output(), Some(&10));
    assert_eq!(ran(scratch.path(), "gamma")?, 1);
    Ok(())
}

/// A process killed mid-run resumes without re-running what it finished.
///
/// The child records `alpha` and `beta`, publishes its run id, then parks inside
/// `gamma`. The parent kills it with a real `SIGKILL` and resumes under that run
/// id. `alpha` and `beta` must each still be at one — their records were
/// `sync_all`-ed before their markers — `gamma` must be at two (it ran once in
/// the child without a record, and once here), and the output must be 10.
#[test]
fn a_killed_process_resumes_without_rerunning_finished_steps() -> TestResult {
    let scratch = Scratch::new("kill")?;
    let work = scratch.join("work");
    let store = scratch.join("store");
    let run_file = scratch.join("run-id");
    std::fs::create_dir_all(&work)?;

    let mut child = spawn_child(&work, &store, &run_file)?;
    let run = wait_for_run(&run_file, Duration::from_secs(30))?;
    wait_for_marker(&marker(&work, "beta"), Duration::from_secs(30))?;
    // Wait for `gamma`'s marker too: the child is parked *inside* that step's
    // body, so its marker is the proof the step started and has no record. Killing
    // before it would leave nothing to prove.
    wait_for_marker(&marker(&work, "gamma"), Duration::from_secs(30))?;
    child.kill()?;
    child.wait()?;

    assert_eq!(
        ran(&work, "alpha")?,
        1,
        "alpha ran once in the child and must not run again"
    );
    assert_eq!(ran(&work, "beta")?, 1);
    assert_eq!(
        ran(&work, "gamma")?,
        1,
        "gamma started in the child but its record never landed"
    );

    let host = stored_host("acme", &store)?;
    let report = lgwks_bot::block_on(host.resume(run, &three_step_task()?, work.clone()));

    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the resumed run must reach its output: {:?}",
        report.error()
    );
    assert_eq!(report.output(), Some(&10));
    assert_eq!(ran(&work, "alpha")?, 1, "a recorded step replays");
    assert_eq!(ran(&work, "beta")?, 1, "a recorded step replays");
    assert_eq!(
        ran(&work, "gamma")?,
        2,
        "the interrupted step re-runs exactly once"
    );
    Ok(())
}

/// The environment that tells a re-executed test binary to be the killed child.
const CHILD_ENV: &str = "LGWKS_RESUME_CHILD";
const CHILD_WORK: &str = "LGWKS_RESUME_WORK";
const CHILD_STORE: &str = "LGWKS_RESUME_STORE";
const CHILD_RUN: &str = "LGWKS_RESUME_RUN";
const CHILD_TEST: &str = "child_process_runs_the_task_and_parks";

/// The child body: record two steps, publish the run id, then park in the third.
///
/// The parking loop is bounded, so a parent that never kills leaves a loud
/// failure rather than a stray process.
fn child_body() -> TestResult {
    let work = PathBuf::from(std::env::var(CHILD_WORK)?);
    let store = PathBuf::from(std::env::var(CHILD_STORE)?);
    let run_file = PathBuf::from(std::env::var(CHILD_RUN)?);

    let host = Host::builder("acme")?.run_store(&store)?.build()?;
    let run_file = Arc::new(run_file);
    let run_file_in = Arc::clone(&run_file);
    let body = task("three", move |scope: Scope, dir: PathBuf| {
        let run_file = Arc::clone(&run_file_in);
        async move {
            remember(&scope, "alpha", || async {
                record_run(&dir, "alpha")?;
                Ok::<_, FlowError>(2u32)
            })
            .await?;
            // The run id is published only after `alpha`'s record was acknowledged,
            // so the parent's wait for it is a wait for durable evidence.
            let run = scope
                .run()
                .ok_or_else(|| FlowError::failed("a stored run must carry a run id"))?;
            std::fs::write(run_file.as_path(), run.id().to_hex()).map_err(FlowError::failed)?;
            remember(&scope, "beta", || async {
                record_run(&dir, "beta")?;
                Ok::<_, FlowError>(3u32)
            })
            .await?;
            // Park here: this step's body starts, its record never lands, and the
            // parent kills the process while it is outstanding.
            remember(&scope, "gamma", || async {
                record_run(&dir, "gamma")?;
                for _ in 0..600 {
                    pause(100);
                }
                Err::<u32, _>(FlowError::failed("the child was never killed"))
            })
            .await?;
            Ok::<_, FlowError>(10u32)
        }
    })?;

    let report = lgwks_bot::block_on(host.run(&body, work));
    if report.disposition() != Disposition::Succeeded {
        return Err(format!("the child run did not succeed: {:?}", report.error()).into());
    }
    Ok(())
}

fn spawn_child(work: &Path, store: &Path, run_file: &Path) -> Result<Child, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    Ok(std::process::Command::new(executable)
        .args([CHILD_TEST, "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env(CHILD_WORK, work)
        .env(CHILD_STORE, store)
        .env(CHILD_RUN, run_file)
        .spawn()?)
}

/// The child process, re-entered as the parked task.
#[test]
fn child_process_runs_the_task_and_parks() -> TestResult {
    if std::env::var_os(CHILD_ENV).is_some() {
        return child_body();
    }
    Ok(())
}

/// Wait for the child to publish its run id.
fn wait_for_run(path: &Path, within: Duration) -> Result<RunId, Box<dyn Error>> {
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or("an unrepresentable deadline")?;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Ok(RunId::from_hex(trimmed)?);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("the child never published its run id".into());
        }
        pause(20);
    }
}

/// Wait for a marker file to exist.
fn wait_for_marker(path: &Path, within: Duration) -> TestResult {
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or("an unrepresentable deadline")?;
    while !path.exists() {
        if std::time::Instant::now() >= deadline {
            return Err(format!("the child never wrote {}", path.display()).into());
        }
        pause(20);
    }
    Ok(())
}

/// Two tenants, one run id: one resumes and one is refused.
#[test]
fn two_tenants_resuming_one_run_id_stay_isolated() -> TestResult {
    let scratch = Scratch::new("tenants")?;
    let store_dir = scratch.join("store");
    let alpha = stored_host("alpha", &store_dir)?;
    let beta = stored_host("beta", &store_dir)?;
    let input = scratch.path().to_path_buf();

    let seeded = lgwks_bot::block_on(alpha.run(&three_step_task()?, input.clone()));
    let run = seeded.run_id().ok_or("a stored run must name its run id")?;

    let mine = lgwks_bot::block_on(alpha.resume(run, &three_step_task()?, input.clone()));
    assert_eq!(mine.output(), Some(&10));

    let theirs = lgwks_bot::block_on(beta.resume(run, &three_step_task()?, input));
    assert_ne!(
        theirs.disposition(),
        Disposition::Succeeded,
        "another tenant must not be served another tenant's records"
    );
    let text = format!("{:?}", theirs.error()).to_lowercase();
    assert!(
        text.contains("beta"),
        "the refusal must name the store that was asked, got: {text}"
    );
    assert!(
        text.contains("cannot attribute"),
        "the refusal must say why, got: {text}"
    );

    assert_eq!(
        alpha
            .run_store()
            .ok_or("a stored host keeps a store")?
            .record_count(run),
        3,
        "alpha's three records must survive the foreign refusal"
    );
    Ok(())
}

/// A truncated final record loses only itself.
#[test]
fn a_torn_final_record_is_dropped_and_earlier_ones_survive() -> TestResult {
    let scratch = Scratch::new("torn")?;
    let store_dir = scratch.join("store");
    let host = stored_host("acme", &store_dir)?;
    let input = scratch.path().to_path_buf();

    let report = lgwks_bot::block_on(host.run(&three_step_task()?, input.clone()));
    let run = report.run_id().ok_or("a stored run must name its run id")?;
    let path = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .path()
        .to_path_buf();
    drop(host);

    // Cut the file inside its last frame: the shape a `SIGKILL` mid-append
    // leaves.
    let whole = std::fs::read(&path)?;
    assert!(whole.len() > 64, "the store must hold more than a header");
    let file = std::fs::OpenOptions::new().write(true).open(&path)?;
    file.set_len(u64::try_from(whole.len() - 8)?)?;
    file.sync_all()?;
    drop(file);

    let reopened = stored_host("acme", &store_dir)?;
    let store = reopened.run_store().ok_or("a stored host keeps a store")?;
    assert_eq!(
        store.record_count(run),
        2,
        "the torn tail is dropped and the two whole records survive"
    );
    let resumed = lgwks_bot::block_on(reopened.resume(run, &three_step_task()?, input));
    assert_eq!(resumed.output(), Some(&10));
    assert_eq!(ran(scratch.path(), "gamma")?, 2);
    Ok(())
}

/// A committed frame that does not follow is refused, not trimmed.
#[test]
fn a_store_corrupt_before_the_tail_is_refused_not_trimmed() -> TestResult {
    let scratch = Scratch::new("corrupt")?;
    let store_dir = scratch.join("store");
    let host = stored_host("acme", &store_dir)?;
    let report = lgwks_bot::block_on(host.run(&three_step_task()?, scratch.path().to_path_buf()));
    assert!(report.run_id().is_some());
    let path = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .path()
        .to_path_buf();
    drop(host);

    // Flip a byte inside the first record's payload. Its stored head no longer
    // follows, so the frame is committed evidence and is refused.
    let mut bytes = std::fs::read(&path)?;
    bytes[20] ^= 0xff;
    std::fs::write(&path, &bytes)?;

    match Host::builder("acme")?.run_store(&store_dir) {
        Err(error) => {
            assert!(
                matches!(error, StoreError::Corrupt { .. }),
                "a chain break is corruption, got: {error}"
            );
            assert!(
                error.to_string().contains("not trimmed"),
                "the refusal must say the bytes were not trimmed"
            );
        }
        Ok(_) => return Err("a store whose chain does not verify must not open".into()),
    }
    Ok(())
}

/// A value past the per-record ceiling is refused, and nothing is written.
#[test]
fn an_oversized_record_is_refused_naming_the_ceiling() -> TestResult {
    let scratch = Scratch::new("caps")?;
    let store_dir = scratch.join("store");
    let host = stored_host("acme", &store_dir)?;

    let huge = task("huge", |scope: Scope, _dir: PathBuf| async move {
        remember(&scope, "huge", || async {
            Ok::<_, FlowError>(vec![7u8; lgwks_bot::task::MAX_RECORD_BYTES + 1])
        })
        .await
    })?;
    let report = lgwks_bot::block_on(host.run(&huge, scratch.path().to_path_buf()));
    assert_eq!(report.disposition(), Disposition::Failed);
    let text = format!("{:?}", report.error());
    assert!(
        text.contains("exceeds the ceiling"),
        "the refusal must name the ceiling, got: {text}"
    );
    let run = report.run_id().ok_or("a stored run must name its run id")?;
    assert_eq!(
        host.run_store()
            .ok_or("a stored host keeps a store")?
            .record_count(run),
        0,
        "a refused record must leave the store empty, not half-written"
    );
    Ok(())
}

/// A resume answers from the record, so a body that changed does not run.
#[test]
fn a_recorded_step_replays_under_a_changed_body() -> TestResult {
    let scratch = Scratch::new("changed")?;
    let host = stored_host("acme", &scratch.join("store"))?;

    let first = task("value", |scope: Scope, n: u32| async move {
        remember(&scope, "the-step", || async move { Ok::<_, FlowError>(n) }).await
    })?;
    let report = lgwks_bot::block_on(host.run(&first, 1u32));
    assert_eq!(report.output(), Some(&1));
    let run = report.run_id().ok_or("a stored run must name its run id")?;

    // Same run id and step path, a body that would answer differently. The record
    // is the truth about that step, so the recorded value wins.
    let second = task("value", |scope: Scope, n: u32| async move {
        remember(&scope, "the-step", || async move { Ok::<_, FlowError>(n) }).await
    })?;
    let replayed = lgwks_bot::block_on(host.resume(run, &second, 2u32));
    assert_eq!(
        replayed.output(),
        Some(&1),
        "the recorded value answers; the changed body must not overwrite it"
    );
    Ok(())
}

/// A stopped run carries a ticket naming where it stopped.
#[test]
fn a_stopped_run_carries_a_ticket_that_names_where_it_stopped() -> TestResult {
    let scratch = Scratch::new("ticket")?;
    let host = stored_host("acme", &scratch.join("store"))?;
    let failing = task("boom", |scope: Scope, _dir: PathBuf| async move {
        remember(&scope, "doomed", || async {
            Err::<u32, _>(FlowError::transient("this step cannot succeed"))
        })
        .await
    })?;
    let report = lgwks_bot::block_on(host.run(&failing, scratch.path().to_path_buf()));

    assert_eq!(report.disposition(), Disposition::Failed);
    let ticket = report
        .ticket()
        .ok_or("a stopped stored run must carry a ticket")?;
    assert_eq!(ticket.tenant().as_str(), "acme");
    assert_eq!(ticket.task(), "boom");
    assert_eq!(ticket.disposition(), Disposition::Failed);
    assert_eq!(
        ticket.stopped_at(),
        Some("boom/doomed"),
        "the ticket names the step the run stopped at"
    );
    assert_eq!(
        ticket.run().id().to_hex(),
        report
            .run_id()
            .ok_or("a stored run must name a run id")?
            .id()
            .to_hex()
    );
    let rendered = ticket.to_string();
    assert!(
        rendered.contains("acme")
            && rendered.contains("Failed")
            && rendered.contains("boom/doomed"),
        "the ticket renders its tenant, disposition and step, got: {rendered}"
    );

    // A succeeded run carries none: there is nothing to resume.
    let ok = lgwks_bot::block_on(host.run(&three_step_task()?, scratch.path().to_path_buf()));
    assert_eq!(ok.ticket(), None);
    Ok(())
}

/// A ticket for another tenant is refused before any step runs.
#[test]
fn a_foreign_ticket_is_refused_before_any_step_runs() -> TestResult {
    let scratch = Scratch::new("foreign")?;
    let store_dir = scratch.join("store");
    let alpha = stored_host("alpha", &store_dir)?;
    let failing = task("boom", |scope: Scope, _dir: PathBuf| async move {
        remember(&scope, "doomed", || async {
            Err::<u32, _>(FlowError::transient("stop"))
        })
        .await
    })?;
    let report = lgwks_bot::block_on(alpha.run(&failing, scratch.path().to_path_buf()));
    let ticket = report
        .ticket()
        .ok_or("a stopped stored run must carry a ticket")?;

    let beta = stored_host("beta", &store_dir)?;
    let refused =
        lgwks_bot::block_on(beta.resume_ticket(ticket, &three_step_task()?, PathBuf::new()));
    match refused {
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains("alpha") && text.contains("beta"),
                "the refusal names both tenants, got: {text}"
            );
        }
        Ok(report) => assert_ne!(
            report.disposition(),
            Disposition::Succeeded,
            "another tenant must not resume this run"
        ),
    }
    assert_eq!(
        ran(scratch.path(), "alpha")?,
        0,
        "the refused resume must not have run a single step"
    );
    Ok(())
}

/// The same fact recorded twice appends no byte.
#[test]
fn a_duplicate_append_of_the_same_record_is_a_no_op() -> TestResult {
    let scratch = Scratch::new("dupe")?;
    let host = stored_host("acme", &scratch.join("store"))?;
    let once = lgwks_bot::block_on(host.run(&three_step_task()?, scratch.path().to_path_buf()));
    let run = once.run_id().ok_or("a stored run must name its run id")?;
    let store = host.run_store().ok_or("a stored host keeps a store")?;
    let before = store.committed_bytes();

    for _ in 0..3 {
        let replay = lgwks_bot::block_on(host.resume(
            run,
            &three_step_task()?,
            scratch.path().to_path_buf(),
        ));
        assert_eq!(replay.output(), Some(&10));
    }
    assert_eq!(
        store.committed_bytes(),
        before,
        "a replayed step must not append a byte"
    );
    assert_eq!(store.record_count(run), 3);
    Ok(())
}

/// A store is opened while the host is built, not at the first step.
#[test]
fn the_store_is_opened_at_installation_not_at_the_first_step() -> TestResult {
    let scratch = Scratch::new("install")?;
    let store_dir = scratch.join("store");
    std::fs::create_dir_all(&store_dir)?;
    std::fs::write(store_dir.join("acme.runstore"), b"not a store at all")?;

    match Host::builder("acme")?.run_store(&store_dir) {
        Err(error) => assert!(
            matches!(error, StoreError::NotAStore),
            "a foreign file is refused as not a store, got: {error}"
        ),
        Ok(_) => return Err("a foreign file must not be accepted as a run store".into()),
    }
    Ok(())
}
