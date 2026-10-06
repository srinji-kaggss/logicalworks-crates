//! `verb` owns the four fixed verb traits and enforces INV-BOT-FOUR-VERBS:
//! Observe, Evaluate, Execute, Query. No fifth verb without a crate-level change.
//!
//! [`RefreshReason`] is not a verb. It is the typed statement a source makes
//! about its own caching through [`Observe::cache_state`], and it exists because
//! the substrate's "unchanged, so do not look again" shortcut is only sound
//! while the value it compares against is too.

use super::cap::{Auth, Cap};
use super::error::BotError;
use std::future::Future;

// ── Observe ────────────────────────────────────────────────────────────────

/// Why a source cannot be trusted to answer "has this changed?" this tick.
///
/// The substrate keeps a value per source and skips a poll whose answer would
/// be the value it already holds. That shortcut is only sound while the
/// *baseline* is sound, and four things can make it unsound without the
/// substrate noticing: the transport went away, a bounded queue overflowed and
/// dropped the changes the baseline would have absorbed, a remote key the
/// baseline was derived from has been rotated or evicted, or an invalidation
/// the source owes its reader could not be delivered. In every one of them the
/// substrate's answer to "unchanged" is indistinguishable from "I stopped
/// looking", so each one forces the next tick to re-observe rather than settle
/// into a permanent quiet state.
///
/// Typed rather than a boolean for the same reason the rest of this crate is:
/// the caller's repair differs per cause. A reconnect is not a stale key, and a
/// queue that overflowed needs a bound raised rather than a key rotated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RefreshReason {
    /// The transport this source reads over is gone.
    ///
    /// A subscription, a socket, or a handle the domain holds went away. The
    /// cached value is still the last thing it saw, which is not the same as
    /// the thing it is now.
    Disconnected,
    /// A bounded queue or ring between the producer and this source overflowed.
    ///
    /// The baseline absorbed a window of changes the overflow ate. Nothing that
    /// happened inside that window was ever compared, so the baseline is stale
    /// by an amount nobody can name.
    WatchOverflow,
    /// The remote key this source's baseline was derived from is no longer the
    /// current one — rotated, evicted, or aged out.
    ///
    /// The value is readable and unchanged, which is exactly the trap: a poll
    /// against the new key would return the *new* revision's value, and the
    /// comparison against a value keyed on the old one can never match.
    StaleRemoteKey,
    /// An invalidation this source owes its reader could not be delivered.
    ///
    /// The producer has moved on and the notification that would have said so
    /// did not arrive, so nothing distinguishes "nothing changed" from "the
    /// change was never announced".
    InvalidationFailed,
    /// An intermediate value was replaced before any entry acted on it.
    ///
    /// Reported only in latest-state mode, and only where the replacement is
    /// the substrate's own decision rather than a domain failure: the walk was
    /// still walking generation *N* when generation *N+1* arrived, so *N* will
    /// never be acted on and *N+1* will be. That is not a loss — the value was
    /// never due — but it is also not a success and not a retire, and a report
    /// that folded it into either would answer a question this is the answer to.
    Superseded,
}

impl RefreshReason {
    /// Whether this reason means the source's cached baseline is unsound.
    ///
    /// [`Self::Superseded`] is the one arm that does not: nothing was lost, and
    /// the newer value that replaced it is already on its way through the
    /// walk. It is reported for the same caller-visible reason — "you did not
    /// see every value" — and it is deliberately not a reason to re-observe,
    /// because the value that would be re-observed is the one already queued.
    #[must_use]
    pub const fn invalidates_baseline(self) -> bool {
        !matches!(self, Self::Superseded)
    }

    /// A stable short name, for a report that is keyed by string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::WatchOverflow => "watch-overflow",
            Self::StaleRemoteKey => "stale-remote-key",
            Self::InvalidationFailed => "invalidation-failed",
            Self::Superseded => "superseded",
        }
    }
}

