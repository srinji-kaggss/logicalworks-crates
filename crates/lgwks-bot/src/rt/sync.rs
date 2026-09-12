//! Async synchronization and channels.
//!
//! Channels are the composed-message boundary between agent tasks; the async
//! locks are what a shared handle is guarded by. `std::sync::Mutex` already
//! covers a lock never held across an `await`; use these only when the guard
//! must cross an await point.

pub use lgwks_deps::tokio::sync::{Barrier, Mutex, Notify, OnceCell, RwLock, Semaphore};
pub use lgwks_deps::tokio::sync::{broadcast, mpsc, oneshot, watch};
