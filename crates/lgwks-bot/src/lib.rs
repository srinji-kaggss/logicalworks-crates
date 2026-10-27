#![doc = include_str!("lexicon.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! Capability-gated automation bots built on four verbs: Observe, Evaluate,
//! Execute, Query.
//!
//! The supported interface is `script!`: this page shows the `script` module its
//! expansion calls into and the `script!` and `try_join!` macros, and nothing
//! else. They are plain code, not links, because each exists only under its
//! feature (see the table below) and a link to an absent item breaks the
//! no-default-features documentation build.
//! Every other module and root re-export is `#[doc(hidden)]` support, still
//! reachable by this crate's own tests and by the expansion, and kept out of the
//! documented surface by #526.
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
//! issued with: not a signature, not an identity, and not isolation. A proof
//! minted from a grant that named a lifetime lapses once that lifetime has
//! passed, but the lifetime is fixed at the mint and a bot mints afresh on
//! every call, so it bounds a proof in hand rather than a running bot. Narrowing
//! a running bot is one operation: `Bot::replace_grants` (under `ecs`)
//! swaps the retained set between ticks, atomically with respect to them, and
//! reports the capabilities it withdrew — which the next tick's calls are denied
//! for. Replacing the caller's set is still not that operation, and the boundary
//! is spelled out under "Authority is a snapshot" in the crate `README.md`.
//!
//! `Evaluate` takes no proof: it is pure (boolean in, boolean out) with no
//! side effect to gate.
//!
//! # Async orchestration runtime
//!
//! `lgwks_bot` is the async orchestration runtime of this workspace: `Runtime`,
//! `rt::task::join_all_bounded`, timers, channels, and the opt-in `net`/`process`/
//! `fs`/`signal` drivers. The driver behind it is sourced from the `lgwks_deps`
//! dependency storefront (`feature = "tokio"`), so no other crate authors a driver
//! edge and no consumer names one. `--no-default-features` is a narrow test build
//! with no driver at all, driving the four verbs on `lgwks_std::task`'s executor
//! instead; it is not the supported surface. The supported surface is the runtime:
//! one front door (`Runtime`), one way to start supervised work (`Supervisor`,
//! `join_all_bounded`, `script!`), and no handle to a running task anywhere.
//!
//! # Features
//!
//! | Feature | Default | Enables |
//! |---|---|---|
//! | `rt` | on | the orchestration runtime surface, driven by the `lgwks_deps` driver |
//! | `time`, `sync`, `macros` | on | the matching runtime drivers; each implies `rt` |
//! | `script` | on | `script!` and the `script` module; implies `rt`, `time`, `sync`, `macros` and `wire` |
//! | `ecs` | on | the `bevy_ecs` substrate `Bot` executes on; opt out with `--no-default-features` for a process-only build |
//! | `trace` | on | the tracing facade; the installer lives in `lgwks_std/trace-install` |
//! | `wire`, `ron` | on | the wire encoding and the RON document notation the journal and the session persist |
//! | `io` | off | reader and writer traits; implies `rt` |
//! | `net`, `fs` | off | sockets, files; each implies `io` (`net` also `lgwks_std/random` and `lgwks_std/http`) |
//! | `process` | off | supervised child processes; implies `io`, `sync` and `time` |
//! | `signal` | off | OS signals; implies `rt` |
//! | `inspect` | off | structural code inspection through `lgwks_ast` (host only) |
//! | `ephemeral` | off | run identities minted from `lgwks_std::random` |
//! | `profile` | off | the per-stage tick profiler |
//! | `full` | off | every feature above |
//!
//! # Release boundary
//!
//! This page describes the tree the crate was published from. Which release
//! carries a given symbol is in the changelog (`CHANGELOG.md` in the repository,
//! with the 3.0.0 migration notes under `docs/changelog/`); check it before
//! relying on a symbol. Everything outside `script!`, `try_join!` and the
//! `script` module is `#[doc(hidden)]` support code with no compatibility
//! promise (#526).
//!
//! # Quick start
//!
//! A flow is written as indented blocks. Each block is one bounded primitive: a
//! fan-out with a ceiling, a deadline, a retry under a shared budget. The flow
//! below squares every number, retries a failed attempt under the same step key
//! and gives each attempt a second.
//!
//! ```
//! # use lgwks_bot::script::{FlowError, Scope, Tenant};
//! lgwks_bot::script! {
//!     /// Square every number, as many at once as the machine sustains.
//!     pub flow squares(numbers: Vec<u64>) -> u64:
//!         let squared = each number in numbers:
//!             retry up to 3 times, waiting 10ms:
//!                 within 1s:
//!                     number.saturating_mul(number)
//!         give back squared.iter().sum()
//! }
//!
//! # fn main() -> Result<(), FlowError> {
//! // Every flow takes a scope first; it carries the tenant, the step path and
//! // the cancellation token.
//! let scope = Scope::root(Tenant::new("acme")?);
//! let total = lgwks_bot::block_on(squares(&scope, vec![1, 2, 3]))?;
//! assert_eq!(total, 14);
//!
//! // The block also emitted a map of what it declared.
//! assert!(ARCHITECTURE.to_string().contains("each number in numbers"));
//! # Ok(())
//! # }
//! ```
//!
//! The same flow is available as ordinary functions: `script::within`,
//! `script::retry` and `script::each` take a `script::Scope` and a body and
//! return a `script::FlowError` that names the step where it failed. Use the
//! macro for the shape and the functions when the body already exists. The
//! blocks need the default `script` feature (see the table above).

