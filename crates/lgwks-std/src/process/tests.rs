//! Unit tests for the process primitives, capture, identity and reaping.

#[cfg(unix)]
use std::fmt::Write as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

#[cfg(target_os = "linux")]
use super::capture::{proc_stat_says_running, stat_parent, status_has_kill_pending};

/// The `/proc` table snapshot reads a stat line's parent after the last `)`,
/// so a command name holding spaces and parentheses cannot shift it (#345).
#[test]
#[cfg(target_os = "linux")]
fn a_stat_line_names_its_parent_after_the_command() {
    assert_eq!(
        stat_parent("4242 (a (tricky) name) S 77 4242 4242 0 -1 0"),
        Some(77),
        "the parent is the second field after the command name"
    );
    assert_eq!(stat_parent("4242 (cut"), None, "a cut line names no parent");
    assert_eq!(
        stat_parent("4242 (sh) S notapid"),
        None,
        "a non-numeric parent is refused, not guessed"
    );
}
use super::*;

#[test]
fn invalid_pgid_is_rejected() {
    assert!(kill_process_group(0).is_err());
    assert!(kill_process_group(-1).is_err());
}

#[test]
#[cfg(unix)]
fn nonexistent_group_surfs_os_error() {
    // i32::MAX is not a live group; the OS must refuse, not succeed.
    assert!(kill_process_group(i32::MAX).is_err());
}

/// A spawned shell inherits the default SIGCONT disposition (#691).
///
/// A gated child is resumed with SIGCONT; an ignored SIGCONT would leave it
/// stopped forever, so no spawn may hand a child an ignored one.
#[test]
#[cfg(all(unix, feature = "process"))]
fn spawned_children_do_not_ignore_sigcont() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("trap -p CONT")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.trim().is_empty(),
        "a spawned child must not ignore SIGCONT, got: {text:?}"
    );
    assert!(
        output.status.success(),
        "the probe shell exits cleanly: {:?}",
        output.status
    );
    Ok(())
}

/// A shell that stops itself stays stopped until it is continued (#691).
///
/// The gate shell's contract in one process: `kill -STOP` stops the shell on
/// its first instruction, a waiter observes the stop without reaping, and
/// SIGCONT resumes exactly that shell to its own exit.
#[test]
#[cfg(all(unix, feature = "process"))]
fn a_self_stopping_shell_survives_until_continued() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};

    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("kill -STOP $$; exit 42")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = i32::try_from(child.id())?;
    // Bounded wait for the stop: the shell stops on its first instruction.
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(10)) else {
        let _killed = child.kill();
        return Err("the stop deadline overflowed the clock".into());
    };
    loop {
        if child_has_exited_without_reaping(pid)? {
            let _waited = child.wait()?;
            return Err("a self-stopping shell must stop, not exit".into());
        }
        if child_stopped_without_reaping(pid)? {
            break;
        }
        if Instant::now() >= deadline {
            let _killed = child.kill();
            return Err("a self-stopping shell never stopped".into());
        }
        // Yield rather than park: the stop lands on the shell's first
        // instruction, so the wait is a reschedule, never a sleep.
        std::thread::yield_now();
    }
    continue_process(pid)?;
    let status = child.wait()?;
    assert_eq!(
        status.code(),
        Some(42),
        "a continued shell runs its payload to its own exit: {status:?}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_live_group_is_present_and_a_reaped_one_is_absent() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let group = i32::try_from(child.id())?;
    assert!(
        process_group_exists(group)?,
        "the live child owns its group"
    );
    child.kill()?;
    child.wait()?;
    assert!(!process_group_exists(group)?, "the reaped group is absent");
    Ok(())
}

#[test]
#[cfg(unix)]
fn the_callers_group_and_nonpositive_ids_are_not_probed() {
    for refused in [0, -1] {
        assert_eq!(
            process_group_exists(refused).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "group id {refused} names the caller or a process, not a group"
        );
    }
}

#[test]
fn invalid_pid_is_rejected_for_the_exit_observation() {
    assert!(child_has_exited_without_reaping(0).is_err());
    assert!(child_has_exited_without_reaping(-1).is_err());
}

