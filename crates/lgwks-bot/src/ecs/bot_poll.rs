//! Polling sources inside a tick: waves, refreshes and stalls.

use crate::error::BotError;
use crate::spec::Erased;
#[cfg(doc)]
use crate::verb::Observe;
use crate::verb::RefreshReason;

use super::bot::EcsBot;
use super::ledger::Ledger;
use super::resources::Chains;
use super::resources::ChangeTicks;
use super::resources::ForcedRefresh;
use super::resources::Grants;
use super::resources::Invalidated;
use super::resources::Observed;
use super::resources::Order;
use super::resources::PollScratch;
use super::resources::SourceId;
use super::resources::StalledSource;
use super::resources::TickReport;
use super::watchdog::MAX_IN_FLIGHT_POLLS;
use super::watchdog::PollWatchdog;
use super::watchdog::WavePoll;
use super::watchdog::bounded_wave;
#[cfg(doc)]
use crate::spec::ObserveAny;

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
        // Cleared and left empty: a slot per chain is filled the first time a
        // wave has a source to poll, below. A tick on which every source reports
        // the revision its value was read at polls nothing, and a `Result` the
        // size of a `BotError` written for every chain to say "did not move" was
        // most of what such a tick cost. Empty means nothing was polled, which is
        // what `observe_fold` reads it as.
        polled.clear();
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
            // The first wave with a source to poll gives every chain its slot, as
            // "did not move", so the results land on the chains that produced
            // them and every skipped chain reads as the answer it gave.
            if polled.is_empty() {
                polled.resize_with(count, || Ok(None));
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
            // A chain with no slot was skipped, which is an answer of "did not
            // move" and counts as a poll that succeeded.
            let answered = match polled.get(index) {
                Some(result) => result.is_ok(),
                None => true,
            };
            match (chain.source.cache_state(), answered) {
                (Some(declared_now), true) => *reason = Some(declared_now),
                // A failed poll carries no value at all, so the reason the source
                // would have named is not the one that matters: nothing was read,
                // and the baseline this tick did not replace is still standing.
                // The mark stays whatever it already was, and defaults to
                // `Disconnected` for a chain that had declared nothing, because a
                // source that could not be read is a source whose transport is at
                // least unavailable.
                (_, false) if reason.is_none() => {
                    *reason = Some(RefreshReason::Disconnected);
                }
                _ => {}
            }
        }
        // The `false` arm above is exactly what a cancelled poll reaches,
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
    pub(super) fn declared_refresh(&self, declared: &mut Vec<Option<RefreshReason>>, count: usize) {
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
                    let domain = match order
                        .0
                        .get(chain)
                        .and_then(|entity| self.world.get::<SourceId>(*entity))
                    {
                        Some(id) => id.domain().to_owned(),
                        None => String::new(),
                    };
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
                .map(|chain| {
                    let domain = match order
                        .0
                        .get(*chain)
                        .and_then(|entity| self.world.get::<SourceId>(*entity))
                    {
                        Some(id) => id.domain().to_owned(),
                        None => String::new(),
                    };
                    StalledSource {
                        chain: *chain,
                        domain,
                        deadline: self.poll_deadline,
                    }
                })
                .collect()
        };
        let mut report = self.world.resource_mut::<TickReport>();
        report.stalled = rows;
        report.watchdogs = watchdogs;
    }
}
