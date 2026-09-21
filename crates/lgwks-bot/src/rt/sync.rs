//! Async synchronization and channels.
//!
//! Channels are the composed-message boundary between agent tasks; the async
//! locks are what a shared handle is guarded by. `std::sync::Mutex` already
//! covers a lock never held across an `await`; use these only when the guard
//! must cross an await point.

pub use lgwks_deps::tokio::sync::{Barrier, Mutex, Notify, OnceCell, RwLock, Semaphore};
pub use lgwks_deps::tokio::sync::{broadcast, mpsc, oneshot, watch};

// The estate's own, not the engine's. `tokio_util::sync::CancellationToken` is
// the crate this could have come from, but that would add a third-party edge to
// the storefront for a single type the engine's `watch` already expresses. It is
// defined in `rt::cancel` and named only here, so `rt::sync::CancellationToken`
// is the one path to it.
pub use super::cancel::{CancellationToken, DropGuard};
