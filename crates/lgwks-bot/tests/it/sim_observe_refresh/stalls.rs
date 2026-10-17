//! T06, the slow source: the per-poll deadline under a seeded mix of fast and wedged chains.

use super::*;

// ── T06 slow source: the per-poll deadline under a seeded mix ───────────────

/// How a seeded source behaves on a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    /// Answers immediately, with the value the plan gave it.
    Fast,
    /// Never resolves: the poll parks forever and is cancelled at the deadline.
    Wedged,
}

/// The pace and value one seed gave one chain.
#[derive(Clone, Copy, Debug)]
struct PaceWindow {
    /// Whether the chain answers or never resolves on this tick.
    pace: Pace,
    /// The value it reports when it answers.
    value: u32,
}

/// One tenant's seeded mix of fast and wedged chains over the run.
///
/// Fast chains dominate by construction, and that is the point: a run in which
/// everything stalls proves that stalling is reported and nothing about what it
/// costs the chains beside it. The healthy ones are what make "and the fast ones
/// still committed" checkable.
#[derive(Clone, Copy, Debug)]
struct PacePlan {
    /// Per chain, what it does on each tick.
    windows: [[PaceWindow; MAX_CHAINS]; MAX_TICKS],
    /// How many ticks the run drives.
    ticks: u32,
}

impl PacePlan {
    /// Chain `chain`'s behaviour on `tick`, defaulting to fast so a table this
    /// short cannot leave a chain undefined and panic on it later.
    ///
    /// The row is `tick - 1`, because the table is filled one-based: row 0 is
    /// tick 1. Reading `tick` directly would look one tick into the future and
    /// hand back the fallback for the last tick of every run, so a schedule would
    /// silently arm every chain *fast* on the tick after its last — and the run
    /// would still pass every report assertion, because the arming and the
    /// expectation would have shifted together.
    /// The window this schedule declares for `chain` at `tick`, or `None` when
    /// the schedule declares no such cell.
    ///
    /// `None` rather than a fast window carrying the value `1`: a cell the
    /// schedule does not declare has no pace and no value, and a stand-in would
    /// put one into the trace as though the schedule had drawn it.
    fn at(&self, chain: usize, tick: u32) -> Option<PaceWindow> {
        let row = tick.saturating_sub(1);
        let row = self.windows.get(usize::try_from(row).ok()?)?;
        row.get(chain).copied()
    }

    /// Whether `chain` answers on every tick of the run.
    ///
    /// "Never stalled" rather than "fast right now", because the differential it
    /// gates compares a chain's *whole* log against a run with no wedging at all,
    /// and a chain that stalled on one tick and not on another cannot be compared
    /// that way. Deciding it here rather than at the call site is what keeps the
    /// filter and the comparison describing the same set.
    fn never_stalled(&self, chain: usize) -> bool {
        // A chain the schedule leaves undeclared at a tick has no pace, and a
        // chain that was never wedged is exactly that: absence is the answer.
        (1..=self.ticks).all(|tick| {
            self.at(chain, tick)
                .is_none_or(|window| window.pace == Pace::Fast)
        })
    }

    /// A plan whose *last* tick is `tick`, with `per_chain` naming each chain's
    /// pacing on it.
    ///
    /// Built per tick rather than drawn for the whole run, because the family that
    /// uses it arms one tick at a time: the question is how one *wave* spends its
    /// watchdogs, and a plan that decided a run's pacings up front would answer
    /// it only on whichever tick the draw happened to land on.
    fn one_tick(per_chain: &[u32], tick: u32) -> Result<PacePlan, Box<dyn std::error::Error>> {
        let mut windows = [[PaceWindow {
            pace: Pace::Fast,
            value: 1,
        }; MAX_CHAINS]; MAX_TICKS];
        let row = usize::try_from(tick.saturating_sub(1))?;
        for (index, window) in windows.iter_mut().enumerate() {
            for (chain, cell) in window.iter_mut().enumerate() {
                *cell = PaceWindow {
                    pace: if per_chain.get(chain).copied() == Some(WEDGED_PACE) {
                        Pace::Wedged
                    } else {
                        Pace::Fast
                    },
                    // A distinct value per cell, so a commit of one chain's
                    // value can never be mistaken for another's.
                    value: u32::try_from(index.saturating_mul(MAX_CHAINS).saturating_add(chain))?
                        .saturating_add(1),
                };
            }
        }
        Ok(PacePlan {
            windows,
            ticks: u32::try_from(row.saturating_add(1))?,
        })
    }
}

