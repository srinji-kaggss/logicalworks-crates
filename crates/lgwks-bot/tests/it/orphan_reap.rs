//! #318: a supervisor killed with SIGKILL leaves its lanes running, and its
//! successor stops them through the identities it recorded.
//!
//! A `Supervisor` owns its children's process groups only while it runs: its
//! guards kill them on drop, on cancel and on shutdown, and none of that runs
//! when the process is killed outright. The groups are then orphans — their
//! leader re-parented to init, every member still running — and only a record
//! written *before* the kill can find them again. These tests drive that whole
//! journey with real processes:
//!
//! 1. a coordinator (this test binary re-run as a probe child) starts a lane
//!    through [`Supervisor::spawn_process_identified`], whose leader backgrounds
//!    a `sleep 300` and then becomes one, and writes the leader's identity down;
//! 2. the parent sends the coordinator `SIGKILL`, so no cleanup runs, and
//!    observes that both lane processes are still alive — the premise;
//! 3. the parent, as the successor, parses the stored identity and calls
//!    [`reap_orphaned_group`], which must stop the leader and the member;
//! 4. a second reap of the same record signals nothing, because the leader the
//!    record names no longer exists.
//!
//! This is logical_ci#16's acceptance — "`kill -9` a coordinator whose lane runs
//! `sleep 300`; the next invocation leaves no such `sleep` running" — observed
//! against the crate's own surface.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::{OrphanReap, ProcessIdentity, ProcessSpec, reap_orphaned_group};
use lgwks_bot::rt::supervise::Supervisor;

use crate::journal_fixtures::{ProbeGuard, pause};
use crate::process_probe::{kill_pid, pid_is_alive, wait_for_pid, wait_for_pid_gone};
use crate::scratch::Scratch;

/// What every test here returns.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long a recorded pid, or a stopped process's exit, may take.
const BUDGET: Duration = Duration::from_secs(10);

/// Turns this test binary into the coordinator probe.
const PROBE_ENV: &str = "LGWKS_ORPHAN_REAP_PROBE";

/// The directory the coordinator writes its records into.
const PROBE_DIR: &str = "LGWKS_ORPHAN_REAP_DIR";

/// The lane a coordinator runs: a member `sleep 300` in the background, its pid
/// recorded, and the leader itself becoming a second `sleep 300`.
fn lane(member_file: &std::path::Path) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(format!(
        "sleep 300 & echo $! > {}; exec sleep 300",
        member_file.display()
    ));
    spec
}

/// Stops, by pid, every lane process a test recorded, however the test ends.
///
/// The reap under test is what should stop them; this is the backstop for a
/// failed assertion, so a red run does not leave `sleep 300`s behind.
struct LaneGuard(Vec<i32>);

impl Drop for LaneGuard {
    fn drop(&mut self) {
        for pid in &self.0 {
            if pid_is_alive(*pid) {
                kill_pid(*pid);
            }
        }
    }
}

/// The coordinator: start the lane, write its leader's identity whole, park
/// until killed.
fn coordinator(dir: &std::path::Path) -> TestResult {
    let runtime = Runtime::new()?;
    let member_file = dir.join("member.pid");
    // The supervisor is returned out of the runtime so it — and the lane it
    // owns — stays alive for as long as this process does.
    let (supervisor, spawned) = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let spawned = supervisor
            .spawn_process_identified(&lane(&member_file))
            .await?;
        Ok::<_, std::io::Error>((supervisor, spawned))
    })?;
    wait_for_pid(&member_file, BUDGET).ok_or("the lane never recorded its member")?;
    // Written aside and renamed, so the parent never reads a half-written record.
    let partial = dir.join("leader.partial");
    std::fs::write(&partial, spawned.leader().to_string())?;
    std::fs::rename(&partial, dir.join("leader"))?;
    for _ in 0..600 {
        pause(100);
    }
    drop((supervisor, runtime));
    Err("the coordinator parked for its whole bound and was never killed".into())
}

#[test]
fn a_killed_coordinators_lane_is_stopped_by_its_successor() -> TestResult {
    if let Some(dir) = std::env::var_os(PROBE_DIR).filter(|_| std::env::var_os(PROBE_ENV).is_some())
    {
        return coordinator(std::path::Path::new(&dir));
    }
    let scratch = Scratch::new("orphan-reap")?;
    let record = scratch.path().join("leader");
    let mut probe = ProbeGuard(Some(
        crate::probe_command(&crate::probe_test(
            module_path!(),
            "a_killed_coordinators_lane_is_stopped_by_its_successor",
        ))?
        .env(PROBE_ENV, "1")
        .env(PROBE_DIR, scratch.path())
        .stdout(std::process::Stdio::null())
        .spawn()?,
    ));
    probe.kill_after_marker(&record, "orphan_reap coordinator")?;

    let leader: ProcessIdentity = std::fs::read_to_string(&record)?.parse()?;
    let member = wait_for_pid(&scratch.path().join("member.pid"), BUDGET)
        .ok_or("the lane's member pid was never recorded")?;
    let _backstop = LaneGuard(vec![leader.pid(), member]);
    assert!(
        pid_is_alive(leader.pid()) && pid_is_alive(member),
        "the premise: a SIGKILLed coordinator runs no cleanup, so its lane is still running"
    );

    let descendants = match reap_orphaned_group(&leader)? {
        OrphanReap::Signalled { descendants } => descendants,
        other => {
            return Err(format!("the successor's reap of a live leader answered {other:?}").into());
        }
    };
    assert!(
        descendants.contains(member),
        "the member is a descendant of the leader the record names: {descendants:?}"
    );
    assert!(
        wait_for_pid_gone(leader.pid(), BUDGET).is_some(),
        "the lane's leader is stopped by the successor"
    );
    assert!(
        wait_for_pid_gone(member, BUDGET).is_some(),
        "no `sleep 300` of the dead coordinator's lane is left running"
    );
    let again = reap_orphaned_group(&leader)?;
    assert!(
        !matches!(again, OrphanReap::Signalled { .. }),
        "a record whose leader is gone signals nothing, got {again:?}"
    );
    Ok(())
}

