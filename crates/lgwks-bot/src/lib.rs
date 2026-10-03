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
//! The `broker`, `effect`, `frontier`, `interface`, `journal`, `language`,
//! `retry`, `semantic`, and `session` modules are **development APIs**: they
//! exist on `main` and are in no published tag. The newest `lgwks_bot` tag is
//! `lgwks_bot-v0.4.2`, and the manifest reads `0.5.0` for the release cut that
//! carries them, which has no tag yet either. Documentation on this page
//! describes the `main` tree and must not be read as a statement about a version
//! installed from a registry. Check the changelog for the release that carries a
//! given symbol before relying on it.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use lgwks_bot::broker::Broker;
//! use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
//! use lgwks_bot::journal::MemoryJournal;
//! use lgwks_bot::spec::{EffectIdentity, EffectScope};
//! use lgwks_bot::{Bot, Cap, GrantSet};
//!
//! // The host is the one authority. It names the run, the environment that run
//! // acts on, and the flow revision it came from, and it holds the broker that
//! // owns the environment's generation. A bot cannot be built without one: an
//! // identity the caller did not choose is one it cannot recover against, and
//! // recovery is what stops a restart from resending a merge.
//! let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
//! let mut broker = Broker::new();
//! broker.register(environment)?;
//! let effects = EffectScope::new(
//!     EffectIdentity::new(
//!         RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
//!         environment,
//!         FlowRevision::from_tagged(
//!             "blake3_256",
//!             "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
//!         )?,
//!     ),
//!     broker,
//!     Box::new(MemoryJournal::new()),
//! );
//!
//! let mut bot = Bot::builder("my-bot")
//!     // .observe(source).on(condition, action)
//!     .with_effects(effects)
//!     .build(&GrantSet::all_shipped())
//!     .expect("shipped domains are covered by all_shipped");
//!
//! let fired = bot.tick().expect("tick propagates domain errors");
//! # Ok::<(), Box<dyn std::error::Error>>(())
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