/// The longest run this family drives.
///
/// Fixed rather than drawn: a seed that drew the tick count would renumber every
/// other family for no added evidence, which is the reason the existing
/// `plan` in this file fixes its own chain count the same way.
const MAX_TICKS: usize = 6;

/// Draw one tenant's pace schedule from `rng`.
///
/// Roughly a third of the chain-ticks are wedged. The draw is per chain *per
/// tick* rather than per chain, because the property that matters is a source
/// that recovers — a chain wedged for the whole run would satisfy "every
/// cancellation is reported" without ever exercising "the next tick re-polls it".
fn pace_plan(rng: &mut Rng, ticks: u32) -> Result<PacePlan, Box<dyn std::error::Error>> {
    let drawn = usize::try_from(ticks)?.min(MAX_TICKS);
    let mut windows = [[PaceWindow {
        pace: Pace::Fast,
        value: 1,
    }; MAX_CHAINS]; MAX_TICKS];
    let span = u32::try_from(MAX_CHAINS)?;
    for (tick, row) in windows.iter_mut().enumerate().take(drawn) {
        // One-based, because the families drive ticks from 1. A zero-based table
        // read at tick 1 would describe the tick *before* the first one, so every
        // arming decision in the run would be off by one — and an off-by-one here
        // surfaces only as "the report does not match the schedule", which is the
        // one failure mode a simulation family cannot debug from its trace.
        let tick_index = u32::try_from(tick)?.saturating_add(1);
        for (chain, window) in row.iter_mut().enumerate() {
            // Distinct per chain and tick, so a commit that did not happen is
            // distinguishable from a commit of somebody else's value.
            *window = PaceWindow {
                pace: if rng.chance(350) {
                    Pace::Wedged
                } else {
                    Pace::Fast
                },
                value: tick_index
                    .saturating_mul(span)
                    .saturating_add(u32::try_from(chain)?)
                    .saturating_add(1),
            };
        }
    }
    Ok(PacePlan { windows, ticks })
}

/// Build one tenant's bot over [`MAX_CHAINS`] paced sources.
///
/// The chain count is the same written-out constant as [`tenant`]'s, for the
/// same reason: `Bot::observe` returns a builder whose type follows its source,
/// so a runtime chain count cannot be expressed.
fn paced_tenant(tenant: u32, deadline: std::time::Duration) -> Result<Tenant, Box<dyn Error>> {
    let ran = Rc::new(RefCell::new(Vec::new()));
    let holds = Rc::new(Cell::new(false));
    let (first, second, third) = (
        ChainHandles::new(),
        ChainHandles::new(),
        ChainHandles::new(),
    );
    let paced = |held: &ChainHandles, chain| Paced {
        pace: Rc::clone(&held.pace),
        value: Rc::clone(&held.value),
        declared: Rc::clone(&held.declared),
        polls: Rc::new(Cell::new(0)),
        chain,
        tenant,
    };
    let bot = three_chains(
        Bot::builder(format!("sim-observe-stall-t{tenant}")).with_poll_deadline(deadline),
        [paced(&first, 0), paced(&second, 1), paced(&third, 2)],
        &ran,
        &holds,
        tenant,
    )?;

    Ok(Tenant {
        bot,
        handles: vec![first, second, third],
        ran,
        tenant,
    })
}

/// A source whose answer-or-not is the plan's, under a real per-poll deadline.
struct Paced {
    /// Whether this poll answers or never resolves.
    pace: Rc<Cell<u32>>,
    /// The value it reports when it answers.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers.
    declared: Rc<Cell<Option<RefreshReason>>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The chain's index, for the domain identity a report names it by.
    chain: usize,
    /// The tenant this source belongs to, for the same reason.
    tenant: u32,
}

impl Observe for Paced {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        if self.pace.get() == WEDGED_PACE {
            // Parks forever without waking. The deadline is the only thing that
            // ends this poll, which is the property the family is about.
            std::future::pending::<()>().await;
        }
        Ok(self.value.get())
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.declared.get()
    }

    fn domain_id(&self) -> &str {
        domain_of(self.tenant, self.chain)
    }
}

