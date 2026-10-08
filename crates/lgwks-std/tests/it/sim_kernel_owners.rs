//! Seeded simulation of the kernel-owner containment vocabulary (#263).
//!
//! A cleanup that stops a tree through a kernel owner — a cgroup v2 scope or
//! the subreaper's adoptees — names that owner in its receipt, and the name is
//! a merge of every owner that ran. The merge is [`ContainmentMechanism::strongest`]:
//! a pure function of two mechanisms, so it is checked here without forking
//! anything. One seed draws a pair, the model ranks the pair from the
//! documented contract, and the trace folds what the merge answered; the sweep
//! runs twice per seed, so the same seed replaying a different hash is a
//! determinism failure rather than a passing test.
//!
//! The scope-name validation beside it is pure too: a scope name is one safe
//! directory entry, decided before any directory is touched. The seed draws
//! names from an alphabet with hostile members, and the model decides validity
//! from the documented rule. Refusals of invalid names touch no filesystem,
//! which is what makes this a simulation rather than an integration test.

#![cfg(all(unix, feature = "process"))]

#[cfg(target_os = "linux")]
use std::error::Error;

use lgwks_std::process::ContainmentMechanism;

use crate::rng::Rng;
use crate::seeded_sweep::{SWEEP_SEEDS, fold, initial_trace};

/// What every test here returns: a fixture that cannot be built fails the test
/// with its cause instead of panicking.
#[cfg(target_os = "linux")]
type TestResult = Result<(), Box<dyn Error>>;

/// Every mechanism the merge may be asked about, in one place so a new variant
/// cannot be added without this family asking about it.
const MECHANISMS: [ContainmentMechanism; 5] = [
    ContainmentMechanism::ProcessGroupOnly,
    ContainmentMechanism::ProcessTableSnapshot,
    ContainmentMechanism::ProcChildrenTree,
    ContainmentMechanism::SubreaperAdoption,
    ContainmentMechanism::CgroupKill,
];

/// The model's rank: a kernel-level owner beats a table reading, a per-process
/// walk beats a whole-table snapshot, and the group floor beats nothing.
///
/// Restated from the documented contract rather than copied from the
/// implementation, so the test pins the promise rather than the code.
fn rank(mechanism: ContainmentMechanism) -> u64 {
    match mechanism {
        ContainmentMechanism::ProcessGroupOnly => 0,
        ContainmentMechanism::ProcessTableSnapshot => 1,
        ContainmentMechanism::ProcChildrenTree => 2,
        ContainmentMechanism::SubreaperAdoption => 3,
        ContainmentMechanism::CgroupKill => 4,
        // The enum is non-exhaustive across crates, so a wildcard is
        // mandatory here. It ranks above everything on purpose: a new variant
        // the implementation orders anywhere else fails the sweep below, which
        // forces this model to name it rather than absorb it silently. The
        // `MECHANISMS` list above is what makes the sweep ask about it.
        _ => 5,
    }
}

/// Every ordered pair of mechanisms, swept per seed.
const PAIRS: usize = MECHANISMS.len() * MECHANISMS.len();

/// One sweep pass over every pair: the model's rank decides, the merge answers.
fn sweep_trace() -> u64 {
    let mut trace = initial_trace();
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..PAIRS {
            let first = MECHANISMS[rng.below(MECHANISMS.len())];
            let second = MECHANISMS[rng.below(MECHANISMS.len())];
            let merged = first.strongest(second);
            // Commutativity: the merge is a set union of evidence, so the
            // order the owners ran in must not change what the receipt names.
            assert_eq!(
                merged,
                second.strongest(first),
                "seed {seed:#x}: strongest({first:?}, {second:?}) must not depend on order"
            );
            // The model's rank decides the winner.
            let winner = if rank(first) >= rank(second) {
                first
            } else {
                second
            };
            assert_eq!(
                merged, winner,
                "seed {seed:#x}: strongest({first:?}, {second:?}) must name the stronger owner"
            );
            fold(&mut trace, rank(merged));
        }
        // Idempotence: merging a mechanism with itself names it.
        for mechanism in MECHANISMS {
            assert_eq!(
                mechanism.strongest(mechanism),
                mechanism,
                "seed {seed:#x}: strongest({mechanism:?}, {mechanism:?}) must name itself"
            );
            fold(&mut trace, rank(mechanism));
        }
    }
    trace
}

