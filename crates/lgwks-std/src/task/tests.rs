//! Tests of `lgwks_std::task` itself. Waking the executor across threads needs
//! a real second thread, so the tests start one with `thread::scope` or a named
//! `thread::Builder` whose handle the test joins. No test waits on wall time:
//! every "let the other side get there first" is a channel hand-off or an
//! observed state, so a loaded host changes how long a test takes and never
//! what it proves.

use super::*;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};

#[test]
fn immediate_future_returns_value() {
    let result = block_on(async { 42 });
    assert_eq!(result, 42);
}

#[test]
fn yields_and_resumes() {
    async fn step() -> String {
        let first_part = async { "hello" }.await;
        let second_part = async { "world" }.await;
        format!("{first_part} {second_part}")
    }

    assert_eq!(block_on(step()), "hello world");
}

/// Resolves once another thread hands it a value through the shared slot.
///
/// The first poll finds no value, registers its waker, and only then tells the
/// other thread it has done so; the value therefore always arrives after the
/// pending poll and is delivered by the waker, never found on the first poll.
struct DeferredValue {
    value: Arc<Mutex<Option<i32>>>,
    waker_slot: Arc<Mutex<Option<Waker>>>,
    /// Told once the waker is registered, which is what the sender waits on.
    registered: mpsc::SyncSender<()>,
}

impl Future for DeferredValue {
    type Output = i32;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(value) = lock(&self.value).take() {
            return Poll::Ready(value);
        }
        *lock(&self.waker_slot) = Some(cx.waker().clone());
        // A full channel means an earlier poll already announced itself; the
        // sender needs one announcement, not one per poll, so a refused send
        // has nothing left to do.
        let _announced = self.registered.try_send(());
        Poll::Pending
    }
}

#[test]
fn threaded_waker_unparks() {
    let value: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    let waker_slot: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
    let sender_value = Arc::clone(&value);
    let sender_slot = Arc::clone(&waker_slot);
    let (registered, on_registered) = mpsc::sync_channel(1);

    // Scoped, so the handing-off thread is joined before the test returns
    // instead of outliving it.
    thread::scope(|scope| {
        scope.spawn(move || {
            // The hand-off is load-bearing: the value is set only after
            // `block_on` has polled once and is waiting, so the wake below is
            // what resumes the future rather than a value found on the opening
            // poll. A closed channel means the future never pended.
            if on_registered.recv().is_err() {
                return;
            }
            *lock(&sender_value) = Some(100);
            if let Some(waker) = lock(&sender_slot).take() {
                waker.wake();
            }
        });

        let result = block_on(DeferredValue {
            value,
            waker_slot,
            registered,
        });
        assert_eq!(result, 100);
    });
}

#[test]
fn join_all_empty_resolves_immediately() {
    let output: Vec<u8> = block_on(join_all(std::iter::empty::<std::future::Ready<u8>>()));
    assert!(output.is_empty());
}

#[test]
fn join_all_preserves_input_order_across_completion_order() {
    // The jobs finish in the order 2, 3, 1 by construction: the third waits for
    // the second to finish, and the first for the third. Input order must
    // still be what comes back.
    let (second_done, after_second) = mpsc::sync_channel::<()>(1);
    let (third_done, after_third) = mpsc::sync_channel::<()>(1);
    let output = block_on(join_all(vec![
        spawn_blocking(move || match after_third.recv() {
            Ok(()) => 1u32,
            Err(_) => 0,
        }),
        spawn_blocking(move || match second_done.send(()) {
            Ok(()) => 2u32,
            Err(_) => 0,
        }),
        spawn_blocking(move || match after_second.recv() {
            Ok(()) => match third_done.send(()) {
                Ok(()) => 3u32,
                Err(_) => 0,
            },
            Err(_) => 0,
        }),
    ]));
    assert_eq!(output, vec![1, 2, 3]);
}

#[test]
fn join_all_runs_children_concurrently() {
    // Both jobs block on a shared barrier: the pair can only complete if
    // the two dedicated threads are alive at the same time.
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            spawn_blocking(move || {
                barrier.wait();
                7u32
            })
        })
        .collect();
    let output = block_on(join_all(handles));
    assert_eq!(output, vec![7, 7]);
}

