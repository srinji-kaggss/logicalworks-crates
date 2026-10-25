//! The ordering of the run bound against the call bound, held by measurement.
//!
//! A host's deadline is a wall bound on a whole run (`script::within` arms an
//! engine timer for either clock), and every `gh` call in a run is already
//! bounded by [`HANG_BOUND`]. If the run bound is shorter than those calls
//! together, a loaded machine turns a slow run into a `DeadlineExceeded` verdict
//! and the same seed concludes two ways: main CI run 40085291776 failed
//! `same_seed_same_trace_hash_subject_band_09` (seed 75) and
//! `a_diff_past_the_file_ceiling_publishes_nothing` (seed 15) that way, with a
//! 60 s family host beside 8 busy tests (issue #751).
//!
//! The number of calls a run makes is not written down here. It is read from the
//! receiver's own argv log over both fault spaces, so a journey that grows a call
//! grows the bound this module demands.

use super::fake_gh::HANG_BOUND;
use super::subject::run_subject_seed;
use super::{Run, SEEDS, TestResult, host_named, run_seed};

/// The hosts the binary builds: the family host and the widest saturation tier.
const HOST_SIZES: [usize; 2] = [64, 10_000];

/// How many `gh` calls `run` made, from the receiver's own record.
///
/// A client that was never started records a marker line in place of an argv, and
/// it made no call, so a line that opens with `<` is not counted.
fn calls_of(run: &Run) -> usize {
    run.argv
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('<'))
        .count()
}

/// No host in this binary declares a run bound shorter than the per-call bound
/// times the most calls any run of either fault space makes.
///
/// The sweep runs the journey's whole fault space and the subject space through
/// the real path, takes the largest call count the receivers recorded, and holds
/// every host size to `HANG_BOUND * calls`. The clean run is measured on its own
/// first, so a sweep in which no run made more than one call cannot pass for a
/// bound that was never exercised.
#[test]
fn the_run_bound_outlasts_every_call_a_run_makes() -> TestResult {
    let (_clean_fault, clean) = run_seed(0, 0)?;
    let clean_calls = calls_of(&clean);
    assert!(
        clean_calls > 1,
        "a clean run reads the pull request before it publishes, so it makes more \
         than one call; the receiver recorded {clean_calls}:\n{}",
        clean.argv
    );

    let mut most = clean_calls;
    for index in 0..SEEDS {
        let (_fault, run) = run_seed(index, 0)?;
        most = most.max(calls_of(&run));
        let (_subject, run) = run_subject_seed(index)?;
        most = most.max(calls_of(&run));
    }

    let needed = HANG_BOUND
        .checked_mul(u32::try_from(most)?)
        .ok_or("the per-call bound times the calls of a run does not fit a duration")?;
    for tasks in HOST_SIZES {
        let bound = host_named("bounds-sim", tasks)?.limits().default_deadline();
        assert!(
            bound >= needed,
            "a host admitting {tasks} runs declares a {bound:?} run bound, shorter \
             than {most} calls of {HANG_BOUND:?} each ({needed:?}): a loaded machine \
             would end a slow run with DeadlineExceeded while every call is still \
             inside its own bound, and the same seed would conclude two ways"
        );
    }
    Ok(())
}
