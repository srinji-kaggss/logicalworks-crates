//! Simulation family: `remember`, `admit` and `ready` on the real run path.
//!
//! Each word in flows the real `script!` macro expanded, driven on the real
//! `lgwks_bot::script` runtime: `remember` through a real [`Host`] over a real
//! run store file, `admit` through a real [`Gate`] over the shared proposal
//! surface, `ready` on a real [`Readiness`] measured on a virtual clock. What
//! the seed controls is the input — the remembered value and tenant, the
//! payload shape, and whether the readiness is signalled, failed or left to
//! run out — never a wall clock.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `remember_replays_on_resume` | a resume on a fresh host over the same store gives back the recorded value and never runs the block |
//! | `admit_refusal_located` | the word admits exactly what `admit` admits; a refusal is `Refused`, located at `<flow>/admit`, with the provenance of the exact bytes |
//! | `ready_released_or_timed_out` | a signal before or during the wait releases it; a failure is the readiness's; past the deadline it is `TimedOut`, with no real wait |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(all(feature = "script", feature = "ephemeral", feature = "time"))]

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lgwks_bot::BoxFuture;
use lgwks_bot::proposal::{Provenance, Source};
use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{Clock, FlowError, Gate, Generation, Readiness, Scope, Tenant, admit};
use lgwks_bot::task::{Host, Task, task};

use crate::band_family;
use crate::proposal_fixtures::{SHAPES, TENANT, gate};
use crate::sim::{self, Band};
use crate::sim_script::{Turns, drive, tenant_scope};
use crate::sim_script_words::{TestResult, unexpected};

/// The task the `remember` family runs, and the one its definition names.
const TASK: &str = "remembered";

/// The deadline the `ready` flow writes.
const DEADLINE: Duration = Duration::from_secs(30);

/// How many polls the actor branch waits for the flow to park on the
/// readiness before it gives up: one is enough, the rest is headroom.
const PARK_TURNS: u32 = 8;

lgwks_bot::script! {
    flow answer(asked: &AtomicU64, value: u32) -> u32:
        let reply = remember ask:
            asked.fetch_add(1, Ordering::SeqCst)
            value
        give back reply

    flow plan(gate: &Gate, reply: &[u8]) -> usize:
        let admitted = admit from model through (gate):
            reply
        give back admitted.wanted().len()

    flow query(db: &Readiness<u32>) -> u32:
        let rows = ready pool from (db) within 30s:
            pool.awaited().saturating_add(1)
        give back rows
}

/// What one `remember` run is given: the counter its block moves, and the
/// value it remembers.
#[derive(Clone)]
struct Job {
    /// How many times the block ran.
    asked: Arc<AtomicU64>,
    /// The value the block gives.
    value: u32,
}

/// The future the task body returns.
type Answered = BoxFuture<'static, Result<u32, FlowError>>;

/// The task body's type, as the task holds it.
type AnswerBody = fn(Scope, Job) -> Answered;

/// The task body: the `answer` flow over the job.
fn answer_body(scope: Scope, job: Job) -> Answered {
    Box::pin(async move { answer(&scope, &job.asked, job.value).await })
}

/// The task the `remember` family runs.
fn answer_task() -> Result<Task<AnswerBody>, FlowError> {
    task(TASK, answer_body)
}

/// A scratch directory removed when the family ends, however it ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// A host for `tenant` over the run store in `dir`.
fn host_for(tenant: &str, dir: &Scratch) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?.run_store(&dir.0)?.build()?)
}

/// A resume on a fresh host over the same store gives back the recorded value
/// and never runs the block.
fn remember_replays_on_resume(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let value = sim.rng().below(1_000);
        let tenant = format!("tenant-{}", sim.rng().below(64));
        let dir = Scratch(sim.scratch("remember")?);
        let asked = Arc::new(AtomicU64::new(0));
        let job = Job {
            asked: Arc::clone(&asked),
            value,
        };
        let work = answer_task()?;
        let host = host_for(&tenant, &dir)?;
        let identity = host.definition(TASK, 1, None, 1);
        let first = lgwks_bot::block_on(host.run_under(&identity, &work, job.clone()));
        assert_eq!(
            first.output(),
            Some(&value),
            "the first run gives its value"
        );
        let run = first.run_id().ok_or("a stored run names its run id")?;
        drop(first);
        drop(host);
        let reopened = host_for(&tenant, &dir)?;
        let replayed = lgwks_bot::block_on(reopened.resume_under(run, &identity, &work, job));
        assert_eq!(
            replayed.output(),
            Some(&value),
            "the resume gives back the recorded value"
        );
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "the block ran once across the run and its resume"
        );
        sim.record(&format!("tenant={tenant} value={value}"));
        Ok(())
    })
}

