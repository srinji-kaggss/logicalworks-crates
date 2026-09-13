//! Capability-gated automation bots built on four verbs: Observe, Evaluate,
//! Execute, Query.
//!
//! Bots are assembled from `(condition, action)` chains that bind observed
//! sources to side effects. Authority is proof-carrying: every `poll`,
//! `execute_action`, and `query` takes an `(Auth, input)` tuple, and only
//! `GrantSet::issue` can mint the `Auth` half. Capabilities are validated at
//! build time — a bot that requires `bot.net` without a grant fails before it
//! runs — and proven again on every call, so a grant revoked after build cannot
//! fire.
//!
//! `Evaluate` takes no proof: it is pure (boolean in, boolean out) with no
//! side effect to gate.
//!
//! # Async surface
//!
//! With the default `rt` feature, `lgwks_bot` is also the estate's async and
//! runner surface: `Runtime`, `rt::task::join_all_bounded`, timers, channels,
//! and the opt-in `net`/`process`/`fs`/`signal` drivers. The engine is sourced
//! from the `lgwks_deps` storefront (`feature = "tokio"`), so no other crate
//! authors a `tokio` edge. `--no-default-features` withdraws the async surface
//! and leaves the synchronous `lgwks_std::task` executor as the only runtime.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use lgwks_bot::{Bot, Cap, GrantSet};
//!
//! let bot = Bot::builder("my-bot")
//!     // .observe(source).on(condition, action)
//!     .build(&GrantSet::all_shipped())
//!     .expect("shipped domains are covered by all_shipped");
//!
//! let fired = bot.block_on_tick().expect("tick propagates domain errors");
//! ```

#![allow(async_fn_in_trait)]
// Verbs are async by design (single-threaded `lgwks_std::task` driver, futures
// deliberately not `Send`). Remaining lint contract (missing_docs deny,
// unsafe_code forbid, broken intra-doc links deny) comes from the workspace.

use std::future::Future;
use std::pin::Pin;

/// A boxed, non-`Send` future used to type-erase the async verbs behind the
/// `Bot`'s dynamic chains and behind [`domain::flow::PipelineStep`]. The public
/// traits stay native `async fn`; only the erasure boundary boxes, which is
/// what avoids an `async-trait` dependency.
///
/// Public because it appears in the public `PipelineStep` signature: an
/// implementer must be able to name the return type.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Capability tokens and sealed authority proofs.
pub mod cap;
pub mod domain {
    //! Shipped automation domains.
    //!
    //! Both paths are stable: `lgwks_bot::net` and `lgwks_bot::domain::net`
    //! name the same module. Prefer the short path in new code.
    pub mod chat;
    pub mod data;
    pub mod eval;
    pub mod flow;
    pub mod fs;
    pub mod gh;
    pub mod net;
    pub mod notify;
    pub mod sys;
}
/// Typed bot errors.
pub mod error;
/// Grant sets: build-time admission and per-tick proof minting.
pub mod gate;
/// JSON through the estate facade (`lgwks_std::json`).
pub mod json;
/// Async runtime surface (feature `rt`): owned `Runtime`, bounded fan-out,
/// timers, channels, and opt-in drivers.
#[cfg(feature = "rt")]
pub mod rt;
/// The serializable spec contract and the builder that assembles bots.
///
/// `BotSpec` is validate-only: there is no `from_spec` materializer. A spec
/// that validates still builds through `Bot::builder`, so capability grants
/// stay explicit at the call site. The builder DSL is the DSL — there is
/// deliberately no `bot!` proc-macro (it would drag `syn` into every consumer
/// and hide the per-call `Auth::check` that auditors read).
pub mod spec;
/// The four verbs: Observe, Evaluate, Execute, Query. No fifth verb exists.
pub mod verb;

pub use cap::{Auth, Cap};
pub use error::BotError;
pub use gate::GrantSet;
#[cfg(feature = "rt")]
pub use rt::{Builder, Handle, Runtime, block_on};
pub use spec::{Bot, BotSpec, Chain, ChainEntry};
pub use verb::{Evaluate, Execute, Observe, Query};

/// Wait for all of a set of futures, returning their outputs in input order.
///
/// Re-exported from the storefront engine so callers never name `tokio`. For a
/// fan-out that must be bounded, prefer `rt::task::join_all_bounded`.
#[cfg(all(feature = "rt", feature = "macros"))]
pub use lgwks_deps::tokio::join;

/// Race a set of futures, running the first branch that becomes ready.
///
/// Re-exported from the storefront engine so callers never name `tokio`.
#[cfg(all(feature = "rt", feature = "macros"))]
pub use lgwks_deps::tokio::select;

/// Wait for all of a set of fallible futures, short-circuiting on the first
/// error. Re-exported from the storefront engine so callers never name `tokio`.
#[cfg(all(feature = "rt", feature = "macros"))]
pub use lgwks_deps::tokio::try_join;

pub use domain::chat;
pub use domain::data;
pub use domain::eval;
pub use domain::flow;
pub use domain::fs;
pub use domain::gh;
pub use domain::net;
pub use domain::notify;
pub use domain::sys;
