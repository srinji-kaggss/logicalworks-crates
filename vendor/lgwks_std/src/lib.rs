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
//! - `random` — random, id. Adds `getrandom`.
//! - `hash` — hash. Adds `blake3`.
//! - `pattern` — pattern. Adds `regex`.
//! - `json` — json. Adds `serde`, `serde_json`.
//! - `ron` — ron. Adds `serde`, `ron`.
//! - `wire` — wire. Adds `rkyv`.
//! - `full` — all of the above.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
pub mod encoding;
pub mod fs;
pub mod glob;
#[cfg(feature = "hash")]
pub mod hash;
pub mod hex;
#[cfg(feature = "random")]
pub mod id;
#[cfg(feature = "json")]
pub mod json;
pub mod leb128;
#[cfg(feature = "pattern")]
pub mod pattern;
#[cfg(feature = "random")]
pub mod random;
#[cfg(feature = "ron")]
pub mod ron;
pub mod task;
pub mod time;
#[cfg(feature = "wire")]
pub mod wire;
