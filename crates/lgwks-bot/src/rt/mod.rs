//! Estate async runtime — the async and runner surface of [`lgwks_bot`].
//!
//! This is the estate's async tier. It wraps tokio **core** behind a deliberate
//! surface and sources the engine from the [`lgwks_deps`] storefront
//! (`feature = "tokio"`), so `lgwks_deps` is the only crate that authors a
//! `tokio` edge (`INV-DEP-EDGE-OWNED`) and `lgwks_bot` is the crate a consumer
//! reaches async through.
//!
//! [`lgwks_bot`]: crate
//! [`lgwks_deps`]: lgwks_deps
//!
//! # What this is
//!
//! - [`runtime`] — an explicitly owned runtime ([`Runtime`]),
//!   its builder, a cloneable [`Handle`], and a free [`block_on`].
//! - [`task`] — [`spawn`](task::spawn), `JoinHandle`, `JoinSet`, abort, and
//!   [`join_all_bounded`](task::join_all_bounded): bounded-concurrency fan-out
//!   that preserves input order and never exceeds the limit.
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
//! # Limits
//!
//! This is not a scheduler with realtime guarantees. Future completion order
//! across worker threads is not deterministic; only the *result* order of
//! [`join_all_bounded`](task::join_all_bounded) is.

pub mod runtime;
pub mod task;

#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(all(feature = "signal", any(unix, windows)))]
pub mod signal;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "time")]
pub mod time;

pub use runtime::{Builder, Handle, Runtime, block_on};
