//! Seeded simulation of the `process` feature's group primitives (#263).
//!
//! A supervisor's cleanup is three calls: `kill_process_group` stops a group,
//! the leader is reaped, and `process_group_exists` confirms nothing is left.
//! `child_has_exited_without_reaping` is what lets it observe the leader's end
//! without releasing the id it still owes signals to. Each call has a sharp
//! edge: `0` names the caller's own group, a negative id names a process, a
//! zombie leader still occupies its group, and a group whose members were
//! killed is not gone until whoever inherited them has reaped them.
//!
//! One seed draws every schedule here: how many groups exist, how many members
//! each holds, and which of spawn, probe, kill, observe and reap happens next.
//! A reference model written from the documented contract says what each call
//! must answer in each phase, and every answer the OS gives is checked against
//! it. Only the model's answers are folded into the trace, so the same seed
//! replays the same trace although process timing is the OS's.
//!
//! The phases are real processes, not a mock: a test that mocked `kill(2)`
//! could not see the zombie-leader or the reparented-member edge at all.

#![cfg(all(unix, feature = "process"))]

use std::error::Error;
use std::io::ErrorKind;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lgwks_std::process::{
    child_has_exited_without_reaping, kill_process_group, process_group_exists,
};

use crate::rng::Rng;
use crate::seeded_sweep::{SWEEP_SEEDS, fold, fold_usize, initial_trace};

/// What every test here returns: a fixture that cannot be built fails the
/// test with its cause instead of panicking.
type TestResult = Result<(), Box<dyn Error>>;

/// `ESRCH`, "no such process", on every Unix this feature targets.
const ESRCH: i32 = 3;

/// `EPERM`, "operation not permitted", on every Unix this feature targets.
const EPERM: i32 = 1;

/// `SIGKILL`, fixed by POSIX.
const SIGKILL: i32 = 9;

/// The lowest id no supported OS hands out as a pid: one past Linux's
/// `PID_MAX_LIMIT` (2^22). macOS stops at 99,999.
const VACANT_FLOOR: i32 = 4_194_305;

/// How long a killed group may take to leave the process table. Its members
/// are reparented and reaped by init on init's schedule, so absence is awaited
/// within this bound and never assumed.
const SETTLE: Duration = Duration::from_secs(10);

/// Scheduled operations per seeded lifecycle.
const STEPS: usize = 24;

/// Most groups one lifecycle holds at once.
const MAX_GROUPS: usize = 4;

/// Most background members one group holds beside its leader.
const MAX_MEMBERS: usize = 3;

/// Seeded ids each refusal and vacancy sweep draws, beside its fixed edges.
const DRAWN_IDS: usize = 256;

/// Where one group is in a supervisor's cleanup, as the contract sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Running, unsignalled.
    Live,
    /// Group killed, leader not yet reaped: a zombie still holds the id.
    Killed,
    /// Leader reaped and the group confirmed empty.
    Reaped,
}

impl Phase {
    /// The phase's number in a trace.
    const fn code(self) -> u64 {
        match self {
            Self::Live => 1,
            Self::Killed => 2,
            Self::Reaped => 3,
        }
    }
}

/// One process group: a leader that owns it and `members` sleepers beside it.
struct Group {
    /// The leader. Its pid is the group id.
    leader: Child,
    /// The group id.
    id: i32,
    /// The phase the model holds this group in.
    phase: Phase,
}

impl Group {
    /// Starts a leader in a new group holding `members` background sleepers.
    ///
    /// The shell backgrounds the members and then becomes the last sleeper
    /// itself, so every process in the group is a `sleep` that outlives any
    /// test unless the group is killed.
    fn spawn(members: usize) -> Result<Self, Box<dyn Error>> {
        let mut script = "sleep 30 & ".repeat(members);
        script.push_str("exec sleep 30");
        let leader = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let id = i32::try_from(leader.id())?;
        Ok(Self {
            leader,
            id,
            phase: Phase::Live,
        })
    }

