//! Simulated cargo: the gate that lets one cargo process at a time into a run's
//! work root, and the environment cargo children are started in.
//!
//! Two cargo processes sharing one target directory are not a measurement: the
//! second one's build time and peak memory belong to the first. The gate is what
//! makes every number in the table the number of one process alone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{OpenOptions, TryLockError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use super::BenchError;
use super::cargo::Cargo;
use super::children::{Children, forget_launched_crate};
use super::replay::{Failure, replay_sweep};
use super::seed::Rng;
use super::sim_kit::{Scratch, count, ensure};

/// The lock file the work root holds, as the manifest of a run documents it.
const GATE_FILE: &str = ".cargo-gate.lock";

/// How long a test lets a child run; no test here starts one.
const BOUND: Duration = Duration::from_secs(60);

/// Source: `flock(2)` and `std::fs::File::try_lock` (a lock belongs to the open
/// file, so it excludes another open of the same file in the same process).
/// Requirement: while one build or oracle run holds the gate no other holder can
/// take it, whoever asks; once it is released the next can. Deterministic: the
/// rival asks with `try_lock`, so nothing waits and nothing sleeps.
#[test]
fn the_gate_excludes_every_other_holder_until_it_is_released() -> Result<(), Failure> {
    replay_sweep("gate", 40, |rng, trace| {
        let scratch = Scratch::new("gate")?;
        let children = Children::new(false);
        let cargo = Cargo::new(scratch.path(), &children, BOUND, BOUND);
        for round in 0..count(rng, 1, 4)? {
            let held = cargo.gate()?;
            let rival = OpenOptions::new()
                .append(true)
                .open(scratch.path().join(GATE_FILE))?;
            match rival.try_lock() {
                Err(TryLockError::WouldBlock) => {}
                Ok(()) => {
                    return Err(
                        format!("round {round}: a rival took the gate while it was held").into(),
                    );
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
            drop(held);
            match rival.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    return Err(format!(
                        "round {round}: the gate stayed held after its holder left"
                    )
                    .into());
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
            rival.unlock()?;
            trace.record_number("round", round);
        }
        Ok(())
    })
}

/// One thread's visits to the gate: it holds the gate, counts the threads inside,
/// does a little work and leaves.
fn visit(
    cargo: &Cargo<'_>,
    inside: &AtomicUsize,
    peak: &AtomicUsize,
    rounds: usize,
) -> Result<(), BenchError> {
    for _ in 0..rounds {
        let held = cargo.gate()?;
        let now = inside.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        peak.fetch_max(now, Ordering::SeqCst);
        for step in 0..500_u32 {
            std::hint::black_box(step);
        }
        inside.fetch_sub(1, Ordering::SeqCst);
        drop(held);
    }
    Ok(())
}

/// Requirement: however many trials ask at once, one is inside the gate at a time,
/// every one gets in, and none leaves the count above zero. A broken gate fails
/// this by letting two in; a correct one never does.
#[test]
fn trials_asking_together_pass_the_gate_one_at_a_time() -> Result<(), Failure> {
    replay_sweep("threads", 12, |rng, trace| {
        let scratch = Scratch::new("threads")?;
        let children = Children::new(false);
        let cargo = Cargo::new(scratch.path(), &children, BOUND, BOUND);
        let (inside, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let threads = count(rng, 2, 6)?;
        let rounds = count(rng, 1, 4)?;
        let outcomes: Vec<Result<(), BenchError>> = thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| scope.spawn(|| visit(&cargo, &inside, &peak, rounds)))
                .collect();
            handles
                .into_iter()
                .map(|handle| match handle.join() {
                    Ok(outcome) => outcome,
                    Err(_) => Err(BenchError::Refused(
                        "a thread at the gate panicked".to_owned(),
                    )),
                })
                .collect()
        });
        ensure(outcomes.len() == threads, || "a thread was lost".to_owned())?;
        for outcome in outcomes {
            outcome?;
        }
        ensure(
            peak.load(Ordering::SeqCst) == 1 && inside.load(Ordering::SeqCst) == 0,
            || {
                format!(
                    "{} threads were inside the gate at once",
                    peak.load(Ordering::SeqCst)
                )
            },
        )?;
        trace.record_number("threads", threads);
        trace.record_number("rounds", rounds);
        Ok(())
    })
}