struct CountPolls {
    polls: Arc<AtomicUsize>,
}

impl Future for CountPolls {
    type Output = usize;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<usize> {
        // `fetch_add` yields the count before this poll, so the 1-based
        // poll ordinal is one past it. Saturating rather than `+`: the
        // counter is bounded by the test's poll count, far below
        // `usize::MAX`.
        let n = self
            .polls
            .fetch_add(1, AtomicOrdering::SeqCst)
            .saturating_add(1);
        Poll::Ready(n)
    }
}

/// Pends on first poll, wakes the group from another thread, then resolves.
struct PendingThenReady {
    polls: Arc<AtomicUsize>,
    waker_slot: Arc<Mutex<Option<Waker>>>,
    /// The waker thread, owned here so the test joins it rather than
    /// dropping a handle on the floor.
    waker_thread: Option<thread::JoinHandle<()>>,
    /// Why no waker thread could be started, for the test to report: with
    /// no thread there is no wake, so the future would never resolve and the
    /// test would hang instead of naming the cause.
    spawn_refusal: Option<io::Error>,
}

impl Future for PendingThenReady {
    type Output = usize;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        // See `CountPolls::poll`: the ordinal is `fetch_add`'s prior value
        // plus one, saturating against a bound the test cannot reach.
        let n = self
            .polls
            .fetch_add(1, AtomicOrdering::SeqCst)
            .saturating_add(1);
        if n > 1 {
            return Poll::Ready(n);
        }
        let this = self.get_mut();
        // Registered before the thread starts, so the thread always finds it:
        // a wake that lands before the group waits is kept by the executor's
        // flag, so no pause is needed to order the two.
        *lock(&this.waker_slot) = Some(cx.waker().clone());
        let slot = Arc::clone(&this.waker_slot);
        match thread::Builder::new()
            .name("pending-then-ready-waker".to_owned())
            .spawn(move || {
                if let Some(waker) = lock(&slot).take() {
                    waker.wake();
                }
            }) {
            Ok(joined) => this.waker_thread = Some(joined),
            Err(error) => this.spawn_refusal = Some(error),
        }
        Poll::Pending
    }
}

#[test]
fn join_all_never_repolls_a_completed_future() {
    let fast_polls = Arc::new(AtomicUsize::new(0));
    let slow_polls = Arc::new(AtomicUsize::new(0));
    let fast: Pin<Box<dyn Future<Output = usize>>> = Box::pin(CountPolls {
        polls: Arc::clone(&fast_polls),
    });
    let mut slow = PendingThenReady {
        polls: Arc::clone(&slow_polls),
        waker_slot: Arc::new(Mutex::new(None)),
        waker_thread: None,
        spawn_refusal: None,
    };
    let output = block_on(join_all(vec![fast, Box::pin(&mut slow)]));
    assert_eq!(output, vec![1, 2]);
    // The waker thread belongs to this test: joined here, never detached.
    let Some(joined) = slow.waker_thread.take() else {
        assert!(
            slow.spawn_refusal.is_none(),
            "the waker thread must start: {:?}",
            slow.spawn_refusal
        );
        return;
    };
    // A panicking waker thread is re-raised here rather than counted as a
    // joined thread, so its own message is the failure report.
    match joined.join() {
        Ok(()) => {}
        Err(panic) => std::panic::resume_unwind(panic),
    }
    // The fast child resolved on its first poll. When the slow child wakes
    // the group, only the incomplete set may be polled again, so the fast
    // child's count must stay at one.
    assert_eq!(fast_polls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(slow_polls.load(AtomicOrdering::SeqCst), 2);
}

#[test]
fn join_all_boxed_matches_join_all_and_accepts_boxes() {
    let plain = block_on(join_all(vec![std::future::ready(1), std::future::ready(2)]));
    let boxed: Vec<Pin<Box<dyn Future<Output = i32>>>> = vec![
        Box::pin(std::future::ready(1)),
        Box::pin(std::future::ready(2)),
    ];
    assert_eq!(plain, vec![1, 2]);
    assert_eq!(block_on(join_all_boxed(boxed)), vec![1, 2]);

    let empty: Vec<Pin<Box<dyn Future<Output = i32>>>> = Vec::new();
    assert_eq!(block_on(join_all_boxed(empty)), Vec::<i32>::new());
}

/// Wakers parked on one latch, released together when it opens.
#[derive(Default)]
struct Latch {
    open: AtomicBool,
    parked: Mutex<Vec<Waker>>,
}

/// Pends until the latch opens, counting every poll.
struct Waiter {
    latch: Arc<Latch>,
    polls: Arc<AtomicUsize>,
}

impl Future for Waiter {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.polls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.latch.open.load(AtomicOrdering::SeqCst) {
            return Poll::Ready(());
        }
        lock(&self.latch.parked).push(cx.waker().clone());
        Poll::Pending
    }
}