// Verbs are async by design (single-threaded `lgwks_std::task` driver, futures
// deliberately not `Send`). The verb traits declare their async methods as
// return-position `impl Future` rather than `async fn`, which is exactly the
// shape INV-BOT-FOUR-VERBS requires without a crate-level lint suppression:
// a `Send` bound would force every domain to be `Send` and rule out the
// thread-local state `Bot::tick` is built for, and implementers still write
// `async fn` in their impls.
//
// Remaining lint contract (missing_docs deny, unsafe_code forbid,
// broken_intra_doc_links deny) comes from the workspace.

/// Compiles every Rust block in the README pages as a doctest.
///
/// The README is the first thing a consumer reads and the last thing anyone
/// updates: it described a `lgwks_std::task` executor and an `async fn tick`
/// for two commits after both had been replaced, and nothing failed, because a
/// prose example is not compiled by anything. This makes it compiled.
///
/// Only `README-script.md` holds a Rust block now: it is the one that goes
/// through `script!`. The examples that named the crate by path became tests
/// (`tests/it/readme_front_door.rs`, `tests/it/rt_front_door.rs`), and
/// `contract/PUBLIC_SURFACE.toml` refuses a README page that gains one.
///
/// `#[cfg(doctest)]` so the item exists only under `cargo test --doc`: it is
/// not part of the library, not built by a normal compile, and not exported.
#[cfg(doctest)]
#[doc = include_str!("../README-script.md")]
struct ReadmeExamples;

use std::future::Future;
use std::pin::Pin;