impl core::fmt::Display for RefreshReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// Declared as a return-position [`Future`] rather than `async fn` so the
    /// trait carries no crate-level lint suppression: the returned future is
    /// local to the driving thread (not `Send`), because `lgwks_std::task`
    /// drives bots on one thread and a domain may hold thread-local state; a
    /// `Send` bound would force every domain to be `Send` and rule out the
    /// thread-local state `Bot::tick` is built for. `Bot::tick` polls every
    /// source concurrently.
    fn poll(&self, call: (Auth, ())) -> impl Future<Output = Result<Self::Output, BotError>>;

    /// The state of this source's own caching, when it has one.
    ///
    /// Returns `None` for a source that holds no cache and therefore has no
    /// baseline to invalidate: every one of its polls is a fresh read and there
    /// is nothing to force. A source that *does* cache must say so here, and
    /// must return a reason rather than a value the substrate would have to
    /// guess the meaning of.
    ///
    /// Polled after [`Observe::poll`], on the same tick, and read without
    /// awaiting anything: it is a statement about state the poll already
    /// touched, not another call into the source. The substrate forces its next
    /// poll exactly when this reports a
    /// [`RefreshReason::invalidates_baseline`]
    /// reason, and records the reason on the tick report.
    fn cache_state(&self) -> Option<RefreshReason> {
        None
    }

    /// This source's own revision of its value, when it has one.
    ///
    /// The cheap answer to "has anything moved?", and the substrate's second
    /// change-detection path beside the value comparison it always does. When a
    /// source reports `Some(revision)` and that revision is the one the substrate
    /// last **committed** for this chain, the substrate **skips the poll
    /// entirely**: no future is built, no value is boxed, and nothing is
    /// allocated. The check is one call and one integer comparison.
    ///
    /// # The contract, which is the whole of the method
    ///
    /// **A revision moves if and only if the value moves.** Both directions, and
    /// the cost of each failure is different:
    ///
    /// - A revision that does **not** move while the value does is a **missed
    ///   effect**: the bot goes quiet about a change that happened. That is the
    ///   failure this substrate exists to avoid, and nothing downstream can
    ///   recover it.
    /// - A revision that moves while the value does is a **spare poll**: the
    ///   source is polled, the value compares equal, nothing commits, and the
    ///   substrate asks again next tick. It costs a poll and never suppresses
    ///   work — which is why the substrate does not "correct" it by committing
    ///   the revision anyway: believing a revision that lied once is how a later
    ///   real change is skipped.
    ///
    /// So the substrate believes a reported revision, and a source that cannot
    /// keep that promise returns `None`.
    ///
    /// # When `None` is the right answer, and it usually is
    ///
    /// **`None` is the default, and most sources are correct to leave it.** A
    /// revision only pays when something *other than the poll* maintains it. A
    /// source whose poll is how it learns everything about the world — an HTTP
    /// `GET`, a file read, a `gh` snapshot, a process dispatch — has no revision
    /// to report: the poll is what would discover that anything moved, so asking
    /// first asks the question the poll answers. Those sources take the
    /// comparison path, which is what the substrate does anyway.
    ///
    /// A revision is for a source whose world is pushed at it: a subscription, a
    /// socket, a filesystem watcher, or a counter another component already
    /// maintains. For those, one comparison per tick replaces a poll.
    ///
    /// Every source this crate ships takes the comparison path, and the table of
    /// which takes which, with the reason for each, is in
    /// [`docs/bot-on-ecs.md`](https://github.com/srinji-kaggss/logicalworks-crates/blob/main/docs/bot-on-ecs.md)
    /// §4.1. It is a table rather than a claim because the answer is a property of
    /// each domain's poll, and a reader deciding whether to implement this is
    /// deciding whether their own source has a cheaper question to ask.
    ///
    /// # What it is compared against
    ///
    /// The revision of the value the substrate last **committed** for this chain,
    /// and the commitment is the only thing that moves it. A poll that failed, was
    /// cancelled at its deadline, or produced a value equal to the one already
    /// held commits no revision — so the next tick asks again. That is the
    /// fingerprint-commit rule (#99 / INV-BOT-5) applied to a change tick instead
    /// of a digest: **a revision is committed together with its value, or not at
    /// all**, and a revision committed alone would make a failed read look like
    /// "nothing changed" for ever.
    ///
    /// Equality is the only comparison. A revision that wraps, or regresses, is a
    /// *different* revision and is therefore a change: there is no ordering here
    /// to get wrong, and a source is free to use a saturating counter, a
    /// timestamp in its own units, or a content digest as its revision.
    ///
    /// Read on the calling thread before the poll it may skip, and never while a
    /// poll of this source is in flight: the revision belongs to the value the
    /// substrate holds, so a revision read beside a pending poll could describe a
    /// value the poll never returns.
    #[must_use]
    fn revision(&self) -> Option<u64> {
        None
    }

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

/// Whether the effect an action models reaches outside this process.
///
/// The distinction an ephemeral journal exists to enforce: a local effect is
/// gone with the process, and an external one outlives it. An adapter that
/// does not declare is treated as [`EffectLifetime::External`] — the
/// conservative default, because an unclassified handoff is exactly the one
/// that must not be allowed to leave on a record that cannot survive the writer
/// (issue #100).
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
    ///
    /// Declared as a return-position [`Future`] for the same reason as
    /// [`Observe::poll`]: no crate-level suppression, and the future stays
    /// local to the driving thread.
    fn execute_action(
        &self,
        call: (Auth, &Self::Input),
    ) -> impl Future<Output = Result<Self::Output, BotError>>;

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
    /// Declared as a return-position [`Future`] for the same reason as
    /// [`Observe::poll`]: the future stays local to the driving thread and the
    /// trait needs no crate-level lint suppression.
    fn query(
        &self,
        call: (Auth, &Self::Input),
    ) -> impl Future<Output = Result<Self::Output, BotError>>;

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
            // A split yields at least one piece, so the first piece is the
            // declared name; an empty piece (or none) means the line names no
            // trait and the line is skipped rather than recorded under "".
            let Some(name) = rest
                .split(|character: char| !(character.is_alphanumeric() || character == '_'))
                .next()
                .filter(|name: &&str| !name.is_empty())
            else {
                continue;
            };
            names.push(name);
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

    /// Every reason a source can declare, and whether it invalidates the
    /// baseline the substrate compares against.
    ///
    /// Written as one table rather than four separate tests because the property
    /// is the *partition*: exactly one arm must decline to invalidate, and an
    /// arm added later that defaults the wrong way would not fail any of four
    /// independent tests.
    #[test]
    fn only_supersession_leaves_the_baseline_sound() {
        let cases: [(RefreshReason, bool, &str); 5] = [
            (RefreshReason::Disconnected, true, "disconnected"),
            (RefreshReason::WatchOverflow, true, "watch-overflow"),
            (RefreshReason::StaleRemoteKey, true, "stale-remote-key"),
            (
                RefreshReason::InvalidationFailed,
                true,
                "invalidation-failed",
            ),
            (RefreshReason::Superseded, false, "superseded"),
        ];
        for (reason, invalidates, name) in cases {
            assert_eq!(
                reason.invalidates_baseline(),
                invalidates,
                "{name} invalidates the cached baseline: {invalidates}"
            );
            assert_eq!(reason.as_str(), name, "the report keys on this spelling");
            assert_eq!(
                reason.to_string(),
                name,
                "Display and the stable name are one spelling, not two"
            );
        }
    }
}
