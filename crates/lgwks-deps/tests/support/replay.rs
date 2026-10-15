//! The replay sweep the dependency-policy simulations share.
//!
//! Split from `sim.rs` because `lgwks_ast`'s scan simulation includes that file
//! for its draws and receipt and runs its own bands, so nothing here would be
//! reached from there. Every family in this crate that checks "the same seed
//! replays to the same trace" goes through [`replay_sweep`], once.

use crate::sim::{Rng, Trace, receipt};

/// Why a scenario failed, as every family reports it.
pub type Failure = Box<dyn std::error::Error>;

/// A scenario failure carrying `cause`, emitted before it is returned so a
/// failing seed is visible in the trace log as well as in the test result.
///
/// # Errors
///
/// Always: this is the refusal.
pub fn refuse<T>(cause: impl Into<String>) -> Result<T, Failure> {
    let refusal: Result<T, Failure> = Err(cause.into().into());
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sim: a scenario failed");
    refusal
}

/// Run `body` for every seed in `1..=seeds`, twice per seed, and require the
/// two runs' receipts to agree: a seed is a replayable answer, not a sample.
///
/// # Errors
///
/// The first seed whose run fails, whose run records nothing, or whose replay
/// records a different trace, named with `name` and the seed.
pub fn replay_sweep(
    name: &str,
    seeds: u64,
    mut body: impl FnMut(&mut Rng, &mut Trace) -> Result<(), Failure>,
) -> Result<(), Failure> {
    for seed in 1..=seeds {
        let mut first = Trace::new();
        body(&mut Rng::new(seed), &mut first)
            .map_err(|error| format!("{name}: seed {seed}: {error}"))?;
        let mut again = Trace::new();
        body(&mut Rng::new(seed), &mut again)
            .map_err(|error| format!("{name}: seed {seed}: {error}"))?;
        if receipt(&first)? != receipt(&again)? {
            return refuse(format!(
                "{name}: seed {seed} did not replay to the same trace"
            ));
        }
    }
    Ok(())
}
