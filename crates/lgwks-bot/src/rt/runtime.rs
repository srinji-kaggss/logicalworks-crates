//! The orchestration runtime: one process driver, and runtimes a caller owns.
//!
//! ADR 0006 decision 2: a process holds one tokio runtime, owned by the driver.
//! [`driver`] builds it on first use and hands out its [`Handle`]; the host's
//! `block_on`, the estate's supervised tiers and the free [`block_on`] all run
//! on it, so none of them builds a runtime of its own. Every multi-thread
//! runtime is its own worker pool, I/O driver and timer wheel, and several of
//! them in one process compete for the same cores. The driver lives for the
//! process: dropping a host or an estate cancels and aborts that owner's work,
//! and leaves the driver to the others.
//!
//! A caller that wants a runtime of its own, with its own shutdown, still builds
//! a [`Runtime`] and owns it; [`Handle`] is the cloneable capability to place
//! work on a runtime owned elsewhere.

use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Maximum number of native worker threads accepted by [`Builder`].
pub const MAX_WORKER_THREADS: usize = 1024;

/// The largest worker stack [`Builder::thread_stack_size`] accepts: 256 MiB.
///
/// Stacks are reserved address space per worker, so the ceiling is a multiple of
/// worker count's bound, not a limit on how deep any one call may recurse.
pub const MAX_THREAD_STACK_SIZE: usize = 256 * 1024 * 1024;

/// The OS thread name [`Builder`] gives its native workers until a caller names
/// its own.
///
/// A thread's name is how it is identified in a stack trace and in a profiler,
/// and an unnamed worker in a process that also runs other libraries' threads is
/// unattributable. The default is a field value rather than a substitution
/// [`Builder::build`] makes for an absent setting, so the name is decided once at
/// construction and every later read sees the same string.
pub(crate) const DEFAULT_THREAD_NAME: &str = "lgwks-bot";

/// Configures and builds the owned orchestration [`Runtime`].
///
/// Native targets drive a multi-thread scheduler through the crate-owned
/// engine. WASM targets use the
/// current-thread scheduler because WASM has no native worker-thread driver.
/// Native defaults are discovered, never hardcoded: worker count follows
/// [`std::thread::available_parallelism`] and the thread name is
/// `"lgwks-bot"`. Setting either makes the native choice explicit.
/// Explicit worker counts above [`MAX_WORKER_THREADS`] are rejected; a discovered
/// count is capped at that value to keep resource use bounded.
#[derive(Debug)]
pub struct Builder {
    /// Explicit native worker count, or `None` to discover it. `NonZeroUsize`
    /// because a runtime with no worker cannot make progress; `Option` because
    /// discovery, not a hardcoded constant, is the default.
    worker_threads: Option<NonZeroUsize>,
    /// The complete OS thread name for native workers, `"lgwks-bot"`
    /// until [`Builder::thread_name`] sets one. Ignored on WASM, which has no
    /// worker thread.
    thread_name: String,
    /// Ceiling on the blocking pool used by the `fs` driver, or `None` for the
    /// engine's default. `NonZeroUsize` because the engine refuses zero.
    max_blocking_threads: Option<NonZeroUsize>,
    /// Stack size in bytes for native workers, or `None` for the engine's default
    /// of 2 MiB. Ignored on WASM.
    thread_stack_size: Option<NonZeroUsize>,
}

impl Default for Builder {
    /// A builder with every knob at its declared default: the discovered worker
    /// count, `"lgwks-bot"`, and the driver's own blocking and stack
    /// defaults. Written out rather than derived because `#[derive(Default)]`
    /// would leave `thread_name` empty, and an empty OS thread name is not a
    /// default a stack trace can be read from.
    fn default() -> Self {
        Self {
            worker_threads: None,
            thread_name: String::from(DEFAULT_THREAD_NAME),
            max_blocking_threads: None,
            thread_stack_size: None,
        }
    }
}

impl Builder {
    /// A builder using native defaults; WASM uses a current-thread scheduler.
    ///
    /// `a_builder_names_its_workers_and_defaults_to_this_crates_name` (`tests/it/rt_front_door.rs`)
    /// holds it: workers are named `lgwks-bot` until a caller says otherwise, so a thread in a stack
    /// trace is attributable with no configuration, and a caller that wants its own name replaces it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fix the number of worker threads on native targets, or pass `None` for
    /// the discovered default. On WASM, a supplied worker count is rejected
    /// because its runtime is necessarily current-thread. The value is a
    /// [`NonZeroUsize`] because a runtime with zero worker threads cannot make
    /// progress.
    #[must_use]
    pub fn worker_threads(mut self, workers: Option<NonZeroUsize>) -> Self {
        self.worker_threads = workers;
        self
    }