/// Wakes itself `wakes` times, then opens the latch and resolves.
struct Busy {
    wakes: usize,
    latch: Arc<Latch>,
    polls: Arc<AtomicUsize>,
}

impl Future for Busy {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.polls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.wakes == 0 {
            self.latch.open.store(true, AtomicOrdering::SeqCst);
            for waker in lock(&self.latch.parked).drain(..) {
                waker.wake();
            }
            return Poll::Ready(());
        }
        self.wakes = self.wakes.saturating_sub(1);
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

#[test]
fn a_wake_polls_only_the_child_that_woke() {
    // One busy child wakes itself `wakes` times while `waiters` children
    // sit silent, then releases them. Polling only the woken child costs
    // `wakes + 1` busy polls plus two per waiter. A group that re-polls
    // every pending child on every wake costs about `waiters · wakes`,
    // which is what this count refuses.
    for (waiters, wakes) in [
        (1usize, 0usize),
        (10, 3),
        (1_000, 50),
        (10_000, 20),
        (100_000, 5),
    ] {
        let latch = Arc::new(Latch::default());
        let polls = Arc::new(AtomicUsize::new(0));
        // The busy child goes last, so every waiter has parked before it
        // first runs, whatever `wakes` is.
        let mut children: Vec<Pin<Box<dyn Future<Output = ()>>>> = (0..waiters)
            .map(|_| -> Pin<Box<dyn Future<Output = ()>>> {
                Box::pin(Waiter {
                    latch: Arc::clone(&latch),
                    polls: Arc::clone(&polls),
                })
            })
            .collect();
        children.push(Box::pin(Busy {
            wakes,
            latch: Arc::clone(&latch),
            polls: Arc::clone(&polls),
        }));
        let output = block_on(join_all_boxed(children));
        assert_eq!(output.len(), waiters.saturating_add(1));
        assert_eq!(
            polls.load(AtomicOrdering::SeqCst),
            wakes
                .saturating_add(1)
                .saturating_add(waiters.saturating_mul(2)),
            "{waiters} waiters beside a child waking {wakes} times"
        );
    }
}

/// A gate every job waits on, and the peak number of jobs inside it.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: std::sync::Condvar,
    entered: std::sync::Condvar,
    inside: AtomicUsize,
    peak: AtomicUsize,
}

impl Gate {
    fn pass(&self) {
        let now = self
            .inside
            .fetch_add(1, AtomicOrdering::SeqCst)
            .saturating_add(1);
        self.peak.fetch_max(now, AtomicOrdering::SeqCst);
        let mut open = lock(&self.open);
        // Announced under the lock the waiter checks `inside` under, so an
        // entry between its check and its wait is never missed.
        self.entered.notify_all();
        while !*open {
            open = wait(&self.opened, open);
        }
        drop(open);
        self.inside.fetch_sub(1, AtomicOrdering::SeqCst);
    }

