//! `EcsBot`'s observation and step phases: polling, refresh, and the chain walk.

// imports
use super::BotError;
use super::DispatchCertainty;
use super::DispatchError;
use super::EffectKey;
use super::EffectLifetime;
use super::Erased;
use super::RefreshReason;
use super::attempts::RetryTurn;
use super::attempts::Settled;
use super::bot::EcsBot;
use super::components::Chains;
use super::components::ChangeTicks;
use super::components::Decision;
use super::components::ForcedRefresh;
use super::components::Grants;
use super::components::Invalidated;
use super::components::MAX_IN_FLIGHT_POLLS;
use super::components::Observed;
use super::components::Order;
use super::components::Policy;
use super::components::PollScratch;
use super::components::SourceId;
use super::components::StalledSource;
use super::components::Step;
use super::components::TickReport;
use super::ledger::Ledger;
use super::watchdog::PollWatchdog;
use super::watchdog::WavePoll;
use super::watchdog::bounded_wave;
use super::work::EffectEvidence;
use super::work::PendingWork;
use super::work::WorkId;

impl EcsBot {
    /// Poll every source, `MAX_IN_FLIGHT_POLLS` at a time, awaiting each wave
    /// on the caller's executor, appending to `polled`.
    ///
    /// Bounded waves, joined concurrently: a source poll may occupy one
    /// `spawn_blocking` thread, so polling them one at a time would make a tick
    /// as slow as the sum of its sources rather than as slow as its slowest.
    /// The wave cap is what keeps that from becoming unbounded blocking-thread
    /// fan-out. Determinism is unaffected: the results are collected in
    /// declaration order whatever order they resolve in.
    ///
    /// # Bounded in time as well as in width
    ///
    /// Each poll runs under [`bounded_poll`], which gives it
    /// [`Self::poll_deadline`] of *real* elapsed time before the wave drops it.
    /// Without that, the fan-out cap is the only bound this phase has and a
    /// source that never resolves holds the tick forever — which is T06's
    /// slow-source half, and it was the one claim this substrate could not make.
    ///
    /// The bound is measured on the wall watchdog half of the crate's one
    /// declared clock ([`crate::rt::clock::Clock`], INV-BOT-30) rather than on
    /// its logical counter, because a source that stopped answering is not
    /// waiting for time to pass: it has stopped making progress entirely, and a
    /// logical clock a caller can advance would either fire the deadline for a
    /// poll that was merely slow or wait on the wedged one forever. What the
    /// clock names is the *source* of the elapsed time, so a reader can ask
    /// which clock governed the cancellation.
    ///
    /// A cancelled poll commits nothing, keeps its chain's baseline and its
    /// forced-refresh mark standing, and is reported through
    /// [`TickReport::stalled`]. Its siblings in the wave are not stopped: the
    /// deadline is per poll, so the chains beside a wedged one still commit and
    /// act in the same tick.
    ///
    /// # Why the output is a parameter
    ///
    /// `polled` is the caller's buffer, already cleared and already holding the
    /// capacity of the previous tick's results. Returning a fresh `Vec` instead
    /// would make the observation phase allocate its staging area every tick,
    /// which is one of the costs this substrate is measured on. The caller owns
    /// the buffer because it also owns the resource the buffer lives in; this
    /// function only borrows the world.
    ///
    /// # What each source is handed
    ///
    /// Each source is given the payload the substrate currently holds for its
    /// chain — the newest committed observation, or the binding of a transition
    /// that owns it — so the source itself can answer "did I move?" against a
    /// concrete value, before anything is boxed. A source that returned
    /// `Ok(None)` did not move, and nothing is allocated for it.
    ///
    /// A source that declared its cache unsound is handed **`None`** instead,
    /// whatever the substrate holds. That is the whole of the forced refresh:
    /// with a baseline in hand the source can answer "equal to this" and the
    /// substrate keeps a value it now knows is wrong. With `None` there is
    /// nothing to be equal to, so the poll is a read and its value becomes the
    /// new baseline — and because a read is a read, the chain also *moved*, so
    /// the change filter re-evaluates the entries against the value that is
    /// actually current rather than against the one the failure left behind.
    ///
    /// # The change-tick skip, and what it is not
    ///
    /// A source that implements [`Observe::revision`](crate::verb::Observe::revision)
    /// and reports the revision its committed value was read at is **not polled
    /// at all**. No future is built, no value is boxed, nothing is allocated, and
    /// the wave is narrower by however many sources are quiet. The check is one
    /// `&self` call and one integer comparison.
    ///
    /// The skip is suppressed for a chain that declared its baseline unsound,
    /// which is the case where believing a revision would be exactly wrong: the
    /// substrate has been told its held value is not what the source would report
    /// now, and a revision is a claim about the value rather than a repair of it.
    ///
    /// # Where the reason comes from
    ///
    /// Read from the source itself, after its poll resolves, never guessed by
    /// this function: the substrate cannot know whether a domain's transport is
    /// up or its queue overflowed, and a heuristic would be a guess about
    /// somebody else's transport. A source that declares nothing is a source
    /// with nothing to declare.
    ///
    /// # What it returns
    ///
    /// The chains whose baseline this tick could not trust: one entry per chain,
    /// in declaration order, `None` where the chain declared nothing; the chains
    /// whose poll the wave gave up on, in the same order; and how many deadline
    /// threads those waves started. Handed back rather than written to the world
    /// here, so `&self` stays a shared borrow across every poll. A `&mut World`
    /// would have to be held while each poll future is alive, and holding it
    /// across awaits to publish three words is exactly the shape that lets one
    /// phase's ordering become another's.
    pub(super) async fn poll_sources(
        &self,
        polled: &mut Vec<Result<Option<Erased>, BotError>>,
        revisions: &mut Vec<Option<u64>>,
        scratch: &mut PollScratch,
    ) -> u32 {
        let count = self.world.non_send::<Chains>().0.len();
        polled.clear();
        polled.resize_with(count, || Ok(None));
        revisions.clear();
        revisions.resize(count, None);

        // What each chain declares about its own caching, one entry per chain and
        // read once per tick. Copied out of the world rather than borrowed across
        // the polls: the marks are written after the polls resolve, and the
        // closures below borrow the world's sources immutably for their whole
        // life.
        self.declared_refresh(&mut scratch.declared, count);
        // The chains whose poll was cancelled, in declaration order. Collected
        // here rather than published by the caller because the poll phase is the
        // only place that knows which poll was cut short, and the report must be
        // published before the schedule step runs on the strength of this tick's
        // observation — a tick that then failed still has to say which source it
        // gave up on.
        scratch.stalled.clear();
        let PollScratch {
            ref mut declared,
            ref mut stalled,
            ref mut slots,
        } = *scratch;
        // The deadline threads this tick actually started, one per wave that had
        // a poll left pending. Published beside the stalls they may have caused,
        // because a caller triaging a wedged source wants to know whether the bot
        // spent a thread on it and how many.
        let mut watchdogs = 0_u32;

        // One guard for the whole phase, held across every await, rather than a
        // fresh `Vec` of `(chain, source)` pairs per wave. The old code copied the
        // wave out because `join_all_boxed` takes an iterator of futures and each
        // of those borrows the world, and the borrow was thought to need an owner
        // rather than a guard: it does not. `Chains` is borrowed shared, nothing
        // in this function is mutably borrowed, and no system runs while a poll is
        // pending — so the guard *is* the owner, and the wave's pairs cost nothing.
        let chains = self.world.non_send::<Chains>();
        for wave_index in 0..count.div_ceil(MAX_IN_FLIGHT_POLLS) {
            let base = wave_index.saturating_mul(MAX_IN_FLIGHT_POLLS);
            let width = count.saturating_sub(base).min(MAX_IN_FLIGHT_POLLS);

            // Which chains in this wave must be polled. A chain is **skipped**
            // when it reported a revision equal to the one its committed value
            // was read at and did not declare its baseline unsound: it keeps its
            // slot in `polled` as `Ok(None)` — "this source did not move" — and no
            // future is built for it, which is the whole saving.
            //
            // The indices are the *wave's* own slots, not positions in `polls`,
            // so each `WavePoll::slot` is still its own index in the wave's wakers
            // and the results below land on the chains that produced them.
            slots.clear();
            {
                let ticks = self.world.non_send::<ChangeTicks>();
                for slot in 0..width {
                    let index = base.saturating_add(slot);
                    let Some(chain) = chains.0.get(index) else {
                        continue;
                    };
                    let unsound = declared
                        .get(index)
                        .copied()
                        .flatten()
                        .is_some_and(|reason| reason.invalidates_baseline());
                    let reported = chain.source.revision();
                    if let Some(slot) = revisions.get_mut(index) {
                        *slot = reported;
                    }
                    // `None` from a source that cannot report a revision is never
                    // quiet: that is the value-comparison path, and it runs on
                    // every tick as it always has.
                    if !(ticks.quiet(index, reported) && !unsound) {
                        slots.push(slot);
                    }
                }
            }

            // A wave whose every source was skipped is a tick that asked and was
            // told nothing had moved. There is no future to join, no deadline to
            // keep, and no watcher to arm — the watchdog is two `Arc`s and its
            // state is a `Mutex`, so arming one for a wave with no members to
            // watch was a thread's worth of bookkeeping spent on the answer
            // "nothing happened".
            if slots.is_empty() {
                continue;
            }

            // One watchdog for the wave rather than one per poll, and one
            // deadline the whole wave shares: `bounded_wave` owns it and reports
            // whether it really did start a thread, which is the number the tick
            // report names. Its width is the number of polls that will actually
            // run.
            let wave_watchdog = PollWatchdog::new(self.poll_deadline, slots.len());
            let (results, armed) = {
                // Scoped to the wave, because each poll borrows that wave's
                // watchdog and a buffer outliving the watchdog would be a buffer
                // holding dangling borrows. Built from `drain(..)` into the wave
                // rather than moved, so a bot with more chains than fit in one
                // wave reuses one allocation across its waves.
                let mut polls: Vec<WavePoll<'_>> = Vec::new();
                for &slot in slots.iter() {
                    {
                        let index = base.saturating_add(slot);
                        let chain = &chains.0[index];
                        let seen = self.world.non_send::<Observed>();
                        let ledger = self.world.non_send::<Ledger>();
                        let grants = self.world.resource::<Grants>();
                        // `None` here means *the source declared nothing*, which
                        // is the ordinary case and must hand the baseline over —
                        // so the two are separated by an explicit test, not by an
                        // `and_then` over the option. `and_then` treats "no
                        // declaration" and "declares nothing usable" as the same
                        // `None` and drops the baseline in the first case, which
                        // is how every poll started reading as a fresh read and
                        // every tick looked like a movement.
                        let unsound = declared
                            .get(index)
                            .copied()
                            .flatten()
                            .is_some_and(|reason| reason.invalidates_baseline());
                        let baseline = if unsound {
                            None
                        } else {
                            seen.0
                                .get(index)
                                .and_then(|slot| slot.as_ref())
                                .or_else(|| ledger.bound(index))
                        };
                        polls.push(WavePoll {
                            chain: index,
                            slot,
                            installed: false,
                            poll: Box::pin(chain.source.poll_any(&grants.0, baseline)),
                            watchdog: &wave_watchdog,
                        });
                    }
                }
                bounded_wave(self.poll_deadline, &wave_watchdog, polls.drain(..)).await
            };
            watchdogs = watchdogs.saturating_add(u32::from(armed));

            // Zipped against the slots that were polled, not against the wave:
            // `results` has one entry per built future, so pairing it with the
            // wave would put every result after the first skip on the wrong
            // chain — a mis-pairing the rendezvous would then refuse as a type
            // mismatch, or, worse, commit one chain's value to another's.
            for (&slot, result) in slots.iter().zip(results) {
                let index = base.saturating_add(slot);
                let Some(slot) = polled.get_mut(index) else {
                    continue;
                };
                if matches!(result, Err(BotError::PollStalled { .. })) {
                    stalled.push(index);
                }
                *slot = result;
            }
        }

        // A poll that failed never reached the value that would have been
        // compared, so its baseline is exactly as unsound as it was and the mark
        // has to outlive this tick. A poll that succeeded is read once more,
        // after it resolved: a source that reached its failure *inside* the poll
        // has only now recorded it, and asking before the poll would have asked
        // about the previous tick's state.
        for (index, reason) in declared.iter_mut().enumerate() {
            let Some(chain) = chains.0.get(index) else {
                continue;
            };
            match (
                chain.source.cache_state(),
                polled.get(index).map(Result::is_ok),
            ) {
                (Some(declared_now), Some(true)) => *reason = Some(declared_now),
                // A failed poll carries no value at all, so the reason the source
                // would have named is not the one that matters: nothing was read,
                // and the baseline this tick did not replace is still standing.
                // The mark stays whatever it already was, and defaults to
                // `Disconnected` for a chain that had declared nothing, because a
                // source that could not be read is a source whose transport is at
                // least unavailable.
                (_, Some(false)) if reason.is_none() => {
                    *reason = Some(RefreshReason::Disconnected);
                }
                _ => {}
            }
        }
        // The `Some(false)` arm above is exactly what a cancelled poll reaches,
        // and that is deliberate: a stalled poll read nothing, so its baseline is
        // as unsound as it was and the mark has to outlive this tick, which is
        // the same rule a failed poll already follows. Clearing the mark on a
        // stall would re-read the source against the very baseline the stall
        // left standing, which is the quiet state INV-BOT-120 exists to rule
        // out.
        //
        // Sorted rather than in wave-completion order: a chain's index is its
        // identity everywhere else in this crate, and a report whose order
        // depended on which wave finished first could not be compared across two
        // runs of one seed.
        stalled.sort_unstable();
        watchdogs
    }