    /// Kills the group. The leader stays unreaped.
    fn kill(&mut self) -> TestResult {
        kill_process_group(self.id)?;
        self.phase = Phase::Killed;
        Ok(())
    }

    /// Reaps the leader, waits for the group to empty, and returns the signal
    /// that ended the leader.
    fn reap(&mut self) -> Result<Option<i32>, Box<dyn Error>> {
        let status = self.leader.wait()?;
        self.phase = Phase::Reaped;
        if !settles_absent(self.id)? {
            return Err(format!("group {} outlived {SETTLE:?} after its reap", self.id).into());
        }
        Ok(status.signal())
    }

    /// Kills the group and reaps it, returning the leader's signal.
    fn stop(&mut self) -> Result<Option<i32>, Box<dyn Error>> {
        self.kill()?;
        self.reap()
    }
}

impl Drop for Group {
    /// A test that fails part-way still leaves no process behind. The results
    /// are discarded because a drop has no caller to report to; the test that
    /// owned the group has already failed with its own cause.
    fn drop(&mut self) {
        if self.phase != Phase::Reaped {
            drop(kill_process_group(self.id));
            drop(self.leader.wait());
        }
    }
}

/// Whether `id` names no group within [`SETTLE`].
fn settles_absent(id: i32) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(SETTLE)
        .ok_or("the settle deadline overflows the clock")?;
    while Instant::now() < deadline {
        if !process_group_exists(id)? {
            return Ok(true);
        }
        std::thread::yield_now();
    }
    Ok(false)
}

/// Whether the unreaped leader `pid` is observed as exited within [`SETTLE`].
fn exit_observed(pid: i32) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(SETTLE)
        .ok_or("the settle deadline overflows the clock")?;
    while Instant::now() < deadline {
        if child_has_exited_without_reaping(pid)? {
            return Ok(true);
        }
        std::thread::yield_now();
    }
    Ok(false)
}

/// A seeded id in `i32::MIN..=0`, the ids that name no group.
fn non_positive(rng: &mut Rng) -> Result<i32, Box<dyn Error>> {
    let magnitude = i32::try_from(rng.below(usize::try_from(i32::MAX)?))?;
    Ok(0_i32.saturating_sub(magnitude))
}

/// The non-positive ids every refusal sweep states its property at: the
/// fixed edges, then [`DRAWN_IDS`] drawn from `seed`.
fn refused_ids(seed: u64) -> Result<Vec<i32>, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut ids = vec![0, -1, i32::MIN, i32::MIN.saturating_add(1)];
    for _ in 0..DRAWN_IDS {
        ids.push(non_positive(&mut rng)?);
    }
    Ok(ids)
}

/// The vacant ids every vacancy sweep states its property at: the floor, the
/// ceiling, then [`DRAWN_IDS`] drawn from `seed` between them.
fn vacant_ids(seed: u64) -> Result<Vec<i32>, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let span = usize::try_from(i32::MAX.saturating_sub(VACANT_FLOOR))?;
    let mut ids = vec![VACANT_FLOOR, i32::MAX];
    for _ in 0..DRAWN_IDS {
        ids.push(VACANT_FLOOR.saturating_add(i32::try_from(rng.below(span))?));
    }
    Ok(ids)
}

/// Whether `error` is the OS's "no such process".
fn is_esrch(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(ESRCH)
}

/// Whether `error` is Darwin's answer to signalling a group that holds only
/// zombies: `EPERM`, found by this simulation. Linux reports success or
/// `ESRCH` there, so elsewhere this is never accepted.
fn zombie_group_refusal(error: &std::io::Error) -> bool {
    cfg!(target_os = "macos") && error.raw_os_error() == Some(EPERM)
}