#[test]
/// The owner merge is commutative, idempotent, and ranks kernel owners first,
/// and the same seeds replay the same trace.
fn strongest_ranks_kernel_owners_above_table_readings() {
    assert_eq!(
        sweep_trace(),
        sweep_trace(),
        "the same seeds must replay the same merge trace"
    );
}

#[test]
/// Only the group floor claims nothing was read: every other mechanism rests
/// on a reading the receipt can point at.
fn read_a_table_is_false_only_for_the_group_floor() {
    for mechanism in MECHANISMS {
        assert_eq!(
            mechanism.read_a_table(),
            !matches!(mechanism, ContainmentMechanism::ProcessGroupOnly),
            "{mechanism:?} must say whether a reading backs it"
        );
    }
}

/// Bytes a scope-name draw may hold, hostile members included.
#[cfg(target_os = "linux")]
const NAME_ALPHABET: &[u8] = b"abZ019-_.+/.. \x7f\xc3\xa9";

/// The model's validity: one safe directory entry, restated from the rule.
///
/// `.` and `..` are refused with everything else invalid: they name the mount
/// and its parent rather than a scope in it, so accepting them would attach a
/// tree to a scope the supervisor does not own.
#[cfg(target_os = "linux")]
fn model_valid(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
}

/// One sweep pass over drawn names: validity matches the model, and refusals
/// name the input rather than touching the filesystem.
#[cfg(target_os = "linux")]
fn names_trace() -> Result<u64, Box<dyn Error>> {
    use lgwks_std::process::CgroupScope;
    use std::io::ErrorKind;

    let mut trace = initial_trace();
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..64 {
            let length = rng.below(81);
            let bytes: Vec<u8> = (0..length)
                .map(|_| NAME_ALPHABET[rng.below(NAME_ALPHABET.len())])
                .collect();
            // The draw is bytes; only UTF-8 spellings reach the validator,
            // because scope names are directory entries and those are bytes on
            // Linux — but this API takes `&str`, so a non-UTF-8 draw is a case
            // the API cannot express rather than a case it mishandles.
            let Ok(name) = std::str::from_utf8(&bytes) else {
                fold(&mut trace, 7);
                continue;
            };
            let expected = model_valid(name);
            match CgroupScope::create(name) {
                Ok(scope) => {
                    assert!(
                        expected,
                        "seed {seed:#x}: {name:?} was accepted but the model refuses it"
                    );
                    assert_eq!(
                        scope.path().file_name().and_then(|entry| entry.to_str()),
                        Some(name),
                        "seed {seed:#x}: the scope directory must be named for the tree"
                    );
                    fold(&mut trace, 1);
                    drop(scope);
                }
                Err(error) if error.kind() == ErrorKind::InvalidInput => {
                    assert!(
                        !expected,
                        "seed {seed:#x}: {name:?} was refused but the model accepts it"
                    );
                    fold(&mut trace, 2);
                }
                Err(error) if error.kind() == ErrorKind::Unsupported => {
                    // A host without a writable cgroup mount refuses every
                    // valid name before any directory is touched: the fallback
                    // the supervisor reads, not a failure of the name.
                    assert!(
                        expected,
                        "seed {seed:#x}: {name:?} is invalid, so it must be refused as input, not as unsupported"
                    );
                    fold(&mut trace, 3);
                }
                Err(error) => {
                    lgwks_std::trace::warn!(
                        seed = seed,
                        scope_name = name,
                        kind = ?error.kind(),
                        "sim_kernel_owners: a scope name was refused with an unmodelled kind: {error}"
                    );
                    return Err(format!(
                        "seed {seed:#x}: {name:?} refused with an unmodelled kind: {error}"
                    )
                    .into());
                }
            }
        }
    }
    Ok(trace)
}

#[test]
/// Scope-name validation matches its model on every drawn name, and the same
/// seeds replay the same trace.
#[cfg(target_os = "linux")]
fn scope_names_are_one_safe_entry() -> TestResult {
    assert_eq!(
        names_trace()?,
        names_trace()?,
        "the same seeds must replay the same validation trace"
    );
    Ok(())
}

