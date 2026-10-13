//! The ledger's repairs: generation, barrier and binding, driven on the synchronous adapter and so not guarded.

use super::*;

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

/// A settlement is accepted only for the binding it names.
///
/// The counterexample this closes, stated as the review states it: the entry at
/// `(0, 0)` is indeterminate under one binding, and the caller reads
/// `pending()`. Its report is delayed. The entry is settled, the transition
/// resolves, the source moves, and the same slot is held again — under a second
/// binding. The delayed report for the first binding then arrives.
///
/// Before the repair it was accepted, because `resolve_effect` named only
/// `(chain, entry)` and the ledger only ever holds one transition per chain, so
/// a slot is reused by every generation over it and a `WorkId` alone cannot tell
/// them apart. `Applied` acknowledged an effect at a generation nobody asked
/// about; `NotApplied`, worse, made an attempt the caller never saw eligible for
/// a retry. The action digest — the binding of the payload and the generation
/// the attempt ran under — is the part of the identity that survives the reuse,
/// and it is what [`EffectKey`] carries and what this binds.
///
/// Three more things are asserted here, because they are the same invariant read
/// from the other side: a settlement repeated for its own binding is a no-op
/// that succeeds, a settlement *contradicting* its own binding's is refused,
/// and neither refusal is `NoSuchWork` — that variant has to keep meaning "there
/// is no held effect here", or a caller reading it for a stale report is being
/// told the wrong thing about work that is still held.
#[test]
fn a_settlement_names_the_binding_it_settles_and_no_other() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let uncertain = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = doubtful_bot("generations", &value, &uncertain, &seen)?;

    // Revision 1, held: the attempt may be live, so nothing settles it on its
    // own and the tick says so rather than guessing.
    match bot.tick() {
        Ok(fired) => {
            return Err(format!("an indeterminate effect was reported as {fired} fired").into());
        }
        Err(error) => assert!(
            matches!(error, BotError::EffectIndeterminate { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    let first = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported")?;
    let first_binding = binding(&first)?;
    assert!(
        matches!(
            first.hold(),
            TransitionHold::OutcomeUnknown { attempts: 1, .. }
        ),
        "held as an unknown outcome, not as a decision: {:?}",
        first.hold()
    );

    // The caller's own settlement for the first binding, naming the first
    // binding. Accepted, and the generation it settles is now decided, so the
    // transition has nothing open and is dropped by the next tick.
    bot.resolve_effect(&first_binding, EffectEvidence::Applied)?;
    assert_eq!(bot.tick()?, 0, "an acknowledged effect is not replayed");
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is resolved: {:?}",
        bot.pending()
    );

    // The source moves and the same slot is held again — under a second
    // binding. The `WorkId` is identical to the one above; only the binding
    // differs, which is the whole point.
    uncertain.set(true);
    value.set(2);
    match bot.tick() {
        Ok(fired) => {
            return Err(format!("an indeterminate effect was reported as {fired} fired").into());
        }
        Err(error) => assert!(
            matches!(error, BotError::EffectIndeterminate { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    let second = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported again")?;
    let second_binding = binding(&second)?;
    assert_eq!(second.id(), first.id(), "the slot is the same one");
    assert_ne!(
        second_binding, first_binding,
        "the binding it belongs to is not, which is the fact identity loses"
    );
    assert_eq!(
        *seen.borrow(),
        vec![1, 2],
        "each attempt ran against the value its own transition was opened under"
    );

    // The delayed report for the first binding arrives now. It is the same
    // report the caller read above, held across a tick and a movement — a move
    // rather than a clone, because that is what the caller has: the one report
    // it was given.
    //
    // The refusal is asserted as the exact variant and not merely as "not
    // `NoSuchWork`". `EvidenceSuperseded` naming both bindings is the invariant:
    // the caller has to be able to tell that its report was about a binding that
    // is gone, which binding it named, and which one stands there now — a
    // generic refusal would leave it guessing whether to retry the report or
    // re-read the work.
    let delayed = first_binding;
    // Both evidence kinds are refused by the same check, so they are refused by
    // the same closure: a helper written for `Applied` alone would let
    // `NotApplied` pass for a reason nobody checked.
    let refuse_as_superseded = |bot: &mut Bot, evidence: EffectEvidence| -> TestResult {
        match bot.resolve_effect(&delayed, evidence) {
            Ok(()) => Err(format!(
                "{evidence:?} for the first binding was accepted against the second"
            )
            .into()),
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        BotError::EvidenceSuperseded { work, named, current }
                            if work == second.id()
                                && named == delayed.digest()
                                && current == second_binding.digest()
                    ),
                    "expected the named binding and the live one to be reported, got {error:?}"
                );
                assert_eq!(
                    held(bot).map(|(chain, entry, key, _)| (chain, entry, key)),
                    Some((0, 0, Some(second_binding))),
                    "the refusal left the held binding exactly as it was: {:?}",
                    bot.pending()
                );
                Ok(())
            }
        }
    };
    refuse_as_superseded(&mut bot, EffectEvidence::Applied)?;

    // `NotApplied` is the sharper half: accepting it would make an attempt the
    // caller was never asked about eligible to run again. It is refused by the
    // same variant — the reason is the binding, not the evidence.
    refuse_as_superseded(&mut bot, EffectEvidence::NotApplied)?;
    assert_eq!(
        held(&bot).map(|(chain, entry, key, _)| (chain, entry, key)),
        Some((0, 0, Some(second_binding))),
        "the entry is still held and still refused, not restarted: {:?}",
        bot.pending()
    );

    // The current binding, settled with the evidence that is actually about it:
    // accepted.
    bot.resolve_effect(&second_binding, EffectEvidence::Applied)?;

    // The same identity and the same evidence again is a duplicate report, not
    // a contradiction and not an error: a caller whose first delivery was
    // ambiguous has to be able to repeat it.
    bot.resolve_effect(&second_binding, EffectEvidence::Applied)?;

    // Evidence contradicting what this generation was settled with is refused,
    // and the settlement it contradicts stands. The refusal names both pieces of
    // evidence, because "your report was refused" without saying which way round
    // leaves the caller unable to tell whether it misread its own observation.
    match bot.resolve_effect(&second_binding, EffectEvidence::NotApplied) {
        Ok(()) => {
            return Err(
                "evidence contradicting the generation's own settlement was accepted".into(),
            );
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::EvidenceContradicted {
                    settled: EffectEvidence::Applied,
                    submitted: EffectEvidence::NotApplied,
                    ..
                }
            ),
            "expected the recorded and the refused evidence to be reported, got {error:?}"
        ),
    }
    assert!(
        bot.pending().is_empty(),
        "the acknowledged generation is still acknowledged: {:?}",
        bot.pending()
    );
    assert_eq!(
        bot.tick()?,
        0,
        "an acknowledged effect is never replayed, whatever was said about it afterwards"
    );
    assert_eq!(*seen.borrow(), vec![1, 2], "and no attempt was run for it");
    Ok(())
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

#[path = "ledger/barriers.rs"]
mod barriers;