    /// Set the complete OS thread name for native worker threads. Has no effect
    /// on WASM, which has no worker thread. The name replaces
    /// `"lgwks-bot"`; there is no way back to an absent one, because
    /// an unnamed native worker is not a state this crate is willing to build.
    #[must_use]
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = name.into();
        self
    }

    /// Cap the blocking pool used by the `fs` driver. Pass `None` for the
    /// driver's default (512). The value is a [`NonZeroUsize`] because the driver refuses
    /// zero (`assert!(val > 0)`), so an unbounded or zero pool cannot be
    /// expressed by accident; callers that fan out blocking work pair this with
    /// [`crate::rt::task::join_all_bounded`] to keep both async and blocking
    /// concurrency explicit.
    ///
    /// A blocking *call* is made with
    /// [`lgwks_std::task::spawn_blocking`],
    /// which owns its own OS thread per call and reports its result through a
    /// future. This knob bounds the runtime's own pool — the one the `fs`
    /// driver borrows — not those threads.
    #[must_use]
    pub fn max_blocking_threads(mut self, threads: Option<NonZeroUsize>) -> Self {
        self.max_blocking_threads = threads;
        self
    }

    /// Give each native worker thread a stack of `bytes`, or pass `None` for the
    /// driver's default of 2 MiB. Has no effect on WASM.
    ///
    /// A future's own state lives in the task, but every synchronous call a poll
    /// makes uses the worker's stack, and a chain of bounded but deep calls can
    /// exceed the default; the process then aborts with a stack overflow rather
    /// than returning an error. Sizes above [`MAX_THREAD_STACK_SIZE`] are refused
    /// when the runtime is built, as [`Builder::worker_threads`] counts above its
    /// ceiling are, so the knob cannot reserve unbounded address space per worker.
    #[must_use]
    pub fn thread_stack_size(mut self, bytes: Option<NonZeroUsize>) -> Self {
        self.thread_stack_size = bytes;
        self
    }

    /// Build the runtime.
    ///
    /// This allocates native worker threads immediately; a failure is the OS
    /// refusing a thread, and it is returned rather than panicked so a caller
    /// can degrade instead of aborting. WASM builds a current-thread runtime.
    pub fn build(self) -> io::Result<Runtime> {
        #[cfg(target_family = "wasm")]
        if self.worker_threads.is_some() {
            let refusal = Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "lgwks_bot: worker_threads is unsupported on WASM",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
            return refusal;
        }

        #[cfg(not(target_family = "wasm"))]
        let workers = self.worker_threads.or_else(discover_workers);
        #[cfg(not(target_family = "wasm"))]
        let mut builder = lgwks_deps::tokio::runtime::Builder::new_multi_thread();
        #[cfg(target_family = "wasm")]
        let mut builder = lgwks_deps::tokio::runtime::Builder::new_current_thread();
        #[cfg(not(target_family = "wasm"))]
        if let Some(workers) = workers {
            if self.worker_threads.is_some() && workers.get() > MAX_WORKER_THREADS {
                let refusal = Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "lgwks_bot: worker_threads exceeds MAX_WORKER_THREADS",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                return refusal;
            }
            builder.worker_threads(workers.get().min(MAX_WORKER_THREADS));
        }
        #[cfg(not(target_family = "wasm"))]
        builder.thread_name(&self.thread_name);
        if let Some(max_blocking) = self.max_blocking_threads {
            builder.max_blocking_threads(max_blocking.get());
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(bytes) = self.thread_stack_size {
            if bytes.get() > MAX_THREAD_STACK_SIZE {
                let refusal = Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "lgwks_bot: thread_stack_size exceeds MAX_THREAD_STACK_SIZE",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build: returning an error to the caller");
                return refusal;
            }
            builder.thread_stack_size(bytes.get());
        }
        builder.enable_all();
        #[cfg(test)]
        BUILT.fetch_add(1, Ordering::Relaxed);
        builder.build().map(|inner| Runtime { inner })
    }
}

/// Runtimes [`Builder::build`] has made in this test process, so a test can
/// count them (`one_process_holds_one_driver`).
#[cfg(test)]
pub(crate) static BUILT: AtomicUsize = AtomicUsize::new(0);