/// A scratch directory and a leader that writes its own pid into it and then
/// becomes a `sleep 300`, so the pid it wrote is the one that leads the group.
///
/// Returns the scratch (which removes the directory on drop), the file the pid
/// lands in, and the spec that writes it.
fn self_recording_leader(
    name: &str,
) -> Result<(Scratch, std::path::PathBuf, ProcessSpec), Box<dyn std::error::Error>> {
    let scratch = Scratch::new(name)?;
    let leader_file = scratch.path().join("leader.pid");
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(format!(
        "echo $$ > {}; exec sleep 300",
        leader_file.display()
    ));
    Ok((scratch, leader_file, spec))
}

#[test]
fn an_identified_spawn_names_its_leader_and_stays_supervised() -> TestResult {
    let (_scratch, leader_file, spec) = self_recording_leader("identified-spawn")?;
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let spawned = supervisor.spawn_process_identified(&spec).await?;
        let recorded =
            wait_for_pid(&leader_file, BUDGET).ok_or("the leader never recorded its pid")?;
        let _backstop = LaneGuard(vec![recorded]);
        assert_eq!(
            spawned.leader().pid(),
            recorded,
            "the identity names the process that leads the child's group"
        );
        assert_eq!(
            spawned.leader().to_string().parse::<ProcessIdentity>()?,
            *spawned.leader(),
            "the stored form reads back as the same identity"
        );
        // Still owned: shutdown takes the group, exactly as for `spawn_process`.
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.outcomes().first().map(|outcome| outcome.task()),
            Some(spawned.task()),
            "the outcome is reported under the task id the identified spawn returned"
        );
        assert!(
            wait_for_pid_gone(recorded, BUDGET).is_some(),
            "a supervised identified spawn is stopped by its supervisor's shutdown"
        );
        Ok(())
    })
}

/// T21, PID reuse: a record whose pid is now held by a process that started at
/// another instant is refused as `LeaderReused`, and that process is never
/// signalled.
///
/// A pid is a number the kernel hands out again, so a record that outlives its
/// leader can name a stranger. The start instant is what tells them apart, and
/// the kernel will not recycle a pid on demand, so the reuse is presented the
/// one way a test controls: a live, supervised leader and a record that names
/// its pid with a start it never had. A reap that trusted the number would
/// signal the live group. The refusal must name the process that actually holds
/// the pid, and that process must still be running and still be owned, which
/// the supervisor's own shutdown then shows by stopping it.
#[test]
fn a_record_naming_a_recycled_pid_signals_nothing_t21() -> TestResult {
    let (_scratch, leader_file, spec) = self_recording_leader("recycled-pid")?;
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let spawned = supervisor.spawn_process_identified(&spec).await?;
        let pid = wait_for_pid(&leader_file, BUDGET).ok_or("the leader never recorded its pid")?;
        let _backstop = LaneGuard(vec![pid]);
        let holder = spawned.leader().clone();
        let stale = ProcessIdentity::new(holder.pid(), &format!("{}0", holder.started()))?;
        assert_ne!(stale, holder, "the stale record names another process");

        match reap_orphaned_group(&stale)? {
            OrphanReap::LeaderReused { holder: named } => assert_eq!(
                named, holder,
                "the refusal names the process that holds the pid now"
            ),
            other => {
                let refusal = Err(format!(
                    "a record whose start disagrees with the pid's holder must be refused as \
                     reused, got {other:?}"
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "a_record_naming_a_recycled_pid_signals_nothing_t21: returning an error to the caller");
                return refusal;
            }
        }
        assert!(
            pid_is_alive(pid),
            "the process holding the recycled number was not signalled"
        );

        let report = supervisor.shutdown().await;
        assert_eq!(
            report.outcomes().first().map(|outcome| outcome.task()),
            Some(spawned.task()),
            "the holder is still the supervisor's to stop"
        );
        assert!(
            wait_for_pid_gone(pid, BUDGET).is_some(),
            "and its own shutdown stops it"
        );
        Ok(())
    })
}
