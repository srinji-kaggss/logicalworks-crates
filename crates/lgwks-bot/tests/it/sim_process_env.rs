//! Simulation family: a supervised child's environment is the ordered fold of
//! its spec's deltas, and a cleared one carries nothing the owner did not name.
//!
//! Every scenario starts a **real** `/usr/bin/env` through the real
//! [`Supervisor::run_process`], so what is compared is the environment the
//! kernel handed the child, not the spec's own description of it. The seed draws
//! an ordered list of `env`, `env_remove` and `env_clear` calls over a small key
//! alphabet; a model folds the same list over the test's own environment, and the
//! child's printed environment must be exactly the model's.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `fold` | the child's environment is the in-order fold of every delta, and a clear drops every inherited variable and every earlier delta |
//! | `tenants` | 100, 1,000 and 10,000 concurrent children, each cleared and given its own value, never see another's value or the supervisor's environment |

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::sim;

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::num::NonZeroUsize;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::{EnvDelta, ProcessSpec};
use lgwks_bot::rt::supervise::Supervisor;

use sim::Band;

/// What a scenario reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this family sweeps, and how many bands it is cut into.
const SEEDS: u64 = 160;
const PARTS: u64 = 4;

/// The keys a scenario draws from; a prefix nobody's environment carries, so an
/// inherited value can never be mistaken for a drawn one.
const KEYS: [&str; 4] = [
    "LGWKS_SIM_ENV_A",
    "LGWKS_SIM_ENV_B",
    "LGWKS_SIM_ENV_C",
    "LGWKS_SIM_ENV_D",
];

/// The longest delta list a scenario draws.
const MAX_DELTAS: u32 = 12;

/// The retained-byte ceiling for the child's printed environment.
const CAPTURE: usize = 1 << 20;

/// An environment, as names to values.
type Env = BTreeMap<OsString, OsString>;

/// The one band of seeds a declared test sweeps.
fn band(index: usize) -> Band {
    sim::bands(SEEDS, PARTS).swap_remove(index)
}

/// A spec running `/usr/bin/env` with its stdout captured.
///
/// Named by its absolute path because a cleared environment has no `PATH` of
/// the supervisor's to search, which is what `env_clear` documents.
fn printing_env() -> Result<ProcessSpec, Box<dyn Error>> {
    let mut spec = ProcessSpec::new("/usr/bin/env");
    spec.capture_stdout(NonZeroUsize::new(CAPTURE).ok_or("the capture ceiling is not zero")?);
    Ok(spec)
}

/// The child's printed environment, one `NAME=value` per line.
///
/// Only lines naming a drawn key are kept unless `whole` is set: an inherited
/// value may itself hold a newline, and splitting one would invent a variable
/// the child never had. A cleared environment holds only drawn keys, whose
/// values carry no newline, so it is read whole.
fn read_env(stdout: &[u8], whole: bool) -> Result<Env, Box<dyn Error>> {
    let text = std::str::from_utf8(stdout)?;
    let mut env = Env::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| format!("a printed variable has no `=`: {line:?}"))?;
        if whole || KEYS.contains(&name) {
            env.insert(OsString::from(name), OsString::from(value));
        }
    }
    Ok(env)
}

/// The environment the model says the child receives: `deltas` folded in order
/// over `inherited`.
fn fold(inherited: &Env, deltas: &[EnvDelta]) -> Env {
    let mut env = inherited.clone();
    for delta in deltas {
        match *delta {
            EnvDelta::Set { ref key, ref value } => {
                env.insert(key.clone(), value.clone());
            }
            EnvDelta::Remove { ref key } => {
                env.remove(key);
            }
            EnvDelta::Clear => env.clear(),
            _ => {}
        }
    }
    env
}