/// The process driver, built on first use by [`driver`] and never dropped.
static DRIVER: Mutex<Option<Runtime>> = Mutex::new(None);

/// The handle of the one process driver, building it on first use.
///
/// Built with [`Builder`]'s defaults: the discovered worker count, named
/// `lgwks-bot`. The lock is held only while the slot is read or filled, never
/// while work runs, and a poisoned lock is recovered because a [`Runtime`] has
/// no half-built state a panic could leave behind.
///
/// # Errors
///
/// The OS refusing the driver's worker threads or I/O resources. Nothing is
/// stored then, so the next call tries again.
pub(crate) fn driver() -> io::Result<Handle> {
    let mut slot = match DRIVER.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(runtime) = slot.as_ref() {
        return Ok(runtime.handle());
    }
    let runtime = Builder::new().build()?;
    let handle = runtime.handle();
    *slot = Some(runtime);
    Ok(handle)
}

/// The native worker count discovered from the OS, or `None` when the platform
/// cannot report it.
///
/// Discovery rather than a constant: a hardcoded worker count is either too
/// small for the machine it lands on or wastes threads on a small one, and the
/// engine's own default would be a second, invisible policy. A caller that
/// wants a fixed count sets [`Builder::worker_threads`] explicitly.
///
/// Native-only: WASM has no worker-thread driver, so the current-thread
/// scheduler is used unconditionally there and this is not compiled.
#[cfg(not(target_family = "wasm"))]
fn discover_workers() -> Option<NonZeroUsize> {
    std::thread::available_parallelism().ok()
}

/// An owned orchestration runtime. Native targets use multiple worker threads; WASM
/// targets use a current-thread scheduler.
///
/// Dropping the runtime shuts it down and aborts async tasks that have not
/// finished. Started blocking tasks cannot be aborted by the driver and may outlive
/// the runtime. [`Runtime::shutdown_timeout`] bounds the wait for the blocking
/// pool; it does not give async tasks a grace period. A task already executing
/// non-yielding code cannot be forcibly stopped, and a started blocking task
/// may continue on its blocking thread after shutdown returns.
#[derive(Debug)]
pub struct Runtime {
    /// The owned driver runtime. Private so the driver type never appears in
    /// this crate's public surface: a consumer names [`Runtime`], never
    /// `lgwks_deps::tokio::runtime::Runtime`.
    inner: lgwks_deps::tokio::runtime::Runtime,
}

impl Runtime {
    /// Build a runtime with the discovered native worker count, or a
    /// current-thread runtime on WASM.
    pub fn new() -> io::Result<Self> {
        Builder::new().build()
    }

    /// A cloneable handle that can place work on this runtime from any thread.
    #[must_use]
    pub fn handle(&self) -> Handle {
        Handle {
            inner: self.inner.handle().clone(),
        }
    }

    /// Drive one future to completion, blocking the calling thread.
    ///
    /// Panics if called from within an async context (a future already running
    /// on this or another runtime). Blocking inside an async task would stall a
    /// worker thread; the caller must `await` instead.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.inner.block_on(future)
    }

    /// Wait up to `timeout` for shutdown to finish.
    ///
    /// Shutdown stops the async workers as it begins, dropping tasks that
    /// have not completed. A task already executing non-yielding code cannot be
    /// forcibly stopped. The timeout applies to the blocking pool; a started
    /// blocking task cannot be aborted by the driver and may continue on its
    /// blocking thread after this method returns.
    pub fn shutdown_timeout(self, timeout: Duration) {
        self.inner.shutdown_timeout(timeout);
    }
}

/// A cloneable capability to run work on a [`Runtime`] owned elsewhere.
///
/// Cloning is cheap. A handle keeps no ownership of the runtime: it does not
/// keep the runtime alive. What a handle can do is **drive** work —
/// [`Handle::block_on`] runs one future to completion on the owning runtime —
/// and it deliberately cannot *start* work: a `spawn` here would hand back a
/// droppable handle to a running task, which is the shape this crate removes
/// everywhere (see [`crate::rt::task`]). Work that outlives the call belongs in
/// a [`Supervisor`](crate::rt::supervise::Supervisor), which owns it and
/// reports how it ended.
#[derive(Clone, Debug)]
pub struct Handle {
    /// The driver's cloneable handle. Private for the same reason as
    /// [`Runtime::inner`]: the driver type stays behind the orchestration
    /// surface. It holds no
    /// ownership of the runtime, which is why a handle outliving its runtime
    /// cannot keep the runtime alive.
    inner: lgwks_deps::tokio::runtime::Handle,
}

