//! T03: a non-yielding callback is detected by an *outside* process, and the
//! report says detected rather than preempted.
//!
//! # Why an in-process watchdog is not enough
//!
//! The existing T03 evidence — `tests/task_front_door.rs`'s
//! `dropping_a_suspended_run_releases_its_permit` — runs its watchdog as a thread
//! inside the same process as the executor. That proves the permit comes back, and
//! it is the right test for that claim. But it cannot answer the second half of
//! the row, because a thread shares the fate of the process it watches: if the
//! executor is wedged so hard that it never returns from a poll, the watchdog
//! thread is still scheduled only if the OS can schedule *anyone*, and the
//! evidence the test would collect — "did the watchdog fire?" — is itself
//! produced by the thing under test.
//!
//! So this file re-executes the test binary as a **separate process**. The child
//! runs a callback that never yields and never returns. The parent does not share
//! a scheduler, a stack, an allocator or an address space with it, so the only
//! thing that can end the child is an external `SIGKILL` the parent sends after
//! its own wall-clock deadline has passed.
//!
//! # What this proves, and what it does not
//!
//! It proves **detection**: a non-yielding callback is observable from outside,
//! and the observer is bounded by a clock the callback cannot reach or pause.
//!
//! It does **not** prove preemption, and the test says so in its own report rather
//! than letting a green run imply it. Nothing in this crate can preempt a poll in
//! flight — `Supervisor::shutdown` says so in its own documentation — and the
//! only thing that ended the child here was the parent killing a whole process.
//! The claim being made is exactly the one a process boundary supports.
//!
//! # The two-branch shape
//!
//! One test, two branches, selected by an environment variable, in the pattern
//! `tests/sim_deep_cancel.rs` uses: with the variable set the body *is* the
//! non-yielding child, so the same binary that holds the oracle also holds the
//! subject and there is no second program to keep in step.

#![cfg(feature = "script")]

use std::error::Error;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, task};

/// What a test reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// Set in the child so a re-executed binary takes the child branch.
const CHILD_ENV: &str = "LGWKS_BOT_T03_NON_YIELDING_CHILD";

/// This test's own name, for the child's `--exact` filter.
const TEST_NAME: &str = "a_non_yielding_callback_is_detected_from_outside_and_not_preempted";

/// How long the parent lets the child run before deciding it never yielded.
///
/// Short enough that a wedged executor is caught quickly, long enough that a
/// child that *does* return is never mistaken for a wedged one. The child writes
/// its marker before entering the callback, so the parent's wait is on the
/// subject having started, not on it having finished.
const DETECT_AFTER: Duration = Duration::from_secs(2);

/// How long the parent waits for the child to reach its marker.
const START_BUDGET: Duration = Duration::from_secs(10);

/// How often the parent looks at the child while it waits.
const POLL: Duration = Duration::from_millis(10);

/// A guard whose two counters are the child's evidence about its own body.
///
/// The same device `tests/task_front_door.rs` uses for its in-process proof,
/// with a second counter because the two facts are different: `entered` is set
/// when the body *starts*, so it distinguishes "the body ran" from "a future was
/// constructed and admitted"; `ended` is set when the body is *dropped*, which is
/// the only thing that would reclaim it. A wedged run must show the first and
/// never the second.
struct BodyGuard {
    /// Counts the body's entry.
    entered: std::rc::Rc<std::cell::Cell<usize>>,
    /// Counts the body's drop.
    ended: std::rc::Rc<std::cell::Cell<usize>>,
}

impl BodyGuard {
    /// Record that the body reached its first instruction.
    fn enter(&self) {
        self.entered.set(self.entered.get().saturating_add(1));
    }
}

impl Drop for BodyGuard {
    /// Record that the body ended without reaching its end.
    fn drop(&mut self) {
        self.ended.set(self.ended.get().saturating_add(1));
    }
}

/// A callback that occupies its poll and never gives it back.
///
/// Deliberately **not** `std::future::pending()`. A pending future suspends and
/// therefore yields the thread, which is exactly what cooperative scheduling is
/// built to reclaim — the case this crate already handles. What the row asks
/// about is the other case: work that never reaches an await point, so no amount
/// of yielding, cancelling or aborting inside this process can reclaim it.
///
/// `black_box` keeps the loop from being optimized into a single no-op branch,
/// which would leave the thread parked rather than busy and would let the parent
/// mistake a yielding child for a wedged one.
fn never_yields() -> ! {
    let mut spin: u64 = 0;
    loop {
        spin = spin.wrapping_add(1);
        std::hint::black_box(spin);
    }
}

