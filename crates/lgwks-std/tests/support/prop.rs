//! The property-test harness every crate's `prop_*` target shares (#273).
//!
//! Included by path (`#[path = ".../support/prop.rs"] mod prop;`) from each
//! crate's property targets, so the seed rule, the persistence rule and the
//! mutant rule are one definition rather than four copies that drift.
//!
//! - **One seed.** Every runner starts from the seed its target names, so a
//!   run replays exactly. Changing a seed is a new corpus, not a retry.
//! - **Persisted failures.** A property that must hold writes a failing seed
//!   under `proptest-regressions/` beside its target and replays it first on
//!   every later run, with no environment variable.
//! - **Mutants must fail.** [`shrunk`] runs a property against a deliberately
//!   broken implementation and hands back the minimal input it fails on; a
//!   property that cannot catch its mutant proves nothing.
//!
//! It carries its own tests rather than an `allow`: each property target that
//! includes this module exercises a different subset of it, and a harness whose
//! unused half is silenced is a harness nobody has read.

use std::error::Error;
use std::fmt::Debug;

use proptest::prelude::Strategy;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngSeed, TestCaseError, TestError, TestRunner,
};

/// What every property test returns: a refusal carries the shrunk input.
pub type Outcome = Result<(), Box<dyn Error>>;

/// A runner for a property that must hold: fixed seed, failures persisted
/// beside `source_file` (pass `file!()`).
pub fn runner(seed: u64, cases: u32, source_file: &'static str) -> TestRunner {
    TestRunner::new(Config {
        cases,
        rng_seed: RngSeed::Fixed(seed),
        source_file: Some(source_file),
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..Config::default()
    })
}

/// Run `property` against a mutant and return the minimal input it fails on.
///
/// Nothing is persisted: the mutant is meant to fail, and its seed is not a
/// regression of the real implementation. A case that failed in [`setup`]
/// did not reach the mutant, so it is refused rather than reported as a catch.
pub fn shrunk<S>(
    seed: u64,
    strategy: &S,
    property: impl Fn(S::Value) -> Result<(), TestCaseError>,
) -> Result<S::Value, Box<dyn Error>>
where
    S: Strategy,
    S::Value: Debug + 'static,
{
    let mut mutant_runner = TestRunner::new(Config {
        cases: 4_096,
        rng_seed: RngSeed::Fixed(seed),
        failure_persistence: None,
        ..Config::default()
    });
    match mutant_runner.run(strategy, property) {
        Err(TestError::Fail(reason, _)) if reason.message().starts_with(SETUP_FAILED) => {
            Err(format!("the mutant was never reached: {reason}").into())
        }
        Err(TestError::Fail(_, minimal)) => Ok(minimal),
        Err(TestError::Abort(reason)) => Err(format!("the mutant run aborted: {reason}").into()),
        Ok(()) => Err("the property did not catch its mutant".into()),
    }
}

/// Fail the case with `message` unless `holds`.
pub fn check(holds: bool, message: impl FnOnce() -> String) -> Result<(), TestCaseError> {
    if holds {
        Ok(())
    } else {
        Err(TestCaseError::fail(message()))
    }
}

/// How a [`setup`] failure's reason begins, so [`shrunk`] can tell it apart.
const SETUP_FAILED: &str = "setup failed: ";

/// Lift a setup error into a case failure that names it.
pub fn setup<T, E: Debug>(result: Result<T, E>) -> Result<T, TestCaseError> {
    result.map_err(|error| TestCaseError::fail(format!("{SETUP_FAILED}{error:?}")))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{Strategy, any};
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestCaseError;

    use super::{check, runner, setup, shrunk};

    /// A property that holds for every input it is given: a `u32` is four
    /// bytes wide on every target Rust supports.
    fn is_four_bytes(value: u32) -> Result<(), TestCaseError> {
        check(value.to_le_bytes().len() == 4, || {
            format!("a u32 occupied {} bytes", value.to_le_bytes().len())
        })
    }

    /// A property that holds for no input, which is the shape a mutation of a
    /// real property takes.
    fn always_fails(value: u32) -> Result<(), TestCaseError> {
        check(value.to_le_bytes().len() > 8, || {
            format!(
                "a u32 occupied {} bytes and was reported wider than a u64",
                value.to_le_bytes().len()
            )
        })
    }

    #[test]
    fn a_holding_property_passes_under_the_fixed_seed() -> Result<(), Box<dyn std::error::Error>> {
        let outcome = runner(7, 32, file!()).run(&any::<u32>(), is_four_bytes);
        assert!(
            outcome.is_ok(),
            "a property that holds must pass under a fixed seed, got {outcome:?}"
        );
        Ok(())
    }

    /// The seed rule the harness exists for: one seed, one corpus. Neither run
    /// below holds or fails, so neither writes a regression file into the source
    /// tree — a draw is read, not recorded.
    /// The first case `runner` draws under `seed`, or the reason it drew none.
    fn first_case(seed: u64) -> Result<u32, String> {
        let mut runner = runner(seed, 64, file!());
        match any::<u32>().new_tree(&mut runner) {
            Ok(tree) => Ok(tree.current()),
            Err(reason) => Err(format!("seed {seed} drew no case at all: {reason}")),
        }
    }

    #[test]
    fn sim_the_same_seed_draws_the_same_first_case() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            first_case(11)?,
            first_case(11)?,
            "seed 11 must draw one case, not two"
        );
        Ok(())
    }

    #[test]
    fn sim_distinct_seeds_draw_different_first_cases() -> Result<(), Box<dyn std::error::Error>> {
        assert_ne!(
            first_case(11)?,
            first_case(12)?,
            "two seeds drew one case, so the seed is not reaching the draw"
        );
        Ok(())
    }

    #[test]
    fn a_mutant_that_always_fails_is_caught_and_shrunk() -> Result<(), Box<dyn std::error::Error>> {
        let minimal = shrunk(3, &any::<u32>(), always_fails)?;
        assert_eq!(
            minimal, 0,
            "the minimal input that always fails a property about a u32 is zero"
        );
        Ok(())
    }

    #[test]
    fn a_property_that_catches_nothing_is_refused() {
        let outcome = shrunk(3, &any::<u32>(), is_four_bytes);
        assert!(
            outcome.is_err(),
            "a mutant run that passes proves nothing and must be reported as such"
        );
    }

    #[test]
    fn a_setup_failure_is_told_apart_from_a_property_failure() {
        assert!(
            setup(Err::<(), _>("no case source")).is_err(),
            "a setup error must become a case failure, not a value"
        );
        assert!(
            setup(Ok::<_, &str>(7)).is_ok(),
            "a setup that succeeded must pass its value through"
        );
    }
}
