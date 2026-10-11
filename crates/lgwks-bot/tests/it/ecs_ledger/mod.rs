//! The ledger's repairs on a ticking `Bot`: generation, barrier, and binding.
//!
//! Moved from `tests/ecs_tick.rs`, whose guarded bodies need a process of
//! their own; these drive immediate verbs on the synchronous adapter and
//! cannot park, so they link into the shared binary. The fixtures live here
//! and the tests in the two child modules.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lgwks_bot::spec::{
    AbandonReason, EffectEvidence, EffectKey, PendingWork, RetryPolicy, TransitionHold,
};
use lgwks_bot::{
    Auth, Bot, BotError, Cap, DispatchCertainty, EffectLifetime, Execute, GrantSet, Observe,
};

use crate::effects::memory_scope as test_effects;
use crate::poll::admit_poll;
use crate::undelivered::undelivered;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One attempt per entry, retried on no clock: the first definite failure
/// spends the budget and abandons the entry.
fn one_attempt() -> RetryPolicy {
    RetryPolicy::new(1, std::time::Duration::ZERO, std::time::Duration::MAX)
}

// ── The ledger's repairs: generation, barrier, and binding ─────────────────
//
// The tests below are not `guarded`, and the reason is the watchdog's subject
// rather than a shortcut: `guarded` exists for a body that may *never return* —
// a tick parked on a driver its own poll is blocking — so it re-executes the
// binary as a child and reads a marker. Every body here drives immediate verbs
// on the synchronous adapter and cannot park, so a watchdog would add a process
// spawn and, worse, replace an assertion's own diff with "the guarded child
// failed on its own". The failure this file's tests are read for is the
// assertion, so the assertion is what has to survive.

/// A source whose value the test moves by hand.
///
/// The three repairs are about what the substrate does *between* polls — which
/// generation a settlement belongs to, whether an abandoned entry is a barrier,
/// and which value an open transition is bound to — so the source has to be the
/// one part with no timing in it. A value the test sets makes "the source moved"
/// a fact the test states rather than one it waits for, which is what lets each
/// counterexample be written down exactly as the review states it.
struct Dial {
    /// The value the next poll yields.
    value: Rc<Cell<u32>>,
}

/// A source that counts real public-interface polls and can refuse once.
struct PollProbe {
    /// Stable public domain identity.
    domain: &'static str,
    /// The value returned after any one-shot refusal.
    value: u32,
    /// Number of calls made through `Bot::tick`.
    polls: Rc<Cell<u32>>,
    /// Whether this source's next poll must refuse.
    refuse_next: Rc<Cell<bool>>,
}

impl Observe for PollProbe {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        if self.refuse_next.replace(false) {
            let refusal = Err(BotError::DomainError {
                domain: self.domain.to_owned(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "one-shot poll refusal".to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "poll: returning an error to the caller");
            return refusal;
        }
        Ok(self.value)
    }

    fn fingerprint(&self) -> Option<u128> {
        Some(u128::from(self.value))
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

impl Observe for Dial {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(self.value.get())
    }

    fn domain_id(&self) -> &str {
        "test::dial"
    }
}

/// An action whose outcome the test chooses, recording the value it ran with.
///
/// `uncertain` is the state the ledger exists for: an attempt that returns
/// [`BotError::EffectIndeterminate`] may already be live, so the entry is held
/// rather than retried. Flipping it back is how a test produces a *settled*
/// generation and then a fresh one at the same slot, which is the shape every
/// generation counterexample needs.
struct Doubtful {
    /// Whether the next attempt is indeterminate instead of succeeding.
    uncertain: Rc<Cell<bool>>,
    /// The value each attempt actually ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for Doubtful {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.seen.borrow_mut().push(*call.1);
        if self.uncertain.get() {
            let refusal = Err(BotError::EffectIndeterminate {
                domain: "test::doubtful".to_owned(),
                cause: "the acknowledgment never arrived".to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "execute_action: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::doubtful"
    }
}

/// The binding and hold of the first entry the bot is still holding, as one
/// value, so an assertion can say *which generation* is held rather than only
/// that something is.
fn held(bot: &Bot) -> Option<(usize, usize, Option<EffectKey>, TransitionHold)> {
    bot.pending().first().map(|work| {
        (
            work.id().chain(),
            work.id().entry(),
            work.key(),
            work.hold().clone(),
        )
    })
}

/// The binding of a held entry, or a report that it has none.
///
/// An entry is given its [`EffectKey`] when an attempt begins, so a report that
/// has no key is a report about work nothing has dispatched — which is a
/// different fact from a key that failed to match, and one these tests never
/// expect to see.
fn binding(work: &PendingWork) -> Result<EffectKey, Box<dyn std::error::Error>> {
    work.key().ok_or_else(|| {
        format!(
            "chain {} entry {} is held with no attempt",
            work.id().chain(),
            work.id().entry()
        )
        .into()
    })
}

/// A one-chain bot whose only effect never settles: the fixture both identity
/// rows drive.
///
/// The generation row and the attempt row are the same property read from two
/// fields of the same binding, so they share one bot: a copy could differ in the
/// action's own bookkeeping and the two rows would then be asserting about
/// different machines.
fn doubtful_bot(
    name: &str,
    value: &Rc<Cell<u32>>,
    uncertain: &Rc<Cell<bool>>,
    seen: &Rc<RefCell<Vec<u32>>>,
) -> Result<Bot, Box<dyn std::error::Error>> {
    Ok(Bot::builder(name)
        .observe(Dial {
            value: Rc::clone(value),
        })
        .on(
            |_observed: &u32| true,
            Doubtful {
                uncertain: Rc::clone(uncertain),
                seen: Rc::clone(seen),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?)
}

/// An action that definitely did not happen, counting its attempts.
///
/// "Definitely" is the whole distinction: [`BotError::DomainError`] is the one
/// failure the substrate reads as a fact about the effect rather than a doubt
/// about it, so it is what spends an attempt budget and what eventually makes an
/// entry abandoned.
struct Refuses {
    /// Whether the next attempt refuses. The test flips this to let the entry
    /// through once it has been revived, which is how the barrier is shown to
    /// lift rather than merely to hold.
    refusing: Rc<Cell<bool>>,
    /// How many times the action was called.
    attempts: Rc<Cell<usize>>,
}

impl Execute for Refuses {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.attempts.set(self.attempts.get().saturating_add(1));
        if self.refusing.get() {
            let refusal = Err(undelivered("test::refuses", "refused"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "execute_action: returning an error to the caller");
            return refusal;
        }
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::refuses"
    }
}

/// An action that succeeds and records the value it ran against.
struct Noted(Rc<RefCell<Vec<u32>>>);

impl Execute for Noted {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.borrow_mut().push(*call.1);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::noted"
    }
}

mod barriers;
mod generations;
