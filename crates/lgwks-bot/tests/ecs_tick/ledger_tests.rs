//! The ledger's repairs: generation, barrier, and binding.

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
pub(super) fn binding(work: &PendingWork) -> Result<EffectKey, Box<dyn std::error::Error>> {
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
pub(super) struct Noted(pub(super) Rc<RefCell<Vec<u32>>>);

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

/// T07, the subsequent change: a source moves to a new value while a sibling is
/// refusing, and the sibling then recovers. The moved source's update was never
/// committed, so it runs exactly once on recovery: not lost with the aborted
/// observation group, and not run again once it has committed.
#[test]
fn a_moved_source_runs_exactly_once_after_a_sibling_recovers() -> TestResult {
    let moving = Rc::new(Cell::new(17));
    let refusing = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("moved-source-sibling-recovery")
        .observe(Dial {
            value: Rc::clone(&moving),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .observe(PollProbe {
            domain: "test::poll_refuses_twice",
            value: 23,
            polls: Rc::new(Cell::new(0)),
            refuse_next: Rc::clone(&refusing),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    // The initial change: the sibling refuses, then recovers, and both fire.
    assert!(
        bot.tick().is_err(),
        "the first tick reports the sibling's refusal"
    );
    assert_eq!(
        bot.tick()?,
        2,
        "both sources fire once the sibling recovers"
    );
    assert_eq!(*seen.borrow(), vec![17, 23]);

    // The subsequent change: the source moves while the sibling refuses again.
    moving.set(18);
    refusing.set(true);
    let aborted = bot.tick();
    assert!(
        matches!(aborted, Err(BotError::DomainError { ref domain, .. }) if domain == "test::poll_refuses_twice"),
        "the second refusal is reported exactly, got {aborted:?}"
    );
    assert_eq!(
        *seen.borrow(),
        vec![17, 23],
        "an aborted observation admits no action, including for the moved source"
    );

    assert_eq!(
        bot.tick()?,
        1,
        "only the moved source fires on recovery; the sibling's value did not change"
    );
    assert_eq!(
        *seen.borrow(),
        vec![17, 23, 18],
        "the update that was held back runs with its own value"
    );
    assert_eq!(
        bot.tick()?,
        0,
        "a committed update is never run a second time"
    );
    assert_eq!(*seen.borrow(), vec![17, 23, 18]);
    Ok(())
}

/// A sibling's first refusal must not publish or cache away this source's
/// successful observation; the next public tick must poll it again and run
/// the action with the exact observed value.
#[test]
fn a_successful_public_poll_is_not_lost_when_a_sibling_refuses() -> TestResult {
    let successful_polls = Rc::new(Cell::new(0));
    let failing_polls = Rc::new(Cell::new(0));
    let refuse_next = Rc::new(Cell::new(true));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("poll-sibling-recovery")
        .observe(PollProbe {
            domain: "test::poll_success",
            value: 17,
            polls: Rc::clone(&successful_polls),
            refuse_next: Rc::new(Cell::new(false)),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .observe(PollProbe {
            domain: "test::poll_refuses_once",
            value: 23,
            polls: Rc::clone(&failing_polls),
            refuse_next: Rc::clone(&refuse_next),
        })
        .on(|_observed: &u32| true, Noted(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    let first = bot.tick();
    assert!(
        matches!(first, Err(BotError::DomainError { ref domain, .. }) if domain == "test::poll_refuses_once"),
        "the first tick must report the sibling's exact refusal, got {first:?}"
    );
    assert!(
        seen.borrow().is_empty(),
        "an aborted observation admits no action"
    );

    assert_eq!(
        bot.tick()?,
        2,
        "both recovered observations reach their actions"
    );
    assert_eq!(
        successful_polls.get(),
        2,
        "the good sibling is polled again on recovery"
    );
    assert_eq!(
        failing_polls.get(),
        2,
        "the refusing sibling is retried on recovery"
    );
    assert_eq!(
        *seen.borrow(),
        vec![17, 23],
        "actions consume the actual successful values in source order"
    );
    Ok(())
}

/// An abandoned entry is never a quiet tick.
///
/// The short half of the counterexample, stated as the review states it: with
/// only the failed entry and nothing behind it, the second tick returned `Ok(0)`
/// while `pending()` still named the abandonment. A caller reading a clean `Ok`
/// as "this chain is handled" read something that was not true — the work had
/// not been handled, it had been given up on, and those call for different
/// responses from whoever owns the run.
///
/// Under a one-attempt [`RetryPolicy`] the first definite failure spends the
/// budget, so the entry is abandoned and the first tick reports the action's own
/// typed error rather than a summary of it. The source then holds still. What
/// the second tick owes the caller is the abandonment, named.
#[test]
fn an_abandoned_entry_is_never_a_quiet_tick() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let mut lonely = Bot::builder("abandoned-alone")
        .with_retry_policy(RetryPolicy::new(1, Duration::ZERO, Duration::MAX))
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| true,
            Refuses {
                refusing: Rc::new(Cell::new(true)),
                attempts: Rc::clone(&attempts),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    match lonely.tick() {
        Ok(fired) => return Err(format!("a definite failure was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert_eq!(attempts.get(), 1, "the declared budget was one attempt");
    assert_eq!(
        held(&lonely).map(|(chain, entry, _, hold)| (chain, entry, hold)),
        Some((
            0,
            0,
            TransitionHold::Abandoned {
                reason: AbandonReason::AttemptsExhausted { attempts: 1 },
                cause: "test::refuses: refused".to_owned(),
            }
        )),
        "the abandonment is named after the tick that caused it"
    );

    // The source holds still, so there is nothing to attempt and — before the
    // repair — nothing to report either. The tick must not say the chain is
    // handled while `pending()` says otherwise.
    match lonely.tick() {
        Ok(fired) => {
            return Err(format!(
                "an abandoned entry was reported as {fired} fired while pending() still names \
                 it: {:?}",
                lonely.pending()
            )
            .into());
        }
        Err(error) => assert!(
            matches!(
                error,
                BotError::PendingTransition { ref work, outstanding: 1 }
                    if (work.id().chain(), work.id().entry()) == (0, 0)
            ),
            "expected the abandoned entry to be the reported work, got {error:?}"
        ),
    }
    assert_eq!(attempts.get(), 1, "an abandoned entry is not retried");
    assert!(
        !lonely.pending().is_empty(),
        "and it stays reported rather than being dropped"
    );
    Ok(())
}

/// T10, the skipped arm and its contrast. An entry whose condition was false is a
/// *decided* fact: it ran nothing, so it left nothing unsettled, and the entry
/// behind it runs. That is the other side of an abandoned or unknown predecessor,
/// which blocks its successors because something may have happened. A successor
/// is held back by an effect that is in doubt, never merely because a guard in
/// front of it did not hold.
#[test]
fn a_skipped_entry_does_not_block_its_successor() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut chained = Bot::builder("skipped-then-run")
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| false,
            Noted(Rc::new(RefCell::new(Vec::new()))),
        )
        .on(|_observed: &u32| true, Noted(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    assert_eq!(
        chained.tick()?,
        1,
        "only the entry whose condition held fires"
    );
    assert_eq!(
        *log.borrow(),
        vec![1],
        "the successor of a skipped entry ran with the observed value"
    );
    assert!(
        chained.pending().is_empty(),
        "a skipped entry leaves nothing held: {:?}",
        chained.pending().len()
    );
    Ok(())
}

/// An abandoned entry is a barrier to its successors.
///
/// The long half of the counterexample, stated as the review states it: one
/// chain — reserve a draft, then send it — under a one-attempt [`RetryPolicy`].
/// The first action definitely fails, so the entry is abandoned and the first
/// tick reports the action's own typed error. The source then holds still.
///
/// Before the repair, the second tick walked past the abandoned entry as though
/// it had succeeded and ran the send: the chain executed a command whose
/// prerequisite had been given up on — the draft that was never reserved, then
/// the send of it. Declaration order is not success dependency, and treating an
/// abandonment as "behind us" is what confused the two.
///
/// The invariant is that an abandonment is a *barrier*, not a decision that the
/// work is behind us. Successors under the default sequential disposition are
/// not attempted while it stands, and they stay reported. Abandonment is not a
/// way to finish a chain, and not a way to finish it quietly: the caller either
/// supplies evidence that the effect did not happen — which revives the entry
/// and its successors with it — or the chain stays reported as unresolved.
#[test]
fn an_abandoned_entry_blocks_its_successors() -> TestResult {
    let attempts = Rc::new(Cell::new(0));
    let refusing = Rc::new(Cell::new(true));
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut chained = Bot::builder("abandoned-then-blocked")
        .with_retry_policy(RetryPolicy::new(1, Duration::ZERO, Duration::MAX))
        .observe(Dial {
            value: Rc::new(Cell::new(1)),
        })
        .on(
            |_observed: &u32| true,
            Refuses {
                refusing: Rc::clone(&refusing),
                attempts: Rc::clone(&attempts),
            },
        )
        .on(|_observed: &u32| true, Noted(Rc::clone(&log)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    match chained.tick() {
        Ok(fired) => return Err(format!("a definite failure was reported as {fired} fired").into()),
        Err(error) => assert!(
            matches!(error, BotError::DomainError { .. }),
            "expected the action's own error, got {error:?}"
        ),
    }
    assert!(
        log.borrow().is_empty(),
        "the successor is not run in the tick that abandoned its prerequisite"
    );

    // The tick that used to run the successor. It must not: the send would be
    // for a draft that was never reserved.
    match chained.tick() {
        Ok(fired) => {
            return Err(format!(
                "a tick with an abandoned prerequisite reported {fired} fired and ran {:?}",
                log.borrow()
            )
            .into());
        }
        Err(error) => assert!(
            matches!(error, BotError::PendingTransition { .. }),
            "expected the unresolved chain to be reported, got {error:?}"
        ),
    }
    assert!(
        log.borrow().is_empty(),
        "the successor of an abandoned entry is not attempted while it stands abandoned: {:?}",
        log.borrow()
    );

    // Both are still reported — the abandonment and the entry it blocks — so
    // the caller can see which entry is the reason and which is waiting on it.
    let reported: Vec<(usize, usize)> = chained
        .pending()
        .iter()
        .map(|work| (work.id().chain(), work.id().entry()))
        .collect();
    assert_eq!(
        reported,
        vec![(0, 0), (0, 1)],
        "the abandonment and the entry it blocks are both listed: {:?}",
        chained.pending()
    );
    assert!(
        matches!(
            held(&chained).map(|(_, _, _, hold)| hold),
            Some(TransitionHold::Abandoned { .. })
        ),
        "and the first of them is the abandonment, which is the fact that explains the other"
    );

    // Evidence that the effect did not happen is what reopens the chain: the
    // entry is eligible again and its successor with it. This is the only way
    // past a barrier, which is what makes the barrier a decision the caller
    // makes rather than one the substrate makes for them. The action is allowed
    // through this time, so the chain is shown to complete rather than to stay
    // wedged.
    refusing.set(false);
    let blocked = chained
        .pending()
        .into_iter()
        .next()
        .ok_or("the abandoned entry is reported")?;
    chained.resolve_effect(&binding(&blocked)?, EffectEvidence::NotApplied)?;
    assert_eq!(
        chained.tick()?,
        2,
        "the revived entry and the successor it was blocking both run"
    );
    assert_eq!(
        attempts.get(),
        2,
        "the revived entry attempts again, because evidence is a new fact"
    );
    assert_eq!(
        *log.borrow(),
        vec![1],
        "and the successor runs, because its prerequisite is no longer abandoned"
    );
    assert!(
        chained.pending().is_empty(),
        "the chain is finished: {:?}",
        chained.pending()
    );
    Ok(())
}