/// A boxed, non-`Send` future used to type-erase the async verbs behind the
/// `Bot`'s dynamic chains and behind [`domain::flow::PipelineStep`]. The public
/// verb traits declare return-position `impl Future`, so a domain still writes
/// `async fn` in its impl and only the erasure boundary boxes — which is what
/// avoids an `async-trait` dependency.
///
/// Public because it appears in the public `PipelineStep` signature: an
/// implementer must be able to name the return type.
#[doc(hidden)]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The environment broker: which environments a run owns, which generation each
/// is at, and the authority to hand one exact attempt to one of them.
///
/// Fencing lives here rather than in the journal because a durable record of a
/// dispatch aimed at a replaced environment would be accurate and still wrong.
/// See [`broker`] for why a stale generation and a generation the broker
/// never issued are deliberately different errors.
#[doc(hidden)]
pub mod broker;
/// Capability tokens and sealed authority proofs.
#[doc(hidden)]
pub mod cap;
/// Versioned shared records: compare-and-set writes for concurrent editors.
///
/// [`CasWrite`](crate::cas::CasWrite) applies a write only when the record still
/// holds the version the writer saw; a late write is the typed
/// [`BotError::Conflict`] outcome — recorded in the
/// store's bounded conflict log, never retried blindly (issue #278, row 5).
#[doc(hidden)]
pub mod cas;
#[doc(hidden)]
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
    /// The network Observe domain, behind `net` like the HTTP exchange it polls
    /// through: a process-only build compiles no HTTP client (#687).
    #[cfg(feature = "net")]
    pub mod net;
    pub mod notify;
    pub mod sys;
}
/// The one declared clock, which every deadline this crate evaluates names.
///
/// Ungated because the module is `std`-only — it imports `fmt`, `sync::Arc`, an
/// atomic and `time::Instant`, and no engine facade — and it is reached by three
/// surfaces with different gates: `rt::time` re-exports it beside `Deadline`,
/// `rt::supervise` reads it to measure its budgets, and the observation phase
/// reads its wall watchdog to bound a source poll. Gating it on the narrowest of
/// those would leave a `--no-default-features` build — whose tick phase still
/// runs and still needs a bound on its sources — unable to name the clock that
/// governs it.
///
/// Re-exported unchanged from `rt::clock` for every build that has `rt`, so
/// this is one clock reached two ways and never two clocks: `rt::clock` is the
/// path a caller who has the async surface already writes.
#[doc(hidden)]
pub mod clock;
/// The cross-host lease with fencing epochs, the bounded dispatch queue, and
/// the coordinator that commits every dispatch row before the lane starts.
#[doc(hidden)]
pub mod dist_lease;
/// The `bevy_ecs` substrate the verbs execute on. Private: it is the
/// implementation, not a second way to run a bot. Its substrate submodules are
/// gated behind `ecs` inside, so a process-only consumer never compiles them
/// (#687); the pending-work vocabulary they report in is built everywhere.
mod ecs;
/// Durable effect identity: the exact key a settlement is a statement about.
///
/// The ledger already refuses a contradicting settlement; this supplies the
/// identity that makes "contradicting" decidable. An attempt count cannot: two
/// deliveries both arriving as "attempt 3" are indistinguishable, so a repeat
/// of an old settlement and a statement about a new one look the same at the
/// moment the ledger accepts one. See [`EffectKey`](effect::EffectKey).
#[doc(hidden)]
pub mod effect;
/// Typed bot errors.
#[doc(hidden)]
pub mod error;
/// Politeness and admission: whether a host may be contacted now, and if not,
/// what the scheduler does instead.
///
/// A verdict, not a delay. The gate reports one of three physical actions, and
/// the reasons behind them are typed state machines rather than more arms — see
/// the module documentation for why that is the difference between a schedule
/// that replays and an execution token that became an audit log.
#[doc(hidden)]
pub mod frontier;
/// Grant sets: build-time admission and per-tick proof minting.
#[doc(hidden)]
pub mod gate;
/// One effect per key: POST through a retrying proxy without sending twice.
/// Behind `net` like the HTTP exchange it posts through (#687).
///
/// [`IdempotentPost`](crate::idempotent::IdempotentPost) carries the
/// caller-generated `Idempotency-Key` every attempt of one operation reuses,
/// so a proxy that duplicates and replays requests still delivers exactly one
/// effect. Wired rather than offered: an upstream credential refusal on this
/// path is the credential row's typed repair, and the row's tests stand a
/// real duplicating proxy in front of the adapter (issue #278, row 6).
#[cfg(feature = "net")]
#[doc(hidden)]
pub mod idempotent;
/// Typed in-process structural code inspection (R8, feature `inspect`).
///
/// Check risky code over its structure without executing it: parse the subject
/// bytes with [`lgwks_ast`], walk the tree against a versioned structural rule
/// set, and return a typed [`Verdict`](inspect::Verdict) that distinguishes a
/// complete clean scope from unsupported, undecidable, incomplete and
/// infrastructure-failure outcomes. The subject is never compiled, imported,
/// built, shelled or loaded.
#[cfg(feature = "inspect")]
#[doc(hidden)]
pub mod inspect;
/// The interface model: recognizing the element a step names.
#[doc(hidden)]
pub mod interface;
/// The durable journal an effect is appended to before it leaves the process.
///
/// Identity without persistence is identity that is lost exactly when it is
/// needed, so this is the other half of what [`effect`] supplies. It is
/// also where the durability grade lives: a journal that cannot survive its own
/// writer dying refuses to be the record behind an external handoff, rather
/// than accepting an append it will lose.
#[doc(hidden)]
pub mod journal;
/// JSON through the shared facade (`lgwks_std::json`).
#[doc(hidden)]
pub mod json;
#[doc(hidden)]
#[cfg(feature = "time")]
pub mod keyed;
/// Language understanding: the tiered lexicon behind the resolver seam.
#[doc(hidden)]
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
/// [`script::admit`], the one step a run uses to admit model or
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
#[doc(hidden)]
pub mod proposal;
/// The `domain_id -> constructor` registry: what a spec's strings resolve to.
///
/// Private, with curated re-exports beside `BotSpec` below, because the registry
/// is one concern — which identifiers a binary can run — rather than a second
/// namespace to learn.
mod registry;
/// Whether a failed attempt may be tried again.
///
/// The vocabulary lives in [`error`]: [`RetryClass`] says
/// what a failure permits and [`DispatchCertainty`] says
/// what it established. What this module adds is RQ-009's decision rule, which
/// composes those with the budget, the authority, the intent and any remote
/// deduplication contract.
#[doc(hidden)]
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
/// The task is built and driven against a fake `gh` in `tests/it/pr_review_journey.rs` and
/// `tests/it/sim_review_pr.rs`. A real run needs a `gh` on `PATH` and a repository to read, so no
/// documentation example reaches the network.
#[cfg(all(feature = "script", feature = "process"))]
#[doc(hidden)]
pub mod review;
/// Async runtime surface (feature `rt`): owned `Runtime`, bounded fan-out,
/// timers, channels, and opt-in drivers.
#[cfg(feature = "rt")]
#[doc(hidden)]
pub mod rt;
/// Orchestration blocks for [`script!`]: scoped, bounded, deadline-carrying,
/// keyed, and mapped (feature `script`).
#[cfg(feature = "script")]
pub mod script;
/// The semantic tier: resolving an utterance a lexicon cannot reach.
#[doc(hidden)]
pub mod semantic;
/// Synchronous validated guidance flows and session runner.
#[doc(hidden)]
pub mod session;
/// The error vocabulary's escaping and retry-class promises, simulated; declared
/// here because `error.rs` is past the size law.
#[cfg(test)]
mod sim_error;
/// The lexicon resolver's verdicts, simulated; declared here beside the other
/// in-crate simulations.
#[cfg(test)]
mod sim_language;
/// The semantic tier behind the lexicon, simulated with a scripted embedder.
#[cfg(test)]
mod sim_semantic;
/// The seeded draw stream the in-module simulations share.
#[cfg(test)]
mod sim_stream;
/// Cross-host time: monotonic deadlines, wall timestamps, and the skew bound.
///
/// [`SkewBound`](crate::skew::SkewBound) is how far two honest clocks may disagree;
/// [`MonoDeadline`](crate::skew::MonoDeadline) and [`WallStamp`](crate::skew::WallStamp) keep
/// the deadline a lease carries apart from the timestamp a record shows. Wired
/// rather than offered: [`Auth::check_remote`] judges
/// a proof minted on one clock against a foreign reading within the bound
/// (issue #278, row 2).
#[doc(hidden)]
pub mod skew;
/// The serializable spec contract, the materializer that walks one against a
/// registry, and the builder that assembles bots.
///
/// `Bot::from_spec` (under `ecs`) is the materializer: a [`BotSpec`] plus a
/// `DomainRegistry` plus the caller's [`GrantSet`] produces a runnable
/// `Bot` through the same `assemble`/`build` path a native bot uses, with no
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
#[doc(hidden)]
pub mod spec;
/// The stable-read protocol: what an `Observe` adapter must establish before a
/// reading is allowed to count as a change.
///
/// A subject being written while it is read is not a different subject, and the
/// value a reader gets out of that is one nobody on the system ever held. This
/// module is the discipline commercial automation applies to it with: two
/// independent reads must agree, and a reading that cannot be confirmed is
/// reported as [`BotError::UnstableObservation`] — pending, never a change.
///
/// Wired rather than offered: [`domain::data::JsonStore`] reads through
/// [`stability::read_stable_file`], so every poll of a JSON-backed store is
/// confirmed before it is reported.
#[doc(hidden)]
pub mod stability;
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
#[doc(hidden)]
pub mod task;
/// The estate-owned durable keyed store behind multi-tenant run records:
/// tenant policies, run rows and lane rows with per-tenant keyed reads. Every
/// row persists through the wire encoding, so the store exists with `wire`.
#[cfg(feature = "wire")]
#[doc(hidden)]
pub mod tenant_store;
/// The four verbs: Observe, Evaluate, Execute, Query. No fifth verb exists.
#[doc(hidden)]
pub mod verb;

