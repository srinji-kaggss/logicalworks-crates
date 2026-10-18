//! Simulation family: the async tier's documented entry points under a seeded sweep.
//!
//! `rt_front_door` pins one shape per door: four in flight, sixteen inputs, one token. This target
//! asks the same questions of *sizes*: every ceiling against every burst, every fan-out width over
//! every input count, every number of tasks a token has to stop, and every capture ceiling against
//! every amount of output.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `a_burst_never_exceeds_the_supervisor_ceiling_and_every_task_is_reported` | a supervisor admits `min(burst, ceiling)`, refuses the rest by name, and reports each admitted task |
//! | `a_fan_out_returns_every_input_in_input_order_whatever_the_limit` | `join_all_bounded` returns one result per input, in input order, at any limit and any completion order |
//! | `a_cancelled_token_stops_every_task_a_join_set_owns_at_any_size` | one cancel drains every task of a set, however many it owns |
//! | `a_command_captures_exactly_the_smaller_of_its_output_and_the_ceiling` | a capture keeps the first `ceiling` bytes of what the child wrote, and never more |
//!
//! # What is real and what is seeded
//!
//! The runtime, the supervisor, the join set, the token and the child process are the shipped ones.
//! The seed decides the sizes and the order in which inputs finish; no assertion reads a clock, so a
//! replay that differs is a defect in the code under test.
use std::error::Error;

use lgwks_bot::Runtime;
use lgwks_bot::rt::supervise::{Supervisor, TrySpawnRefusal};
use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::rt::task::{JoinError, JoinSet, join_all_bounded};
use lgwks_bot::rt::time::{Duration, sleep};

use crate::sim;

use sim::band_of;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// A supervisor admits `min(burst, ceiling)`, refuses the rest by name, and reports each admitted task.
#[test]
fn a_burst_never_exceeds_the_supervisor_ceiling_and_every_task_is_reported() -> TestResult {
    let runtime = Runtime::new()?;
    sim::assert_replays(band_of(10), |sim| {
        let ceiling = usize::try_from(sim.rng().between(1, 6))?;
        let burst = usize::try_from(sim.rng().between(1, 12))?;
        runtime.block_on(async {
            let mut supervisor = Supervisor::new(ceiling);
            let mut admitted = 0_usize;
            let mut refused = 0_usize;
            for _ in 0..burst {
                // A body that waits for its token keeps its slot, so the ceiling is genuinely full.
                let placed = supervisor.try_spawn(|token| async move {
                    token.cancelled_owned().await;
                });
                match placed {
                    Ok(()) => admitted = admitted.saturating_add(1),
                    Err(TrySpawnRefusal::AtCapacity) => refused = refused.saturating_add(1),
                    Err(other) => return Err(format!("unexpected refusal {other:?}").into()),
                }
            }
            assert_eq!(
                admitted,
                burst.min(ceiling),
                "the ceiling {ceiling} bounds the burst {burst}"
            );
            assert_eq!(
                refused,
                burst.saturating_sub(admitted),
                "the rest are refused by name"
            );
            let counted = supervisor.stats();
            assert_eq!(
                usize::try_from(counted.refused)?,
                refused,
                "each refusal is counted"
            );
            let report = supervisor.shutdown().await;
            assert_eq!(
                usize::try_from(report.stats().spawned)?,
                admitted,
                "only admitted tasks spawn"
            );
            assert_eq!(
                report.outcomes().len(),
                admitted,
                "each admitted task is reported once"
            );
            Ok::<(), Box<dyn Error>>(())
        })?;
        sim.record(&format!("ceiling={ceiling} burst={burst}"));
        Ok(())
    })
}

/// `join_all_bounded` returns one result per input, in input order, at any limit and any completion order.
#[test]
fn a_fan_out_returns_every_input_in_input_order_whatever_the_limit() -> TestResult {
    let runtime = Runtime::new()?;
    sim::assert_replays(band_of(11), |sim| {
        let limit = usize::try_from(sim.rng().between(1, 8))?;
        let inputs = sim.rng().between(0, 24);
        // A drawn delay per input scrambles the order in which they finish.
        let delays: Vec<u64> = (0..inputs).map(|_| u64::from(sim.rng().below(3))).collect();
        let results = runtime.block_on(join_all_bounded(
            limit,
            delays
                .iter()
                .copied()
                .enumerate()
                .map(|(index, delay)| async move {
                    sleep(Duration::from_millis(delay)).await;
                    index.saturating_mul(3)
                }),
        ));
        let expected: Vec<usize> = (0..delays.len())
            .map(|index| index.saturating_mul(3))
            .collect();
        assert_eq!(
            results,
            expected,
            "{} inputs at limit {limit}",
            delays.len()
        );
        sim.record(&format!("limit={limit} inputs={}", delays.len()));
        Ok(())
    })
}

/// One cancel drains every task of a set, however many it owns.
#[test]
fn a_cancelled_token_stops_every_task_a_join_set_owns_at_any_size() -> TestResult {
    let runtime = Runtime::new()?;
    sim::assert_replays(band_of(12), |sim| {
        let owned = usize::try_from(sim.rng().between(1, 16))?;
        let joined = runtime.block_on(async {
            let token = CancellationToken::new();
            let mut tasks = JoinSet::new();
            for _ in 0..owned {
                let worker = token.clone();
                tasks.spawn(async move {
                    worker.cancelled_owned().await;
                });
            }
            token.cancel();
            let mut joined = 0_usize;
            while let Some(done) = tasks.join_next().await {
                done?;
                joined = joined.saturating_add(1);
            }
            Ok::<usize, JoinError>(joined)
        })?;
        assert_eq!(
            joined, owned,
            "every owned task was drained by the one cancel"
        );
        sim.record(&format!("owned={owned}"));
        Ok(())
    })
}

/// A capture keeps the first `ceiling` bytes of what the child wrote, and never more.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_command_captures_exactly_the_smaller_of_its_output_and_the_ceiling() -> TestResult {
    use std::num::NonZeroUsize;

    use lgwks_bot::rt::process::ProcessSpec;

    let runtime = Runtime::new()?;
    sim::assert_replays(band_of(13), |sim| {
        let written = usize::try_from(sim.rng().between(0, 200))?;
        let ceiling = usize::try_from(sim.rng().between(1, 200))?;
        let captured = runtime.block_on(async {
            let mut spec = ProcessSpec::new("sh");
            spec.arg("-c").arg(format!("head -c {written} /dev/zero"));
            spec.capture_stdout(NonZeroUsize::new(ceiling).ok_or("a non-zero ceiling")?)
                .deadline(Duration::from_secs(10));
            let mut supervisor = Supervisor::new(1);
            let run = supervisor.run_process(&spec).await?;
            Ok::<Vec<u8>, Box<dyn Error>>(run.stdout().bytes().to_vec())
        })?;
        assert_eq!(
            captured.len(),
            written.min(ceiling),
            "{written} bytes written against a ceiling of {ceiling}"
        );
        assert!(
            captured.iter().all(|byte| *byte == 0),
            "the bytes are the child's own"
        );
        sim.record(&format!("written={written} ceiling={ceiling}"));
        Ok(())
    })
}