    /// What each of `count` chains declares about its own caching right now.
    ///
    /// Read once per tick, in declaration order, and copied out so the callers
    /// can hold it across the polls without borrowing the world. A chain that
    /// declared a reason last tick and nothing this tick is not cleared here:
    /// the mark is the substrate's record that the baseline it forced a read for
    /// was never successfully replaced, and clearing it on silence would be the
    /// permanent quiet state, one cause at a time.
    fn declared_refresh(&self, declared: &mut Vec<Option<RefreshReason>>, count: usize) {
        let chains = self.world.non_send::<Chains>();
        let invalidated = self.world.non_send::<Invalidated>();
        declared.clear();
        declared.resize(count, None);
        for (index, chain) in chains.0.iter().enumerate() {
            // A chain already marked this tick keeps the mark it has: re-reading
            // it would let a later poll within the same tick overwrite the reason
            // the report names with one that happened afterwards.
            if invalidated.is_invalid(index) {
                continue;
            }
            if let Some(reason) = chain.source.cache_state() {
                declared[index] = Some(reason);
            }
        }
    }

    /// Write this tick's marks into the world, so the schedule step and the
    /// report read the same record the observation phase produced.
    ///
    /// A mark that is already standing keeps the reason it has. That is the
    /// difference between "this source is currently unsound" and "this source's
    /// baseline has not been successfully replaced since it first declared
    /// itself unsound", and only the second one is worth re-reading a caller: a
    /// source that reconnects and then overflows on the very next tick reported
    /// two different facts, and the report names the first because the first is
    /// the one whose forced read has not landed yet.
    pub(super) fn record_invalidations(&mut self, declared: &[Option<RefreshReason>]) {
        let mut invalidated = self.world.non_send_mut::<Invalidated>();
        for (chain, reason) in declared.iter().enumerate() {
            if let Some(reason) = *reason {
                invalidated.mark(chain, reason);
            }
        }
    }