/// Requirement: a work root that does not exist is a refusal that names the lock
/// file it could not open, never a build that runs ungated.
#[test]
fn a_missing_work_root_is_refused_naming_the_lock_file() -> Result<(), Failure> {
    replay_sweep("missing root", 20, |rng, trace| {
        let scratch = Scratch::new("missing")?;
        let absent = scratch.path().join(format!("absent-{}", rng.below(1000)));
        let children = Children::new(false);
        let cargo = Cargo::new(&absent, &children, BOUND, BOUND);
        match cargo.gate() {
            Err(BenchError::Io { ref path, .. }) => {
                ensure(
                    path.starts_with(&absent) && path.ends_with(GATE_FILE),
                    || format!("the refusal named {}", path.display()),
                )?;
            }
            Ok(_) => return Err("a gate was granted in a work root that does not exist".into()),
            Err(other) => return Err(format!("the refusal was {other:?}, not an I/O error").into()),
        }
        trace.record("refused");
        Ok(())
    })
}

/// What Cargo sets for the crate it runs, by exact name (the Cargo Book,
/// "Environment variables Cargo sets for crates").
const DESCRIBING: [&str; 4] = [
    "CARGO_CRATE_NAME",
    "CARGO_BIN_NAME",
    "CARGO_PRIMARY_PACKAGE",
    "CARGO_TARGET_TMPDIR",
];

/// The families of those variables: one per manifest field or binary.
const FAMILIES: [&str; 3] = ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_EXE_"];

/// What follows a family's prefix.
const SUFFIXES: [&str; 4] = ["NAME", "VERSION", "DIR", "ai-authoring"];

/// Configuration and look-alikes a child must keep: `CARGO_HOME` never moves
/// (the Director's build rule), `CARGO_TARGET_DIR` is the one the runner then sets,
/// and a name that only contains or resembles a family is not one.
const KEPT: [&str; 10] = [
    "CARGO",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_JOBS",
    "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
    "RUSTFLAGS",
    "PATH",
    "MY_CARGO_PKG_NAME",
    "cargo_pkg_name",
    "CARGO_PKGX",
];

/// A value for an environment variable.
fn value(rng: &mut Rng) -> String {
    format!("v{}", rng.below(1000))
}

/// Requirement: a cargo child started by a `cargo run` owner forgets exactly the
/// variables that describe the crate that owner was launched for (Cargo
/// fingerprints them, so any one left behind rebuilds every crate that reads it)
/// and keeps every other variable, value intact.
#[test]
fn a_child_forgets_the_launched_crate_and_nothing_else() -> Result<(), Failure> {
    replay_sweep("environment", 400, |rng, trace| {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        let mut kept: BTreeMap<String, String> = BTreeMap::new();
        for name in KEPT {
            if rng.coin() {
                let wanted = value(rng);
                env.insert(name.to_owned(), wanted.clone());
                kept.insert(name.to_owned(), wanted);
            }
        }
        for name in DESCRIBING {
            if rng.coin() {
                env.insert(name.to_owned(), value(rng));
            }
        }
        for family in FAMILIES {
            for suffix in SUFFIXES {
                if rng.chance(300) {
                    env.insert(format!("{family}{suffix}"), value(rng));
                }
            }
        }
        forget_launched_crate(&mut env);
        ensure(env == kept, || {
            format!(
                "kept {:?}, expected {kept:?}",
                env.keys().collect::<BTreeSet<_>>()
            )
        })?;
        trace.record_number("kept", env.len());
        Ok(())
    })
}
