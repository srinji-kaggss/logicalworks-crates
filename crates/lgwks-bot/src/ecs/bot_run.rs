//! Running chain steps and resolving held effects.

#[cfg(doc)]
use lgwks_deps::bevy_ecs::schedule::IntoScheduleConfigs;

use crate::broker::DispatchError;
use crate::effect::EffectKey;
use crate::error::{BotError, DispatchCertainty};
use crate::verb::EffectLifetime;
#[cfg(doc)]
use crate::verb::Execute;

use super::bot::EcsBot;
use super::entry::Settled;
use super::ledger::Ledger;
use super::resources::Chains;
use super::resources::Decision;
use super::resources::Grants;
use super::resources::Policy;
use super::resources::Step;
use super::work::EffectEvidence;
use super::work::PendingWork;
#[cfg(doc)]
use super::work::TransitionHold;
use super::work::WorkId;

impl EcsBot {
    /// Run the effects the decision phase selected, in the order it selected
    /// them, awaiting each on the caller's executor.
    ///
    /// Returns the number that fired and the first failure in declaration order,
    /// or `None` when nothing failed.
    ///
    /// Every chain is run on its own, because a failure stops its own chain rather
    /// than every chain after it: the chains behind a failing one still run and
    /// still record their work. Iterating chains rather than steps is also what
    /// keeps a chain that recorded *no* step from being skipped: a condition the
    /// walk could not evaluate stops that chain before it records anything, and
    /// its failure is still the tick's to report. The failure of the first chain
    /// in declaration order that has one is that report, which is the ordering the
    /// synchronous walk had.
    ///
    /// States are written to the ledger as the walk reaches them — an attempt in
    /// flight *before* its effect is awaited, and its outcome after — so a tick
    /// dropped in the middle holds what it had begun instead of leaving a
    /// `NotStarted` entry for the next tick to replay.
    pub(super) async fn run_steps(
        &mut self,
        steps: &[Step],
        failures: &mut [Option<BotError>],
        budget: u32,
    ) -> (usize, Option<BotError>) {
        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        let mut start = 0;
        for chain in 0..failures.len() {
            // `steps` is in `(chain, entry)` walk order, so the steps of `chain`
            // are the run at `start` — and a chain that recorded none contributes
            // the empty run, because its first step is a later chain's.
            let end = match steps[start..].iter().position(|step| step.chain != chain) {
                Some(offset) => start.saturating_add(offset),
                None => steps.len(),
            };
            let run = match steps.get(start) {
                Some(step) if step.chain == chain => &steps[start..end],
                _ => &[],
            };

            // Taken, so the condition failure belongs to the chain it was recorded
            // for and cannot be reported for a chain that never reached it.
            let condition_failure = failures.get_mut(chain).and_then(Option::take);
            let (chain_fired, chain_failure) = self.run_chain(run, condition_failure, budget).await;
            fired = fired.saturating_add(chain_fired);
            // Chain order, so "the first error in declaration order" is the one a
            // caller gets, exactly as the synchronous walk had it — except that
            // the chains after it still ran.
            if failure.is_none() {
                failure = chain_failure;
            }
            start = end;
        }
        (fired, failure)
    }