/// Arm every chain for `tick` and run it, returning what the tick reported.
fn paced_tick(
    subject: &mut Tenant,
    plan: &PacePlan,
    tick: u32,
) -> Result<Vec<ReportRow>, Box<dyn Error>> {
    for (index, held) in subject.handles.iter().enumerate() {
        let window = plan
            .at(index, tick)
            .ok_or("the schedule declares no window for this chain at this tick")?;
        held.pace.set(if window.pace == Pace::Wedged {
            WEDGED_PACE
        } else {
            0
        });
        held.value.set(window.value);
        held.declared.set(None);
    }
    let mut rows = Vec::new();
    subject.observe(&mut rows);
    Ok(rows)
}

/// Arm every chain for `tick` with the *same* values but with nothing wedged, and
/// run it.
///
/// The counterfactual half of the differential: the same schedule, the same
/// values, every source answering. Written beside [`paced_tick`] rather than as a
/// flag on it so the two runs cannot drift apart in how they arm a chain — the
/// only difference between them is the pace, which is the thing under test.
fn unpace_tick(
    subject: &mut Tenant,
    plan: &PacePlan,
    tick: u32,
) -> Result<Vec<ReportRow>, Box<dyn Error>> {
    for (index, held) in subject.handles.iter().enumerate() {
        held.pace.set(0);
        held.value.set(
            plan.at(index, tick)
                .ok_or("the schedule declares no window for this chain at this tick")?
                .value,
        );
        held.declared.set(None);
    }
    let mut rows = Vec::new();
    subject.observe(&mut rows);
    Ok(rows)
}