    /// Waits until `floor` jobs are inside the gate at once, or `budget`
    /// has passed, and returns how many are inside. The wait ends on the
    /// condition, so a slow host costs wall time and never the verdict.
    fn await_inside(&self, floor: usize, budget: Duration) -> usize {
        let started = Instant::now();
        let mut open = lock(&self.open);
        loop {
            let inside = self.inside.load(AtomicOrdering::SeqCst);
            let left = budget.saturating_sub(started.elapsed());
            if inside >= floor || left.is_zero() {
                return inside;
            }
            open = wait_timeout(&self.entered, open, left).0;
        }
    }

    fn release(&self) {
        *lock(&self.open) = true;
        self.opened.notify_all();
    }
}

#[test]
fn no_more_than_the_ceiling_run_at_once_and_every_job_completes() -> Result<(), SpawnError> {
    for jobs in [100usize, 1_000, 10_000] {
        let gate = Arc::new(Gate::default());
        let mut handles = Vec::with_capacity(jobs);
        for index in 0..jobs {
            let gate = Arc::clone(&gate);
            handles.push(try_spawn_blocking(move || {
                gate.pass();
                index
            })?);
        }
        // The pool must reach `floor` jobs at once before the gate opens.
        // A fixed pause asserted the host's thread-start speed instead: at
        // load 145 only 42 threads had started inside 200 ms.
        let floor = jobs.min(MAX_BLOCKING_THREADS).min(64);
        gate.await_inside(floor, Duration::from_secs(60));
        gate.release();
        let output = block_on(join_all(handles));
        assert_eq!(output, (0..jobs).collect::<Vec<_>>(), "{jobs} jobs");
        let peak = gate.peak.load(AtomicOrdering::SeqCst);
        assert!(
            peak <= MAX_BLOCKING_THREADS,
            "{jobs} jobs: {peak} ran at once, over the ceiling"
        );
        assert!(
            peak >= floor,
            "{jobs} jobs: only {peak} ran at once; the pool is not running in parallel"
        );
    }
    Ok(())
}

#[test]
fn past_the_queue_bound_a_job_is_refused_with_a_typed_reason() {
    const OFFERED: usize = 100_000;
    let gate = Arc::new(Gate::default());
    let mut accepted = Vec::new();
    let mut refused = 0usize;
    for _ in 0..OFFERED {
        let gate = Arc::clone(&gate);
        match try_spawn_blocking(move || gate.pass()) {
            Ok(handle) => accepted.push(handle),
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        SpawnError::AtCapacity {
                            threads: MAX_BLOCKING_THREADS,
                            queued: MAX_QUEUED_BLOCKING_JOBS,
                        }
                    ),
                    "refused for the wrong reason: {error}"
                );
                refused = refused.saturating_add(1);
            }
        }
    }
    gate.release();
    let ceiling = MAX_QUEUED_BLOCKING_JOBS.saturating_add(MAX_BLOCKING_THREADS);
    assert!(
        (MAX_QUEUED_BLOCKING_JOBS..=ceiling).contains(&accepted.len()),
        "{} accepted, outside [{MAX_QUEUED_BLOCKING_JOBS}, {ceiling}]",
        accepted.len()
    );
    assert_eq!(accepted.len().saturating_add(refused), OFFERED);
    let completed = block_on(join_all(accepted)).len();
    assert!(
        completed >= MAX_QUEUED_BLOCKING_JOBS,
        "every accepted job ran"
    );
    assert!(gate.peak.load(AtomicOrdering::SeqCst) <= MAX_BLOCKING_THREADS);
}

#[test]
fn a_panicking_job_does_not_cost_the_pool_its_thread() {
    // One thread's worth of panics, then work: the pool must still answer,
    // because the job's panic is caught inside the job, not the thread.
    for _ in 0..4 {
        let handle = spawn_blocking(|| -> u32 {
            assert_eq!(1, 2, "deliberate");
            0
        });
        let caught = std::panic::catch_unwind(AssertUnwindSafe(|| block_on(handle)));
        assert!(caught.is_err(), "the panic reaches the awaiter");
    }
    assert_eq!(block_on(spawn_blocking(|| 5u32)), 5);
}

