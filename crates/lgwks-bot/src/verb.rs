//! `verb` owns the four fixed verb traits and enforces INV-BOT-FOUR-VERBS:
//! Observe, Evaluate, Execute, Query. No fifth verb without a crate-level change.

use super::cap::{Auth, Cap};
use super::error::BotError;

// ── Observe ────────────────────────────────────────────────────────────────

/// Watch a source. Poll, listen, stream. Produces a value each tick.
///
/// Takes an `(Auth, ())` tuple: the proof must cover [`required_caps`](Observe::required_caps)
/// or `poll` denies before touching the source. A domain that implements
/// `Observe` can be bound to `(condition, action)` tuples in a bot spec.
/// The framework calls `poll` on the interval or event the bot declares.
pub trait Observe {
    /// The value produced each observation tick.
    type Output;

    /// Capabilities this observer requires. Checked at `Bot::build()`, and
    /// proven per call by the `Auth` half of the tuple.
    fn required_caps(&self) -> &[Cap];

    /// Poll the source for the current state.
    ///
    /// Async: the returned future is local to the driving thread (not `Send`),
    /// because `lgwks_std::task` drives bots on one thread and a domain may hold
    /// thread-local state. `Bot::tick` polls every source concurrently.
    async fn poll(&self, call: (Auth, ())) -> Result<Self::Output, BotError>;

    /// The domain identifier (e.g. `"gh::pr_status"`).
    fn domain_id(&self) -> &str;
}

// ── Evaluate ───────────────────────────────────────────────────────────────

/// Gate on a condition. Boolean over observed state. The `condition` half of
/// the `(condition, action)` tuple.
///
/// Pure: takes no `Auth` because it performs no side effect and returns only
/// a boolean. Authority was proven when the observed value was produced and
/// is proven again when the action runs.
pub trait Evaluate<T> {
    /// Returns `true` when the condition is met.
    fn check(&self, value: &T) -> Result<bool, BotError>;

    /// The condition identifier (e.g. `"changed"`, `"threshold::below(5)"`).
    fn condition_id(&self) -> &str;
}

/// Blanket: closures are evaluators.
impl<T, F> Evaluate<T> for F
where
    F: Fn(&T) -> bool,
{
    fn check(&self, value: &T) -> Result<bool, BotError> {
        Ok((self)(value))
    }

    fn condition_id(&self) -> &str {
        "<closure>"
    }
}

// ── Execute ────────────────────────────────────────────────────────────────

/// Perform a side effect. Capability-gated. The callable surface — the
/// `action` half of the `(condition, action)` tuple, and directly invocable
/// via `bot.execute()`.
///
/// Takes an `(Auth, input)` tuple: the proof must cover
/// [`required_caps`](Execute::required_caps) or `execute_action` denies before
/// acting.
pub trait Execute {
    /// Input to the action.
    type Input;
    /// Output of the action.
    type Output;

    /// Capabilities this action requires. Checked at `Bot::build()`, and
    /// proven per call by the `Auth` half of the tuple.
    fn required_caps(&self) -> &[Cap];

    /// Perform the effect this action models, after `call.0` proves the
    /// required caps. Awaited by `Bot::tick` in chain order; blocking work
    /// belongs on a `lgwks_std::task::spawn_blocking` thread inside the domain.
    async fn execute_action(&self, call: (Auth, &Self::Input)) -> Result<Self::Output, BotError>;

    /// The domain identifier (e.g. `"notify::slack"`).
    fn domain_id(&self) -> &str;
}

// ── Query ──────────────────────────────────────────────────────────────────

/// Read without side effects. Direct call, no causal chain required.
///
/// Takes an `(Auth, input)` tuple like [`Execute`]: reads cross trust
/// boundaries too, so the proof must cover
/// [`required_caps`](Query::required_caps).
pub trait Query {
    /// Input to the query.
    type Input;
    /// Output of the query.
    type Output;

    /// Capabilities this query requires.
    fn required_caps(&self) -> &[Cap];

    /// Run the query.
    ///
    /// Async for the same reason as [`Observe::poll`].
    async fn query(&self, call: (Auth, &Self::Input)) -> Result<Self::Output, BotError>;

    /// The identifier the query reports in findings (e.g. `"gh::pr_state"`).
    fn domain_id(&self) -> &str;
}
