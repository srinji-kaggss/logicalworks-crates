//! Explicit ownership of the async runtime.
//!
//! The estate does not hide a global reactor. A [`Runtime`] is constructed,
//! owned, and dropped by its caller; every task runs on a runtime the caller
//! can name. [`Handle`] is the cloneable capability to place work on a runtime
//! that is owned elsewhere.

use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::time::Duration;

use crate::rt::task::JoinHandle;

/// Maximum number of native worker threads accepted by [`Builder`].
pub const MAX_WORKER_THREADS: usize = 1024;

/// Configures and builds an owned [`Runtime`].
///
/// Native targets use Tokio's multi-thread scheduler. WASM targets use the
/// current-thread scheduler because WASM has no native worker-thread driver.
/// Native defaults are discovered, never hardcoded: worker count follows
/// [`std::thread::available_parallelism`] and the thread name is `lgwks-bot`.
/// Setting either makes the native choice explicit. Explicit worker counts
/// above [`MAX_WORKER_THREADS`] are rejected; a discovered count is capped at
/// that value to keep resource use bounded.
#[derive(Debug, Default)]
pub struct Builder {
    worker_threads: Option<NonZeroUsize>,
    thread_name: Option<String>,
}

impl Builder {
    /// A builder using native defaults; WASM uses a current-thread scheduler.
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
    /// on WASM, which has no worker thread.
    #[must_use]
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = Some(name.into());
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
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "lgwks_bot: worker_threads is unsupported on WASM",
            ));
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
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "lgwks_bot: worker_threads exceeds MAX_WORKER_THREADS",
                ));
            }
            builder.worker_threads(workers.get().min(MAX_WORKER_THREADS));
        }
        #[cfg(not(target_family = "wasm"))]
        builder.thread_name(self.thread_name.as_deref().unwrap_or("lgwks-bot"));
        builder.enable_all();
        builder.build().map(|inner| Runtime { inner })
    }
}

#[cfg(not(target_family = "wasm"))]
fn discover_workers() -> Option<NonZeroUsize> {
    std::thread::available_parallelism().ok()
}

/// An owned async runtime. Native targets use multiple worker threads; WASM
/// targets use a current-thread scheduler.
///
/// Dropping the runtime shuts it down and aborts async tasks that have not
/// finished. Started blocking tasks cannot be aborted by Tokio and may outlive
/// the runtime. [`Runtime::shutdown_timeout`] bounds the wait for the blocking
/// pool; it does not give async tasks a grace period. A task already executing
/// non-yielding code cannot be forcibly stopped, and a started blocking task
/// may continue on its blocking thread after shutdown returns.
pub struct Runtime {
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
    /// Tokio shuts down async workers as shutdown begins, dropping tasks that
    /// have not completed. A task already executing non-yielding code cannot be
    /// forcibly stopped. The timeout applies to the blocking pool; a started
    /// blocking task cannot be aborted by Tokio and may continue on its
    /// blocking thread after this method returns.
    pub fn shutdown_timeout(self, timeout: Duration) {
        self.inner.shutdown_timeout(timeout);
    }
}

/// A cloneable capability to run work on a [`Runtime`] owned elsewhere.
///
/// Cloning is cheap. A handle keeps no ownership of the runtime: it does not
/// keep the runtime alive. A [`Handle::spawn`] issued after the runtime has been
/// dropped does **not** panic; the task is never scheduled, and awaiting its
/// [`JoinHandle`] reports [`JoinError::is_cancelled`]. Retain the runtime for as
/// long as its handles are used when the work must run.
///
/// [`JoinError::is_cancelled`]: lgwks_deps::tokio::task::JoinError::is_cancelled
#[derive(Clone, Debug)]
pub struct Handle {
    inner: lgwks_deps::tokio::runtime::Handle,
}

impl Handle {
    /// Place a future on the runtime without waiting for it.
    ///
    /// If the owning runtime has already been dropped, the future is never
    /// scheduled and the returned handle resolves to a cancelled [`JoinError`]
    /// — this call does not panic.
    ///
    /// [`JoinError`]: crate::rt::task::JoinError
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.inner.spawn(future)
    }

    /// Drive one future to completion on the owning runtime, blocking the
    /// calling thread. Panics if called from within an async context.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.inner.block_on(future)
    }
}

/// Run one future to completion on a private current-thread runtime.
///
/// This is the convenience entry for a one-off async call from synchronous
/// code. It builds and tears down a runtime per call, so code that makes
/// repeated async calls should hold a [`Runtime`] instead. Panics if called
/// from within an async context, and panics if the OS refuses the runtime's
/// driver resources — a condition under which no async work could proceed.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut builder = lgwks_deps::tokio::runtime::Builder::new_current_thread();
    builder.enable_all();
    let runtime = builder
        .build()
        .expect("lgwks_bot::rt: the OS refused a current-thread runtime");
    runtime.block_on(future)
}