#[test]
#[cfg(unix)]
fn a_non_child_pid_is_an_error_not_an_observation() {
    // i32::MAX is not our child, so waitid must refuse rather than
    // report an exit; a fabricated `true` would let a caller reap-skip a
    // process that is still running.
    assert!(child_has_exited_without_reaping(i32::MAX).is_err());
}

#[test]
#[cfg(unix)]
fn a_live_child_is_reported_not_yet_exited() -> Result<(), Box<dyn std::error::Error>> {
    // A real child of this test process, held alive until the probe.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = i32::try_from(child.id())?;
    assert!(
        !child_has_exited_without_reaping(pid)?,
        "live child reported exited"
    );
    child.kill()?;
    let status = child.wait()?;
    assert!(
        !status.success(),
        "the killed probe child must report its signal, not success"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn an_exited_child_is_reported_exited_and_stays_reapable() -> Result<(), Box<dyn std::error::Error>>
{
    let mut child = std::process::Command::new("true")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = i32::try_from(child.id())?;
    // The child may need a moment to actually exit.
    let mut observed_exit = false;
    for _ in 0..100_000 {
        if child_has_exited_without_reaping(pid)? {
            observed_exit = true;
            break;
        }
        std::thread::yield_now();
    }
    assert!(
        observed_exit,
        "an exited child must be observed within the bound"
    );
    // The observation must not have consumed the exit: the reap still
    // returns the real status.
    let status = child.wait()?;
    assert!(status.success(), "`true` must report success after reaping");
    Ok(())
}

/// The id `i32::MAX` can never name a process on a supported platform, so a
/// vacancy probe is decidable rather than a race with the next fork.
#[cfg(unix)]
const VACANT: i32 = i32::MAX;

/// `ESRCH`, "no such process", on every Unix this feature targets.
///
/// Read as the raw code rather than as an [`std::io::ErrorKind`]: macOS
/// classifies `ESRCH` as uncategorised where Linux calls it `NotFound`, so
/// the kind is a property of the platform and the errno is the fact.
#[cfg(unix)]
const ESRCH: i32 = 3;

/// A tree the tests build and tear down: `sh` with `depth` backgrounded
/// `sleep` descendants, each of which writes its level and its own pid to the
/// tree's shared standard output.
///
/// The pids are what make the model checkable: the capture is compared against
/// ids the tree itself wrote, rather than against a count the test already
/// assumed. Every process in the tree holds the pipe's write end, so a read
/// returns data when one of them writes and end-of-file once the last of them
/// has exited: the tests wait on those events, never on a clock.
#[cfg(unix)]
struct Tree {
    /// The leader, unreaped: its pid pins the tree until the test ends.
    leader: std::process::Child,
    /// The leader's id, which is also its group id.
    root: i32,
    /// The descendant pids, in fork order.
    descendants: Vec<i32>,
    /// What the tree's processes write.
    output: std::io::BufReader<std::process::ChildStdout>,
}

#[cfg(unix)]
impl Tree {
    /// Start `script` under `sh`, in its own group, with its output piped.
    fn start(script: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut leader = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            // Its own group, or the drop-time group kill would name a group
            // this tree does not lead and leave every sleeper running.
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let root = i32::try_from(leader.id())?;
        let Some(pipe) = leader.stdout.take() else {
            let _killed = kill_process_group(root);
            let _reaped = leader.wait();
            #[cfg(feature = "trace")]
            crate::trace::debug!(root, "the tree's output pipe is missing");
            return Err("the tree's output pipe is missing".into());
        };
        Ok(Self {
            leader,
            root,
            descendants: Vec::new(),
            output: std::io::BufReader::new(pipe),
        })
    }

    /// Grow a tree of `depth` descendants and wait until each has written
    /// its pid.
    fn grow(depth: usize) -> Result<Self, Box<dyn std::error::Error>> {
        let mut script = String::new();
        for level in 0..depth {
            // `exec` and not a second process: the level is then exactly the
            // pid it wrote, so the model and the capture are comparable.
            let _written = write!(script, "sh -c 'echo {level} $$; exec sleep 30' & ");
        }
        script.push_str("exec sleep 30");
        let mut tree = Self::start(&script)?;
        let mut found: Vec<Option<i32>> = vec![None; depth];
        for _ in 0..depth {
            let line = tree
                .line()?
                .ok_or("a level exited before it wrote its pid")?;
            let (level, pid) = line
                .split_once(' ')
                .ok_or_else(|| format!("{line:?} is not a level and a pid"))?;
            let slot = found
                .get_mut(level.parse::<usize>()?)
                .ok_or_else(|| format!("level {level} is past the depth {depth}"))?;
            *slot = Some(pid.parse::<i32>()?);
        }
        tree.descendants = found
            .into_iter()
            .enumerate()
            .map(|(level, pid)| pid.ok_or_else(|| format!("level {level} never wrote its pid")))
            .collect::<Result<Vec<i32>, String>>()?;
        Ok(tree)
    }

    /// The next line the tree wrote, or `None` once every process holding the
    /// pipe has exited.
    fn line(&mut self) -> std::io::Result<Option<String>> {
        let mut text = String::new();
        let read = std::io::BufRead::read_line(&mut self.output, &mut text)?;
        Ok(if read == 0 {
            None
        } else {
            Some(text.trim_end().to_owned())
        })
    }

    /// Block until every process in the tree has exited: they all hold the
    /// pipe's write end, so its end-of-file is the last of them going.
    fn wait_for_exit(&mut self) -> std::io::Result<()> {
        std::io::copy(&mut self.output, &mut std::io::sink()).map(|_copied| ())
    }
}

#[cfg(unix)]
impl Drop for Tree {
    /// Leave nothing behind: the group kill takes the whole tree, and the
    /// leader is reaped by the wait that follows it.
    fn drop(&mut self) {
        let _ignored = kill_process_group(self.root);
        let _reaped = self.leader.wait();
    }
}

#[test]
#[cfg(unix)]
fn a_non_positive_root_is_refused_before_any_table_is_read()
-> Result<(), Box<dyn std::error::Error>> {
    for refused in [0, -1, i32::MIN] {
        assert_eq!(
            capture_descendants(refused).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "root {refused} names the caller or no process, so no tree is walked"
        );
    }
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_non_positive_pid_is_refused_by_both_per_process_primitives()
-> Result<(), Box<dyn std::error::Error>> {
    for refused in [0, -1, i32::MIN] {
        assert_eq!(
            process_exists(refused).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "pid {refused} names the caller or no process, so nothing is probed"
        );
        assert_eq!(
            kill_process(refused).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "pid {refused} must be refused before any signal leaves the process"
        );
    }
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_vacant_pid_is_absent_and_signalling_it_says_esrch() -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        !process_exists(VACANT)?,
        "an id above every pid ceiling names no process"
    );
    assert_eq!(
        kill_process(VACANT).map_err(|error| error.raw_os_error()),
        Err(Some(ESRCH)),
        "signalling a vacant pid must say it does not exist, never succeed"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn the_capture_finds_every_descendant_of_a_live_root() -> Result<(), Box<dyn std::error::Error>> {
    let tree = Tree::grow(3)?;
    let captured = capture_descendants(tree.root)?;
    assert!(
        captured.mechanism().read_a_table(),
        "a live capture must name a mechanism that read a table, got {:?}",
        captured.mechanism()
    );
    assert!(
        !captured.is_truncated(),
        "a depth-3 tree is inside every bound"
    );
    let mut expected = tree.descendants.clone();
    expected.sort_unstable();
    assert_eq!(
        captured.pids(),
        expected.as_slice(),
        "the capture must be exactly the tree the pids describe"
    );
    assert!(
        !captured.contains(tree.root),
        "the root leads the group; a capture that listed it would double-signal the leader"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_descendant_that_left_the_group_is_still_captured() -> Result<(), Box<dyn std::error::Error>> {
    // The case a group signal cannot reach: the grandchild makes itself a
    // session leader, so it is in a different group from its parent and only
    // a pid signal can stop it. The capture is what makes it reachable.
    // The escapee writes its own pid to the shared pipe and the test reads it:
    // the pid is data to wait for, and a host without perl ends the pipe
    // when the leader's sleep does, which reads as `None`. The pid is printed
    // after `setsid` returns, so the line names a process already out of the
    // group; `$|` flushes it at once, since a pipe is block-buffered.
    let mut tree = Tree::start(
        "perl -e 'use POSIX; setsid(); $| = 1; print $$, chr(10); sleep 30' & exec sleep 30",
    )?;
    let root = tree.root;
    let Some(line) = tree.line()? else {
        return Err("this host cannot leave a process group, so nothing was exercised".into());
    };
    let escaped = line.parse::<i32>()?;
    let captured = capture_descendants(root)?;
    assert!(
        captured.contains(escaped),
        "a descendant that called setsid is still a child of the root, so the capture must \
         hold pid {escaped}: captured {:?} of root {root}",
        captured.pids()
    );
    assert!(
        process_exists(escaped)?,
        "the premise: the escapee is running and needs a pid signal to stop"
    );
    kill_process(escaped)?;
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_signal_to_one_captured_pid_stops_that_pid_and_leaves_its_siblings()
-> Result<(), Box<dyn std::error::Error>> {
    // The shell is the sleepers' parent, so it reaps the first when that dies
    // and says so: the death is a line to read, not an interval to wait out.
    let mut tree = Tree::start(
        "sleep 30 & first=$!; sleep 30 & second=$!; echo $first $second; wait $first; echo reaped; wait",
    )?;
    let named = tree
        .line()?
        .ok_or("the shell exited before it named its sleepers")?;
    let (first, sibling) = named
        .split_once(' ')
        .ok_or_else(|| format!("{named:?} does not name two pids"))?;
    let (first, sibling) = (first.parse::<i32>()?, sibling.parse::<i32>()?);
    let captured = capture_descendants(tree.root)?;
    assert!(
        captured.contains(first) && captured.contains(sibling),
        "the capture must hold both sleepers, {first} and {sibling}: {:?}",
        captured.pids()
    );
    kill_process(first)?;
    assert_eq!(
        tree.line()?.as_deref(),
        Some("reaped"),
        "the shell reaps pid {first} once the signal has stopped it"
    );
    assert!(
        running_processes(&[first])?.is_empty(),
        "the signalled pid {first} must have stopped running"
    );
    assert_eq!(
        running_processes(&[sibling])?
            .into_iter()
            .collect::<Vec<i32>>(),
        vec![sibling],
        "a per-process signal must reach exactly one process: pid {sibling} is untouched"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_capture_of_a_live_root_never_reports_a_truncated_tree_of_its_own_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let tree = Tree::grow(1)?;
    let captured = capture_descendants(tree.root)?;
    assert!(
        !captured.is_truncated(),
        "a one-descendant tree is inside every declared bound, so the flag must stay clear"
    );
    assert!(
        captured.pids().len() <= MAX_CAPTURED_DESCENDANTS,
        "a capture is bounded by MAX_CAPTURED_DESCENDANTS, got {}",
        captured.pids().len()
    );
    Ok(())
}

#[test]
fn absorbing_two_captures_unions_them_and_keeps_the_stronger_mechanism() {
    let mut first = DescendantSet {
        mechanism: ContainmentMechanism::ProcessTableSnapshot,
        pids: vec![30, 10],
        truncated: true,
    };
    let second = DescendantSet {
        mechanism: ContainmentMechanism::ProcChildrenTree,
        pids: vec![10, 20],
        truncated: false,
    };
    first.absorb(&second);
    assert_eq!(
        first.pids(),
        [10, 20, 30].as_slice(),
        "a merged capture is the sorted union of both"
    );
    assert_eq!(
        first.mechanism(),
        ContainmentMechanism::ProcChildrenTree,
        "the merged mechanism must not understate what was read"
    );
    assert!(
        first.is_truncated(),
        "a truncation in either half survives the merge"
    );
}

#[test]
fn a_merge_past_the_declared_ceiling_is_refused_and_reported()
-> Result<(), std::num::TryFromIntError> {
    let mut first = DescendantSet {
        mechanism: ContainmentMechanism::ProcChildrenTree,
        pids: (0..MAX_CAPTURED_DESCENDANTS)
            .map(i32::try_from)
            .collect::<Result<_, _>>()?,
        truncated: false,
    };
    let before = first.pids().len();
    first.absorb(&DescendantSet {
        mechanism: ContainmentMechanism::ProcChildrenTree,
        pids: vec![i32::MAX],
        truncated: false,
    });
    assert_eq!(
        first.pids().len(),
        before,
        "a capture at the ceiling must not grow past it"
    );
    assert!(
        first.is_truncated(),
        "a refused id must leave the set reported as a prefix, not as the whole tree"
    );
    Ok(())
}

/// A `stat` line names its process running unless it is a zombie or is
/// exiting. The command name may hold spaces and parentheses.
#[test]
#[cfg(target_os = "linux")]
fn a_stat_line_is_running_unless_it_is_a_zombie_or_exiting() {
    let line = |state: char, flags: u64| {
        format!(
            "4242 (a (tricky) name) {state} 1 4242 4242 0 -1 {flags} 0 0 0 0 1 2 0 0 20 0 1 0 77"
        )
    };
    assert!(proc_stat_says_running(&line('R', 0x0040_0000)));
    assert!(proc_stat_says_running(&line('S', 0x0040_0040)));
    assert!(
        !proc_stat_says_running(&line('Z', 0x0040_0000)),
        "a zombie has stopped"
    );
    assert!(
        !proc_stat_says_running(&line('R', 0x0040_0004)),
        "a process in do_exit never returns to user code"
    );
    assert!(
        proc_stat_says_running("4242 (truncated"),
        "an unparsable line understates the cleanup rather than overstating it"
    );
}

/// `SIGKILL` (signal 9) is bit 8 of either pending mask.
#[test]
#[cfg(target_os = "linux")]
fn a_pending_kill_is_read_from_either_pending_mask() {
    let status = |thread: &str, shared: &str| {
        format!(
            "Name:\tsh\nState:\tS (sleeping)\nSigPnd:\t{thread}\nShdPnd:\t{shared}\nSigBlk:\t0000000000000000\n"
        )
    };
    assert!(status_has_kill_pending(&status(
        "0000000000000100",
        "0000000000000000"
    )));
    assert!(status_has_kill_pending(&status(
        "0000000000000000",
        "0000000000000100"
    )));
    assert!(
        !status_has_kill_pending(&status("0000000000000000", "0000000000004000")),
        "SIGTERM pending is not a kill"
    );
    assert!(
        !status_has_kill_pending("Name:\tsh\n"),
        "no mask claims nothing"
    );
}

/// A process killed while stopped has the kill pending until it is resumed,
/// and is not running from the moment the kill is delivered.
#[test]
#[cfg(target_os = "linux")]
fn a_killed_process_is_not_running_even_before_it_is_scheduled()
-> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
    let pid = i32::try_from(child.id())?;
    assert_eq!(running_processes(&[pid])?.len(), 1, "the premise: it runs");
    kill_process(pid)?;
    assert!(
        running_processes(&[pid])?.is_empty(),
        "pid {pid} was sent SIGKILL: pending, exiting or a zombie, it runs no more user code"
    );
    let _reaped = child.wait();
    Ok(())
}

/// How many table reads [`all_stop_running`] makes before it gives up.
#[cfg(unix)]
const SETTLE_READS: usize = 100_000;

/// Whether every pid in `pids` stops running within [`SETTLE_READS`] reads.
///
/// The tree's pipe ends once the last process has closed its descriptors,
/// which is a few instructions before the table marks it a zombie, so this
/// reads the table until it agrees, yielding the processor between reads and
/// counting reads, not time. Running, not present: a killed process whose
/// parent has not reaped it yet is a zombie, which has stopped and is not a
/// survivor.
#[cfg(unix)]
fn all_stop_running(pids: &[i32]) -> Result<bool, Box<dyn std::error::Error>> {
    for _ in 0..SETTLE_READS {
        if running_processes(pids)?.is_empty() {
            return Ok(true);
        }
        std::thread::yield_now();
    }
    Ok(false)
}

#[test]
#[cfg(unix)]
fn a_live_process_reads_one_identity_that_round_trips_through_its_text()
-> Result<(), Box<dyn std::error::Error>> {
    let pid = i32::try_from(std::process::id())?;
    let first = identify_process(pid)?.ok_or("this process has no identity")?;
    let second = identify_process(pid)?.ok_or("this process lost its identity")?;
    assert_eq!(first, second, "one process read twice is one identity");
    assert_eq!(first.pid(), pid);
    let parsed: ProcessIdentity = first.to_string().parse()?;
    assert_eq!(parsed, first, "the stored text names the same process");
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_vacant_pid_has_no_identity_and_a_non_positive_one_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(identify_process(VACANT)?, None, "no process holds {VACANT}");
    for refused in [0, -1, i32::MIN] {
        assert_eq!(
            identify_process(refused).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "pid {refused} names no single process"
        );
    }
    Ok(())
}

#[test]
fn a_damaged_record_is_refused_by_the_arm_that_names_its_damage() {
    let long = format!("ps:{}", "x".repeat(MAX_START_TOKEN_BYTES));
    let cases: [(&str, ProcessIdentityError); 9] = [
        ("42", ProcessIdentityError::MissingSeparator),
        ("4x2/ps:Mon", ProcessIdentityError::MalformedPid),
        ("+42/ps:Mon", ProcessIdentityError::MalformedPid),
        ("042/ps:Mon", ProcessIdentityError::MalformedPid),
        ("0/ps:Mon", ProcessIdentityError::NonPositivePid { pid: 0 }),
        ("42/", ProcessIdentityError::EmptyStart),
        (
            "42/ps:Mon Oct",
            ProcessIdentityError::StartNotPrintable { at: 6 },
        ),
        ("42/when:Mon", ProcessIdentityError::UnknownScheme),
        ("", ProcessIdentityError::MissingSeparator),
    ];
    for (text, expected) in cases {
        assert_eq!(
            text.parse::<ProcessIdentity>(),
            Err(expected),
            "{text:?} is refused by its own arm"
        );
    }
    assert_eq!(
        ProcessIdentity::new(42, &long),
        Err(ProcessIdentityError::StartTooLong {
            len: MAX_START_TOKEN_BYTES.saturating_add(3)
        }),
        "a token one prefix past the bound is refused before it is kept"
    );
}

#[test]
#[cfg(unix)]
fn a_matching_leader_has_its_group_and_every_descendant_stopped()
-> Result<(), Box<dyn std::error::Error>> {
    let mut tree = Tree::grow(3)?;
    let leader = identify_process(tree.root)?.ok_or("the live leader has no identity")?;
    let OrphanReap::Signalled { descendants } = reap_orphaned_group(&leader)? else {
        return Err("a leader that still holds its pid must be signalled".into());
    };
    let mut expected = tree.descendants.clone();
    expected.sort_unstable();
    assert_eq!(
        descendants.pids(),
        expected.as_slice(),
        "the reap captures exactly the tree below the leader"
    );
    let status = tree.leader.wait()?;
    assert!(!status.success(), "the leader ends by the reap's signal");
    tree.wait_for_exit()?;
    assert!(
        all_stop_running(&expected)?,
        "every descendant of the reaped group stops running: {expected:?}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_forged_start_on_a_live_pid_is_never_signalled() -> Result<(), Box<dyn std::error::Error>> {
    let mut tree = Tree::grow(1)?;
    let real = identify_process(tree.root)?.ok_or("the live leader has no identity")?;
    let forged = ProcessIdentity::new(real.pid(), &format!("{}0", real.started()))?;
    assert_eq!(
        reap_orphaned_group(&forged)?,
        OrphanReap::LeaderReused { holder: real },
        "a pid whose start differs from the record is another process"
    );
    assert!(
        tree.leader.try_wait()?.is_none(),
        "the process holding the number was not signalled"
    );
    assert_eq!(
        running_processes(&tree.descendants)?
            .into_iter()
            .collect::<Vec<i32>>(),
        tree.descendants,
        "nor was anything below it"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_leader_that_is_gone_is_left_alone() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new("true")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = i32::try_from(child.id())?;
    let recorded = identify_process(pid)?.ok_or("the child has no identity")?;
    child.wait()?;
    let reaped = reap_orphaned_group(&recorded)?;
    assert!(
        !matches!(reaped, OrphanReap::Signalled { .. }),
        "a reaped leader's record signals nothing, got {reaped:?}"
    );
    Ok(())
}
