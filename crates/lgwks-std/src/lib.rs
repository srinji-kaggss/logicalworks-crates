#![doc = include_str!("lexicon.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! Zero-config primitives that replace a dozen crates.
//!
//! Every module is a single-import, single-call primitive (hex, base64,
//! timestamps, UUIDs, hashing, glob matching, regex, JSON, async), backed by
//! a vetted dependency stack that bottoms out at two leaves.
//!
//! The supported interface is `script!` (`lgwks_bot::script!`); the modules of
//! this crate are the support code under it. Every module is `#[doc(hidden)]`,
//! still reachable by the sibling crates and by this crate's own tests, and kept
//! out of the documented surface by #526. The feature map below names what each
//! feature unlocks.
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
//! - `core` (default) — encoding, fs, glob, hex, leb128, retry, seeded,
//!   similarity, task, time. Zero deps.
//! - `trace` (default) — trace. Adds `tracing` plus the default subscriber
//!   bootstrap (`std` only; no `attributes`, so no `syn`).
//! - `random` — random, id. Adds `getrandom`.
//! - `hash` — hash. Adds `blake3`.
//! - `build` — build. A build script's input digest and cargo directives.
//!   Implies `hash` and `trace`; adds no dependency of its own.
//! - `sign` — sign. Adds `ed25519-dalek` (default-features off, `alloc` only).
//!   Implies `random` (key generation reads the one entropy source) and `hash`
//!   (the sealed message carries the BLAKE3 of the record).
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
//!   supervisor that still owes signals to it). Unix adds `rustix/process`;
//!   Windows reaches `kernel32` directly (`process::ProcessJob` is its job
//!   object) and adds no dependency.
//! - `full` — all of the above.
//!
//! Lint contract: workspace `missing_docs` deny, `unsafe_code` forbid,
//! `broken_intra_doc_links` deny; see the workspace root `Cargo.toml`.
/// The crate under its published name inside its own unit tests, so a test
/// fixture shared with the integration tests (`tests/support/`) names one path
/// (`lgwks_std::…`) in both.
#[cfg(test)]
extern crate self as lgwks_std;

/// A build script's input digest and checked cargo directives (feature `build`).
#[cfg(feature = "build")]
#[doc(hidden)]
pub mod build;
///
/// Single-import encoding primitives: base64 and percent-encoding.
#[doc(hidden)]
pub mod encoding;
#[doc(hidden)]
pub mod fs;
/// Shell-style glob matching.
#[doc(hidden)]
pub mod glob;
/// BLAKE3 content-addressable hashing (feature `hash`).
#[cfg(feature = "hash")]
#[doc(hidden)]
pub mod hash;
/// Hex encode and decode.
#[doc(hidden)]
pub mod hex;
/// Blocking HTTP exchange with strict URL validation (feature `http`).
#[cfg(feature = "http")]
#[doc(hidden)]
pub mod http;
/// UUID generation and parsing (feature `random`).
#[cfg(feature = "random")]
#[doc(hidden)]
pub mod id;
/// JSON encoding and decoding via serde (feature `json`).
#[cfg(feature = "json")]
#[doc(hidden)]
pub mod json;
/// LEB128 variable-length integer encoding.
#[doc(hidden)]
pub mod leb128;
/// TCP reachability probing, zero-dep (feature `online`).
#[cfg(feature = "online")]
#[doc(hidden)]
pub mod online;
/// Compiled regex matching with a linear-time guarantee (feature `pattern`).
#[cfg(feature = "pattern")]
#[doc(hidden)]
pub mod pattern;
/// Supervised subprocess termination (feature `process`, Unix and Windows).
#[cfg(feature = "process")]
#[doc(hidden)]
pub mod process;
/// OS entropy (feature `random`).
#[cfg(feature = "random")]
#[doc(hidden)]
pub mod random;
/// Retry budgets: attempts, backoff, deadlines. Zero-dep policy values.
#[doc(hidden)]
pub mod retry;
/// RON encoding and decoding via serde (feature `ron`).
#[cfg(feature = "ron")]
#[doc(hidden)]
pub mod ron;
/// The estate's one deterministic stream for simulations: a seed names one
/// sequence on every target. Zero-dep; not a source of randomness.
#[doc(hidden)]
pub mod seeded;
/// The borrowing contract the serde-backed codec facades share, asserted once
/// for `json` and `ron` (test builds only).
#[cfg(all(test, any(feature = "json", feature = "ron")))]
mod serde_facade;
/// Detached ed25519 seals over domain-separated records (feature `sign`).
#[cfg(feature = "sign")]
#[doc(hidden)]
pub mod sign;
/// Pure, replaceable similarity metrics and weighted composition.
#[doc(hidden)]
pub mod similarity;
/// Single-threaded executor: `block_on`, `join_all`, `spawn_blocking`.
#[doc(hidden)]
pub mod task;
/// RFC 3339 timestamps and calendar math.
#[doc(hidden)]
pub mod time;
/// Structured, levelled logging and default debugger bootstrap (feature
/// `trace`, default-on).
#[cfg(feature = "trace")]
#[doc(hidden)]
pub mod trace;
/// Zero-copy binary wire serialization via rkyv (feature `wire`).
#[cfg(feature = "wire")]
#[doc(hidden)]
pub mod wire;
