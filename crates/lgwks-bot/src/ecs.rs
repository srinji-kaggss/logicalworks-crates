//! The four verbs on a `bevy_ecs` substrate.
//!
//! [`Bot`] executes a spec by polling every source on every tick and
//! evaluating every condition on the freshly polled value. That is correct and
//! it re-derives an answer it already had: a source that did not move is
//! evaluated anyway. This module keeps the same four verbs and the same
//! `(condition, action)` tuples, and moves the *execution* onto an ECS where the
//! condition is bevy's own change detection.
//!
//! # The mapping
//!
//! | Verb | On this substrate |
//! |---|---|
//! | `GrantSet` | the `Grants` resource: one authority per world |
//! | `Observe` | a `Chains` entry, polled by the `observe_fold` system |
//! | `Evaluate::check` | `Changed<Revision>` on the source entity |
//! | `Execute::execute_action` | the `fire_plan` system, whose effects run between schedule steps |
//! | `Bot::tick()` | [`EcsBot::tick_async`], one `Schedule::run` between two awaited phases |
//!
//! # One execution path, and who drives it
//!
//! A tick is four phases, and only the middle one is a synchronous schedule
//! step:
//!
//! 1. **observe** — every source is polled, in bounded waves, awaited on the
//!    caller's executor.
//! 2. **decide** — one `Schedule::run` folds those observations into the world
//!    (committing each value and bumping `Revision` where it moved) and records
//!    the effect program for this tick.
//! 3. **act** — the recorded effects run in declaration order, awaited on the
//!    caller's executor.
//! 4. **report** — the count and the first failure are written back.
//!
//! [`EcsBot::tick_async`] is the whole of it, and it is the way to run a bot
//! from async code: the verb futures are awaited *by* the caller's runtime, so
//! a source that waits on a timer, a socket, or a sibling task has a driver
//! making progress underneath it. [`EcsBot::tick`] is a synchronous adapter
//! over the same four phases for verbs that need no reactor; it is refused,
//! not hung, when an async runtime is already driving the calling thread.
//!
//! # One semantic delta, stated rather than discovered
//!
//! [`Bot::tick`] evaluates a condition on **every** tick.
//! `EcsBot::tick` evaluates it only on a tick where the observed value
//! **moved**. For a condition that stays true ("status is 500" while the
//! endpoint stays down) the first fires on every tick and the second fires
//! once, on the transition. That is the point of the substrate, and it is a
//! behaviour change rather than a re-implementation: a bot that must act on a
//! *held* state belongs on `Bot`, and a bot that acts on a *transition* belongs
//! here.
//!
//! # Work outlives the change filter
//!
//! `Changed<Revision>` says a chain is *eligible*; it is not a record of what
//! still has to happen. Committing the revision and then failing halfway marks
//! the source handled, so the entries the failure skipped are never reached on
//! an unchanged source, and a failure in the first chain stops every later
//! chain as well, because the loop that stops is outside the per-chain one.
//!
//! Eligible work therefore lives in its own structure — the ledger — keyed by
//! `(chain, entry)` and by the revision that made it due, and `fire` walks
//! *that* rather than the change set. The filter is consulted only to open a
//! transition; a chain with work outstanding is walked on every tick until the
//! work is settled, whether or not its source moves. One transition per chain
//! at a time: a source that moves again while its work is outstanding joins the
//! transition in progress, because the observed value is the only one the
//! substrate keeps (`Box<dyn Any>` is compared, not cloned) and a second
//! transition would replay effects the first already acknowledged.
//!
//! An entry is never skipped to reach a later one. The walk resumes at the
//! first entry that is not resolved and stops at the first it cannot settle, so
//! an effect that already happened is never replayed to get past a successor —
//! the failure mode where retrying a refused second entry duplicates the first
//! entry's merge. An entry that *may* have happened
//! ([`BotError::EffectIndeterminate`]) is held outright: it is attempted again
//! only when the caller supplies evidence, through `EcsBot::resolve_effect`.
//!
//! A tick is clean only when nothing is left holding a transition. When
//! something is, `EcsBot::tick` reports it — the entry, what is holding it, how
//! many entries are outstanding — so a clean subsequent tick cannot be read as
//! "the transition was handled". An error a source or an action produced keeps
//! precedence over that report, because it carries the typed variant a retry
//! classifier reads; the abandonment it caused is named by `EcsBot::pending`,
//! which lists every entry that is not finished, given-up entries included, with
//! the reason and the source revision. A tick that gives up on work is never
//! clean, and an entry it gave up on is never silently dropped.
//!
//! # A restart continues the record rather than the process
//!
//! [`EcsBot::tick`] is not a boundary the run has to survive: the journal is.
//! Every dispatch appends `IntentAdmitted` and `DispatchPrepared` *before* the
//! effect exists to hand over, so a process that dies mid-attempt leaves a
//! record of an attempt with no outcome behind it — and
//! [`EcsBot::into_journal`] is how a host carries that record into the next
//! process. Assembly folds in every attempt the record names, and the three
//! decisions it takes are deliberately different from one another:
//!
//! - **Outcome unknown** — nothing established whether the bytes arrived, so
//!   the action is *held*. The walk stops at it rather than beginning another
//!   attempt, the tick reports
//!   [`BotError::PendingTransition`], and
//!   the key a caller settles by is on the report.
//! - **Outcome landed** — the effect is done for the generation its key names,
//!   so the walk retires the entry rather than dispatching it again. That
//!   retirement is scoped to the generation: a source that moves opens the next
//!   one, and new work, which is the only thing that may legitimately re-run an
//!   acknowledged action.
//! - **Outcome missing or negative** — an admitted intent that was never handed
//!   over, or a delivery that was established not to have happened. Neither
//!   leaves anything held, so the entry is eligible again — under a *new*
//!   attempt identity, because the journal's ladder allows one walk per key and
//!   the attempt the record already holds cannot be minted twice.
//!
//! # Limits
//!
//! A panic that unwinds out of an action's future unwinds out of `fire` and out
//! of `tick` with it. The walk holds the chain's transition while it runs, so
//! that unwind loses the transition for the chain in flight — the entries it had
//! already resolved with it. Nothing in this module catches an unwind, and the
//! in-flight record it writes before an attempt (`EntryState::Unrecorded`) is
//! therefore durable for a tick that returns, not for a thread that panics. A
//! caller that catches an unwind around `tick` must treat that chain's work as
//! unknown rather than as handled.
//!
//! # What is measured, and what is deliberately not done
//!
//! Three findings from the measurement behind this module (`bevy_ecs` 0.19.1,
//! `default-features = false, features = ["std"]`), each with a control:
//!
//! - **`NonSend` is the only route for a verb.** `Component: Send + Sync +
//!   'static` is unconditional in this version and there is no `non_send`
//!   feature to relax it, so a `Box<dyn Any>` (which the verb erasure produces
//!   and which is not `Send`) cannot be a component. The observers, the
//!   entries and the observed values all live in `NonSend` resources, reachable
//!   only from an exclusive system. The crate's non-`Send` contract is
//!   preserved rather than tightened.
//! - **`Changed<T>` is visible to a second exclusive system in the same tick,
//!   and it is precise.** Against an input that holds still the change filter
//!   matched `[2, 0, 2, 0, 2]` (the ticks where the value actually moved),
//!   which is the whole reason a `Revision` marker exists rather than a
//!   "this system ran" flag.
//! - **Effects do not go through `Commands`.** `auto_insert_apply_deferred`
//!   defaults to `true` but inserts the sync point only where some system is
//!   ordered *after* the writer; a `Commands`-writing system with no successor
//!   has its queue dropped under a manually stepped `Schedule::run`, measured
//!   as 0 effects where 3 were expected. Adding a bare `ApplyDeferred` is not a
//!   fix either: unordered relative to the writer it runs first and defers the
//!   effect by a tick, giving 2 of 3. Both are silent. Here the effect is a
//!   direct resource write from an exclusive system, ordered by `.chain()`.
//!
//! # One path
//!
//! This is how a bot executes, not a candidate the caller may decline. There is
//! no `ecs` feature: `Bot` *is* this, and the project's build and test gate
//! compiles and tests it like any other code. A default-off flag would have left
//! it unexercised and unowned.
//!
//! # Build-time validation
//!
//! `EcsObserveBuilder::build` runs `Schedule::initialize()` with
//! `ambiguity_detection: LogLevel::Error`, so a schedule whose systems cannot be
//! totally ordered is a refusal at build rather than a silent misordering at
//! tick. An exclusive system cannot return an error, so a failure at *tick* time
//! is recorded in the `TickError` resource and surfaced by
//! `EcsBot::tick`: the same error ordering `Bot::tick` documents, where the
//! first error in declaration order is returned.