/// One seeded lifecycle, checked against the model at every step, as its
/// trace hash.
fn lifecycle(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut trace = initial_trace();
    let mut groups: Vec<Group> = Vec::new();
    for step in 0..STEPS {
        let live = groups
            .iter()
            .filter(|group| group.phase != Phase::Reaped)
            .count();
        let op = if live == 0 || (groups.len() < MAX_GROUPS && rng.below(4) == 0) {
            0
        } else {
            rng.below(4).saturating_add(1)
        };
        fold_usize(&mut trace, op);
        if op == 0 {
            let members = rng.below(MAX_MEMBERS.saturating_add(1));
            fold_usize(&mut trace, members);
            groups.push(Group::spawn(members)?);
            continue;
        }
        let slot = rng.below(groups.len());
        let group = groups
            .get_mut(slot)
            .ok_or_else(|| format!("seed {seed:#x} step {step}: slot {slot} is out of range"))?;
        fold_usize(&mut trace, slot);
        fold(&mut trace, group.phase.code());
        match (op, group.phase) {
            // Probe.
            (1, Phase::Live) => {
                let present = process_group_exists(group.id)?;
                assert!(
                    present,
                    "seed {seed:#x} step {step}: a live group must be present"
                );
            }
            (1, Phase::Killed) => {
                // A zombie leader still occupies its group; whether the OS
                // reports it present is the OS's, but it is never an error.
                process_group_exists(group.id)?;
            }
            (1, Phase::Reaped) => {
                let present = process_group_exists(group.id)?;
                assert!(
                    !present,
                    "seed {seed:#x} step {step}: a reaped group must be absent"
                );
            }
            // Kill.
            (2, Phase::Live) => group.kill()?,
            (2, Phase::Killed) => {
                // Members may be zombies or gone: success or ESRCH, or on
                // Darwin EPERM, which is what it answers for a group that
                // holds only zombies. Never another error.
                if let Err(error) = kill_process_group(group.id) {
                    assert!(
                        is_esrch(&error) || zombie_group_refusal(&error),
                        "seed {seed:#x} step {step}: a second kill must succeed or say ESRCH, got {error}"
                    );
                }
            }
            (2, Phase::Reaped) => {
                let refused = kill_process_group(group.id);
                assert!(
                    refused.as_ref().is_err_and(is_esrch),
                    "seed {seed:#x} step {step}: killing a reaped group must say ESRCH, got {refused:?}"
                );
            }
            // Observe the leader without reaping it.
            (3, Phase::Live) => {
                let exited = child_has_exited_without_reaping(group.id)?;
                assert!(
                    !exited,
                    "seed {seed:#x} step {step}: a live leader must not read as exited"
                );
            }
            (3, Phase::Killed) => {
                assert!(
                    exit_observed(group.id)?,
                    "seed {seed:#x} step {step}: a killed leader must read as exited within {SETTLE:?}"
                );
            }
            (3, Phase::Reaped) => {
                assert!(
                    child_has_exited_without_reaping(group.id).is_err(),
                    "seed {seed:#x} step {step}: a reaped leader is no longer a child to observe"
                );
            }
            // Reap, killing first when the model holds it live.
            (_, Phase::Live) => {
                let signal = group.stop()?;
                assert_eq!(
                    signal,
                    Some(SIGKILL),
                    "seed {seed:#x} step {step}: the leader ends by SIGKILL"
                );
            }
            (_, Phase::Killed) => {
                let signal = group.reap()?;
                assert_eq!(
                    signal,
                    Some(SIGKILL),
                    "seed {seed:#x} step {step}: the leader ends by SIGKILL"
                );
            }
            (_, Phase::Reaped) => {}
        }
        fold(&mut trace, group.phase.code());
    }
    for group in &mut groups {
        match group.phase {
            Phase::Live => {
                group.stop()?;
            }
            Phase::Killed => {
                group.reap()?;
            }
            Phase::Reaped => {}
        }
        fold(&mut trace, group.phase.code());
    }
    Ok(trace)
}