#[test]
/// The subreaper attribution primitives are safe on empty input: nothing
/// adopted, nothing reaped, and the flag enables without disturbing the host.
#[cfg(target_os = "linux")]
fn subreaper_attribution_is_checked_before_it_signals() -> TestResult {
    use lgwks_std::process::{adopted_descendants, enable_child_subreaper, reap_descendants};

    enable_child_subreaper()?;
    assert!(
        adopted_descendants(&[])?.is_empty(),
        "no candidates is no adoptees"
    );
    assert!(
        adopted_descendants(&[0, -1, i32::MAX])?.is_empty(),
        "ids no process holds are adopted by nobody"
    );
    assert!(reap_descendants(&[])?.is_empty(), "no pids is no reaps");
    assert!(
        reap_descendants(&[i32::MAX])?.is_empty(),
        "an id no process holds is gone, which is what the reap was for"
    );
    Ok(())
}

#[test]
/// An orphan the subreaper adopts is named by its adoption, then killed and
/// reaped by pid.
///
/// The middle shell forks a sleeper and exits at once, so the sleeper is
/// re-parented to this process rather than init. The adoption check names it
/// because it is both a pid the test recorded and a current child of this
/// process — provably still the same process, since the OS cannot reissue the
/// pid of a child nobody reaped.
#[cfg(target_os = "linux")]
fn an_adopted_orphan_is_named_by_its_adoption() -> TestResult {
    use lgwks_std::process::{
        adopted_descendants, enable_child_subreaper, kill_process, process_exists, reap_descendants,
    };
    use std::time::{Duration, Instant};

    enable_child_subreaper()?;
    let dir = std::env::temp_dir().join(format!("lgwks-std-adopt-{}", std::process::id()));
    let _cleared = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("grandchild.pid");
    let mut middle = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 30 & echo $! > {}; exit 0", file.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // The middle writes the file before it exits, so the pid on disk is the
    // sleeper's; the exit adopts the sleeper into this process. Polled rather
    // than waited on: the middle is an untracked child, so a concurrent sweep
    // may collect its exit first, and only the file proves the fork happened.
    let recorded = Instant::now();
    let text = loop {
        if let Ok(text) = std::fs::read_to_string(&file)
            && !text.trim().is_empty()
        {
            break text;
        }
        if recorded.elapsed() > Duration::from_secs(10) {
            let _reaped = middle.wait();
            let _removed = std::fs::remove_dir_all(&dir);
            return Err("the middle shell never recorded its sleeper".into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    };
    // Best effort: the middle already exited, and its exit may already have
    // been collected. What matters below is that it exited, because the exit
    // is what adopts the sleeper into this process — so the adoption check
    // waits for the exit rather than assuming it.
    let exited = Instant::now();
    loop {
        match middle.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            // A concurrent sweep already collected it: still exited.
            Err(_) => break,
        }
        if exited.elapsed() > Duration::from_secs(10) {
            let _removed = std::fs::remove_dir_all(&dir);
            return Err("the middle shell never exited".into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    let grandchild: i32 = text.trim().parse().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the middle shell recorded no pid: {text:?}"),
        )
    })?;
    assert!(
        process_exists(grandchild)?,
        "the sleeper must outlive the middle shell that forked it"
    );
    let adopted = adopted_descendants(&[grandchild])?;
    assert!(
        adopted.contains(&grandchild),
        "an orphan of this process is its child, so the adoption check must name it"
    );
    kill_process(grandchild)?;
    // The signal is delivered; the scheduler ends the process on its own
    // time, so the reap is earned by polling the reap itself rather than by
    // asking whether the process still runs: a kill the scheduler has not yet
    // acted on already reads as "not running", which is no evidence the pid
    // is collectable.
    let settle = Instant::now();
    let reaped = loop {
        let reaped = reap_descendants(&[grandchild])?;
        if reaped == vec![grandchild] {
            break reaped;
        }
        if settle.elapsed() > Duration::from_secs(10) {
            return Err("the signalled adoptee was never reaped".into());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    };
    assert_eq!(
        reaped,
        vec![grandchild],
        "a signalled adoptee is reaped by pid, releasing its number"
    );
    assert!(
        adopted_descendants(&[grandchild])?.is_empty(),
        "a reaped adoptee is gone, so nothing is adopted any more"
    );
    let _removed = std::fs::remove_dir_all(&dir);
    Ok(())
}