// `self` is load-bearing for the derives in the child modules.
/// The per-stage tick profiler. Compiled out entirely without the default-off
/// `profile` feature: every charge site in this file sits behind the same
/// `cfg`, so a build without it reads no clock and does no per-stage arithmetic.
#[cfg(feature = "profile")]
mod profile;
#[cfg(feature = "profile")]
#[cfg(feature = "profile")]
pub use profile::{TickProfile, TickStage};

// The substrate: every module that names `bevy_ecs`, behind `ecs` (#687).
#[cfg(feature = "ecs")]
mod admit;
#[cfg(feature = "ecs")]
mod bot;
#[cfg(feature = "ecs")]
mod bot_poll;
#[cfg(feature = "ecs")]
mod bot_run;
#[cfg(feature = "ecs")]
mod builder;
#[cfg(feature = "ecs")]
mod effects;
#[cfg(feature = "ecs")]
mod entry;
#[cfg(feature = "ecs")]
mod fold;
#[cfg(feature = "ecs")]
mod ledger;
#[cfg(feature = "ecs")]
mod resources;
// Runs into the world: the admitted and settled events (ADR 0006 R3).
#[cfg(feature = "ecs")]
mod runs;
#[cfg(feature = "ecs")]
mod watchdog;
// The admitted-input identity the registry derives for `script` and
// `inspect` as well as the substrate.
#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
mod identity;
#[cfg(feature = "ecs")]
mod scope;
// The vocabulary the substrate reports in, built in every configuration.
mod work;

