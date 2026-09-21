//! Structured, levelled logging (feature `trace`, default-on).
//!
//! This module exists because this workspace forbids `println!`/`eprintln!` in
//! library code and names `tracing` as the replacement, but `tracing` was
//! absent from every crate. A library author following that rule had nothing to
//! use, and one ignoring it had `println!`; the rule was enforceable against
//! the wrong practice and unenforceable in favour of the right one.
//!
//! It is a re-export, like the `json` module: this crate names the
//! capability rather than the crate, so replacing the implementation is one
//! change here rather than one per consumer.
//!
//! # What is here, and what is not
//!
//! The **macros and core types** are here, which is what calling code needs.
//! The `attributes` feature is deliberately not enabled (see the note in
//! `Cargo.toml`), so `#[instrument]` is unavailable from this path; it would
//! pull `syn` into the foundation crate.
//!
//! There is also **no subscriber**. `tracing` records; a separate crate
//! decides where records go. Bundling one would choose an output format, a
//! writer, and a filter syntax on behalf of every consumer, which is a
//! different decision from "the macros are available". A binary installs its
//! own subscriber; a library must only emit.
//!
//! # Levels
//!
//! `ERROR` for a broken contract, `WARN` for degraded but serving, `INFO` for
//! lifecycle, `DEBUG` for developer diagnostics, `TRACE` for wire-level.
//!
//! # Example
//!
//! ```
//! use lgwks_std::trace::{info, warn};
//!
//! // A library emits. With no subscriber installed these are near-free no-ops;
//! // the binary that links this installs the subscriber that decides where
//! // records go.
//! info!(bytes = 512, "read a record");
//! warn!(remaining = 3, "retry budget is nearly spent");
//! ```
//!
//! Structured fields, not interpolated strings: a subscriber can filter on
//! `bytes` without parsing the message, which is the property `println!` cannot
//! provide, and the reason `println!` is banned in library code.

pub use tracing::{self, Level};

pub use tracing::{debug, error, info, trace, warn};
pub use tracing::{debug_span, error_span, info_span, span, trace_span};
pub use tracing::{event, event_enabled};

pub use tracing::{Event, Span, Value};
// `Instrument` (the future combinator) is core; `#[instrument]` (the attribute)
// is not, because it lives behind the `attributes` feature this crate declines.
pub use tracing::Instrument;

/// Structured fields attached to an event or span.
///
/// Module rather than a re-export: call sites spell `field::Empty` and
/// `field::display`, so the path has to survive verbatim.
pub use tracing::field;
