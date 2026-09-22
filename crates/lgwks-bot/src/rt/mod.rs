//! Async runtime: the async and runner surface of [`lgwks_bot`].
//!
//! This is the crate's async tier. It wraps tokio **core** behind a deliberate
//! surface and sources the engine from the [`lgwks_deps`] storefront
//! (`feature = "tokio"`), so `lgwks_deps` is the only crate that authors a
//! `tokio` edge (`INV-DEP-EDGE-OWNED`) and `lgwks_bot` is the crate a consumer
//! reaches async through.
//!
//! [`lgwks_bot`]: crate
//! [`lgwks_deps`]: lgwks_deps
//! [`runtime`]: crate::rt::runtime
//! [`task`]: crate::rt::task
//! [`time`]: crate::rt::time
//! [`sync`]: crate::rt::sync
//! [`Runtime`]: crate::rt::runtime::Runtime
//! [`Handle`]: crate::rt::runtime::Handle
//! [`block_on`]: crate::rt::runtime::block_on
//!
//! # What this is
//!
//! - [`runtime`] — an explicitly owned runtime ([`Runtime`]),
//!   its builder, a cloneable [`Handle`], and a free [`block_on`].
//! - [`task`] — [`JoinSet`](crate::rt::task::JoinSet) and
//!   [`join_all_bounded`](crate::rt::task::join_all_bounded): bounded-concurrency fan-out
//!   that preserves input order and never exceeds the limit.
//! - [`supervise`](crate::rt::supervise) — `Supervisor`: the way concurrent
//!   work, and a subprocess, is started. `Supervisor::default` is the ceiling
//!   when the caller has no opinion, `Supervisor::spawn` and
//!   `Supervisor::spawn_process` are the two starters, and nothing here returns
//!   a handle to a running task or process, so nothing can be started and then
//!   forgotten.
//! - [`time`] — `sleep`, `timeout`, `interval`, `Instant` (feature `time`).
//! - [`sync`] — `mpsc`, `oneshot`, `broadcast`, `watch`, `Mutex`, `RwLock`,
//!   `Semaphore`, `Notify`, `Barrier` (feature `sync`).
//! - `net` — async TCP/UDP and `lookup_host` (feature `net`).
//! - `process`, `fs`, `signal` — opt-in drivers.
//! - `select!`, `join!`, `try_join!` — tokio's `macro_rules!` combinators,
//!   re-exported at the crate root so a consumer never names `tokio`.
//!
//! # Entering async
//!
//! There is no attribute macro: a re-exported proc-macro expands to `::tokio`
//! paths a consumer without a `tokio` edge cannot resolve. Entry is explicit:
//!
//! ```
//! use lgwks_bot::Runtime;
//!
//! let runtime = Runtime::new().expect("runtime");
//! let answer = runtime.block_on(async { 2 + 2 });
//! assert_eq!(answer, 4);
//! ```
//!
//! # Starting work
//!
//! A future can be awaited, and the runtime can drive one to completion, but no
//! verb here hands a caller a droppable handle to a *running* task or process.
//! Work that outlives the call is placed on a
//! [`Supervisor`](crate::rt::supervise::Supervisor), which owns it, applies the
//! in-flight ceiling, and reports how each task or process ended:
//!
//! ```
//! use lgwks_bot::Runtime;
//! use lgwks_bot::rt::supervise::Supervisor;
//!
//! let runtime = Runtime::new().expect("runtime");
//! runtime.block_on(async {
//!     let mut supervisor = Supervisor::default();
//!     supervisor.spawn(|_token| async { /* one unit of work */ }).await;
//!     // Dropping it stops what it started; `shutdown().await` waits instead.
//! });
//! ```
//!
//! # Limits
//!
//! This is not a scheduler with realtime guarantees. Future completion order
//! across worker threads is not deterministic; only the *result* order of
//! [`join_all_bounded`](crate::rt::task::join_all_bounded) is.

pub mod runtime;
pub mod task;

/// This crate's own cancellation primitive. Private, because its only public
/// name is `rt::sync::CancellationToken` (one type, one path), matching how
/// the ECS substrate is reached through `spec` rather than named directly.
#[cfg(feature = "sync")]
mod cancel;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "io")]
pub mod io;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(all(feature = "signal", any(unix, windows)))]
pub mod signal;
#[cfg(feature = "sync")]
pub mod supervise;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "time")]
pub mod time;