/// A starter that never starts a thread, as when the OS is out of them.
fn refuse_to_start(_pool: Arc<Pool>, _first: Handoff) -> io::Result<ThreadHandle> {
    Err(io::Error::new(
        io::ErrorKind::OutOfMemory,
        "injected: no thread",
    ))
}

#[test]
fn a_thread_that_cannot_start_refuses_the_job_and_it_never_runs() {
    fn refusing() -> &'static Arc<Pool> {
        static OWN: OnceLock<Arc<Pool>> = OnceLock::new();
        OWN.get_or_init(|| Arc::new(Pool::new(4, refuse_to_start)))
    }
    let pool = refusing();
    let ran = Arc::new(AtomicBool::new(false));
    let witness = Arc::clone(&ran);
    let (_handle, work) = prepare(move || witness.store(true, AtomicOrdering::SeqCst));
    // Not `unreachable!`: a refusal of another shape is a finding, not a
    // proof that this test is wrong about its own input, so the report says
    // which shape arrived instead of asserting that none could.
    let observed = match pool.submit(work, Some(8)) {
        Err(SpawnError::Os(error)) => {
            format!("{:?}: {error}", error.kind())
        }
        other => format!("a refusal of another shape: {other:?}"),
    };
    assert_eq!(
        observed, "OutOfMemory: injected: no thread",
        "a thread that cannot start refuses the job, and the OS's own error is the source"
    );
    let state = lock(&pool.state);
    assert!(
        state.queue.is_empty() && state.live == 0,
        "a refused job leaves nothing queued and no thread counted"
    );
    drop(state);
    assert!(
        !ran.load(AtomicOrdering::SeqCst),
        "a refused job never runs"
    );
}

#[test]
fn spawn_blocking_reports_a_refusal_to_its_awaiter_as_the_job_failing() {
    fn refusing() -> &'static Arc<Pool> {
        static OWN: OnceLock<Arc<Pool>> = OnceLock::new();
        OWN.get_or_init(|| Arc::new(Pool::new(4, refuse_to_start)))
    }
    let pool = refusing();
    let handle = spawn_blocking_on(pool, || 7u32);
    let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| block_on(handle)));
    let payload = unwound
        .err()
        .map(|payload| payload.downcast::<SpawnError>());
    assert!(
        matches!(payload, Some(Ok(ref error)) if matches!(**error, SpawnError::Os(_))),
        "the awaiter unwinds with the SpawnError itself"
    );
}

#[test]
fn a_failed_start_beside_a_live_thread_leaves_the_job_to_that_thread() {
    /// Starts the first thread, then refuses every later one.
    fn first_only(pool: Arc<Pool>, first: Handoff) -> io::Result<ThreadHandle> {
        static STARTS: AtomicUsize = AtomicUsize::new(0);
        if STARTS.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            start_os_thread(pool, first)
        } else {
            refuse_to_start(pool, first)
        }
    }
    fn single() -> &'static Arc<Pool> {
        static OWN: OnceLock<Arc<Pool>> = OnceLock::new();
        OWN.get_or_init(|| Arc::new(Pool::new(4, first_only)))
    }
    let pool = single();
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    let first = spawn_blocking_on(pool, move || {
        held.pass();
        thread::current().id()
    });
    let second = spawn_blocking_on(pool, || thread::current().id());
    assert_eq!(lock(&pool.state).live, 1, "the second start was refused");
    gate.release();
    let (first, second) = (block_on(first), block_on(second));
    assert_eq!(
        first, second,
        "the live thread ran the job its sibling could not start"
    );
}

#[test]
fn spawn_blocking_returns_value() {
    assert_eq!(block_on(spawn_blocking(|| 99u32)), 99);
}

