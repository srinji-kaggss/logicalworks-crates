//! `verb` owns the four fixed verb traits and enforces INV-BOT-FOUR-VERBS:
//! Observe, Evaluate, Execute, Query. No fifth verb without a crate-level change.

use super::cap::Cap;
use super::error::BotError;

// ── Observe ────────────────────────────────────────────────────────────────

/// Watch a source. Poll, listen, stream. Produces a value each tick.
///
/// A domain that implements `Observe` can be bound to `(condition, action)`
/// tuples in a bot spec. The framework calls `poll` on the interval or event
/// the bot declares.
pub trait Observe {
    /// The value produced each observation tick.
    type Output;

    /// Capabilities this observer requires. Checked at `Bot::build()`.
    fn required_caps(&self) -> &[Cap];

    /// Poll the source for the current state.
    fn poll(&self) -> Result<Self::Output, BotError>;

    /// The domain identifier (e.g. `"gh::pr_status"`).
    fn domain_id(&self) -> &str;
}

// ── Evaluate ───────────────────────────────────────────────────────────────

/// Gate on a condition. Boolean over observed state. The `condition` half of
/// the `(condition, action)` tuple.
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
pub trait Execute {
    /// Input to the action.
    type Input;
    /// Output of the action.
    type Output;

    /// Capabilities this action requires. Checked at `Bot::build()`.
    fn required_caps(&self) -> &[Cap];

    /// Run the action.
    fn run(&self, input: &Self::Input) -> Result<Self::Output, BotError>;

    /// The domain identifier (e.g. `"notify::slack"`).
    fn domain_id(&self) -> &str;
}

// ── Query ──────────────────────────────────────────────────────────────────

/// Read without side effects. Direct call, no causal chain required.
pub trait Query {
    /// Input to the query.
    type Input;
    /// Output of the query.
    type Output;

    /// Capabilities this query requires.
    fn required_caps(&self) -> &[Cap];

    /// Run the query.
    fn query(&self, input: &Self::Input) -> Result<Self::Output, BotError>;

    /// The domain identifier.
    fn domain_id(&self) -> &str;
}