/// The environment broker: which environments a run owns, which generation each
/// is at, and the authority to hand one exact attempt to one of them.
///
/// Fencing lives here rather than in the journal because a durable record of a
/// dispatch aimed at a replaced environment would be accurate and still wrong.
/// See [`broker`](broker) for why a stale generation and a generation the broker
/// never issued are deliberately different errors.
pub mod broker;
/// Capability tokens and sealed authority proofs.
pub mod cap;
pub mod domain {
    //! Shipped automation domains.
    //!
    //! One path, `lgwks_bot::domain::net`, rather than a second short alias at
    //! the crate root. Nine generic words — `fs`, `net`, `sys`, `data` — read
    //! like the standard library's at a call site that has not imported this
    //! crate's prelude, and a second path is a second thing to keep stable.
    pub mod chat;
    pub mod data;
    pub mod eval;
    pub mod flow;
    pub mod fs;
    pub mod gh;
    /// The structural-inspection domain (`inspect` feature): R8's operation as
    /// an [`Observe`](crate::verb::Observe) source and a
    /// [`Query`](crate::verb::Query), plus a [`Task`](crate::task::Task) a
    /// [`Host`](crate::task::Host) can run.
    #[cfg(feature = "inspect")]
    pub mod inspect;
    pub mod net;
    pub mod notify;
    pub mod sys;
}
/// The one declared clock, which every deadline this crate evaluates names.
///
/// Ungated because the module is `std`-only — it imports `fmt`, `sync::Arc`, an
/// atomic and `time::Instant`, and no engine facade — and it is reached by three
/// surfaces with different gates: [`rt::time`] re-exports it beside `Deadline`,
/// [`rt::supervise`] reads it to measure its budgets, and the observation phase
/// reads its wall watchdog to bound a source poll. Gating it on the narrowest of
/// those would leave a `--no-default-features` build — whose tick phase still
/// runs and still needs a bound on its sources — unable to name the clock that
/// governs it.
///
/// Re-exported unchanged from [`rt::clock`] for every build that has `rt`, so
/// this is one clock reached two ways and never two clocks: `rt::clock` is the
/// path a caller who has the async surface already writes.
pub mod clock;
/// The `bevy_ecs` substrate the verbs execute on. Private: it is the
/// implementation, not a second way to run a bot.
mod ecs;
/// Durable effect identity: the exact key a settlement is a statement about.
///
/// The ledger already refuses a contradicting settlement; this supplies the
/// identity that makes "contradicting" decidable. An attempt count cannot: two
/// deliveries both arriving as "attempt 3" are indistinguishable, so a repeat
/// of an old settlement and a statement about a new one look the same at the
/// moment the ledger accepts one. See [`EffectKey`](effect::EffectKey).
pub mod effect;
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
/// Typed in-process structural code inspection (R8, feature `inspect`).
///
/// Check risky code over its structure without executing it: parse the subject
/// bytes with [`lgwks_ast`], walk the tree against a versioned structural rule
/// set, and return a typed [`Verdict`](inspect::Verdict) that distinguishes a
/// complete clean scope from unsupported, undecidable, incomplete and
/// infrastructure-failure outcomes. The subject is never compiled, imported,
/// built, shelled or loaded.
#[cfg(feature = "inspect")]
pub mod inspect;
/// The interface model: recognizing the element a step names.
pub mod interface;
/// The durable journal an effect is appended to before it leaves the process.
///
/// Identity without persistence is identity that is lost exactly when it is
/// needed, so this is the other half of what [`effect`](effect) supplies. It is
/// also where the durability grade lives: a journal that cannot survive its own
/// writer dying refuses to be the record behind an external handoff, rather
/// than accepting an append it will lose.
pub mod journal;
/// JSON through the shared facade (`lgwks_std::json`).
pub mod json;
/// Language understanding: the tiered lexicon behind the resolver seam.
pub mod language;
/// Model output admitted as data: bounded decode, provenance, evidence-gated
/// completion, bounded repair, tenant-scoped artifacts, and a context checkpoint
/// (feature `script`).
///
/// The rule it implements (issue #87): an AI proposal is an **untrusted task
/// input**. Validation, provenance, no-progress detection, bounded repair,
/// tenant-scoped artifacts and serialized writes belong in the host's contract,
/// not in a prompt. So nothing here calls a model — [`proposal::StubModel`] is a
/// deterministic double and the bytes arrive as `&[u8]` from a caller that owns
/// the transport — and nothing a payload says can install a tool, read a
/// credential, widen a grant, or turn a truncated observation into a complete
/// one. Each attempt is a typed [`proposal::Refusal`] carrying the
/// [`proposal::Provenance`] of the bytes that made it.
///
/// It is not a fifth verb: the operations a proposal may name are the ones the
/// host already registered in [`proposal::Surface`], and admitting one produces a
/// [`proposal::Plan`] of names to perform through the existing verbs rather than
/// a new way to perform anything.
///
/// It sits on `script` because the checkpoint round-trips through the run store
/// ([`proposal::Checkpoint`] is [`Durable`](script::run_store::Durable)) and the
/// artifact store is what a task's steps reach.
///
/// # It is wired, and how
///
/// Nothing in this module calls itself: a task body reaches the boundary through
/// [`script::admit`](script::admit), the one step a run uses to admit model or
/// tool output. A [`Gate`](script::Gate) bundles the decoder, the surface and one
/// **run-scoped** [`proposal::PlanBudget`] and [`proposal::RepairLedger`], and
/// `admit` charges both — so the fourth identical refusal across four separate
/// [`Host::run`](task::Host::run)s is a finite typed
/// [`proposal::Intervention`] rather than four refusals a caller must correlate.
/// It enters its step, so a refusal reads `admit/plan` like every other located
/// failure, and it returns [`FlowError::Refused`](script::FlowError::Refused) or
/// [`FlowError::Intervention`](script::FlowError::Intervention), both typed and
/// both carrying the [`proposal::Provenance`] of the exact refused bytes. When the
/// run has a store installed, each refusal is recorded through it under
/// `<step>/refusal` before the error is returned, so a run resumed on a fresh host
/// reads back what the first run refused. See `INV-BOT-95`.
#[cfg(feature = "script")]
pub mod proposal;
/// The `domain_id -> constructor` registry: what a spec's strings resolve to.
///
/// Private, with curated re-exports beside `BotSpec` below, because the registry
/// is one concern — which identifiers a binary can run — rather than a second
/// namespace to learn.
mod registry;
/// Whether a failed attempt may be tried again.
///
/// The vocabulary lives in [`error`](error): [`RetryClass`](RetryClass) says
/// what a failure permits and [`DispatchCertainty`](DispatchCertainty) says
/// what it established. What this module adds is RQ-009's decision rule, which
/// composes those with the budget, the authority, the intent and any remote
/// deduplication contract.
pub mod retry;
/// The canonical PR-review task: pin a subject, publish at that commit, and
/// verify the publication independently.
///
/// [`review::review_pr`] pins the subject, runs the caller's analysis, checks
/// freshness, publishes once at the reviewed commit, and verifies through a
/// separate read-back. Its five outcomes are five facts a caller can act on
/// without reading a message: [`review::ReviewOutcome::Published`] carries a verified
/// review id, [`review::ReviewOutcome::Unknown`] an effect that may or may not have
/// landed, [`review::ReviewOutcome::TargetMoved`] the two commits of a head that
/// changed, and [`review::ReviewOutcome::Refused`] something the caller can repair.
///
/// The body manages no pid, no reap loop and no retry over publication. A lost
/// response is reconciled by exactly one read, never by a second create — and
/// [`review::ReviewOutcome::Unknown`] is what makes the local mode's limit visible
/// rather than hidden: it holds no durable record between attempts, so an
/// effect it cannot observe from GitHub is reported as unknown for the caller
/// to decide.
///
/// # Example
///
/// ```
/// # #[cfg(all(feature = "script", feature = "process"))]
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use lgwks_bot::domain::gh::{CommitId, Gh, PullRequest, Repository};
/// use lgwks_bot::review::ReviewRequest;
/// use lgwks_bot::script::{Scope, Tenant};
/// use lgwks_bot::task::{Host, task};
///
/// let host = Host::builder(Tenant::new("reviewer")?.as_str())?.build()?;
/// let pull = PullRequest::new(Repository::new("acme/widgets")?, 7);
/// let review = task("review-pr", |scope: Scope, gh: Gh| async move {
///     let request = ReviewRequest::new(pull, "COMMENT", "two findings")
///         .with_marker("example");
///     lgwks_bot::review::review_pr(scope, gh, request, |_snapshot, _step| {
///         Ok(String::from("two findings"))
///     })
///     .await
/// })?;
/// // The run needs a `gh` on PATH and a repository to read; this example
/// // stops before that rather than reaching the network.
/// let _ = (CommitId::new("0".repeat(40))?, host, review);
/// # Ok(())
/// # }
/// # #[cfg(not(all(feature = "script", feature = "process")))]
/// # fn main() {}
/// ```
#[cfg(all(feature = "script", feature = "process"))]
pub mod review;
/// Async runtime surface (feature `rt`): owned `Runtime`, bounded fan-out,
/// timers, channels, and opt-in drivers.
#[cfg(feature = "rt")]
pub mod rt;
/// Orchestration blocks for [`script!`]: scoped, bounded, deadline-carrying,
/// keyed, and mapped (feature `script`).
#[cfg(feature = "script")]
pub mod script;
/// The semantic tier: resolving an utterance a lexicon cannot reach.
pub mod semantic;
/// Synchronous validated guidance flows and session runner.
pub mod session;
/// The serializable spec contract, the materializer that walks one against a
/// registry, and the builder that assembles bots.
///
/// [`Bot::from_spec`] is the materializer: a [`BotSpec`] plus a
/// [`DomainRegistry`] plus the caller's [`GrantSet`] produces a runnable
/// [`Bot`] through the same `assemble`/`build` path a native bot uses, with no
/// second interpreter and no second execution path. It is all-or-nothing: an
/// unmet need is reported as a complete, attributed [`NeedSet`] and nothing is
/// built, and admission validates a registry with a duplicate identifier rather
/// than letting lookup depend on declaration order.
///
/// The builder is not replaced, and it stays the only path that mints
/// authority: grants come from a `GrantSet` the caller holds, never from the
/// spec, so wire data cannot choose what the bot is able to reach. There is
/// deliberately no `bot!` proc-macro: it would drag the `syn` stack into every
/// consumer and hide the per-call `Auth::check` that auditors read.
/// [`domains!`](crate::domains) declares the registry as data instead, with no
/// proc macro and no new dependency. Orchestration *between* verbs is a
/// different job, and it does have a proc macro: `script!` (feature `script`)
/// expands to plain calls into the `script` module, leaves every verb's
/// `Auth::check` where it was, and reaches `syn` only on the host through the
/// `lgwks_deps` storefront.
pub mod spec;
/// The task front door: a [`Host`](task::Host) runs a
/// [`Task`](task::Task) and returns a [`Report`](task::Report) (feature
/// `script`).
///
/// The smallest complete path from "here is a typed body" to "here is a
/// disposition, an output and evidence": no trait to implement per workflow, no
/// observer to poll, no handle to reap, and no `Send`, `Box` or `'static` bound
/// in the author's code. It sits on `script`, so a task body composes with
/// `each`, `within` and `retry` and inherits their bounds.
#[cfg(feature = "script")]
pub mod task;
/// The four verbs: Observe, Evaluate, Execute, Query. No fifth verb exists.
pub mod verb;