    /// Copy this tick's invalidated marks onto the tick report, in chain order.
    ///
    /// Declaration order rather than mark order, for the reason
    /// [`Invalidated::mark`] already gives: the report is compared across two
    /// runs of one seed, and an order that varied with poll resolution would
    /// make the comparison meaningless.
    pub(super) fn publish_refreshes(&mut self) {
        // Built as a whole vector and then moved in, rather than pushed a row
        // at a time: building it needs two shared borrows of the world (the
        // marks and the source identities) and writing it needs a mutable one,
        // and bevy's access rules cannot express "shared, shared, then mutable"
        // through a chain of borrows on one `self`. One assignment at the end is
        // also cheaper than a `clear` followed by a push per chain.
        let forced: Vec<ForcedRefresh> = {
            let invalidated = self.world.non_send::<Invalidated>();
            let order = self.world.resource::<Order>();
            invalidated
                .reasons
                .iter()
                .enumerate()
                .filter_map(|(chain, reason)| {
                    let reason = (*reason)?;
                    let domain = self.source_domain(order, chain);
                    Some(ForcedRefresh {
                        chain,
                        domain,
                        reason,
                    })
                })
                .collect()
        };
        self.world.resource_mut::<TickReport>().forced = forced;
    }

    /// Copy this tick's cancelled polls onto the tick report, in chain order.
    ///
    /// The same build-then-assign shape as [`Self::publish_refreshes`] and the
    /// same reason: naming a chain's domain needs two shared borrows of the world
    /// and writing the report needs a mutable one.
    pub(super) fn publish_stalls(&mut self, stalled: &[usize], watchdogs: u32) {
        let rows: Vec<StalledSource> = {
            let order = self.world.resource::<Order>();
            stalled
                .iter()
                .map(|chain| StalledSource {
                    chain: *chain,
                    domain: self.source_domain(order, *chain),
                    deadline: self.poll_deadline,
                })
                .collect()
        };
        let mut report = self.world.resource_mut::<TickReport>();
        report.stalled = rows;
        report.watchdogs = watchdogs;
    }

