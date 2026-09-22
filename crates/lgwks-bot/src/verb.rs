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
/// The framework calls `poll` once per tick for each source bound to a chain.
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

    /// A legacy, detached digest of the source state.
    ///
    /// # What this is for
    ///
    /// `EcsBot` no longer uses this method to suppress a later [`Observe::poll`]
    /// call. A value can change from A to B and back to A while the poll future
    /// is pending; a digest read before that future cannot be safely paired with
    /// the value the future eventually returns. Remove overrides of this method.
    ///
    /// The default remains `None` so existing observers keep compiling. This
    /// method will be removed in the next breaking release.
    #[deprecated(
        since = "0.5.0",
        note = "a detached fingerprint cannot be bound to an async poll result and is no longer used by EcsBot"
    )]
    fn fingerprint(&self) -> Option<u128> {
        None
    }

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

/// Perform a side effect. Capability-gated. The callable surface, the
/// `action` half of the `(condition, action)` tuple, invoked as
/// [`Execute::execute_action`].
///
/// Takes an `(Auth, input)` tuple: the proof must cover
/// [`required_caps`](Execute::required_caps) or `execute_action` denies before
/// Whether the effect an action models reaches outside this process.
///
/// The distinction an ephemeral journal exists to enforce: a local effect is
/// gone with the process, and an external one outlives it. An adapter that
/// does not declare is treated as [`Self::External`] — the conservative
/// default, because an unclassified handoff is exactly the one that must not
/// be allowed to leave on a record that cannot survive the writer (issue
/// #100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EffectLifetime {
    /// Local to this process. An ephemeral journal may host it.
    Local,
    /// Reaches a receiver outside this process: a file, a socket, a queue.
    /// Requires a journal that survives the writer dying, and the
    /// `DispatchPrepared` acknowledgment has to say so.
    External,
}

/// Perform a side effect. Capability-gated. The callable surface, the
/// `action` half of the `(condition, action)` tuple, invoked as
/// [`Execute::execute_action`].
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

    /// Whether the effect this action models reaches outside this process.
    ///
    /// Defaults to [`EffectLifetime::External`]. An action that is only local
    /// says so; an action that does not is refused at the handoff rather than
    /// admitted on a record that cannot outlive the process.
    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::External
    }

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

    /// The identifier the query reports in findings (e.g. `"gh::pr_status"`).
    fn domain_id(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four verbs INV-BOT-FOUR-VERBS fixes, as the module declares them.
    const VERBS: [&str; 4] = ["Observe", "Evaluate", "Execute", "Query"];

    /// Collects every `pub trait NAME` this source declares, in file order.
    ///
    /// The scan is deliberately textual: the invariant is about the *set of
    /// verbs*, and a test that asserted it through trait objects would fail to
    /// notice a fifth trait nobody has bound yet.
    fn declared_traits(source: &str) -> Vec<&str> {
        let mut names = Vec::new();
        for line in source.lines() {
            let Some(rest) = line.trim().strip_prefix("pub trait ") else {
                continue;
            };
            let name: &str = rest
                .split(|character: char| !(character.is_alphanumeric() || character == '_'))
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                names.push(name);
            }
        }
        names
    }

    /// Collects the names re-exported from `verb` by the crate root.
    ///
    /// A fifth verb reaches consumers through this list, so the list is where
    /// "without a crate-level change" is either kept or broken.
    fn reexported_from_crate_root(lib: &str) -> Vec<&str> {
        let Some((_, after)) = lib.split_once("pub use verb::{") else {
            return Vec::new();
        };
        let Some((names, _)) = after.split_once('}') else {
            return Vec::new();
        };
        names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect()
    }

    #[test]
    fn the_module_declares_exactly_the_four_reviewed_verbs() {
        let declared = declared_traits(include_str!("verb.rs"));
        assert_eq!(
            declared, VERBS,
            "INV-BOT-FOUR-VERBS fixes this set; a fifth verb is a crate-level change"
        );
    }

    #[test]
    fn the_crate_root_reexports_exactly_those_four_verbs() {
        let mut reexported = reexported_from_crate_root(include_str!("lib.rs"));
        reexported.sort_unstable();
        let mut expected = VERBS;
        expected.sort_unstable();
        assert_eq!(
            reexported, expected,
            "the crate root is the surface a fifth verb would arrive through"
        );
    }

    #[test]
    fn a_closure_is_still_an_evaluator() {
        let above_one = |value: &u8| *value > 1;
        let verdict = above_one.check(&2);
        assert!(
            matches!(verdict, Ok(true)),
            "the blanket impl is what binds a bot spec to a condition: {verdict:?}"
        );
        assert_eq!(
            above_one.condition_id(),
            "<closure>",
            "findings render this identifier, so it is part of the surface"
        );
    }
}