#[test]
/// Every seeded schedule of spawn, probe, kill, observe and reap gets the
/// model's answer from the OS at every step.
fn a_seeded_lifecycle_agrees_with_the_model_at_every_step() -> TestResult {
    for seed in SWEEP_SEEDS {
        lifecycle(seed)?;
    }
    Ok(())
}

#[test]
/// The replay oracle: one seed, one lifecycle trace.
fn the_same_seed_replays_the_same_lifecycle_trace() -> TestResult {
    for seed in SWEEP_SEEDS {
        assert_eq!(
            lifecycle(seed)?,
            lifecycle(seed)?,
            "seed {seed:#x}: the same seed must replay the same lifecycle"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a schedule that ignored its seed would pass the replay.
fn distinct_seeds_drive_distinct_lifecycles() -> TestResult {
    let [first, second, ..] = SWEEP_SEEDS;
    assert_ne!(
        lifecycle(first)?,
        lifecycle(second)?,
        "seeds {first:#x} and {second:#x} must schedule different lifecycles"
    );
    Ok(())
}

#[test]
/// `0` and negative ids name the caller or a process, so the probe refuses them
/// as `InvalidInput` instead of probing something else.
fn every_non_positive_id_is_refused_by_the_probe() -> TestResult {
    for seed in SWEEP_SEEDS {
        for id in refused_ids(seed)? {
            assert_eq!(
                process_group_exists(id).map_err(|error| error.kind()),
                Err(ErrorKind::InvalidInput),
                "seed {seed:#x}: id {id} names the caller or a process, never a group"
            );
        }
    }
    Ok(())
}

#[test]
/// The kill refuses the same ids before any signal leaves the process.
fn every_non_positive_id_is_refused_by_the_kill() -> TestResult {
    // A regression here would signal this test's own group; nextest runs each
    // test in its own group, so the damage stays inside the failing test.
    for seed in SWEEP_SEEDS {
        for id in refused_ids(seed)? {
            assert_eq!(
                kill_process_group(id).map_err(|error| error.kind()),
                Err(ErrorKind::InvalidInput),
                "seed {seed:#x}: id {id} must be refused before any signal is sent"
            );
        }
    }
    Ok(())
}

#[test]
/// The exit observation refuses non-positive pids as `InvalidInput`.
fn every_non_positive_pid_is_refused_by_the_exit_observation() -> TestResult {
    for seed in SWEEP_SEEDS {
        for pid in refused_ids(seed)? {
            assert_eq!(
                child_has_exited_without_reaping(pid).map_err(|error| error.kind()),
                Err(ErrorKind::InvalidInput),
                "seed {seed:#x}: pid {pid} names no child"
            );
        }
    }
    Ok(())
}

#[test]
/// An id above every supported pid ceiling is absent, not an error: the
/// cleanup confirmation must read a vacant id as "nothing left".
fn ids_beyond_every_pid_ceiling_name_no_group() -> TestResult {
    for seed in SWEEP_SEEDS {
        for id in vacant_ids(seed)? {
            assert!(
                !process_group_exists(id)?,
                "seed {seed:#x}: id {id} is above every pid ceiling, so no group can hold it"
            );
        }
    }
    Ok(())
}

#[test]
/// A kill of a vacant group surfaces `ESRCH` rather than a fabricated success.
fn killing_a_vacant_group_reports_esrch_not_success() -> TestResult {
    for seed in SWEEP_SEEDS {
        for id in vacant_ids(seed)? {
            let killed = kill_process_group(id);
            assert!(
                killed.as_ref().is_err_and(is_esrch),
                "seed {seed:#x}: killing vacant group {id} must say ESRCH, got {killed:?}"
            );
        }
    }
    Ok(())
}

#[test]
/// A pid that is not a child is an error, never an observed exit a caller
/// could use to skip a reap.
fn observing_a_vacant_pid_is_an_error_not_an_exit() -> TestResult {
    for seed in SWEEP_SEEDS {
        for pid in vacant_ids(seed)? {
            assert!(
                child_has_exited_without_reaping(pid).is_err(),
                "seed {seed:#x}: pid {pid} is not a child, so no exit can be observed"
            );
        }
    }
    Ok(())
}

#[test]
/// Signal zero checks the group without signalling it: any number of probes
/// leaves the leader running.
fn probing_delivers_no_signal() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let mut group = Group::spawn(rng.below(MAX_MEMBERS.saturating_add(1)))?;
        for probe in 0..rng.below(512).saturating_add(64) {
            assert!(
                process_group_exists(group.id)?,
                "seed {seed:#x} probe {probe}: the probe must find the live group"
            );
        }
        assert!(
            !child_has_exited_without_reaping(group.id)?,
            "seed {seed:#x}: signal zero must not have ended the leader"
        );
        group.stop()?;
    }
    Ok(())
}

#[test]
/// One group kill takes down every member, not only the leader.
fn a_group_kill_reaches_every_member() -> TestResult {
    for seed in SWEEP_SEEDS {
        let members = Rng::new(seed).below(MAX_MEMBERS).saturating_add(1);
        let mut group = Group::spawn(members)?;
        // `reap` fails unless every member has left the table, which only a
        // signal to the whole group achieves: the members are not the leader's
        // to take down with it.
        group.stop()?;
        assert!(
            !process_group_exists(group.id)?,
            "seed {seed:#x}: all {members} members must be gone after the group kill"
        );
    }
    Ok(())
}

#[test]
/// The reaped status names `SIGKILL`, so a supervisor can tell its own kill
/// from the child's own exit.
fn a_killed_leader_reports_sigkill_when_reaped() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut group = Group::spawn(Rng::new(seed).below(MAX_MEMBERS.saturating_add(1)))?;
        assert_eq!(
            group.stop()?,
            Some(SIGKILL),
            "seed {seed:#x}: the reaped status must name the signal the kill sent"
        );
    }
    Ok(())
}