    /// The domain `chain`'s source declared, as the tick report names it.
    ///
    /// Empty only for a chain index with no source entity, which a report row
    /// built from this bot's own chains never has; the row is still written
    /// rather than dropped, because a report that lost a stalled chain would
    /// read as a chain that did not stall.
    fn source_domain(&self, order: &Order, chain: usize) -> String {
        let source = order
            .0
            .get(chain)
            .and_then(|entity| self.world.get::<SourceId>(*entity));
        match source {
            Some(id) => id.domain().to_owned(),
            None => String::new(),
        }
    }

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
        turn: RetryTurn,
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
                // Every remaining step is this chain's: its run ends the slice.
                None => steps.len(),
            };
            let run = match steps.get(start) {
                Some(step) if step.chain == chain => &steps[start..end],
                _ => &[],
            };

            // Taken, so the condition failure belongs to the chain it was recorded
            // for and cannot be reported for a chain that never reached it.
            let condition_failure = failures.get_mut(chain).and_then(Option::take);
            let (chain_fired, chain_failure) = self.run_chain(run, condition_failure, turn).await;
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
    async fn run_chain(
        &mut self,
        steps: &[Step],
        condition_failure: Option<BotError>,
        turn: RetryTurn,
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
                            .finish_recording(work, evidence, attempts, turn)
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
            let declared = self
                .world
                .non_send::<Chains>()
                .0
                .get(step.chain)
                .and_then(|chain| chain.entries.get(step.entry))
                .map(|entry| entry.action.effect_lifetime());
            let lifetime = match declared {
                Some(lifetime) => lifetime,
                // An entry the chains do not hold is treated as the strictest
                // lifetime, so its attempt is journalled rather than assumed
                // ephemeral.
                None => EffectLifetime::External,
            };
            let (key, authority) = match self
                .world
                .non_send_mut::<Ledger>()
                .begin_attempt(work, lifetime, turn.now)
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
                    .fail(work, &error, key.attempt(), turn);
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
                    .fail(work, &error, key.attempt(), turn);
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
                        .fail(work, &error, key.attempt(), turn);
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
    /// Two kinds are reported and [`TransitionHold`](super::work::TransitionHold) distinguishes them: an
    /// entry still being held — waiting for an attempt or for evidence — and
    /// one given up on, which stays listed so lost work has a name after the
    /// tick that lost it. An empty result means the last transition is fully
    /// handled.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingWork> {
        let budget = self.world.resource::<Policy>().0.budget();
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