    /// Apply one chain's walk, in entry order, awaiting each attempt.
    ///
    /// A [`Decision::Skip`] is applied without an attempt. A
    /// [`Decision::Attempt`] is written to the ledger as in flight, awaited, and
    /// then recorded. The run stops at the first failure, leaving the entries
    /// behind it exactly as they were, so an acknowledged effect is never
    /// replayed to reach a successor.
    ///
    /// The report falls back to the condition failure the decision phase recorded
    /// for this chain when no attempt of its own failed: a condition the walk
    /// could not evaluate is a failure of the chain it is in, and the decisions
    /// before it are exactly the effects the walk had already cleared.
    pub(super) async fn run_chain(
        &mut self,
        steps: &[Step],
        condition_failure: Option<BotError>,
        budget: u32,
    ) -> (usize, Option<BotError>) {
        let mut fired: usize = 0;
        let mut failure: Option<BotError> = None;
        for step in steps {
            let work = WorkId {
                chain: step.chain,
                entry: step.entry,
            };
            if matches!(step.decision, Decision::Skip) {
                // A Skip is the plan's answer to a false condition, and a
                // recovered unknown outranks that answer: marking the entry
                // `Skipped` is how the unknown got buried and its successor
                // ran (issue #104). The chain stops instead, and the entry
                // stays where it was.
                let blocked = {
                    let ledger = self.world.non_send::<Ledger>();
                    ledger
                        .action_of(work)
                        .is_some_and(|action| ledger.effects.blocks(action))
                };
                if blocked {
                    break;
                }
                self.world.non_send_mut::<Ledger>().skip(work);
                continue;
            }

            // A known outcome whose record is missing: retry the append and
            // never re-enter the action. This is the whole of issue #102's
            // repair on the recovery side — the occurrence is already a fact,
            // so a second dispatch would be a duplicate, and the only thing
            // left to do is write the record that failed to land.
            if let Some((key, evidence, attempts)) =
                self.world.non_send::<Ledger>().recording_failed(work)
            {
                let result = self
                    .world
                    .non_send_mut::<Ledger>()
                    .effects
                    .ensure_outcome(key, evidence);
                match result {
                    Ok(()) => {
                        if self
                            .world
                            .non_send_mut::<Ledger>()
                            .finish_recording(work, evidence, attempts, budget)
                            && evidence == EffectEvidence::Applied
                        {
                            fired = fired.saturating_add(1);
                        }
                        continue;
                    }
                    Err(cause) => {
                        let error = BotError::EffectUnrecorded {
                            key: Box::new(key),
                            evidence,
                            cause: Box::new(DispatchError::Journal(cause)),
                        };
                        self.world
                            .non_send_mut::<Ledger>()
                            .hold_recording(work, evidence, &error);
                        failure = Some(error);
                        break;
                    }
                }
            }

            // Two questions the ledger answers before anything is sent, and
            // they lead to different places. An action an acknowledgement has
            // retired for this generation is *done*, so the walk steps over it
            // exactly as it steps over a false condition: the effect landed, and
            // the generation is the one it landed in. An action an unsettled
            // recovered attempt stands against is *held*, so the walk stops:
            // nothing established that the bytes did not arrive, and the report
            // a caller needs is the one `first_unresolved` renders from the same
            // fact.
            if let Some(action) = self.world.non_send::<Ledger>().action_of(work) {
                let ledger = self.world.non_send::<Ledger>();
                if ledger.generation(step.chain).is_some_and(|(input, event)| {
                    ledger
                        .effects
                        .applied_in(action, step.chain, step.entry, input, event)
                }) {
                    // Retired, not merely stepped over. An entry whose effect
                    // landed owes nothing for this generation, and leaving it
                    // outstanding would report finished work as pending for as
                    // long as the generation lasts.
                    self.world.non_send_mut::<Ledger>().skip(work);
                    continue;
                }
                if ledger.effects.blocks(action) {
                    break;
                }
            }

            // Written *before* the attempt, not after: a tick dropped while the
            // effect is in flight leaves this behind, and a record that says
            // "dispatched, outcome unknown" is what makes the next tick hold the
            // effect instead of replaying it. Both the intent and the prepared
            // dispatch are committed here, so a crash after this line recovers
            // as an unknown rather than as a never-sent. A panic unwinding out
            // of the action is a limit this substrate does not close, and it is
            // stated in the module documentation.
            //
            // The key comes back *from* the ledger rather than being derived
            // beside it. The attempt a caller is handed and the attempt
            // settlement checks have to be one identity, and two derivations of
            // one identity are two identities the moment they disagree.
            let lifetime = match self
                .world
                .non_send::<Chains>()
                .0
                .get(step.chain)
                .and_then(|chain| chain.entries.get(step.entry))
            {
                Some(entry) => entry.action.effect_lifetime(),
                None => EffectLifetime::External,
            };
            let (key, authority) = match self
                .world
                .non_send_mut::<Ledger>()
                .begin_attempt(work, lifetime)
                .await
            {
                Ok(pair) => pair,
                Err(error) => {
                    // The entry is not in a state an attempt can start from, or
                    // a guard refused it. Either way nothing left the process,
                    // and the run stops here rather than guessing what the entry
                    // now means.
                    failure = Some(error);
                    break;
                }
            };

            // The generation check, at the handoff rather than at the mint.
            // Authorization and handing over are two instants, and a replacement
            // landing between them leaves a warrant that was minted legitimately
            // and is now stale; a check that only ran at mint time would let it
            // through. Nothing is awaited before this, so the warrant is
            // presented as close to the handoff as the substrate can put it.
            if let Err(cause) = self
                .world
                .non_send::<Ledger>()
                .effects
                .broker()
                .revalidate(&authority)
            {
                let error = BotError::EffectRefused {
                    cause: DispatchError::Broker(cause),
                };
                self.world
                    .non_send_mut::<Ledger>()
                    .fail(work, &error, key.attempt(), budget);
                failure = Some(error);
                break;
            }

            // Scoped, so the world is borrowed immutably only across the await
            // and the ledger is reachable mutably on either side of it.
            //
            // The value is the transition's binding, not the newest observation.
            // That is the whole of the invariant: this entry was *selected*
            // against the payload its transition was opened under, so it has to
            // be *run* against that same payload. Reading the observation slot
            // here is how a chain comes to refuse against one input and then
            // retry against another, combining two command inputs into one
            // transition's effects. The slot is empty for as long as the binding
            // is out on loan, so there is nothing to reach for by accident.
            let outcome = {
                let chains = self.world.non_send::<Chains>();
                let grants = self.world.resource::<Grants>();
                let bound = self.world.non_send::<Ledger>().bound(step.chain);
                match (chains.0.get(step.chain), bound) {
                    (Some(chain), Some(value)) => match chain.entries.get(step.entry) {
                        Some(entry) => Some(entry.action.run_any(&grants.0, value).await),
                        None => None,
                    },
                    // No chain, or a transition bound to nothing: the entry
                    // stays held by the record written above rather than being
                    // guessed at.
                    _ => None,
                }
            };

            let Some(outcome) = outcome else {
                break;
            };
            // The outcome is written down for the two cases where it is a fact,
            // and deliberately not for the third. `Applied` and `NotApplied` are
            // answers; an indeterminate effect is the *absence* of one, and
            // appending `NotApplied` for it would record a fact the substrate
            // does not have — which is exactly how a recovery path comes to
            // believe a merge did not land.
            let observed = match outcome {
                Ok(_) => Some(EffectEvidence::Applied),
                Err(ref error) => match error.dispatch_certainty() {
                    DispatchCertainty::Refused | DispatchCertainty::NotDelivered => {
                        Some(EffectEvidence::NotApplied)
                    }
                    DispatchCertainty::Unsettled => None,
                    // An action cannot report `Occurred` as its own failure —
                    // that certainty belongs to a post-effect recording
                    // failure. Nothing is appended for it, because the
                    // substrate does not have the fact that arm would record.
                    DispatchCertainty::Occurred => None,
                },
            };
            if let Some(evidence) = observed
                && let Err(cause) = self
                    .world
                    .non_send_mut::<Ledger>()
                    .effects
                    .ensure_outcome(key, evidence)
            {
                // Post-effect: the action already returned and `evidence` is a
                // fact. This is a recording failure, not a pre-dispatch
                // refusal — `EffectRefused` would tell a controller that
                // nothing left the process and that it may replan, which
                // duplicates a known outcome.
                let error = BotError::EffectUnrecorded {
                    key: Box::new(key),
                    evidence,
                    cause: Box::new(DispatchError::Journal(cause)),
                };
                self.world
                    .non_send_mut::<Ledger>()
                    .fail(work, &error, key.attempt(), budget);
                failure = Some(error);
                break;
            }
            match outcome {
                Ok(_) => {
                    if self.world.non_send_mut::<Ledger>().succeed(work) {
                        fired = fired.saturating_add(1);
                    }
                }
                Err(error) => {
                    self.world
                        .non_send_mut::<Ledger>()
                        .fail(work, &error, key.attempt(), budget);
                    failure = Some(error);
                    break;
                }
            }
        }
        // An action failure happens *earlier* in walk order than a condition
        // failure that follows it, so it is the more specific report.
        (fired, failure.or(condition_failure))
    }