#[test]
/// Observing an exit any number of times leaves it waitable: the reap still
/// returns the real status.
fn an_exit_observation_never_reaps() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let mut group = Group::spawn(rng.below(MAX_MEMBERS.saturating_add(1)))?;
        group.kill()?;
        assert!(
            exit_observed(group.id)?,
            "seed {seed:#x}: the killed leader must read as exited"
        );
        for again in 0..rng.below(64).saturating_add(1) {
            assert!(
                child_has_exited_without_reaping(group.id)?,
                "seed {seed:#x} observation {again}: an observation must leave the exit waitable"
            );
        }
        assert_eq!(
            group.reap()?,
            Some(SIGKILL),
            "seed {seed:#x}: the reap after many observations still returns the real status"
        );
    }
    Ok(())
}

#[test]
/// After the reap the pid is no longer a child, and the observation says so.
fn a_reaped_child_is_no_longer_observable() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut group = Group::spawn(Rng::new(seed).below(MAX_MEMBERS.saturating_add(1)))?;
        group.stop()?;
        assert!(
            child_has_exited_without_reaping(group.id).is_err(),
            "seed {seed:#x}: a reaped pid must not be reported as an exit a second time"
        );
    }
    Ok(())
}

#[test]
/// A group held only by a zombie leader is still observable without error.
fn a_killed_but_unreaped_group_is_probed_without_error() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let mut group = Group::spawn(rng.below(MAX_MEMBERS.saturating_add(1)))?;
        group.kill()?;
        for probe in 0..rng.below(64).saturating_add(1) {
            assert!(
                process_group_exists(group.id).is_ok(),
                "seed {seed:#x} probe {probe}: a group held by a zombie leader is observable"
            );
        }
        group.reap()?;
    }
    Ok(())
}

