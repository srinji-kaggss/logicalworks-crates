//! Zero-config primitives that replace a dozen crates.
//!
//! Every module is a single-import, single-call primitive (hex, base64,
//! timestamps, UUIDs, hashing, glob matching, regex, JSON, async), backed by
//! a vetted dependency stack that bottoms out at two leaves.
//!
//! Each optional feature unlocks one capability with one audited stack. The
//! default build carries `core` **and `trace`**: this workspace forbids
//! `println!` in library code and names `tracing` as the replacement, so a
//! build without `trace` would leave that rule with nothing to point at.
//! `--no-default-features --features core` restores a genuinely
//! zero-dependency build. (Kept on one line on purpose: rustdoc joins these
//! lines but not a word broken across them, so a wrapped flag renders as a
//! flag nobody can paste.)
//!
//! ## Feature map
//!
//! - `core` (default) — encoding, fs, glob, hex, leb128, retry, similarity,
//!   task, time. Zero deps.
//! - `trace` (default) — trace. Adds `tracing` plus the default subscriber
//!   bootstrap (`std` only; no `attributes`, so no `syn`).
//! - `random` — random, id. Adds `getrandom`.
//! - `hash` — hash. Adds `blake3`.
//! - `pattern` — pattern. Adds `regex`.
//! - `json` — json. Adds `serde`, `serde_json`.
//! - `ron` — ron. Adds `serde`, `ron`.
//! - `wire` — wire. Adds `rkyv`.
//! - `http` — http. Adds `ureq` (rustls-only TLS), `iri-string`.
//! - `online` — online. Zero deps.
//! - `fs-raw` — `fs::available_space`, and `fs::capability::Dir`: handle-relative
//!   access (`openat`/`statat`/`unlinkat`/`mkdirat`/`readlinkat`) for trees that
//!   are being rewritten while you walk them. Adds `rustix` (Unix-only).
//! - `process` — the two supervised-subprocess primitives:
//!   `process::kill_process_group` (signal a whole group) and
//!   `process::child_has_exited_without_reaping` (observe an exit *without*
//!   releasing the pid, which is what stops the OS reissuing a group id under a
//!   supervisor that still owes signals to it). Adds `rustix/process`
//!   (Unix-only).
//! - `full` — all of the above.
//!
//! Lint contract: workspace `missing_docs` deny, `unsafe_code` forbid,
//! `broken_intra_doc_links` deny; see the workspace root `Cargo.toml`.
///
/// Single-import encoding primitives: base64 and percent-encoding.
pub mod encoding;
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
/// The borrowing contract the serde-backed codec facades share, asserted once
/// for `json` and `ron` (test builds only).
#[cfg(all(test, any(feature = "json", feature = "ron")))]
mod serde_facade;
/// Pure, replaceable similarity metrics and weighted composition.
pub mod similarity;
/// Single-threaded executor: `block_on`, `join_all`, `spawn_blocking`.
pub mod task;
/// RFC 3339 timestamps and calendar math.
pub mod time;
/// Structured, levelled logging and default debugger bootstrap (feature
/// `trace`, default-on).
#[cfg(feature = "trace")]
pub mod trace;
/// Zero-copy binary wire serialization via rkyv (feature `wire`).
#[cfg(feature = "wire")]
pub mod wire;
