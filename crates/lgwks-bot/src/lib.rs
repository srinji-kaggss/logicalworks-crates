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

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// The verb traits are async by design. `lgwks_std::task` drives every bot on
// one local thread, so a verb's returned future is deliberately not `Send`;
// the `async_fn_in_trait` auto-trait warning does not apply to this crate.
#![allow(async_fn_in_trait)]

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

pub mod cap;
pub mod domain {
    //! Shipped automation domains.
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
pub mod error;
pub mod gate;
pub mod json;
pub mod spec;
pub mod verb;

pub use cap::{Auth, Cap};
pub use error::BotError;
pub use gate::GrantSet;
pub use spec::{Bot, BotSpec, Chain, ChainEntry};
pub use verb::{Evaluate, Execute, Observe, Query};

pub use domain::chat;
pub use domain::data;
pub use domain::eval;
pub use domain::flow;
pub use domain::fs;
pub use domain::gh;
pub use domain::net;
pub use domain::notify;
pub use domain::sys;