#[doc(hidden)]
pub use cap::{Auth, Cap, Deficit, Demand, Shortage};
#[doc(hidden)]
#[cfg(feature = "ecs")]
pub use ecs::{
    DEFAULT_POLL_DEADLINE, ForcedRefresh, MAX_POLL_DEADLINE, StalledSource, SupersededObservation,
    TickReport,
};
/// The per-stage tick profiler (feature `profile`): six named stages and the
/// measured time each spent.
///
/// A measurement instrument rather than a capability, so it is default-off and
/// re-exported on its own line rather than inside the four-verb list — the same
/// reason [`RefreshReason`] is. `bench/` is the caller: the ratio it publishes
/// against a hand-rolled loop is only readable once someone says which stage the
/// ratio is made of.
#[cfg(feature = "profile")]
#[doc(hidden)]
pub use ecs::{TickProfile, TickStage};
#[doc(hidden)]
pub use effect::{EventId, InputIdentity};
#[doc(hidden)]
pub use error::{BotError, DispatchCertainty, RetryClass};
#[doc(hidden)]
pub use gate::GrantSet;
#[doc(hidden)]
pub use language::{Alias, LanguageResolver};
#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
#[doc(hidden)]
pub use registry::{Action, ActionCtor, Condition, DomainRegistry, Source, SourceCtor};
#[cfg(feature = "rt")]
#[doc(hidden)]
pub use rt::runtime::{Builder, Handle, Runtime, block_on};
#[doc(hidden)]
pub use semantic::{Embedder, EmbedderIdentity, SemanticError, SemanticPolicy, SemanticResolver};
#[doc(hidden)]
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
/// The live bot itself, behind `ecs` like the substrate it executes on.
#[cfg(feature = "ecs")]
#[doc(hidden)]
pub use spec::Bot;
#[doc(hidden)]
pub use spec::{Admission, BotSpec, Need, NeedSet};