/// The word admits exactly what `admit` admits, and a refusal is located at
/// the word's own step with the provenance of the exact bytes.
fn admit_refusal_located(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let drawn = usize::try_from(sim.rng().below(u32::try_from(SHAPES.len())?))?;
        let &(shape, bytes) = SHAPES.get(drawn).ok_or("no such shape")?;
        let payload = bytes();
        let scope = tenant_scope(TENANT, CancellationToken::new())?;
        let by_word = drive(plan(&scope, &gate(TENANT)?, &payload));
        let by_hand = drive(admit(
            &scope.enter("plan")?,
            "admit",
            &gate(TENANT)?,
            &payload,
            Source::Model,
        ));
        match (by_word, by_hand) {
            (Ok(count), Ok(admitted)) => {
                assert_eq!(count, admitted.wanted().len(), "{shape}: the same plan");
            }
            (
                Err(FlowError::Refused {
                    ref at,
                    ref provenance,
                    ..
                }),
                Err(FlowError::Refused { at: ref hand, .. }),
            ) => {
                assert_eq!(at, hand, "{shape}: located at the word's own step");
                assert_eq!(
                    provenance.digest(),
                    Provenance::for_model(TENANT, &payload).digest(),
                    "{shape}: the provenance of the exact bytes"
                );
            }
            (word, hand) => {
                return unexpected(format!(
                    "{shape}: the word ended {word:?}, `admit` ended {hand:?}"
                ));
            }
        }
        sim.record(shape);
        Ok(())
    })
}

/// What the actor beside a parked `ready` does.
#[derive(Clone, Copy, Debug)]
enum Actor {
    /// Signal before the flow starts.
    Before,
    /// Signal once the flow is parked on the readiness.
    During,
    /// Fail the readiness once the flow is parked.
    Fails,
    /// Move the clock past the deadline once the flow is parked.
    RunsOut,
}

/// The actors, in the order a seed draws them.
const ACTORS: [Actor; 4] = [Actor::Before, Actor::During, Actor::Fails, Actor::RunsOut];

/// Act on `db` once the flow is parked on it: admitted, so its wait and its
/// deadline are registered.
async fn act(
    db: &Readiness<u32>,
    clock: &Clock,
    actor: Actor,
    value: u32,
) -> Result<(), FlowError> {
    for _ in 0..PARK_TURNS {
        if db.admitted() > 0 {
            break;
        }
        Turns(1).await;
    }
    let acted = match actor {
        Actor::Before => Ok(()),
        Actor::During => db.ready(Generation::FIRST, value),
        Actor::Fails => db.fail(Generation::FIRST, "the pool refused"),
        Actor::RunsOut => {
            let past = DEADLINE.saturating_add(Duration::from_secs(1));
            clock.advance(past).map_err(FlowError::failed)?;
            Ok(())
        }
    };
    acted.map_err(FlowError::failed)
}

/// A signal before or during the wait releases it, a failure is the
/// readiness's, and past the deadline the wait is `TimedOut` with no real
/// wait.
fn ready_released_or_timed_out(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let drawn = usize::try_from(sim.rng().below(u32::try_from(ACTORS.len())?))?;
        let actor = *ACTORS.get(drawn).ok_or("no such actor")?;
        let value = sim.rng().below(1_000);
        let clock = Clock::virtual_at(Duration::ZERO);
        let scope = Scope::with_clock(Tenant::new("acme")?, clock.clone());
        let db: Readiness<u32> = Readiness::new("pool")?;
        if matches!(actor, Actor::Before) {
            db.ready(Generation::FIRST, value)?;
        }
        let outcome = lgwks_bot::block_on(async {
            lgwks_bot::try_join!(query(&scope, &db), act(&db, &clock, actor, value))
        });
        match (actor, outcome) {
            (Actor::Before | Actor::During, Ok((rows, ()))) => {
                assert_eq!(rows, value.saturating_add(1), "the released value");
            }
            (Actor::Fails, Err(error)) => assert!(
                error.to_string().contains("the pool refused"),
                "the readiness's own failure: {error}"
            ),
            (Actor::RunsOut, Err(FlowError::TimedOut { after, .. })) => {
                assert_eq!(after, DEADLINE, "the written deadline");
            }
            (drawn_actor, other) => {
                return unexpected(format!("{drawn_actor:?} ended {other:?}"));
            }
        }
        sim.record(&format!("{actor:?} value={value}"));
        Ok(())
    })
}

band_family::band_family! {
    remember_replays_on_resume_band_00 => remember_replays_on_resume, 0;
    remember_replays_on_resume_band_01 => remember_replays_on_resume, 1;
    admit_refusal_located_band_00 => admit_refusal_located, 0;
    admit_refusal_located_band_01 => admit_refusal_located, 1;
    admit_refusal_located_band_02 => admit_refusal_located, 2;
    ready_released_or_timed_out_band_00 => ready_released_or_timed_out, 0;
    ready_released_or_timed_out_band_01 => ready_released_or_timed_out, 1;
    ready_released_or_timed_out_band_02 => ready_released_or_timed_out, 2;
}
