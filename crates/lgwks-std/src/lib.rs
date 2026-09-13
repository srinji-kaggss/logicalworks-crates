//! Zero-config primitives that replace a dozen crates.
//!
//! Every module is a single-import, single-call primitive — hex, base64,
//! timestamps, UUIDs, hashing, glob matching, regex, JSON, async — backed by
//! a vetted dependency stack that bottoms out at zero external deps.
//!
//! The default `core` feature compiles with **zero external dependencies**.
//! Each optional feature unlocks one capability with one audited stack.
//!
//! ## Feature map
//!
//! - `core` (default) — encoding, fs, glob, hex, leb128, task, time. Zero deps.
//! - `error` — error. Adds `thiserror`.
//! - `random` — random, id. Adds `getrandom`.
//! - `hash` — hash. Adds `blake3`.
//! - `pattern` — pattern. Adds `regex`.
//! - `json` — json. Adds `serde`, `serde_json`.
//! - `ron` — ron. Adds `serde`, `ron`.
//! - `wire` — wire. Adds `rkyv`.
//! - `http` — http. Adds `ureq` (rustls-only TLS), `iri-string`.
//! - `online` — online. Zero deps.
//! - `fs-raw` — fs::available_space. Adds `rustix` (Unix-only).
//! - `process` — process::kill_process_group. Adds `rustix/process` (Unix-only).
//! - `full` — all of the above.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
pub mod encoding;
#[cfg(feature = "error")]
pub mod error;
pub mod fs;
pub mod glob;
#[cfg(feature = "hash")]
pub mod hash;
pub mod hex;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "random")]
pub mod id;
#[cfg(feature = "json")]
pub mod json;
pub mod leb128;
#[cfg(feature = "online")]
pub mod online;
#[cfg(feature = "pattern")]
pub mod pattern;
#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "random")]
pub mod random;
#[cfg(feature = "ron")]
pub mod ron;
pub mod task;
pub mod time;
#[cfg(feature = "wire")]
pub mod wire;

/// Root re-export required by the `thiserror` derive's absolute expansion path.
///
/// `#[derive(Error)]` expands to `::thiserror::__private<N>::…`, an absolute
/// path resolved in the *consuming* crate. A consumer that has no `thiserror`
/// Cargo edge of its own makes that path resolve here by naming this crate
/// `thiserror`:
///
/// ```
/// extern crate lgwks_std as thiserror;
///
/// #[derive(thiserror::Error, Debug)]
/// pub enum StoreError {
///     #[error("value length {actual} exceeds maximum {maximum}")]
///     Length { actual: usize, maximum: usize },
/// }
/// ```
///
/// The glob is what carries the version-suffixed `__private<N>` module, so it
/// is deliberately a glob rather than a named re-export.
#[cfg(feature = "error")]
#[doc(hidden)]
pub use thiserror::*;
