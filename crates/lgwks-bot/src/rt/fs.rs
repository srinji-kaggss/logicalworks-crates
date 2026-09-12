//! Async filesystem operations.
//!
//! These are not a real async file API: each call is dispatched to a blocking
//! thread, so a large or slow read still consumes a blocking thread. For a
//! dedicated blocking read, prefer `spawn_blocking` so the cost is explicit.

pub use lgwks_deps::tokio::fs::*;