/// The child branch: run a callback that never yields and never returns.
///
/// It writes its marker first, so the parent's "did it start" and "did it finish"
/// are two separate facts. Then it enters a loop that neither awaits nor returns.
/// It is *not* `std::future::pending()`, because that is a future that suspends —
/// and a suspended future is exactly what cooperative scheduling is built to
/// reclaim. What the row asks about is a callback that occupies its poll, and
/// only a real spin reproduces that: the thread never reaches an await point, so
/// nothing in this process can take the executor back.
fn run_child_branch() -> TestResult {
    let marker = std::env::var_os("LGWKS_BOT_T03_MARKER")
        .ok_or("the child branch needs LGWKS_BOT_T03_MARKER to tell the parent it started")?;
    let ended = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let entered = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let body_ended = std::rc::Rc::clone(&ended);
    let body_entered = std::rc::Rc::clone(&entered);

    let host = Host::builder("t03")?
        .max_concurrent_tasks(NonZeroUsize::new(1).ok_or("a ceiling of at least one")?)
        .build()?;
    let wedged = task("non-yielding", move |_scope: Scope, ()| {
        // `Fn`, not `FnOnce`, because that is the bound `Host::run` places on a
        // task body — the same one `tests/task_front_door.rs`'s T03 body meets.
        // Both counters are therefore cloned per call rather than moved out of
        // the closure's environment.
        let guard = BodyGuard {
            entered: std::rc::Rc::clone(&body_entered),
            ended: std::rc::Rc::clone(&body_ended),
        };
        async move {
            // `entered` is written *inside* the body, so it distinguishes "the
            // body ran" from "a future was constructed and admitted". The
            // parent cannot read it — this process is about to be killed — so it
            // is checked on the one path that proves the test measured nothing:
            // a run that resolved before wedging.
            guard.enter();
            let _guard = guard;
            // A helper rather than an inline loop, so the body is a call to a
            // function that never returns. `!` coerces to every output type, so
            // the run's output is still `()` and no unreachable tail is needed to
            // give the compiler a return.
            never_yields()
        }
    })?;

    // Driven through the crate's own runtime, because `Host::run` reads the
    // engine's clock and expects a reactor — a bare poll with a no-op waker
    // panics inside `rt::time`. The runtime's work still happens *on this
    // thread*, so the body occupies the thread that runs it; having a reactor
    // around it does not make the body reclaimable.
    let runtime = lgwks_bot::Runtime::new()?;
    // The marker is written before the run is driven, so the parent's "did it
    // start" is established by this process rather than inferred from a poll that
    // may never have been reached.
    // Its presence is the whole signal: the parent waits for the file, never
    // reads an identity out of it.
    std::fs::write(&marker, "started")?;

    // This call never returns: the body never reaches an await point, so the
    // runtime never regains its thread. Reaching the end of it would mean the
    // callback was not wedged, and the parent asserts that it is killed instead.
    let _report = runtime.block_on(host.run::<_, lgwks_bot::task::Report<()>, _, _>(&wedged, ()));
    Err(format!(
        "the non-yielding run resolved, so the body returned or was reclaimed in process \
         (entered={}, dropped={})",
        entered.get(),
        ended.get()
    )
    .into())
}

