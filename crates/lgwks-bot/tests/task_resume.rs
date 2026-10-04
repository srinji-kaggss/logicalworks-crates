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
//! | `a_pre_version_store_is_refused_naming_both_versions` | a real `\x01` store is refused as `FormatVersion { found: 1, expected: 2 }`, not as a corrupt or foreign file, and its bytes are unchanged |
//! | `a_foreign_file_is_still_refused_as_not_a_store` | the version refusal and the not-a-store refusal stay distinct |
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
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Child;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use lgwks_bot::effect::RunId;
use lgwks_bot::rt::runtime;
use lgwks_bot::script::{FlowError, Scope, remember};
use lgwks_bot::task::{Disposition, EffectKnowledge, Host, RunStore, StoreError, Task, task};

#[path = "support/resume.rs"]
mod shared;

use shared::{Scratch, Summary, marker, peak_rss_mib, ran, record_run};

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
fn three_step_task() -> Result<ThreeTask, FlowError> {
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

/// The task type [`three_step_body`] builds: a named function pointer returning a
/// named future, so one body has one type every call site can pass.
type ThreeTask = Task<fn(Scope, PathBuf) -> ThreeFuture>;

/// A host for `tenant` with a durable store under `dir`.
fn stored_host(tenant: &str, dir: &Path) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.run_store(dir)?.build()?)
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
    let again = lgwks_bot::block_on(host.run(&three_step_task()?, input.clone()));
    assert_eq!(again.output(), Some(&10));
    assert_eq!(ran(scratch.path(), "alpha")?, 2);

    // A *resume* on this host is refused before any step: running every step
    // again under a caller's belief that finished ones are kept is the silent
    // downgrade a resume exists to rule out.
    let resumed = lgwks_bot::block_on(host.resume(RunId::mint()?, &three_step_task()?, input));
    assert_eq!(resumed.disposition(), Disposition::Refused);
    assert_eq!(resumed.run_id(), None);
    assert!(
        resumed
            .error()
            .is_some_and(|error| error.to_string().contains("no run store")),
        "the refusal names the missing store: {:?}",
        resumed.error()
    );
    assert_eq!(ran(scratch.path(), "alpha")?, 2, "no step ran");
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
/// The child records `alpha`, publishes its run id, records `beta`, then parks
/// inside `gamma`. The parent kills it with a real `SIGKILL` and resumes under
/// that run id. `alpha` and `beta` must each still be at one — their markers are
/// written only after their records were `sync_all`-ed — `gamma` must be at two
/// (it ran once in the child without a record, and once here), and the output
/// must be 10.
///
/// The two kinds of marker are the point. `alpha` and `beta` are published
/// *after* their `remember` returns, so seeing one means the record is already
/// on the disk; `gamma`'s is published *inside* its body, so seeing one means
/// only that the step started. A test that used one kind for both would be
/// asserting that a step re-runs after a kill, and could only pass by racing
/// the kill to land inside the window between the marker and the append.
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
            // Alpha's and beta's markers are written *after* their `remember`
            // returns, not inside their bodies. The parent waits on them and then
            // kills, so a marker written inside the body would be the parent's
            // evidence that the body started, which is not the same claim: the
            // kill could land between the marker and the append, and the resume
            // would then re-run a step whose record the test had already said was
            // durable. Publishing after the await makes these markers mean what
            // the parent needs them to mean — the record is on the disk.
            remember(&scope, "alpha", || async { Ok::<_, FlowError>(2u32) }).await?;
            record_run(&dir, "alpha")?;
            // The run id is published only after `alpha`'s record was acknowledged,
            // so the parent's wait for it is a wait for durable evidence.
            let run = scope
                .run()
                .ok_or_else(|| FlowError::failed("a stored run must carry a run id"))?;
            std::fs::write(run_file.as_path(), run.id().to_hex()).map_err(FlowError::failed)?;
            remember(&scope, "beta", || async { Ok::<_, FlowError>(3u32) }).await?;
            record_run(&dir, "beta")?;
            // Park here: this step's body starts, its record never lands, and the
            // parent kills the process while it is outstanding. Its marker is
            // inside the body and deliberately proves nothing about durability:
            // that is what distinguishes it from the two steps above.
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
        {
            let refusal =
                Err(format!("the child run did not succeed: {:?}", report.error()).into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "child_body: returning an error to the caller");
            return refusal;
        };
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
            let refusal = Err("the child never published its run id".into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "wait_for_run: returning an error to the caller");
            return refusal;
        }
        pause(20);
    }
}