impl Handle {
    /// Drive one future to completion on the owning runtime, blocking the
    /// calling thread. Panics if called from within an async context.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.inner.block_on(future)
    }
}

/// Whether a runtime is already driving the calling thread.
///
/// The one place this crate asks the driver that question. The synchronous
/// entry points — `EcsBot::tick` and `Host::block_on` — refuse to start a
/// second driver on a thread a runtime already drives, because blocking that
/// thread would stall every task the outer runtime has parked on it. Both read
/// the answer here, so the driver is named once rather than at every refusal.
#[cfg(any(feature = "ecs", feature = "script"))]
pub(crate) fn inside_runtime() -> bool {
    lgwks_deps::tokio::runtime::Handle::try_current().is_ok()
}

#[cfg(feature = "ecs")]
impl crate::ecs::EcsBot {
    /// Whether the driver already drives the calling thread, asked by
    /// `EcsBot::tick` before it parks the thread. Answered here because the
    /// world is core and names no driver (ADR 0006 decision 4).
    pub(crate) fn driver_on_this_thread() -> bool {
        inside_runtime()
    }
}

/// Run one future to completion on the process [`driver`], blocking the calling
/// thread.
///
/// This is the one-off entry for synchronous code that has no runtime to
/// name; it builds nothing after the driver's first use. The future itself is
/// polled on the calling thread, so a borrowed or non-`Send` one-off drives;
/// what it spawns runs on the driver's workers. Panics if called from within an
/// async context, and panics if the OS refuses the driver's resources, a
/// condition under which no async work could proceed.
pub fn block_on<F: Future>(future: F) -> F::Output {
    match driver() {
        Ok(handle) => handle.block_on(future),
        // The contract is that this reports on the calling thread rather than
        // returning an error, and there is no error channel in `F::Output` to
        // report through. `resume_unwind` is the crate's form for a documented,
        // unavoidable panic (`lgwks_std::task::JoinHandle` uses it for the same
        // reason): the caller is a synchronous frame with nowhere to propagate
        // to, and the alternative (hanging on a future no driver can poll) is
        // strictly worse.
        Err(error) => std::panic::resume_unwind(Box::new(format!(
            "lgwks_bot::rt: the OS refused the process driver: {error}"
        ))),
    }
}

/// ADR 0006 R2 falsifier: a process that builds an estate, runs a host and blocks on a future
/// holds exactly one runtime.
#[cfg(all(test, feature = "script"))]
mod one_driver_tests {
    use std::sync::atomic::Ordering;

    use super::{BUILT, block_on, driver};
    use crate::rt::estate::Estate;
    use crate::task::task;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn one_process_holds_one_driver() -> TestResult {
        let before = BUILT.load(Ordering::Relaxed);
        let probe = task("one-driver", |_scope, input: u64| async move {
            Ok(input.saturating_add(1))
        })?;
        let mut supervised = Estate::supervised("acme", 4)?;
        let in_process = Estate::in_process("globex")?;
        let first = supervised.run_blocking(&probe, 1)?;
        let second = in_process.run_blocking(&probe, 2)?;
        let third = in_process.host().block_on(&probe, 3)?;
        let fourth = block_on(async { 4_u64 });
        let handle = supervised
            .handle()
            .ok_or("a supervised estate carries the driver")?;
        let fifth = handle.block_on(async { 5_u64 });
        let drained = supervised.shutdown_blocking()?;
        let after_shutdown = driver()?.block_on(async { 6_u64 });
        assert_eq!(
            (first.output(), second.output(), third.output()),
            (Some(&2), Some(&3), Some(&4)),
            "every door ran its run"
        );
        assert_eq!((fourth, fifth, after_shutdown), (4, 5, 6));
        assert_eq!(drained.runs_admitted(), 1, "the estate counted its own run");
        // At most one: a runner that shares the process across tests may have built the
        // driver before this test began, and then this one builds none.
        assert!(
            BUILT.load(Ordering::Relaxed).saturating_sub(before) <= 1,
            "two estates, a host and two block_on calls built one runtime"
        );
        Ok(())
    }
}

#[cfg(all(test, feature = "script"))]
#[path = "sim_one_driver.rs"]
mod sim_one_driver;
