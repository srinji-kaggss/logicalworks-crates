//! Capability-gated automation bots built on four verbs: Observe, Evaluate,
//! Execute, Query.
//!
//! Bots are assembled from `(condition, action)` chains that bind observed
//! sources to side effects. Authority is proof-carrying: every `poll`,
//! `execute_action`, and `query` takes an `(Auth, input)` tuple, and only
//! `GrantSet::issue` can mint the `Auth` half. Capabilities are validated at
//! build time (a bot that requires `bot.net` without a grant fails before it
//! runs) and proven again on every call.
//!
//! That authority is a **snapshot**, not a live lease. `build` clones the grant
//! set into the bot and `GrantSet` has no revoke operation, so changing or
//! dropping the caller's set afterwards does not narrow a bot already built. An
//! `Auth` in hand is likewise a capability-membership proof over the caps it was
//! issued with: not a signature, not an identity, and not isolation. Narrowing
//! a running bot's authority needs a mechanism this crate does not have yet,
//! and the boundary is spelled out under "Authority is a snapshot" in the crate
//! `README.md`.
//!
//! `Evaluate` takes no proof: it is pure (boolean in, boolean out) with no
//! side effect to gate.
//!
//! # Async surface
//!
//! With the default `rt` feature, `lgwks_bot` is also the async and runner
//! surface of this workspace: `Runtime`, `rt::task::join_all_bounded`, timers,
//! channels, and the opt-in `net`/`process`/`fs`/`signal` drivers. The engine is
//! sourced from the `lgwks_deps` dependency facade (`feature = "tokio"`), so no
//! other crate authors a `tokio` edge. `--no-default-features` withdraws the
//! async surface — no `tokio` at all — and drives the four verbs on
//! `lgwks_std::task`'s executor instead. It is not a dependency-light build:
//! the `bevy_ecs` substrate and the `lgwks_deps` facade are unconditional, so a
//! no-default bot still runs on the ECS schedule. The two axes — the async
//! surface and the ECS substrate — are independent.
//!
//! # Release boundary
//!
//! The `session`, `language`, `semantic`, `interface`, and `frontier` modules
//! are **development APIs**: they exist on `main` and are not in the published
//! `lgwks_bot-v0.4.2` tag. Documentation on this page describes the `main` tree
//! and must not be read as a statement about a version installed from a
//! registry. Check the changelog for the release that carries a given symbol
//! before relying on it.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use lgwks_bot::{Bot, Cap, GrantSet};
//!
//! let mut bot = Bot::builder("my-bot")
//!     // .observe(source).on(condition, action)
//!     .build(&GrantSet::all_shipped())
//!     .expect("shipped domains are covered by all_shipped");
//!
//! let fired = bot.tick().expect("tick propagates domain errors");
//! ```
//!
//! `tick` is the synchronous adapter and belongs outside an async runtime:
//! inside one it returns `BotError::TickInsideRuntime` rather than park the
//! thread whose reactor the bot's own verbs need. Await `bot.tick_async()`
//! there instead — the same four phases, the same `Result`, driven on the
//! caller's executor.

// Verbs are async by design (single-threaded `lgwks_std::task` driver, futures
// deliberately not `Send`). The lint fires on `pub trait` methods declared
// `async fn` without a `-> impl Future + Send` bound; that is exactly the shape
// INV-BOT-FOUR-VERBS requires, because a `Send` bound would force every domain
// to be `Send` and rule out the thread-local state `Bot::tick` is built for.
//
// This is the crate's one suppression, and it names its reason. `expect` is not
// available here: the lint is denied crate-wide rather than triggered by this
// item alone, so an `#[expect]` would report an unfulfilled expectation at every
// other `async fn` in `verb`.
//
// Remaining lint contract (missing_docs deny, unsafe_code forbid,
// broken_intra_doc_links deny) comes from the workspace.
#![allow(
    async_fn_in_trait,
    reason = "the four verb traits are the crate's public async contract and must stay non-Send; \
              see docs/async-sdk-shape.md and INV-BOT-FOUR-VERBS"
)]

