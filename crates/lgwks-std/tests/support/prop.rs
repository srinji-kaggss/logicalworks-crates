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
#![allow(
    dead_code,
    reason = "each property target that includes this module uses a different subset of it"
)]

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
/// regression of the real implementation.
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

/// Lift a setup error into a case failure that names it.
pub fn setup<T, E: Debug>(result: Result<T, E>) -> Result<T, TestCaseError> {
    result.map_err(|error| TestCaseError::fail(format!("setup failed: {error:?}")))
}