/// Every wedged source is reported stalled, and the independent chains beside it
/// do exactly what they would have done with no wedging at all.
///
/// # Why this is a differential, and not a count
///
/// The obvious assertion — "the chains that moved fired" — is wrong about this
/// substrate, and finding out why is the reason the family is shaped this way.
/// `fire_fold` selects its work through `Changed<Revision>`, which a change
/// filter consumes on the tick it is seen, so several chains moving on one tick
/// select one chain's generation and the others' work waits for the next tick.
/// That is pre-existing behaviour (INV-BOT-121, latest-state mode), not something
/// the deadline introduced, and a fixture that asserted "every mover fires" would
/// have been asserting a rule the substrate never had.
///
/// So the claim is stated as what T06 actually asks: **a slow source costs nothing
/// else.** The same seeded schedule is driven twice over, once with the wedging
/// and once with every source answering, and the fast chains' action logs must be
/// **identical**. That is stronger than a count — a substrate that fired
/// everything twice would fail it — and it is immune to the selection rule above,
/// because both runs are subject to it identically.
///
/// The differential also covers what the row is *not* about. A wedged chain's
/// baseline must stay exactly where it was, so the chain that recovers is still
/// compared against the value from before it stalled; a cancellation that spent
/// the baseline would make the recovered chain's log diverge from the un-wedged
/// run, which this catches.
///
/// The report is checked against the schedule per tick and per tenant, so a
/// cancellation is named where it happened rather than in aggregate: one
/// `stalled` row per wedged chain, naming that chain's own domain, and none for a
/// chain that answered. Two tenants share the process and each is checked against
/// its own schedule, so the "and not a neighbour's" half of the two-tenant case
/// lives here rather than in a separate family.
pub(super) fn a_wedged_source_is_reported_and_costs_its_neighbours_nothing(
    band: Band,
) -> TestResult {
    sim::assert_replays(band, |sim| {
        // Both tenants draw from the same stream, so one seed fixes both schedules
        // and both runs.
        let left = pace_plan(sim.rng(), 4)?;
        let right = pace_plan(sim.rng(), 4)?;
        let schedules = [left, right];

        let mut wedged = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];
        let mut answered = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];

        for tick in 1..=left.ticks {
            for index in 0..wedged.len() {
                let plan = *schedules
                    .get(index)
                    .ok_or("a subject this run armed has no schedule")?;

                // The wedged run, as the schedule decided it.
                let rows = paced_tick(&mut wedged[index], &plan, tick)?;
                let report = wedged[index].bot.tick_report();

                // The same tick over the same schedule with nothing wedged. Every
                // source answers, so this run is the counterfactual the first is
                // compared against.
                let _answered = unpace_tick(&mut answered[index], &plan, tick)?;

                // (1) and (2): the report is exactly the schedule's wedged chains,
                // once each, named by the source's own domain.
                let reported: Vec<(usize, String)> = report
                    .stalled()
                    .iter()
                    .map(|row| (row.chain(), row.domain().to_owned()))
                    .collect();
                let wanted: Vec<(usize, String)> = (0..MAX_CHAINS)
                    .filter(|chain| {
                        plan.at(*chain, tick)
                            .is_some_and(|window| window.pace == Pace::Wedged)
                    })
                    .map(|chain| (chain, DOMAINS[index][chain % 8].to_owned()))
                    .collect();
                assert_eq!(
                    reported, wanted,
                    "tick {tick}, tenant {index}: every source the schedule wedged is \
                     reported stalled once, by chain and by its own domain, and no \
                     source that answered is"
                );

                // (3): a chain that has *never* stalled did exactly what the same schedule
                // would have had it do with no wedging anywhere — the whole log,
                // every tick, in order.
                //
                // The whole log rather than this tick's slice, because a cumulative
                // log cannot be filtered by the current tick without also dropping
                // everything a chain did on the ticks when it *was* wedged, and
                // dropping those is exactly what would hide a cancellation that
                // stopped a recovering chain. A chain that stalled even once is
                // excluded from this comparison and checked by the report above
                // instead, which is where its cancellation is named.
                let subject = &wedged[index];
                let peer = &answered[index];
                let (fast_log, answered_log): (Vec<_>, Vec<_>) = (0..MAX_CHAINS)
                    .filter(|chain| plan.never_stalled(*chain))
                    .map(|chain| {
                        (
                            subject
                                .ran
                                .borrow()
                                .iter()
                                .filter(|entry| entry.0 == chain)
                                .copied()
                                .collect::<Vec<(usize, u32)>>(),
                            peer.ran
                                .borrow()
                                .iter()
                                .filter(|entry| entry.0 == chain)
                                .copied()
                                .collect::<Vec<(usize, u32)>>(),
                        )
                    })
                    .unzip();
                assert_eq!(
                    fast_log, answered_log,
                    "tick {tick}, tenant {index}: a chain that never stalled did \
                     different work with a sibling source stalled than it would have \
                     with nothing stalled"
                );

                for row in &rows {
                    sim.record(&row.spell());
                }
                let logged: Vec<(usize, u32)> = subject.ran.borrow().iter().copied().collect();
                for (chain, value) in logged {
                    sim.record(&format!("ran {index} {chain} {value}"));
                }
            }
        }
        Ok(())
    })
}