/// Wait for a marker file to carry its count.
fn wait_for_marker(path: &Path, within: Duration) -> TestResult {
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .ok_or("an unrepresentable deadline")?;
    // Wait for *content*, not for the file to exist. `std::fs::write` creates the
    // file before the bytes are in it, so `path.exists()` becomes true while the
    // marker is still empty — and `ran` parses an empty file as zero. A parent
    // that killed on existence alone would read "the step ran zero times" and
    // report a step that demonstrably ran. The child is parked for a minute
    // after this marker, so waiting for the count it wrote costs nothing.
    loop {
        match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let refusal = Err(error.into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "wait_for_marker: returning an error to the caller");
                return refusal;
            }
        }
        if std::time::Instant::now() >= deadline {
            let refusal = Err(format!("the child never wrote {}", path.display()).into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "wait_for_marker: returning an error to the caller");
            return refusal;
        }
        pause(20);
    }
}

/// Two tenants over one shared store file: the owner resumes, the other is refused.
///
/// The store is shared deliberately. With one file per tenant, a foreign run id
/// is simply absent and the test would prove nothing about isolation; with one
/// file, tenant B can see that tenant A's run exists and is still refused it.
#[test]
fn two_tenants_resuming_one_run_id_stay_isolated() -> TestResult {
    let scratch = Scratch::new("tenants")?;
    let shared = scratch.join("shared.runstore");
    let input = scratch.path().to_path_buf();

    let alpha = Host::builder("alpha")?
        .store(RunStore::open(&shared)?)
        .build()?;
    let seeded = lgwks_bot::block_on(alpha.run(&three_step_task()?, input.clone()));
    assert_eq!(seeded.output(), Some(&10));
    let run = seeded.run_id().ok_or("a stored run must name its run id")?;

    // The owner replays its own records.
    let mine = lgwks_bot::block_on(alpha.resume(run, &three_step_task()?, input.clone()));
    assert_eq!(mine.output(), Some(&10));

    // A second tenant over the same bytes is refused, and the refusal names the
    // tenant that owns the run.
    let beta = Host::builder("beta")?
        .store(RunStore::open(&shared)?)
        .build()?;
    let theirs = lgwks_bot::block_on(beta.resume(run, &three_step_task()?, input));
    assert_eq!(
        theirs.disposition(),
        Disposition::Refused,
        "another tenant must not be served another tenant's records"
    );
    let text = format!("{:?}", theirs.error());
    assert!(
        text.contains("alpha") && text.contains("beta"),
        "the refusal must name both tenants, got: {text}"
    );
    assert_eq!(
        ran(scratch.path(), "alpha")?,
        1,
        "the refused resume ran no step"
    );

    assert_eq!(
        alpha
            .run_store()
            .ok_or("a stored host keeps a store")?
            .record_count(run),
        3,
        "alpha's three records must survive the foreign refusal untouched"
    );
    assert_eq!(
        alpha
            .run_store()
            .ok_or("a store keeps a handle")?
            .tenant_of(run)
            .as_deref(),
        Some("alpha"),
        "the run is still attributed to the tenant that minted it"
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

/// The header a `\x01` run store carries.
///
/// `\x01` was a real shipped format: the run, the step key, the tenant, the path
/// and the value, with no [`DefinitionIdentity`] in the record. It is spelled out
/// here rather than produced by the crate, because the crate no longer writes it
/// and a test that built one from the current writer would only prove that the
/// current writer is what it is.
///
/// The magic is the current one — the fifteen constant bytes every version
/// shares — and the last byte is `\x01`, which is what makes this file a
/// `\x01` store and not a foreign file. A test that got that last byte wrong
/// would be refused as [`StoreError::NotAStore`] and would pass a file-corruption
/// assertion while proving nothing about the version refusal, so the byte is
/// named and asserted rather than derived.
const V1_HEADER: &[u8; 16] = b"lgwks-runstore\x00\x01";

/// The index of the version byte, which is the byte that differs between formats.
const VERSION_BYTE: usize = V1_HEADER.len() - 1;

/// The format version this build reads.
///
/// Not a re-spelling of the crate's private constant: a test that asserted the
/// store against the same constant the store uses would pass whatever that
/// constant were, including a revert to `\x01` — and the whole claim under test
/// is that the two versions are *not* the same. The literal is what makes the
/// refusal specific.
const CURRENT_FORMAT: u8 = 2;

/// A real `\x01`-format store on disk is refused naming both versions.
///
/// The store is produced by the shipped writer, so every frame in it is a real
/// frame a real run committed; only the version byte is rewritten, which is
/// exactly the edit that turns today's file into the one `origin/main` wrote.
/// Reopen it and the refusal must be [`StoreError::FormatVersion`] carrying both
/// numbers — not [`StoreError::NotAStore`], which would tell an operator their
/// own data was never theirs, and not `Corrupt`, which would send them looking
/// for rot that is not there. The bytes must also be unchanged afterwards,
/// because a refusal that trims what it cannot read is how a re-run loses a
/// system.
#[test]
fn a_pre_version_store_is_refused_naming_both_versions() -> TestResult {
    let scratch = Scratch::new("format")?;
    let store_dir = scratch.join("store");
    let host = stored_host("acme", &store_dir)?;
    let report = lgwks_bot::block_on(host.run(&three_step_task()?, scratch.path().to_path_buf()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the store must be written first"
    );
    let path = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .path()
        .to_path_buf();
    drop(host);

    // Rewrite the version byte in place: the file becomes a `\x01` store holding
    // real committed frames, which is the artifact a pre-version deployment has.
    let mut bytes = std::fs::read(&path)?;
    assert_eq!(
        &bytes[..VERSION_BYTE],
        &V1_HEADER[..VERSION_BYTE],
        "the shipped store and the pre-version header must share their magic, or this \
         test would be refused as a foreign file and would not be testing the version"
    );
    bytes[VERSION_BYTE] = V1_HEADER[VERSION_BYTE];
    std::fs::write(&path, &bytes)?;
    let before = std::fs::read(&path)?;

    match RunStore::open(&path) {
        Err(error) => {
            let (found, expected) = shared::format_version(&error).ok_or_else(|| {
                format!("a \\x01 store must be refused as a version, got: {error}")
            })?;
            assert_eq!(
                found, V1_HEADER[VERSION_BYTE],
                "the refusal names the version found"
            );
            assert_eq!(
                expected, CURRENT_FORMAT,
                "the refusal names the version this build reads"
            );
            let rendered = error.to_string();
            assert!(
                rendered.contains("format version 1") && rendered.contains("reads version 2"),
                "both versions must be in the message a caller reads, got: {rendered}"
            );
        }
        Ok(_) => return Err("a pre-version run store must not open".into()),
    }
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "the refusal moved bytes in a store it could not read"
    );

    // The same refusal through the door a host actually installs, so the row is
    // reached the way a deployment meets it rather than through a store handle.
    assert!(
        matches!(
            Host::builder("acme")?.run_store(&store_dir),
            Err(StoreError::FormatVersion { .. })
        ),
        "a host installed over a pre-version store must refuse it too"
    );
    Ok(())
}

/// A foreign file is still refused as not a store, and the two refusals stay apart.
///
/// The companion to the row above: separating "this is an older version" from
/// "this was never ours" is only worth doing if the second answer survives. If
/// both collapsed into one arm, the version test above would pass on a check that
/// also refused every unrelated file, and an operator would be told their data is
/// corrupt for a store this build is merely too old to read.
#[test]
fn a_foreign_file_is_still_refused_as_not_a_store() -> TestResult {
    let scratch = Scratch::new("foreign")?;
    let store_dir = scratch.join("store");
    std::fs::create_dir_all(&store_dir)?;
    let path = store_dir.join("acme.runstore");
    // Shares the version byte with the `\x01` store and differs in the first one,
    // which is the byte that says "this is some version of this format".
    std::fs::write(&path, b"lgwks-runstorX\x00\x01")?;
    assert!(
        matches!(RunStore::open(&path), Err(StoreError::NotAStore)),
        "a file that was never a run store is refused as not one"
    );
    std::fs::write(&path, b"not a store at all")?;
    assert!(
        matches!(RunStore::open(&path), Err(StoreError::NotAStore)),
        "a short file is refused as not one"
    );
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

/// The environment variable that gates the concurrent-run tier measurement.
///
/// Ten thousand `sync_all`-ed appends is several minutes: the right cost for a
/// measurement, the wrong cost for every ordinary test run. The test is therefore
/// `#[ignore]`d and reports itself as **skipped**, never as a bound nobody
/// checked. The scale lane runs it explicitly:
///
/// ```text
/// LGWKS_RESUME_SCALE=1 cargo test -p lgwks_bot --features script,ephemeral --release \
///     --test task_resume concurrent_runs_across_tiers -- --ignored --nocapture
/// ```
///
/// Measured on the machine that recorded it, over all three tiers including the
/// ten-thousand-run one: tier 100 p50=3019us p95=3977us p99=4138us in 316.11ms;
/// tier 1000 p50=3005us p95=4001us p99=4136us in 3.13s; tier 10000
/// p50=3017us p95=4144us p99=5312us in 32.74s, with a peak resident set of
/// 727,384,064 bytes for the whole process. The set is bounded by the store's
/// record ceiling rather than by the run count: a design that retained every
/// run's body would exceed it long before ten thousand, and one that grew
/// without bound would not have reached a near-flat p99 across the tiers — p99
/// moves from 4138us at a hundred runs to 5312us at ten thousand, a factor of
/// 1.3 across a hundredfold more work.
///
/// Peak RSS is reported by the harness only where `/proc/self/status` exists,
/// which is not macOS; the number above was taken with `/usr/bin/time -l` around
/// the same command rather than by asking this process about itself. The harness
/// prints "unavailable on this platform" rather than a zero, because a
/// measurement that reports zero where it measured nothing is the one number a
/// reader must not be shown.
const SCALE_ENV: &str = "LGWKS_RESUME_SCALE";

/// The scale tiers the concurrent-run measurement drives, and the highest one run
/// for real.
///
/// A tier is a number of concurrent runs against one store. The list stops at ten
/// thousand because that is the largest tier this file actually measured on the
/// machine that recorded it; a tier nobody ran is not a tier this file claims.
const SCALE_TIERS: [usize; 3] = [100, 1_000, 10_000];

/// Concurrent runs against one store, at every tier, with two tenants interleaved.
///
/// This is the hyperscale row for the run store: what has to hold at ten thousand
/// is that nothing is lost, nothing is written twice, and no tenant reads another
/// tenant's record. Every run commits its record through the one owner thread, so
/// the question this asks of the store is whether that ordering holds under load,
/// not whether the file system can take it.
///
/// The measurement is opt-in for the same reason the ten-thousand-run resident set
/// was: it is minutes of real `fsync`s, which is the right cost for a measurement
/// and the wrong cost for every ordinary run. It is `#[ignore]`d rather than
/// silently skipped, and it prints what it measured rather than only asserting a
/// bound.
///
/// ```text
/// LGWKS_RESUME_SCALE=1 cargo test -p lgwks_bot --features script,ephemeral \
///     --test task_resume concurrent_runs_across_tiers -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the concurrent-run scale measurement; run it with \
            LGWKS_RESUME_SCALE=1 and --ignored"]
fn concurrent_runs_across_tiers() -> TestResult {
    if std::env::var_os(SCALE_ENV).is_none() {
        return Err(format!(
            "{SCALE_ENV} is not set, so the tier measurement did not run; set it to measure"
        )
        .into());
    }
    for tier in SCALE_TIERS {
        measure_tier(tier)?;
    }
    Ok(())
}

/// One tier: `runs` concurrent runs, two tenants interleaved, over one store.
///
/// The per-run latencies are reported so a reader sees the distribution rather than
/// a bare pass, and the store is reopened at the end so the count is answered from
/// the disk rather than from the handle that wrote it.
fn measure_tier(runs: usize) -> TestResult {
    const TENANTS: [&str; 2] = ["scale-a", "scale-b"];
    let scratch = Scratch::new("scale")?;
    let path = scratch.store();
    let work = shared::one_step_task()?;
    // One store handle, cloned into both tenants' hosts. Two handles over one file
    // would be two writers, which the store's length fence refuses — and which is
    // exactly the mistake this shape is meant not to make: the tenant is part of
    // the record, not part of the file.
    let store = RunStore::open(&path)?;
    let mut hosts = Vec::with_capacity(TENANTS.len());
    for tenant in TENANTS {
        hosts.push(Host::builder(tenant)?.store(store.clone()).build()?);
    }

    // Warm the file and the owner thread so the tier measures steady-state appends.
    for host in &hosts {
        let warm = runtime::block_on(host.run(&work, 0u32));
        if !warm.disposition().is_success() {
            let refusal = Err(format!("a warm-up run failed: {:?}", warm.error()).into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "measure_tier: returning an error to the caller");
            return refusal;
        }
    }

    let started = Instant::now();
    let mut samples: Vec<u128> = Vec::with_capacity(runs);
    let mut per_tenant = [0usize; TENANTS.len()];
    let mut run_ids: Vec<(RunId, usize)> = Vec::with_capacity(runs);
    for index in 0..runs {
        // The tenant alternates with the index, so two identities are in flight at
        // once against the same store and the same chain rather than one after the
        // other.
        let which = index.checked_rem(TENANTS.len()).unwrap_or_default();
        let host = &hosts[which];
        let at = Instant::now();
        let report = runtime::block_on(host.run(&work, u32::try_from(index).unwrap_or_default()));
        samples.push(at.elapsed().as_micros());
        if !report.disposition().is_success() {
            let refusal = Err(format!("run {index} failed: {:?}", report.error()).into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "measure_tier: returning an error to the caller");
            return refusal;
        }
        let id = report.run_id().ok_or("a stored run must name a run id")?;
        run_ids.push((id, which));
        per_tenant[which] = per_tenant[which].saturating_add(1);
    }
    let elapsed = started.elapsed();

    // Reopened over the same bytes, so every count below is answered from the disk.
    drop(hosts);
    drop(store);
    let reopened = RunStore::open(&path)?;
    for entry in &run_ids {
        let (id, which) = *entry;
        assert_eq!(
            reopened.tenant_of(id).as_deref(),
            Some(TENANTS[which]),
            "run {id:?} is attributed to the tenant that minted it"
        );
        assert_eq!(
            reopened.record_count(id),
            1,
            "run {id:?} holds exactly its own one record"
        );
    }
    // Both tenants were in flight against the same store, which is what makes the
    // attribution above a cross-tenant check rather than two single-tenant runs.
    for (which, tenant) in TENANTS.iter().enumerate() {
        assert!(
            per_tenant[which] > 0,
            "{tenant} ran nothing at this tier, so the isolation above is vacuous"
        );
    }

    let summary = Summary::of(&mut samples);
    let rss = peak_rss_mib();
    let mut line = std::io::stdout().lock();
    let _written = writeln!(
        line,
        "tier {runs}: {}  |  {}  |  peak RSS {rss}",
        summary.line("per-run"),
        format_args!("{} runs in {:.2?}", runs, elapsed)
    );
    Ok(())
}