pub use cap::{Auth, Cap, Deficit, Demand, Shortage};
pub use ecs::{
    DEFAULT_POLL_DEADLINE, ForcedRefresh, MAX_POLL_DEADLINE, StalledSource, SupersededObservation,
    TickReport,
};
pub use effect::{EventId, InputIdentity};
pub use error::{BotError, DispatchCertainty, RetryClass};
pub use gate::GrantSet;
pub use language::{Alias, LanguageResolver};
pub use registry::{Action, ActionCtor, Condition, DomainRegistry, Source, SourceCtor};
#[cfg(feature = "rt")]
pub use rt::runtime::{Builder, Handle, Runtime, block_on};
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
pub use spec::{Admission, Bot, BotSpec, Need, NeedSet};

/// Write orchestration as indented blocks that expand to bounded,
/// deadline-carrying, tenant-keyed flows over [`script`](mod@script), plus an
/// `ARCHITECTURE` map of what was declared. The language is documented on
/// the [`script`](mod@script) module and in `lgwks_macros`.
#[cfg(feature = "script")]
pub use lgwks_macros::script;
pub use verb::EffectLifetime;
/// Why a source cannot be trusted to answer "has this changed?" this tick, and the
/// statement it makes through [`Observe::cache_state`](crate::verb::Observe::cache_state).
///
/// Re-exported on its own line rather than inside the four-verb list below,
/// because it is not a verb. `verb::tests::the_crate_root_reexports_exactly_those_four_verbs`
/// reads that list as the four verbs and nothing else, so a fifth name beside them
/// would make the invariant it guards fail for a type that is not a verb.
pub use verb::RefreshReason;
pub use verb::{Evaluate, Execute, Observe, Query};

/// Typed in-process structural inspection (feature `inspect`).
///
/// [`Verdict`](inspect::Verdict) stays under `inspect::` because the session
/// surface already exports a `Verdict` at the crate root; the two are unrelated
/// and one name cannot mean two things.
#[cfg(feature = "inspect")]
pub use inspect::{
    Budgets, Finding, InspectRequest, Inspection, RuleCoverage, RuleSet, UnsupportedReason, inspect,
};

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