/// The same property under saturation: many chains stall at once and the
/// independent ones still commit.
///
/// The saturation tiers are 100 / 1,000 / 10,000 for the *held-chain* half of T06,
/// which `observe_refresh.rs` writes out because a bot of *n* chains needs *n*
/// written-out `observe`/`on` pairs. This family pins the tiers it can build at
/// the fixed [`MAX_CHAINS`] the erasure boundary allows, and says so rather than
/// claiming a tier number it did not run.
///
/// What it does add over the family above is *simultaneity*: every chain in the
/// wave is wedged on the same tick, so there is no independent chain left to
/// attribute the surviving commits to except the tenant beside it. Two tenants,
/// one entirely stalled and one entirely fast, is the saturating shape at this
/// chain count — and the assertion that the stalled tenant's report names every
/// chain while the fast tenant's report names none is what would fail if a
/// cancellation still parked the tick.
pub(super) fn a_saturated_wave_stalls_every_chain_and_still_lets_the_next_tenant_commit(
    band: Band,
) -> TestResult {
    sim::assert_replays(band, |sim| {
        // Tenant 0 is wedged on every chain for the whole run; tenant 1 is fast
        // throughout. The draw is the seed's only contribution beyond that, so a
        // family that stopped varying anything would still replay — and a
        // schedule this asymmetric is the one that can actually distinguish "the
        // cancellation is bounded" from "the whole tick is cancelled".
        let ceiling = u32::try_from(MAX_TICKS)?;
        let ticks = sim.rng().between(2, ceiling);
        let mut all_wedged = PacePlan {
            windows: [[PaceWindow {
                pace: Pace::Fast,
                value: 1,
            }; MAX_CHAINS]; MAX_TICKS],
            ticks,
        };
        let mut all_fast = PacePlan {
            windows: [[PaceWindow {
                pace: Pace::Fast,
                value: 1,
            }; MAX_CHAINS]; MAX_TICKS],
            ticks,
        };
        for tick in 0..MAX_TICKS {
            for chain in 0..MAX_CHAINS {
                all_wedged.windows[tick][chain] = PaceWindow {
                    pace: Pace::Wedged,
                    value: 9,
                };
                all_fast.windows[tick][chain] = PaceWindow {
                    pace: Pace::Fast,
                    value: u32::try_from(tick.saturating_add(1))?,
                };
            }
        }
        // The chain count this family reached, recorded rather than clamped.
        let chains = subject_count(&all_wedged);

        let mut stalled = paced_tenant(0, SIM_DEADLINE)?;
        let mut ready = paced_tenant(1, SIM_DEADLINE)?;

        for tick in 1..=ticks {
            let _rows = paced_tick(&mut stalled, &all_wedged, tick)?;
            let _ready = paced_tick(&mut ready, &all_fast, tick)?;

            let stalled_report = stalled.bot.tick_report();
            assert_eq!(
                stalled_report.stalled().len(),
                chains,
                "every one of the {chains} chains in the saturated wave is reported \
                 stalled — a dropped cancellation is a source nobody looks at again"
            );
            let ready_report = ready.bot.tick_report();
            assert!(
                ready_report.stalled().is_empty(),
                "the tenant beside the saturated wave reports no stall: its sources \
                 never stopped answering ({:?})",
                ready_report.stalled()
            );
            assert!(
                ready.ran.borrow().len() >= usize::try_from(tick)?,
                "and it still committed and acted while the other tenant's whole \
                 wave was cancelled"
            );
            sim.record(&format!(
                "tick {tick} saturated_stalled={} ready_ran={}",
                stalled_report.stalled().len(),
                ready.ran.borrow().len()
            ));
        }
        sim.record(&format!("saturation chains={chains}"));
        Ok(())
    })
}

/// How many chains the saturated wave covers, as the tier it actually reached.
fn subject_count(plan: &PacePlan) -> usize {
    match plan.windows.first() {
        Some(row) => row
            .iter()
            .filter(|window| window.pace == Pace::Wedged)
            .count(),
        // A plan with no rows has no wedged subject in it: zero is that count,
        // not a stand-in for a count nobody took.
        None => 0,
    }
}