/// The parent branch: spawn the child, time it from outside, and kill it.
///
/// Four facts are asserted, and they are deliberately separate:
///
/// 1. the child reached its marker, so the callback really started;
/// 2. the child did **not** exit on its own within the budget, so the callback
///    really did not return;
/// 3. an external `SIGKILL` ended it, so the ending was imposed from outside
///    rather than being the callback's own return;
/// 4. an unrelated sibling process, started by this test, was never signalled.
///
/// The fourth is what separates "we killed a wedged process" from "we killed a
/// process": a signal aimed too broadly fails it.
#[test]
fn a_non_yielding_callback_is_detected_from_outside_and_not_preempted() -> TestResult {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child_branch();
    }

    let runtime = lgwks_bot::Runtime::new()?;
    let scratch = child_scratch("t03-non-yielding")?;
    let marker = scratch.join("started");
    let sibling_marker = scratch.join("sibling.pid");

    // A sibling of the child's own: unrelated to the child's executor, alive
    // throughout, and never a member of anything the parent signals.
    let mut sibling = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("echo $$ > {}; sleep 30", sibling_marker.display()))
        .spawn()?;
    let sibling_pid = read_pid(&sibling_marker);

    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([TEST_NAME, "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("LGWKS_BOT_T03_MARKER", &marker)
        .spawn()?;

    // Wait for the child to say it started, bounded.
    let started = wait_for(&marker, START_BUDGET);
    let exited_early = child.try_wait()?;

    assert!(
        started,
        "the child never reported that its non-yielding callback started, so nothing was \
         under observation (child status: {exited_early:?})"
    );
    assert!(
        exited_early.is_none(),
        "the child exited before it was killed: {exited_early:?}. A callback that never \
         yields must not return on its own, or this row is measuring nothing."
    );

    // The outside clock. Nothing in the child can pause it, reach it, or be
    // scheduled around it, which is the whole reason it is a separate process.
    std::thread::park_timeout(DETECT_AFTER);
    let still_running = child.try_wait()?;
    assert!(
        still_running.is_none(),
        "the child ended on its own after {DETECT_AFTER:?}, so the callback yielded or \
         returned: {still_running:?}. A non-yielding callback has no such moment."
    );

    // Detection, then termination, both from outside.
    child.kill()?;
    let status = child.wait()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            status.signal(),
            Some(9),
            "the child must have been killed by an external SIGKILL, not have exited: {status}. \
             This test detects a non-yielding callback; it does not preempt one."
        );
    }

    // The counterweight: an unrelated process must be untouched. A signal aimed
    // more broadly than the wedged child fails here.
    if let Some(sibling_pid) = sibling_pid {
        assert!(
            pid_is_alive(sibling_pid),
            "the sibling process {sibling_pid} was signalled while cleaning up a wedged child"
        );
    }
    let _reaped = sibling.kill().and_then(|()| sibling.wait()).map(|_| ());

    // The load-bearing statement of scope. This test killed a process; it did not
    // stop a poll, and nothing in this crate can. A future reader who mistakes
    // this green run for a preemption guarantee is reading more than it proves.
    let _ = runtime;
    Ok(())
}

/// A scratch directory named by wall-clock nanos and a monotone sequence.
///
/// A process id would not do: the OS reuses it, so two runs of this test could
/// read each other's marker, and one run's marker would then be this run's proof
/// that a child had started when none had.
fn child_scratch(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("lgwks-bot-{name}-{nanos}-{seq}"));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Wait until `path` exists and is non-empty, up to `budget`.
fn wait_for(path: &std::path::Path, budget: Duration) -> bool {
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .unwrap_or_else(std::time::Instant::now);
    loop {
        if path.metadata().is_ok_and(|meta| meta.len() > 0) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return path.metadata().is_ok_and(|meta| meta.len() > 0);
        }
        std::thread::park_timeout(POLL);
    }
}

/// Read a pid a sibling wrote, if it is there yet.
fn read_pid(path: &std::path::Path) -> Option<i32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse::<i32>().ok()
}

/// Whether `pid` is still running, asked through `kill -0`, which sends nothing.
fn pid_is_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|status| status.success())
}

/// The control the child branch needs: a host admits and reports an ordinary run.
///
/// Without it, "the child was killed" would be consistent with a child that never
/// started its body at all. This asserts the same front door admits and completes
/// a body that *does* yield, so the wedged run is a property of the callback and
/// not of the harness.
#[test]
fn the_same_front_door_admits_a_callback_that_does_yield() -> TestResult {
    if std::env::var_os(CHILD_ENV).is_some() {
        return Ok(());
    }
    let host = Host::builder("t03-control")?
        .max_concurrent_tasks(NonZeroUsize::new(1).ok_or("a ceiling of at least one")?)
        .build()?;
    let yielding = task("yielding", |_scope: Scope, ()| async {
        lgwks_bot::rt::task::yield_now().await;
        Ok::<(), FlowError>(())
    })?;
    let report = lgwks_bot::block_on(host.run(&yielding, ()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the front door admits and completes a yielding body, so the wedged child is a \
         property of its callback: {:?}",
        report.error()
    );
    Ok(())
}