/// The model's environment restricted to the drawn keys.
fn drawn_keys(env: &Env) -> Env {
    env.iter()
        .filter(|&(name, _)| KEYS.iter().any(|key| name.as_os_str() == *key))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// One seed: draw a delta list, run it, compare the child to the model.
fn fold_case(sim: &mut sim::Sim, runtime: &Runtime, inherited: &Env) -> TestResult {
    let mut spec = printing_env()?;
    let count = sim.rng().between(1, MAX_DELTAS);
    for _ in 0..count {
        let key = KEYS
            .get(usize::try_from(sim.rng().below(4))?)
            .ok_or("a key index is drawn below the alphabet's length")?;
        match sim.rng().below(5) {
            0 | 1 => {
                let value = format!("v{}", sim.rng().below(1_000));
                spec.env(key, &value);
                sim.trace.record(&format!("set {key}={value}"));
            }
            2 | 3 => {
                spec.env_remove(key);
                sim.trace.record(&format!("remove {key}"));
            }
            _ => {
                spec.env_clear();
                sim.trace.record("clear");
            }
        }
    }
    let cleared = spec
        .env_deltas()
        .iter()
        .any(|delta| matches!(*delta, EnvDelta::Clear));
    let expected = fold(inherited, spec.env_deltas());
    let mut supervisor = Supervisor::new(1);
    let run = runtime.block_on(supervisor.run_process(&spec))?;
    assert_eq!(run.exit_code(), Some(0), "env exits zero");
    assert!(
        !run.stdout().truncated(),
        "the printed environment fits the capture"
    );
    let seen = read_env(run.stdout().bytes(), cleared)?;
    if cleared {
        assert_eq!(
            seen, expected,
            "a cleared child sees exactly the deltas after the last clear"
        );
    } else {
        assert_eq!(
            seen,
            drawn_keys(&expected),
            "an uncleared child sees every delta folded over what it inherited"
        );
    }
    sim.record(if cleared { "cleared" } else { "inherited" });
    for (name, value) in &drawn_keys(&expected) {
        sim.trace.record(&format!(
            "{}={}",
            name.to_string_lossy(),
            value.to_string_lossy()
        ));
    }
    Ok(())
}

/// The test process's own environment: what an uncleared child inherits.
///
/// Refused when empty, because a clear that dropped nothing would prove nothing.
fn inherited() -> Result<Env, Box<dyn Error>> {
    let env: Env = std::env::vars_os().collect();
    if env.is_empty() {
        let refusal = Err("the test process has no environment for a clear to drop".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "inherited: returning an error to the caller");
        return refusal;
    }
    Ok(env)
}

/// One band of the fold family, swept twice for the replay receipt.
fn fold_band(index: usize) -> TestResult {
    let runtime = Runtime::new()?;
    let inherited = inherited()?;
    sim::assert_replays(band(index), |sim| fold_case(sim, &runtime, &inherited))
}

macro_rules! fold_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded sweep of ordered environment deltas against the model.
            #[test]
            fn $name() -> TestResult {
                fold_band($index)
            }
        )+
    };
}

fold_family!(
    fold_band_00 => 0; fold_band_01 => 1; fold_band_02 => 2; fold_band_03 => 3;
);

/// `tenants` children run at once, each cleared and named; each sees exactly
/// its own name and nothing of the supervisor's environment.
fn tenants_never_cross(tenants: usize) -> TestResult {
    const IN_FLIGHT: usize = 32;
    let runtime = Runtime::new()?;
    let specs = (0..tenants)
        .map(|tenant| {
            let mut spec = printing_env()?;
            spec.env("LGWKS_SIM_ENV_A", "leaked")
                .env_clear()
                .env("LGWKS_SIM_ENV_TENANT", tenant.to_string());
            Ok(spec)
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let runs = runtime.block_on(lgwks_bot::rt::task::join_all_bounded(
        IN_FLIGHT,
        specs.into_iter().map(|spec| async move {
            let mut supervisor = Supervisor::new(1);
            supervisor.run_process(&spec).await
        }),
    ));
    assert_eq!(runs.len(), tenants, "every tenant's child ran");
    for (tenant, run) in runs.into_iter().enumerate() {
        let run = run?;
        assert_eq!(
            run.stdout().bytes(),
            format!("LGWKS_SIM_ENV_TENANT={tenant}\n").as_bytes(),
            "tenant {tenant} sees its own name and nothing else"
        );
    }
    Ok(())
}

/// A hundred concurrent cleared children never cross.
#[test]
fn a_hundred_cleared_tenants_never_cross() -> TestResult {
    tenants_never_cross(100)
}

/// A thousand concurrent cleared children never cross.
#[test]
fn a_thousand_cleared_tenants_never_cross() -> TestResult {
    tenants_never_cross(1_000)
}

/// Ten thousand concurrent cleared children never cross.
///
/// The highest tier this family runs: each tenant is one real fork and exec of
/// `/usr/bin/env`, so a hundred thousand would spend its time in the kernel's
/// process creation rather than in anything this crate decides.
#[test]
fn ten_thousand_cleared_tenants_never_cross() -> TestResult {
    tenants_never_cross(10_000)
}

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_env_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = Vec::new();
    for band in sim::bands(SEEDS, PARTS) {
        covered.extend(band.seeds());
    }
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        u64::try_from(covered.len())?,
        SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}
