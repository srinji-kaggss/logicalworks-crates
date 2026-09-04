//! Capability-gated automation bots built on four verbs: Observe, Evaluate,
//! Execute, Query.
//!
//! Bots are assembled from `(condition, action)` chains that bind observed
//! sources to side effects. Authority is proof-carrying: every poll, run,
//! and query takes an `(Auth, input)` tuple, and only `GrantSet::issue`
//! can mint the `Auth` half. Capabilities are validated at build time — a
//! bot that requires `bot.net` without a grant fails before it runs — and
//! proven again on every call, so a grant revoked after build cannot fire.
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
//! let fired = bot.tick().expect("tick propagates domain errors");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

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