/// Compiles every Rust block in `README.md` as a doctest.
///
/// The README is the first thing a consumer reads and the last thing anyone
/// updates: it described a `lgwks_std::task` executor and an `async fn tick`
/// for two commits after both had been replaced, and nothing failed, because a
/// prose example is not compiled by anything. This makes it compiled.
///
/// `#[cfg(doctest)]` so the item exists only under `cargo test --doc`: it is
/// not part of the library, not built by a normal compile, and not exported.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeExamples;

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
/// The `bevy_ecs` substrate the verbs execute on. Private: it is the
/// implementation, not a second way to run a bot.
mod ecs;
/// Typed bot errors.
pub mod error;
/// Politeness and admission: whether a host may be contacted now, and if not,
/// what the scheduler does instead.
///
/// A verdict, not a delay. The gate reports one of three physical actions, and
/// the reasons behind them are typed state machines rather than more arms — see
/// the module documentation for why that is the difference between a schedule
/// that replays and an execution token that became an audit log.
pub mod frontier;
/// Grant sets: build-time admission and per-tick proof minting.
pub mod gate;
/// The interface model: recognizing the element a step names.
pub mod interface;
/// JSON through the shared facade (`lgwks_std::json`).
pub mod json;
/// Language understanding: the tiered lexicon behind the resolver seam.
pub mod language;
/// Async runtime surface (feature `rt`): owned `Runtime`, bounded fan-out,
/// timers, channels, and opt-in drivers.
#[cfg(feature = "rt")]
pub mod rt;
/// The semantic tier: resolving an utterance a lexicon cannot reach.
pub mod semantic;
/// Synchronous validated guidance flows and session runner.
pub mod session;
/// The serializable spec contract and the builder that assembles bots.
///
/// `BotSpec` is validate-only today: there is no `from_spec` materializer, so a
/// spec that validates still has to be built through `Bot::builder`, which
/// takes verbs rather than wire data. Materializing a bot from a spec needs a
/// `domain_id -> constructor` registry — `"gh::pr_status"` has to become a
/// concrete `Observe` — and no such registry exists. That absence is recorded
/// as open in `experience/invariants/sdk.yaml`, not as a design position, and
/// the registry is the piece of work that closes it.
///
/// Whenever it lands, the builder stays the only path that mints authority:
/// grants come from a `GrantSet` the caller holds, never from the spec, so wire
/// data still cannot choose what the bot is able to reach. There is
/// deliberately no `bot!` proc-macro either — it would drag `syn` into every
/// consumer and hide the per-call `Auth::check` that auditors read.
pub mod spec;
/// The four verbs: Observe, Evaluate, Execute, Query. No fifth verb exists.
pub mod verb;

pub use cap::{Auth, Cap};
pub use error::BotError;
pub use frontier::{
    Admission, CompletionError, ConstraintKey, DeferralKind, Frontier, InFlightPermit,
    PolitenessError, PolitenessPolicy, RejectKind, Resolved, RulesState,
};
pub use gate::GrantSet;
pub use language::{Alias, LanguageResolver};
#[cfg(feature = "rt")]
pub use rt::{Builder, Handle, Runtime, block_on};
pub use semantic::{Embedder, EmbedderIdentity, SemanticError, SemanticPolicy, SemanticResolver};
pub use session::{
    AnswerDomain, AnswerRejection, ChoiceArm, CompiledTemplate, DecisionReceipt, DegradedReason,
    Disposition, EffectLedger, FlowBounds, FlowEdge, FlowNodeKind, FlowSpec, Interpolate, Journal,
    JournalError, MAX_FLOW_BYTES, MAX_FLOW_SPEC_BYTES, MAX_RECORD_BYTES, MAX_SESSION_BYTES,
    MAX_UTTERANCE_BYTES, MAX_VALUE_BYTES, MatchTier, MemoryJournal, NodeId, NodeKind, Outcome,
    PolicyVersion, Predicate, Provenance, Question, RECEIPT_VERSION, ReceiptAcceptance,
    RecordedDecision, Resolution, Resolver, ResourceAxis, ResourceLimits, Session, SessionId,
    TemplateInterpolator, TemplatePart, Terminal, TranscriptEntry, Value, ValueExpr, VarScope,
    VarType, Verdict,
};
pub use spec::{Bot, BotSpec};
pub use verb::{Evaluate, Execute, Observe, Query};

/// Wait for all of a set of futures, returning their outputs in input order.
///
/// Re-exported from the `lgwks_deps` engine so callers never name `tokio`. For a
/// fan-out that must be bounded, prefer `rt::task::join_all_bounded`.
#[cfg(all(feature = "rt", feature = "macros"))]
pub use lgwks_deps::tokio::join;

/// Race a set of futures, running the first branch that becomes ready.
///
/// Re-exported from the `lgwks_deps` engine so callers never name `tokio`.
#[cfg(all(feature = "rt", feature = "macros"))]
pub use lgwks_deps::tokio::select;

/// Wait for all of a set of fallible futures, short-circuiting on the first
/// error. Re-exported from the `lgwks_deps` engine so callers never name `tokio`.
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