    /// Every entry that is not finished, in `(chain, entry)` order.
    ///
    /// Two kinds are reported and [`TransitionHold`] distinguishes them: an
    /// entry still being held — waiting for an attempt or for evidence — and
    /// one given up on, which stays listed so lost work has a name after the
    /// tick that lost it. An empty result means the last transition is fully
    /// handled.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingWork> {
        let budget = self.world.resource::<Policy>().0.max_attempts();
        self.world.non_send::<Ledger>().pending(budget)
    }

    /// Settle an entry whose effect may or may not have happened.
    ///
    /// A held entry is one the substrate cannot decide about on its own, and
    /// retrying it blind is how a bot duplicates a merge, a message, or a
    /// launch. Supply what you know — [`EffectEvidence::Applied`] acknowledges
    /// the effect, [`EffectEvidence::NotApplied`] makes the entry eligible for
    /// an attempt again — and the entry moves.
    ///
    /// An entry that was given up on accepts evidence too, which is how a
    /// caller revives work the attempt budget abandoned.
    ///
    /// The evidence is submitted against the [`PendingWork`] it is about,
    /// rather than against a slot: the value carries the address, the
    /// generation and the attempt, and all three are compared before anything
    /// is read or written. That is what makes the delivery safe to retry — a
    /// caller that never saw its first report acknowledged can send the same
    /// one again and have it answered idempotently, rather than applied a
    /// second time.
    ///
    /// # Errors
    ///
    /// [`BotError::NoSuchWork`] when the entry is not held: it ran, its
    /// condition was false, it has not been reached yet, or its chain holds no
    /// transition at all. Evidence cannot contradict an attempt whose outcome
    /// was recorded, and accepting it silently would let a caller believe an
    /// effect was acknowledged when nothing was.
    ///
    /// [`BotError::EvidenceSuperseded`] when the chain holds a transition whose
    /// revision is not the one the work was read from — the work the caller is
    /// reporting on was superseded, and nothing was changed. Re-read
    /// [`pending`](Self::pending) and report against the generation that is
    /// there now.
    ///
    /// [`BotError::EvidenceStaleAttempt`] when the generation still holds but
    /// the entry has moved on to a later attempt since. The report is about an
    /// attempt that is over, so there is nothing for it to decide; re-read
    /// [`pending`](Self::pending) to see the attempt that is outstanding.
    ///
    /// [`BotError::EvidenceContradicted`] when this generation's entry was
    /// already settled with the opposite evidence. Nothing was changed:
    /// [`Applied`](EffectEvidence::Applied) does not become
    /// [`NotApplied`](EffectEvidence::NotApplied), or the reverse, after the
    /// fact. Repeating the *same* evidence is not a contradiction and succeeds
    /// idempotently, so a caller that never saw its first delivery through can
    /// safely send it again.
    pub fn resolve_effect(
        &mut self,
        key: &EffectKey,
        evidence: EffectEvidence,
    ) -> Result<(), BotError> {
        // A recovered attempt is settled first, and the order is the whole of
        // deliverable F05's second half: a key the journal records as dispatched
        // with no outcome belongs to no live transition, so the ledger's own
        // settlement has nothing to match it against and would answer
        // `NoSuchWork`. It is the one case where "settle it" is the repair for a
        // refusal the substrate itself raised.
        let recovered = self
            .world
            .non_send::<Ledger>()
            .effects
            .unsettled_for(key.action())
            .is_some_and(|held| held == *key);
        if recovered {
            // The generation the acknowledgement covers is the key's own. An
            // `Applied` verdict retires the action for the generation that key
            // names and no other, so a source that moves afterwards gets new
            // work — which is what a new generation *is* — while the generation
            // the effect was produced from never runs it again. Reading the
            // generation off the key rather than off the entry's live
            // transition is what makes this reach the recovered case: a
            // recovered attempt belongs to no live transition, so the entry
            // holds no revision to take it from.
            return self
                .world
                .non_send_mut::<Ledger>()
                .effects
                .settle_recovered(*key, evidence)
                .map_err(|cause| BotError::EffectRefused {
                    cause: DispatchError::Journal(cause),
                });
        }
        let recording = self
            .world
            .non_send::<Ledger>()
            .effects
            .recording_for(key.action())
            .is_some_and(|held| held.key == *key && held.evidence == evidence);
        if recording {
            return self
                .world
                .non_send_mut::<Ledger>()
                .effects
                .settle_recording(*key, evidence)
                .map_err(|cause| BotError::EffectUnrecorded {
                    key: Box::new(*key),
                    evidence,
                    cause: Box::new(DispatchError::Journal(cause)),
                });
        }
        match self
            .world
            .non_send::<Ledger>()
            .classify_settlement(key, evidence)
        {
            // A journal that could not be read is not a settlement fact and
            // must not be reported as one. Nothing has been mutated: the call
            // is a classification, and the caller may repeat it once reads
            // come back (issue #123).
            Err(cause) => Err(BotError::EffectRefused {
                cause: DispatchError::Journal(cause),
            }),
            // Both are success, and deliberately one arm after the journal
            // write: to the caller, a repeat that landed a second time is the
            // same fact as one that landed the first. The append happens
            // *before* either is acknowledged, so a restart cannot resurrect an
            // attempt this process already settled (issue #106).
            Ok(verdict @ (Settled::Decided | Settled::Duplicate)) => {
                self.world
                    .non_send_mut::<Ledger>()
                    .effects
                    .ensure_outcome(*key, evidence)
                    .map_err(|cause| BotError::EffectUnrecorded {
                        key: Box::new(*key),
                        evidence,
                        cause: Box::new(DispatchError::Journal(cause)),
                    })?;
                if verdict == Settled::Decided {
                    self.world
                        .non_send_mut::<Ledger>()
                        .apply_settlement(key, evidence);
                }
                Ok(())
            }
            Ok(Settled::Superseded { id, current }) => Err(BotError::EvidenceSuperseded {
                work: id,
                named: key.digest(),
                current,
            }),
            Ok(Settled::StaleAttempt {
                id,
                reported,
                outstanding,
            }) => Err(BotError::EvidenceStaleAttempt {
                work: id,
                reported,
                outstanding,
            }),
            Ok(Settled::Contradicted { id, settled }) => Err(BotError::EvidenceContradicted {
                work: id,
                settled,
                submitted: evidence,
            }),
            Ok(Settled::NoSuchWork { id: Some(id) }) => Err(BotError::NoSuchWork { work: id }),
            Ok(Settled::NoSuchWork { id: None }) => Err(BotError::ActionNotDeclared {
                action: key.action(),
            }),
        }
    }
}