/// A configure after the pool has run work names the ceiling that stands.
#[test]
fn a_configure_after_the_pool_has_run_is_refused_with_the_running_ceiling() {
    let pool = Arc::new(Pool::new(4, start_os_thread));
    assert_eq!(block_on(spawn_blocking_on(&pool, || 1u32)), 1);
    assert_eq!(
        configure_decision(&pool, 8),
        Err(PoolConfigError::InUse {
            requested: 8,
            running: 4
        }),
        "a configure after first use is refused, never silently ignored"
    );
    // Even the same number is `InUse`: this pool ran before it was asked
    // about, and the answer reports how its ceiling really came about.
    assert_eq!(
        configure_decision(&pool, 4),
        Err(PoolConfigError::InUse {
            requested: 4,
            running: 4
        })
    );
}

/// A configured pool answers a second configure by name: the same ceiling
/// is the one in force, a different one is refused with both numbers.
#[test]
fn a_second_configure_is_refused_named_or_is_the_ceiling_already_in_force() {
    let pool = Arc::new(Pool::at_configured(6, refuse_to_start));
    assert_eq!(
        configure_decision(&pool, 6),
        Ok(()),
        "asking again for the ceiling already in force describes the pool as it is"
    );
    assert_eq!(
        configure_decision(&pool, 9),
        Err(PoolConfigError::AlreadyConfigured {
            requested: 9,
            configured: 6
        })
    );
}

/// A ceiling below one is refused before the pool is touched, so the arm
/// is decided by the argument alone.
#[test]
fn a_ceiling_below_one_is_refused_before_the_pool_is_touched() {
    assert_eq!(
        configure_blocking_pool(0),
        Err(PoolConfigError::InvalidCeiling {
            requested: 0,
            minimum: 1
        })
    );
}

/// After a shutdown both entry points refuse, the job never runs, and the
/// never-refusing one fails its awaiter with the typed refusal as the
/// payload. The drain, deadline and parked-thread arms are the seeded
/// journeys in `sim_pool_lifetime`.
#[test]
fn after_a_shutdown_admission_is_refused_and_the_job_never_runs() {
    let pool = Arc::new(Pool::new(2, start_os_thread));
    let warm = spawn_blocking_on(&pool, || 3u32);
    assert_eq!(block_on(warm), 3);
    assert_eq!(
        pool.shutdown(Duration::from_secs(10)),
        PoolShutdown::Drained { threads: 1 }
    );

    let ran = Arc::new(AtomicBool::new(false));
    let witness = Arc::clone(&ran);
    // The bounded arm, through the pool's own submit: a refusal it hands
    // back leaves the caller no handle, so the refused one is dropped
    // rather than awaited.
    let (refused, work) = prepare(move || witness.store(true, AtomicOrdering::SeqCst));
    assert!(
        matches!(pool.submit(work, Some(8)), Err(SpawnError::Shutdown)),
        "the bounded entry point refuses with the typed reason"
    );
    drop(refused);

    // The never-refusing arm, through the entry point that hands a handle
    // back whatever happened: the refusal is the job's failure.
    let handle = spawn_blocking_on(&pool, || 11u32);
    let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| block_on(handle)));
    let payload = unwound
        .err()
        .map(|payload| payload.downcast::<SpawnError>());
    assert!(
        matches!(payload, Some(Ok(ref error)) if matches!(**error, SpawnError::Shutdown)),
        "the never-refusing entry point fails its awaiter with the refusal"
    );
    assert!(!ran.load(AtomicOrdering::SeqCst), "a refused job never ran");
}

#[test]
fn spawn_blocking_result_ready_before_first_poll() {
    let handle = spawn_blocking(|| 1234u32);
    // Observed, not waited out: the first poll below happens only once the
    // job has recorded its value, which is the state this test is about.
    while !handle.is_done() {
        thread::yield_now();
    }
    assert_eq!(block_on(handle), 1234);
}

#[test]
#[should_panic(expected = "worker exploded")]
fn spawn_blocking_panic_resumes_on_joiner() {
    // The job's panic is the subject under test: it must resume on the
    // joiner rather than vanish with the worker thread. The message rides
    // on a deliberately-false comparison because `clippy::panic` is
    // forbidden workspace-wide with no test carve-out.
    let _: u32 = block_on(spawn_blocking(|| -> u32 {
        assert_eq!(1, 2, "worker exploded");
        0u32
    }));
}