/// Write orchestration as indented blocks that expand to bounded,
/// deadline-carrying, tenant-keyed flows over [`script`](mod@script), plus an
/// `ARCHITECTURE` map of what was declared. The language is documented on
/// the [`script`](mod@script) module and in `lgwks_macros`.
///
/// # Examples
///
/// A flow that sizes every page with a second per step. A limit that comes from
/// outside, such as an upstream's quota, is written `at most (quota) at once`;
/// a literal there is refused at compile time, because it is right on one host
/// and wrong on the next.
///
/// ```
/// # use lgwks_bot::script::{FlowError, Scope, Tenant};
/// lgwks_bot::script! {
///     pub flow total(pages: Vec<u64>) -> u64:
///         let sizes = each page in pages:
///             within 1s:
///                 page.saturating_add(1)
///         give back sizes.iter().sum()
/// }
///
/// # fn main() -> Result<(), FlowError> {
/// let scope = Scope::root(Tenant::new("acme")?);
/// assert_eq!(lgwks_bot::block_on(total(&scope, vec![1, 2, 3]))?, 9);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "script")]
pub use lgwks_macros::script;
#[doc(hidden)]
pub use verb::EffectLifetime;
/// Why a source cannot be trusted to answer "has this changed?" this tick, and the
/// statement it makes through [`Observe::cache_state`].
///
/// Re-exported on its own line rather than inside the four-verb list below,
/// because it is not a verb. `verb::tests::the_crate_root_reexports_exactly_those_four_verbs`
/// reads that list as the four verbs and nothing else, so a fifth name beside them
/// would make the invariant it guards fail for a type that is not a verb.
#[doc(hidden)]
pub use verb::RefreshReason;
#[doc(hidden)]
pub use verb::{Evaluate, Execute, Observe, Query};