#[cfg(feature = "ecs")]
pub use bot::EcsBot;
#[cfg(feature = "ecs")]
pub use builder::EcsBuilder;
#[cfg(feature = "ecs")]
pub use builder::EcsObserveBuilder;
pub use lgwks_std::retry::RetryPolicy;
#[cfg(feature = "ecs")]
pub use resources::ForcedRefresh;
#[cfg(feature = "ecs")]
pub use resources::StalledSource;
#[cfg(feature = "ecs")]
pub use resources::SupersededObservation;
#[cfg(feature = "ecs")]
pub use resources::TickReport;
#[cfg(feature = "ecs")]
pub use runs::{RunCounts, RunFeed, RunOutcome, SettledRun};
#[cfg(feature = "ecs")]
pub(crate) use runs::{RunFeedError, RunTicket};
#[cfg(feature = "ecs")]
pub use scope::EffectScope;
#[cfg(all(feature = "ecs", feature = "ephemeral"))]
pub use scope::EphemeralError;
#[cfg(feature = "ecs")]
pub use watchdog::DEFAULT_POLL_DEADLINE;
#[cfg(feature = "ecs")]
pub use watchdog::MAX_POLL_DEADLINE;
pub use work::AbandonReason;
#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
pub(crate) use work::AdmittedInput;
pub use work::EffectEvidence;
pub use work::PendingWork;
pub use work::TransitionHold;
pub use work::WorkId;
#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
pub(crate) use work::identify_output;
#[cfg(any(feature = "ecs", feature = "script", feature = "inspect"))]
pub(crate) use work::same_output;

#[cfg(all(test, feature = "ecs"))]
pub(super) mod tests;