#[test]
/// Stopping every group one tenant owns leaves the other tenant's groups
/// present and their leaders running.
fn two_tenants_groups_are_isolated() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let mut first: Vec<Group> = Vec::new();
        let mut second: Vec<Group> = Vec::new();
        for _ in 0..rng.below(3).saturating_add(1) {
            first.push(Group::spawn(rng.below(MAX_MEMBERS))?);
            second.push(Group::spawn(rng.below(MAX_MEMBERS))?);
        }
        while !first.is_empty() {
            let mut victim = first.swap_remove(rng.below(first.len()));
            victim.stop()?;
            for (index, survivor) in second.iter().enumerate() {
                assert!(
                    process_group_exists(survivor.id)?,
                    "seed {seed:#x}: tenant two's group {index} must survive tenant one's kill"
                );
                assert!(
                    !child_has_exited_without_reaping(survivor.id)?,
                    "seed {seed:#x}: tenant two's leader {index} must still be running"
                );
            }
        }
        for mut survivor in second {
            survivor.stop()?;
        }
    }
    Ok(())
}

#[test]
/// Concurrent probes of a live and a reaped group each get the model's answer.
fn concurrent_probes_agree_with_the_model() -> TestResult {
    /// Threads probing at once.
    const PROBERS: usize = 16;
    /// Probes each thread makes.
    const PROBES: usize = 256;
    for seed in SWEEP_SEEDS {
        let mut live = Group::spawn(1)?;
        let mut gone = Group::spawn(1)?;
        gone.stop()?;
        let expected = [(live.id, true), (gone.id, false)];
        let disagreements = std::thread::scope(|scope| {
            // Every prober is started before any is joined, so the probes overlap.
            let mut probers = Vec::with_capacity(PROBERS);
            for prober in 0..PROBERS {
                let mut rng =
                    Rng::new(seed.wrapping_add(u64::try_from(prober).unwrap_or(u64::MAX)));
                probers.push(scope.spawn(move || {
                    let mut wrong = 0_usize;
                    for _ in 0..PROBES {
                        let Some(&(id, present)) = expected.get(rng.below(expected.len())) else {
                            continue;
                        };
                        if process_group_exists(id).ok() != Some(present) {
                            wrong = wrong.saturating_add(1);
                        }
                    }
                    wrong
                }));
            }
            probers
                .into_iter()
                .map(|prober| prober.join().unwrap_or(usize::MAX))
                .fold(0_usize, usize::saturating_add)
        });
        assert_eq!(
            disagreements, 0,
            "seed {seed:#x}: {PROBERS} concurrent probers must each see the model's answer"
        );
        live.stop()?;
    }
    Ok(())
}

#[test]
/// A retried cleanup is idempotent and honest: every repeat says `ESRCH`.
fn a_second_kill_of_a_reaped_group_is_esrch_not_success() -> TestResult {
    for seed in SWEEP_SEEDS {
        let mut rng = Rng::new(seed);
        let mut group = Group::spawn(rng.below(MAX_MEMBERS.saturating_add(1)))?;
        group.stop()?;
        for retry in 0..rng.below(16).saturating_add(1) {
            let again = kill_process_group(group.id);
            assert!(
                again.as_ref().is_err_and(is_esrch),
                "seed {seed:#x} retry {retry}: a retried cleanup must not report a kill it did not make, got {again:?}"
            );
        }
    }
    Ok(())
}

#[test]
/// A leader that exits on its own is observed, reaped with its code, and its
/// group is then absent.
fn a_leader_that_exits_by_itself_leaves_its_group_absent_once_reaped() -> TestResult {
    for seed in SWEEP_SEEDS {
        let code = i32::try_from(Rng::new(seed).below(64))?;
        let mut leader = Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let id = i32::try_from(leader.id())?;
        assert!(
            exit_observed(id)?,
            "seed {seed:#x}: the leader's own exit must be observed"
        );
        let status = leader.wait()?;
        assert_eq!(
            status.code(),
            Some(code),
            "seed {seed:#x}: the reap returns the exit code"
        );
        assert!(
            settles_absent(id)?,
            "seed {seed:#x}: a group whose leader exited and was reaped must be absent"
        );
    }
    Ok(())
}