/// Typed in-process structural inspection (feature `inspect`).
///
/// [`Verdict`](inspect::Verdict) stays under `inspect::` because the session
/// surface already exports a `Verdict` at the crate root; the two are unrelated
/// and one name cannot mean two things.
#[cfg(feature = "inspect")]
#[doc(hidden)]
pub use inspect::{
    Budgets, Finding, InspectRequest, Inspection, RuleCoverage, RuleSet, UnsupportedReason, inspect,
};

/// Wait for all of a set of futures, returning their outputs in input order.
///
/// Re-exported from the crate-owned driver so callers never name it. For a
/// fan-out that must be bounded, prefer `rt::task::join_all_bounded`.
#[cfg(all(feature = "rt", feature = "macros"))]
#[doc(hidden)]
pub use lgwks_deps::tokio::join;

/// Race a set of futures, running the first branch that becomes ready.
///
/// Re-exported from the crate-owned driver so callers never name it.
#[cfg(all(feature = "rt", feature = "macros"))]
#[doc(hidden)]
pub use lgwks_deps::tokio::select;

/// Wait for all of a set of fallible futures, short-circuiting on the first
/// error. Re-exported from the crate-owned driver so callers never name it.
///
/// Every branch is polled on the task that awaits the macro, so the branches may
/// borrow the caller's locals. The result is `Ok` with a tuple of the branches'
/// values in the order written when every branch succeeds, or the first `Err`
/// any branch returns, which drops the branches still running. In `script!` it
/// is the `together:` block.
///
/// # Examples
///
/// ```
/// # use lgwks_bot::script::{FlowError, Scope, Tenant};
/// lgwks_bot::script! {
///     pub flow pair(n: u64) -> u64:
///         together:
///             let doubled = n.saturating_mul(2)
///             let tripled = n.saturating_mul(3)
///         give back doubled.saturating_add(tripled)
/// }
///
/// # fn main() -> Result<(), FlowError> {
/// let scope = Scope::root(Tenant::new("acme")?);
/// assert_eq!(lgwks_bot::block_on(pair(&scope, 10))?, 50);
/// # Ok(())
/// # }
/// ```
#[cfg(all(feature = "rt", feature = "macros"))]
pub use lgwks_deps::tokio::try_join;
