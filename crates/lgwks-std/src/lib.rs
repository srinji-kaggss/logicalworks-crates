//! Zero-config primitives that replace a dozen crates.
//!
//! Every module is a single-import, single-call primitive — hex, base64,
//! timestamps, UUIDs, hashing, glob matching, regex, JSON, async — backed by
//! a vetted dependency stack that bottoms out at two leaves.
//!
//! Each optional feature unlocks one capability with one audited stack. The
//! default build carries `core` **and `trace`**: the estate's PRINTS rule
//! forbids `println!` in library code and names `tracing` as the replacement,
//! and a rule that names an unavailable facility is not a rule. `--no-default-
//! features --features core` restores a genuinely zero-dependency build.
//!
//! ## Feature map
//!
//! - `core` (default) — encoding, fs, glob, hex, leb128, retry, task, time. Zero deps.
//! - `trace` (default) — trace. Adds `tracing` (`std` only; no `attributes`,
//!   so no `syn`).
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
//!
//! Lint contract: workspace `missing_docs` deny, `unsafe_code` forbid,
//! `broken_intra_doc_links` deny — see the workspace root `Cargo.toml`.
///
/// Single-import encoding primitives: base64 and percent-encoding.
pub mod encoding;
/// Filesystem traversal with sandbox enforcement.
pub mod fs;
/// Shell-style glob matching.
pub mod glob;
/// BLAKE3 content-addressable hashing (feature `hash`).
#[cfg(feature = "hash")]
pub mod hash;
/// Hex encode and decode.
pub mod hex;
/// Blocking HTTP exchange with strict URL validation (feature `http`).
#[cfg(feature = "http")]
pub mod http;
/// UUID generation and parsing (feature `random`).
#[cfg(feature = "random")]
pub mod id;
/// JSON encoding and decoding via serde (feature `json`).
#[cfg(feature = "json")]
pub mod json;
/// LEB128 variable-length integer encoding.
pub mod leb128;
/// TCP reachability probing, zero-dep (feature `online`).
#[cfg(feature = "online")]
pub mod online;
/// Compiled regex matching with a linear-time guarantee (feature `pattern`).
#[cfg(feature = "pattern")]
pub mod pattern;
/// Supervised subprocess termination (feature `process`, Unix-only).
#[cfg(feature = "process")]
pub mod process;
/// OS entropy (feature `random`).
#[cfg(feature = "random")]
pub mod random;
/// Retry budgets: attempts, backoff, deadlines. Zero-dep policy values.
pub mod retry;
/// RON encoding and decoding via serde (feature `ron`).
#[cfg(feature = "ron")]
pub mod ron;
/// Single-threaded executor: `block_on`, `join_all`, `spawn_blocking`.
pub mod task;
/// RFC 3339 timestamps and calendar math.
pub mod time;
/// Structured, levelled logging via `tracing` (feature `trace`, default-on).
#[cfg(feature = "trace")]
pub mod trace;
/// Zero-copy binary wire serialization via rkyv (feature `wire`).
#[cfg(feature = "wire")]
pub mod wire;