/// A seeded scope spends one watchdog on a wave that has a poll left pending,
/// and none at all on a wave whose every source answers.
///
/// The property the whole wave-level watchdog exists for, stated as a number
/// rather than as an adjective. A bot of *n* chains used to start and join one OS
/// thread *per source poll, per tick* — including the ordinary tick, where every
/// source answered on its first poll and the thread had nothing to do — so a
/// thousand chains paid a thousand spawns a tick that had no slow source in it at
/// all. The ordinary tick is the common one, so its cost is the one that decides
/// whether a bot scales at all.
///
/// # What the number is read from
///
/// `TickReport::watchdogs`, which is operator data a caller reads beside
/// `stalled`. Not a `#[cfg(test)]` counter and not a field only a test can reach:
/// a thread count the product cannot report is a thread count the product has
/// promised nothing about, and a test reading one of its own would be measuring
/// its own instrumentation rather than the substrate.
///
/// # Why the seed is still there
///
/// The *count* is the easy half to assert and the easy half to fake: a family
/// that always wedged a source would pass against an implementation that spawns a
/// thread per poll, and a family that never wedged one would pass against an
/// implementation that never spawns any. The seed picks the scope, so both arms
/// and everything between them is driven, and the count is checked against the
/// scope the seed chose rather than against a fixed expectation.
///
/// Two tenants share the process and are read independently, so "one watchdog per
/// wave" is also "one watchdog per *tenant's* wave": a counter shared across
/// tenants would report one where two were started, and one tenant alone could
/// never see the difference.
///
/// The mixed scope costs one real deadline per wedged tick, because that is what
/// a wedged source *is* — the substrate genuinely has to wait for the wall clock.
/// That is the honest cost of the family, and it is why the scopes are per tick
/// rather than a mixture inside one tick.
pub(super) fn a_wave_spends_one_watchdog_and_a_fast_wave_spends_none(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // Cut before anything is built: the tick count is a draw like any other,
        // and a run that drew six ticks of wedged sources would spend six real
        // deadlines waiting for the wall clock rather than exercising the claim.
        let ceiling = u32::try_from(MAX_TICKS)?;
        let ticks = sim.rng().between(2, ceiling);

        // Which chains wedge is re-drawn per tick from the same stream, so a run
        // that wedged every chain on tick 1 and nothing after it is one this
        // family produces, and so is its exact mirror.
        let mut pacings: [Vec<u32>; 2] = [vec![0; MAX_CHAINS], vec![0; MAX_CHAINS]];
        let mut subjects = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];

        for tick in 1..=ticks {
            for (index, subject) in subjects.iter_mut().enumerate() {
                let drawn = pacings
                    .get_mut(index)
                    .ok_or("the run has exactly two tenants")?;
                for pace in drawn.iter_mut() {
                    *pace = if sim.rng().chance(350) {
                        WEDGED_PACE
                    } else {
                        0
                    };
                }
                let plan = PacePlan::one_tick(drawn, tick)?;
                let _rows = paced_tick(subject, &plan, tick)?;

                let report = subject.bot.tick_report();
                let wedged = drawn.iter().filter(|pace| **pace == WEDGED_PACE).count();
                assert_eq!(
                    report.watchdogs(),
                    u32::from(wedged > 0),
                    "tick {tick}, tenant {index}: {wedged} of this tenant's chains \
                     never resolved, so the wave spent {} watchdog threads — one for \
                     the wave with a poll still pending, and none at all when every \
                     source answered",
                    report.watchdogs()
                );
                assert_eq!(
                    report.stalled().len(),
                    wedged,
                    "tick {tick}, tenant {index}: every wedged chain is reported \
                     stalled, and no chain that answered is"
                );
                sim.record(&format!(
                    "tick {tick} tenant {index} wedged={wedged} watchdogs={}",
                    report.watchdogs()
                ));
            }
        }
        Ok(())
    })
}

/// The stall families replay exactly, twice over.
///
/// `assert_replays` sweeps the band twice and compares the two hash vectors, so a
/// nondeterministic run fails even when every assertion above passes — and a
/// deadline under test is exactly where nondeterminism would hide, because how far
/// into a budget a cancellation lands depends on how loaded the host is. The
/// trace records the *set* of stalled chains rather than any timing, so the
/// receipt stays a statement about what the run decided and not about how fast
/// it decided it.
///
/// The body is the union of the two families above, so the hash covers forced
/// refreshes, pass-overs, per-tenant reports, stall reports and the action logs
/// rather than one scenario's shape.
pub(super) fn the_same_seed_replays_a_stalled_wave(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let left = pace_plan(sim.rng(), 4)?;
        let right = pace_plan(sim.rng(), 4)?;
        let schedules = [left, right];
        let mut subjects = [
            paced_tenant(0, SIM_DEADLINE)?,
            paced_tenant(1, SIM_DEADLINE)?,
        ];

        for tick in 1..=left.ticks {
            for (index, subject) in subjects.iter_mut().enumerate() {
                let plan = *schedules
                    .get(index)
                    .ok_or("a subject this run armed has no schedule")?;
                let _rows = paced_tick(subject, &plan, tick)?;
                let report = subject.bot.tick_report();
                // The watchdog count beside the stall list: a run that replayed
                // its decisions but started a different number of threads is a
                // run whose *cost* was not deterministic, which is exactly what
                // this family exists to notice about a deadline.
                sim.record(&format!("watchdogs {index} {}", report.watchdogs()));
                for row in report.stalled() {
                    sim.record(&format!("stalled {index} {} {}", row.chain(), row.domain()));
                }
                for forced in subject.bot.tick_report().forced() {
                    sim.record(&format!(
                        "forced {index} {} {}",
                        forced.chain(),
                        forced.reason().as_str()
                    ));
                }
                for &(chain, value) in subject.ran.borrow().iter() {
                    sim.record(&format!("ran {index} {chain} {value}"));
                }
            }
        }
        Ok(())
    })
}
