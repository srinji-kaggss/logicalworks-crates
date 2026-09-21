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
    ///
    /// On a source that implements [`fingerprint`](Observe::fingerprint), this
    /// is called only on the ticks the fingerprint moved. A tick where it did
    /// not is a tick this method never runs on — see that method for what the
    /// source is promising when it answers one.
    async fn poll(&self, call: (Auth, ())) -> Result<Self::Output, BotError>;

    /// A cheap digest of what `poll` would return, or `None` to say there is
    /// no cheaper answer than polling.
    ///
    /// # What this is for
    ///
    /// Change detection is an *equality* question. The substrate reduces every
    /// observation to one bit — "is this the same as what I hold?" — and
    /// discards the value. Making the source produce that value in order to ask
    /// the bit is the eager part of the loop: on a source that is holding still,
    /// the tick builds, erases, boxes and drops a value whose only surviving
    /// property was that it compared equal.
    ///
    /// A source that can answer the equality question directly — an `ETag`, an
    /// `mtime`, a row version, a sequence number, a mutation counter, a hash of
    /// a framebuffer — answers it here instead, and the tick never calls `poll`
    /// and never constructs the value at all.
    ///
    /// # The contract, and it is exact
    ///
    /// **Equal fingerprints must imply equal values.** If this returns the same
    /// digest it returned when the chain last admitted a value, the tick
    /// concludes the source has not moved and evaluates nothing. A fingerprint
    /// that collides across two genuinely different values makes the substrate
    /// miss a movement, which is the one failure this must not have: it is a
    /// silent no-op, not a wrong effect. Widening the digest lowers the risk;
    /// so does never returning a digest for a source whose cheap key is not a
    /// faithful function of its value.
    ///
    /// A source that is unsure returns `None`. `None` is not a failure and is
    /// not slower than today — it is today, exactly.
    ///
    /// # Two more things it promises
    ///
    /// *Cheap.* This is called once per chain per tick, unconditionally, on the
    /// hot path. Work proportional to the value defeats the purpose.
    ///
    /// *Pure with respect to the value.* Calling it must not change what `poll`
    /// returns, and it takes no [`Auth`] for the same reason
    /// [`Evaluate::check`] takes none: it is a read of state the source already
    /// holds, not an act on the world. A source that has to *contact* something
    /// to produce a digest has not found a cheaper answer and should return
    /// `None` — the point is to avoid the round trip, not to move it here.
    ///
    /// The default is `None`, so every source written before this method
    /// existed keeps behaving exactly as it did.
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
