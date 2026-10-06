//! Acceptance for the observation layer's failure and supersession rows.
//!
//! Three rows of `docs/orchestration-acceptance.spec.md`, each driven through
//! the public surface only:
//!
//! | Row | What it asks | What this file observes |
//! |---|---|---|
//! | **T08** (LC-04) | Disconnect, watch overflow, stale remote key and invalidation failure force refresh rather than permanent quiet state | Each of the four reasons, armed on one source and left armed, forces the next tick to re-read it and appear in `TickReport::forced` with that reason. A forced read of an *unchanged* value commits nothing and fires nothing; a forced read of a *changed* one commits the newer value and then returns to quiet, so the repair is not "re-read forever". A refresh that *fails* leaves the mark standing. |
//! | **T09** (DX-07) | Identical payloads with different event ids both execute; a duplicate delivery does not. Latest-state mode reports supersession separately | Two `EventId`s over equal payloads are two admitted inputs and fire twice; the same id delivered again retires, as does a redelivery of an *older* id. An intermediate value overtaken before it was acted on is reported in `TickReport::superseded` with the revision it was committed under — neither a fired effect nor a retire. |
//! | **T06** (LC-03) | A saturated task A cannot starve independent B; one slow source does not block unrelated ready work | Both halves. The saturation half: a chain whose entries are all held open, and a mass of 100 / 1,000 / 10,000 such chains, never starves an independent chain declared beside it. The slow-source half, in three tests: a source whose poll never resolves is cancelled at the bot's declared per-poll deadline, the independent chain beside it still commits and acts **in the same tick**, and the tick itself returns inside the deadline on an independent watchdog thread; the cancelled chain is re-polled on the next tick and commits when it answers; and a deadline that bounds nothing — zero, or past the ceiling — is refused at build. |
//!
//! # What is proved here, and how the oracles work
//!
//! Every ordering is established by a shape a reader can inspect — a log of what
//! ran, a chain index in a report, a counter of poll bodies entered — and the one
//! place a clock appears is the *measurement* that the tick returned inside its
//! budget. That measurement is taken on an independent OS thread, the shape
//! `resume_liveness.rs` uses, because an oracle that shares the tick's own driver
//! cannot observe that tick failing to make progress. T06's own text forbids a
//! sleep as the oracle for a *completion*, and none is used: the wedged source
//! is a future that parks forever without waking, so "it never resolves" is a
//! property of the test's own fixture rather than a duration nobody waited for.
//!
//! # Why the oracle is a controlled completion
//!
//! Every *ordering* here is established by the shape of a run the test can read —
//! a held entry, a value that moved, a log of what ran — and never by a clock. The
//! one exception is stated above rather than smuggled in: T06's slow-source test
//! measures how long the tick took, which is a measurement *of the substrate*,
//! and T06's own text forbids a sleep as the oracle for a completion. It is not
//! used as one.
//!
//! # What is real
//!
//! Real `Bot`, real `bevy_ecs` schedule, real `MemoryJournal` behind a real
//! `EffectScope` with a real `Broker`. The sources and actions are the smallest
//! ones that can express the row, and every one of them reports through the
//! public `Observe` / `Execute` traits — nothing reaches past them into the
//! crate's internals.

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lgwks_bot::effect::EventId;
use lgwks_bot::{
    Auth, Bot, BotError, Cap, DEFAULT_POLL_DEADLINE, DispatchCertainty, EffectLifetime, Execute,
    GrantSet, MAX_POLL_DEADLINE, Observe, RefreshReason,
};

use crate::effects;
use crate::poll;

use effects::memory_scope;
use poll::admit_poll;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

// ── Sources ────────────────────────────────────────────────────────────────

use crate::declarable::Declarable;

/// An event source: the next `EventId` the test queued, and the queue's
/// remaining length.
struct EventFeed {
    /// The events still to be delivered, in order.
    queue: Rc<RefCell<Vec<EventId<u32>>>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The domain identity a report names this source by.
    domain: &'static str,
}

impl Observe for EventFeed {
    type Output = EventId<u32>;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<EventId<u32>, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        self.queue.borrow_mut().pop().ok_or(BotError::DomainError {
            domain: self.domain.to_owned(),
            certainty: DispatchCertainty::NotDelivered,
            cause: "the event feed is drained".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

// ── Actions ────────────────────────────────────────────────────────────────

/// An action that lands, and records the value it ran with.
///
/// The log is the oracle for T06 and T09: it is the only place a run's order is
/// visible from outside, so every ordering assertion in this file reads it
/// rather than a counter.
struct Lands {
    /// Every value this action ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for Lands {
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
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::lands"
    }
}

/// An action that never settles: its attempt may still be live.
///
/// This is how a generation stays *open* long enough for a newer observation to
/// overtake it, which is the situation latest-state mode has to report. It
/// returns the action's own indeterminate error rather than a generic refusal,
/// because an indeterminate effect is the only one of the four certainties that
/// leaves the entry held instead of decided.
struct NeverSettles {
    /// Every value this action ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Execute for NeverSettles {
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
        Err(BotError::EffectIndeterminate {
            domain: "test::never_settles".to_owned(),
            cause: "the acknowledgement never arrived".to_owned(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::never_settles"
    }
}

// ── Shared assertions ──────────────────────────────────────────────────────

/// An action that lands over an event, recording the event id it ran with.
///
/// A separate type from [`Lands`] rather than a generic one: `Execute` is the
/// crate's public verb and a generic action would make every call site here
/// carry a type parameter for the difference between "a value moved" and "an
/// event arrived", which is the difference the whole of T09 is about.
struct LandsEvents {
    /// Every event id this action ran with, in the order it ran.
    seen: Rc<RefCell<Vec<u64>>>,
}

impl Execute for LandsEvents {
    type Input = EventId<u32>;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &EventId<u32>)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.seen.borrow_mut().push(call.1.id());
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::lands_events"
    }
}

/// Every value an action ran with, in order.
fn ran_with(seen: &Rc<RefCell<Vec<u32>>>) -> Vec<u32> {
    seen.borrow().clone()
}

/// The tick that fired, or a named failure.
///
/// Ticking returns `Err` for a held transition as well as for a fault, and this
/// file needs to say which it saw — "the tick failed" is not an observation, and
/// several rows are about a run that fires nothing precisely because something
/// was held.
fn tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => {
            assert_eq!(
                fired,
                bot.tick_report().fired(),
                "the tick's count and its report are one fact read twice: {fired} then {}",
                bot.tick_report().fired()
            );
            Ok(())
        }
        Err(BotError::PendingTransition { .. }) => Ok(()),
        Err(error) => Err(format!("the tick failed with {error:?}").into()),
    }
}

/// Run one tick whose source is expected to refuse, and assert it said so.
///
/// Separate from [`tick`] because a refusing source and a healthy one want
/// opposite reports, and folding them into one helper would mean every caller
/// re-asserting the distinction it came here to read. The variant is matched
/// exactly rather than accepted as "any error": a refusal that arrived as a
/// `TypeMismatch` or a `PendingTransition` would be a different defect wearing
/// the same silence.
fn refused_tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => Err(format!(
            "a source that refused read as a {fired}-effect quiet tick; the \
             refusal has to be reported"
        )
        .into()),
        Err(error) => {
            assert!(
                matches!(error, BotError::DomainError { .. }),
                "the refusal is the source's own error and keeps its type: {error:?}"
            );
            Ok(())
        }
    }
}

/// Run one tick whose action is expected to leave its entry held, and assert the
/// hold was reported.
///
/// The third distinct tick outcome in this file, beside a clean tick and a
/// refusing source. A hold is reported as an indeterminate effect on the tick
/// whose walk reaches the attempt, and as `PendingTransition` on a later tick
/// that finds the same attempt still held — both are the substrate refusing to
/// call an unknown outcome a decision, which is the property under test. Folding
/// either into [`tick`] would mean every caller re-deciding which of the three
/// it was looking at.
fn held_tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => Err(format!(
            "an action whose outcome was unknown read as a {fired}-effect clean \
             tick; the hold has to be reported"
        )
        .into()),
        Err(error) => {
            assert!(
                matches!(
                    error,
                    BotError::EffectIndeterminate { .. } | BotError::PendingTransition { .. }
                ),
                "the hold keeps a typed variant — a retry classifier reads it, and \
                 neither arm of this match is a decision: {error:?}"
            );
            Ok(())
        }
    }
}

// ── T06 slow source: a poll that never resolves ─────────────────────────────

/// A source that can be made to never resolve, and reports how often it was
/// entered.
///
/// The wedging is *controlled* rather than a sleep: `wedged` is a cell the test
/// sets, and while it is set the poll returns a future that parks forever without
/// ever completing. Nothing here waits on real elapsed time to *decide*
/// anything — the deadline is what the substrate applies, and the only clock in
/// this test is the one the watchdog thread uses to report how long the tick took,
/// which is a measurement of the substrate rather than an oracle about the
/// domain.
///
/// How many times the body was entered matters as much as what it returned: it is
/// how the test proves the next tick *re-polls* a stalled chain rather than
/// remembering the cancellation and skipping the source.
struct Wedged {
    /// Whether the poll parks forever instead of reading.
    wedged: Rc<Cell<bool>>,
    /// The value this poll reports once it is not wedged.
    value: Rc<Cell<u32>>,
    /// How many times the poll body was entered.
    entered: Rc<Cell<u32>>,
    /// What `cache_state` answers, so a chain can be declared unsound and its
    /// forced-refresh mark watched across a stall.
    reason: Rc<Cell<Option<RefreshReason>>>,
    /// The domain identity a report names this source by.
    domain: &'static str,
}

impl Observe for Wedged {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        self.entered.set(self.entered.get().saturating_add(1));
        if self.wedged.get() {
            // Parks without ever resolving and without waking anyone, which is
            // the one shape a deadline has to survive: it is not a slow source,
            // it is a source that has stopped answering. The `resolve` keeps the
            // guard type-checked without a `loop {}`, so a reader can see this
            // is a deliberate park and not an oversight.
            let resolve = std::future::pending::<()>();
            resolve.await;
        }
        Ok(self.value.get())
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.reason.get()
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}

/// The handles a test needs to drive one wedged chain beside an independent one.
struct WedgedRig {
    /// The bot under test.
    bot: Bot,
    /// Whether the wedged chain's source parks.
    wedged: Rc<Cell<bool>>,
    /// The value the wedged chain reports once it answers.
    wedged_value: Rc<Cell<u32>>,
    /// How many times the wedged chain's poll body was entered.
    wedged_entered: Rc<Cell<u32>>,
    /// What the wedged chain declares about its own cache.
    wedged_reason: Rc<Cell<Option<RefreshReason>>>,
    /// How many times the independent chain's source was polled.
    ready_entered: Rc<Cell<u32>>,
    /// The value the independent chain's source reports, so a test can make that
    /// chain have work ready on the tick it cares about.
    ready_value: Rc<Cell<u32>>,
    /// Every value the independent chain's action ran with, in order.
    ready_seen: Rc<RefCell<Vec<u32>>>,
    /// Every value the wedged chain's action ran with, in order.
    wedged_seen: Rc<RefCell<Vec<u32>>>,
}

/// Build a two-chain bot under `deadline`: chain 0's source can be wedged, chain
/// 1's always answers.
///
/// One bot, one world, one schedule step — the same construction
/// [`saturated_pair`] uses, and for the same reason: T06 is a claim about two
/// chains on *one* substrate, not about two processes that happen to interleave.
/// The difference is which chain is saturated. There the walk is held open by a
/// transition; here the *observation phase* is held open by a source, which is
/// the half that a fan-out cap does not bound.
///
/// The independent chain is declared second so it shares the observation wave with
/// the wedged one: two chains that landed in different waves would not prove
/// that a cancellation in one wave leaves the other free, which is the property.
fn wedged_pair(deadline: Duration) -> Result<WedgedRig, Box<dyn Error>> {
    let wedged = Rc::new(Cell::new(false));
    let wedged_value = Rc::new(Cell::new(1));
    let wedged_entered = Rc::new(Cell::new(0));
    let wedged_reason = Rc::new(Cell::new(None));
    let wedged_seen = Rc::new(RefCell::new(Vec::new()));
    let ready_entered = Rc::new(Cell::new(0));
    let ready_value = Rc::new(Cell::new(2));
    let ready_seen = Rc::new(RefCell::new(Vec::new()));

    let bot = Bot::builder("t06-slow-source")
        .with_poll_deadline(deadline)
        .observe(Wedged {
            wedged: Rc::clone(&wedged),
            value: Rc::clone(&wedged_value),
            entered: Rc::clone(&wedged_entered),
            reason: Rc::clone(&wedged_reason),
            domain: "test::wedged",
        })
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&wedged_seen),
            },
        )
        .observe(Wedged {
            wedged: Rc::new(Cell::new(false)),
            value: Rc::clone(&ready_value),
            entered: Rc::clone(&ready_entered),
            reason: Rc::new(Cell::new(None)),
            domain: "test::independent",
        })
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&ready_seen),
            },
        )
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;

    Ok(WedgedRig {
        bot,
        wedged,
        wedged_value,
        wedged_entered,
        wedged_reason,
        ready_entered,
        ready_value,
        ready_seen,
        wedged_seen,
    })
}

/// A source that never resolves does not stop an independent chain from running
/// in the same tick.
///
/// The row's second clause, and the half that needed a per-poll deadline to
/// exist. Chain 0's source is wedged: its poll is entered, parks forever and
/// never wakes. Chain 1's source answers immediately. Both are in one observation
/// wave, so if the deadline were per *wave* rather than per *poll* — or if the
/// cancellation stopped the wave — chain 1 would never commit and the tick would
/// never reach the walk at all.
///
/// Three observations, and each is a different failure to catch:
///
/// 1. the tick **returns**, measured on an independent watchdog thread against
///    `deadline + slack`. A source that held the tick would make this thread the
///    only thing that ever finished.
/// 2. chain 1's **action ran** in that same tick, from its log. A tick that
///    returned early without running anything would satisfy (1).
/// 3. chain 0 is **reported stalled**, naming its own `domain_id`. A tick that
///    returned silently would satisfy (1) and (2) and hide the whole defect.
///
/// The watchdog thread is the oracle for (1) and is deliberately not the tick's
/// own executor: it is an OS thread that samples the elapsed time and reports
/// it, so a tick that never returns is caught by something that does not depend
/// on the tick.
#[test]
fn a_slow_source_does_not_block_an_independent_chain() -> TestResult {
    // Short enough that the test is quick, long enough that a healthy poll — which
    // answers on its first poll — is never at risk of being cut short by it.
    let deadline = Duration::from_millis(120);
    let slack = Duration::from_secs(20);
    let mut rig = wedged_pair(deadline)?;

    // A clean tick first, so both chains hold a committed baseline before the
    // wedging begins. Without it the first observation of each chain is the thing
    // under test, and a substrate whose *first* read did not fire would fail this
    // for a reason that has nothing to do with a source that stopped answering.
    // Establishing the baseline is also what the row is about: an independent
    // chain's work is a chain that has already been observed.
    tick(&mut rig.bot)?;

    rig.wedged.set(true);
    // The independent chain has work ready on the very tick the wedged one is
    // cancelled, so what is being checked is that it was *not stopped* rather
    // than that it had nothing to do anyway.
    rig.ready_value.set(3);

    let (elapsed, outcome) = time_a_tick(&mut rig.bot, deadline.saturating_add(slack));
    let report = rig.bot.tick_report();

    assert!(
        elapsed <= deadline.saturating_add(slack),
        "the tick took {elapsed:?} against a declared {deadline:?} per-poll deadline \
         and {slack:?} of slack: a source that never resolves still holds the tick"
    );

    // The independent chain committed and acted in the tick that cancelled its
    // sibling, which is the whole of the claim. It ran with `3`, not the `2` the
    // baseline tick committed: a chain that read the same value would not have
    // fired at all, and the row is about a chain that had work ready and was not
    // stopped from doing it.
    assert_eq!(
        ran_with(&rig.ready_seen),
        vec![2, 3],
        "the independent chain's action ran again in the same tick the wedged chain \
         was cancelled in"
    );
    assert!(
        rig.ready_entered.get() >= 2,
        "the independent chain's source was polled again: {} entries",
        rig.ready_entered.get()
    );

    // The wedged chain committed nothing and acted nothing on top of what the
    // baseline tick already fired.
    assert_eq!(
        ran_with(&rig.wedged_seen),
        vec![1],
        "a cancelled poll commits nothing, so the chain it belongs to added no \
         effect beyond the one its baseline tick fired: {:?}",
        ran_with(&rig.wedged_seen)
    );

    // Reported, naming the source that stopped answering rather than a chain
    // index the caller has to resolve itself.
    assert_eq!(
        report
            .stalled()
            .iter()
            .map(|row| (row.chain(), row.domain()))
            .collect::<Vec<_>>(),
        vec![(0, "test::wedged")],
        "the tick reports exactly the wedged chain, by chain index and by the \
         source's own domain_id"
    );
    assert_eq!(
        report.stalled()[0].deadline(),
        deadline,
        "the report names the budget that was actually applied, so a reader is not \
         guessing which bound cancelled the poll"
    );
    assert!(
        report.stalled_any(),
        "`stalled_any` and `stalled` are one fact read twice"
    );

    // And the tick's own result is a *typed* stall rather than a silent success:
    // the chain that did not read is a chain the caller has to know about.
    assert!(
        matches!(outcome, Some(BotError::PollStalled { chain: 0, .. })),
        "the tick reports the cancellation with its own variant, naming the chain: \
         {outcome:?}"
    );
    Ok(())
}

/// A stalled chain is re-polled on the next tick, and commits when it answers.
///
/// The half that makes the stall a deferral rather than a retirement. A
/// cancellation that spent the chain's baseline — or cleared its forced-refresh
/// mark — would leave the source permanently unobserved, which is the quiet state
/// INV-BOT-120 exists to rule out, reached by a different road.
///
/// Three observations:
///
/// 1. the next tick **enters the poll body again**, from the counter. A substrate
///    that remembered the cancellation and skipped the source would never reach
///    the second entry.
/// 2. the chain **commits and fires** once the source answers.
/// 3. it is **no longer reported stalled** on the tick that read it, so the report
///    says the source recovered rather than leaving a stale cancellation up.
#[test]
fn a_stalled_chain_is_re_polled_and_commits_when_it_answers() -> TestResult {
    let deadline = Duration::from_millis(120);
    let mut rig = wedged_pair(deadline)?;

    // The clean baseline tick, for the same reason as the test above: a chain's
    // first observation is its own thing, and this row is about what happens to a
    // chain whose poll has already been cancelled once.
    tick(&mut rig.bot)?;

    rig.wedged.set(true);
    stalled_tick(&mut rig.bot)?;
    let after_stall = rig.bot.tick_report();
    assert_eq!(
        after_stall.stalled().len(),
        1,
        "the first tick cancelled chain 0's poll: {:?}",
        after_stall.stalled()
    );

    // The chain is declared unsound across the stall, so the mark has to survive
    // it: a cancellation that cleared the mark would let the next tick compare a
    // fresh read against the baseline the stall was supposed to replace.
    rig.wedged_reason.set(Some(RefreshReason::Disconnected));

    // The source recovers, with a *different* value, so a commit that did not
    // happen cannot be confused with a commit of the old baseline.
    rig.wedged.set(false);
    rig.wedged_value.set(7);
    tick(&mut rig.bot)?;

    assert!(
        rig.wedged_entered.get() >= 2,
        "the next tick entered the poll body again ({} entries): a substrate that \
         remembered the cancellation skipped the source",
        rig.wedged_entered.get()
    );
    assert_eq!(
        ran_with(&rig.wedged_seen),
        vec![1, 7],
        "the recovered chain committed the value it read and fired on it, on top \
         of the one its baseline tick fired"
    );
    let recovered = rig.bot.tick_report();
    assert!(
        recovered.stalled().is_empty(),
        "the tick that read the source reports no stall: {:?}",
        recovered.stalled()
    );
    assert!(
        recovered.forced().iter().any(|row| row.chain() == 0),
        "and the forced-refresh mark survived the cancellation rather than being \
         spent by a read that never committed: {:?}",
        recovered.forced()
    );
    Ok(())
}

/// A per-poll deadline of zero, or one past the ceiling, is refused at build.
///
/// The bound is only a bound if it cannot be set to something that is not one. A
/// zero budget cancels every poll before its first poll — a bot that observes
/// nothing and reports every chain as stalled — and a budget past the ceiling is
/// a caller asking for a bot that can still hang. Both are refused with a typed
/// error naming the number, because a silently clamped deadline is
/// indistinguishable from the one the caller asked for.
///
/// Run against the *real* builder rather than a helper, because the refusal is
/// what this observes and the builder is the only thing that can make it.
#[test]
fn a_poll_deadline_that_bounds_nothing_is_refused_at_build() -> TestResult {
    let zero = Bot::builder("t06-zero")
        .with_poll_deadline(Duration::ZERO)
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty());
    assert!(
        matches!(zero, Err(BotError::PollDeadlineUnbounded { deadline }) if deadline.is_zero()),
        "a zero per-poll deadline is refused, naming the number: {zero:?}"
    );

    let past = MAX_POLL_DEADLINE.saturating_add(Duration::from_secs(1));
    let over = Bot::builder("t06-over")
        .with_poll_deadline(past)
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty());
    match over {
        Err(BotError::PollDeadlineExceeded { deadline, ceiling }) => {
            assert_eq!(
                deadline, past,
                "the refusal names the budget that was asked for"
            );
            assert_eq!(
                ceiling, MAX_POLL_DEADLINE,
                "and the ceiling that would have been accepted, so the repair is a \
                 number rather than a guess"
            );
        }
        other => return Err(format!("a budget past the ceiling was accepted: {other:?}").into()),
    }

    // The declared default is inside both bounds, so a caller that never calls
    // `with_poll_deadline` gets a bounded poll rather than an unbounded one.
    let default = Bot::builder("t06-default")
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty());
    assert!(default.is_ok(), "the declared default builds: {default:?}");
    assert!(
        DEFAULT_POLL_DEADLINE > Duration::ZERO && DEFAULT_POLL_DEADLINE <= MAX_POLL_DEADLINE,
        "the default ({DEFAULT_POLL_DEADLINE:?}) is inside the bounds the builder \
         enforces, so the default build is a bounded one"
    );
    Ok(())
}

/// A wave spends one watchdog, a mass of waves spends one per wedged wave, and
/// an ordinary tick spends none.
///
/// The row the wave-level watchdog exists for, at the two scales a fixed
/// chain-count fixture cannot reach. The bot declared here is
/// [`MAX_WAVE_CHAINS`] chains wide, so it is exactly **one** observation wave,
/// and it is declared a second time over [`MASS_CHAINS`] chains so the number of
/// waves is larger than one. The source that never resolves is chain 0 in both,
/// which is the first wave.
///
/// The assertion is on `TickReport::watchdogs`, the number an operator reads, not
/// on a test-only counter: a thread count the product cannot report is a thread
/// count the product has promised nothing about, and a test that read a private
/// field would be measuring its own instrumentation.
///
/// Four observations, each a different way to get this wrong:
///
/// 1. a wave with one wedged source spends exactly **one** thread — not one per
///    chain, which is the defect this replaced;
/// 2. the same wave with nothing wedged spends **zero**, which is the lazy half:
///    a poll that answers on its first poll never needed watching;
/// 3. a bot of several waves with one wedged source still spends **one**, because
///    the waves that resolved had nothing pending;
/// 4. every source that was given up on is still **reported**, so a watchdog that
///    was simply never started cannot hide a stall by never firing.
#[test]
fn a_wave_spends_one_watchdog_and_a_mass_of_waves_spends_one_each() -> TestResult {
    // One wave, one wedged source among thirty-one ready ones.
    let mut stalled = mass_bot(
        "t06-watchdog-wave",
        MAX_WAVE_CHAINS,
        true,
        WATCHDOG_DEADLINE,
    )?;
    let _outcome = stalled.tick();
    let report = stalled.tick_report();
    assert_eq!(
        report.watchdogs(),
        1,
        "a wave of {} chains with one source that never resolved spends exactly one \
         watchdog thread, not one per chain",
        MAX_WAVE_CHAINS
    );
    assert_eq!(
        report.stalled().len(),
        1,
        "and reports exactly the one source that stopped answering: {:?}",
        report.stalled()
    );

    // The same wave with nothing wedged: every source answers on its first poll,
    // so there is nothing to watch and the tick spends no thread at all.
    let mut ready = mass_bot(
        "t06-watchdog-ready",
        MAX_WAVE_CHAINS,
        false,
        WATCHDOG_DEADLINE,
    )?;
    let _outcome = ready.tick();
    assert_eq!(
        ready.tick_report().watchdogs(),
        0,
        "an ordinary tick — every source answered on its first poll — starts no \
         deadline thread at all"
    );
    assert!(
        !ready.tick_report().stalled_any(),
        "and reports no stall, because nothing was given up on"
    );

    // Several waves, one wedged source: only the wave holding it is watched.
    let mut mass = mass_bot("t06-watchdog-mass", MASS_CHAINS, true, WATCHDOG_DEADLINE)?;
    let _outcome = mass.tick();
    let mass_report = mass.tick_report();
    assert_eq!(
        mass_report.watchdogs(),
        1,
        "a bot of {MASS_CHAINS} chains is {} waves, and only the one holding the \
         wedged source spends a thread",
        mass_chains(MASS_CHAINS)
    );
    assert_eq!(
        mass_report.stalled().len(),
        1,
        "and the same one chain is reported, whatever the number of waves: {:?}",
        mass_report.stalled()
    );
    Ok(())
}

/// The budget the watchdog fixtures run under.
///
/// Short because this test's subject is a *count* and not a duration: it waits
/// for two real expiries (one wave and one mass) and for nothing else, and the
/// declared thirty-second default would make the count take a minute to observe.
/// Whether a wave spends a thread does not depend on the budget's size, only on
/// whether the reaper had to start at all.
const WATCHDOG_DEADLINE: Duration = Duration::from_millis(120);

/// The chain count one observation wave holds.
///
/// A copy of the substrate's own fan-out rather than a re-derivation: it is the
/// number a wave in *this* fixture spans, and the substrate's constant is the
/// authority on what a wave is. A test that derived it would pass against a
/// substrate whose fan-out had changed, because it would have followed.
const MAX_WAVE_CHAINS: usize = 32;

/// How many chains the multi-wave fixture declares.
///
/// Three full waves plus a remainder, so the number of waves is not a round
/// figure and an assertion that divided by the wave width exactly would miss the
/// fourth wave entirely.
const MASS_CHAINS: usize = 100;

/// How many observation waves a bot of `chains` chains polls in.
fn mass_chains(chains: usize) -> usize {
    chains.div_ceil(MAX_WAVE_CHAINS)
}

/// Declare one more always-answering chain on `$name`, which must already hold a
/// source.
///
/// A macro rather than a loop in the call site because the builder's type follows
/// its source, so a runtime chain count cannot be expressed at all; a macro is
/// the one form in which `n` chains is `n` declarations without `n` copies of the
/// same block for the repository's repetition check to find.
macro_rules! another_chain {
    ($name:ident) => {
        $name = $name
            .observe(Wedged {
                wedged: Rc::new(Cell::new(false)),
                value: Rc::new(Cell::new(1)),
                entered: Rc::new(Cell::new(0)),
                reason: Rc::new(Cell::new(None)),
                domain: "test::mass",
            })
            .on(
                |observed: &u32| *observed > 0,
                Lands {
                    seen: Rc::new(RefCell::new(Vec::new())),
                },
            );
    };
}

/// A bot of `chains` chains whose first source parks forever when `wedged`.
///
/// Every chain after the first is always-ready by construction, which is what
/// makes the count exact: only the first wave can be pending at all, and it is
/// pending only because of chain 0.
fn mass_bot(
    name: &'static str,
    chains: usize,
    wedged: bool,
    deadline: Duration,
) -> Result<Bot, Box<dyn Error>> {
    let mut builder = Bot::builder(name)
        .with_poll_deadline(deadline)
        .observe(Wedged {
            wedged: Rc::new(Cell::new(wedged)),
            value: Rc::new(Cell::new(1)),
            entered: Rc::new(Cell::new(0)),
            reason: Rc::new(Cell::new(None)),
            domain: "test::mass-wedged",
        })
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::new(RefCell::new(Vec::new())),
            },
        );
    for _ in 1..chains {
        another_chain!(builder);
    }
    Ok(builder
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?)
}

/// Run one tick while an independent OS thread samples how long it took.
///
/// The instrument is a separate thread rather than a timer inside the tick, for
/// the reason `resume_liveness.rs` uses one: it must not be the thing being
/// measured. A tick that never returns leaves this call blocked on `bot.tick()`,
/// and the nextest timeout — not this test's own assertion — is what would then
/// report it, which is a measurement of the harness rather than of the substrate.
///
/// The returned `outcome` is the tick's own `Result`, and it is an `Option`
/// because a tick that never returned has none. The pair is returned together
/// so a caller asserts on the elapsed time and the typed error from one
/// observation rather than from two.
fn time_a_tick(bot: &mut Bot, budget: Duration) -> (Duration, Option<BotError>) {
    let started = std::time::Instant::now();
    // An atomic rather than the `Rc<Cell<bool>>` the rest of this file uses for
    // test state: the sampler is an OS thread, so it needs a `Send` flag, and
    // this is the one piece of state that genuinely crosses a thread boundary.
    let released = Arc::new(AtomicBool::new(false));
    let sampler = std::thread::Builder::new()
        .name("t06-tick-watchdog".into())
        .spawn({
            let released = Arc::clone(&released);
            move || sample_a_tick(started, budget, released)
        });
    // Without a watchdog there is no independent oracle, so this is reported as a
    // maximal elapsed time rather than measured from inside the thing being
    // measured. The caller's budget assertion then fails on it, which is the
    // honest outcome: the row could not be evidenced on this host.
    let Ok(sampler) = sampler else {
        return (Duration::MAX, None);
    };
    let outcome = bot.tick().err();
    // Read the clock first, then release and join. A sampler is still sampling
    // until its budget elapses, and joining before reading would charge the tick
    // for the sampler finishing rather than for the tick finishing — which is how
    // a test of "this returns promptly" ends up measuring its own watchdog.
    let elapsed = started.elapsed();
    released.store(true, Ordering::Release);
    let _sampled = sampler.join();
    (elapsed, outcome)
}

/// Run one tick whose source is expected to be cancelled at its deadline, and
/// assert it said so.
///
/// The fourth distinct tick outcome in this file, beside a clean tick, a refusing
/// source and a held transition. It is a separate helper for the same reason the
/// other three are: a caller that came here to read a cancellation wants it
/// matched *exactly*, because `PollStalled` and `DomainError` are different
/// repairs — the first is a source that stopped answering, the second one that
/// refused — and a helper that accepted either would hide which one happened.
fn stalled_tick(bot: &mut Bot) -> TestResult {
    match bot.tick() {
        Ok(fired) => Err(format!(
            "a source that never resolved read as a {fired}-effect quiet tick; the \
             cancellation has to be reported"
        )
        .into()),
        Err(error) => {
            assert!(
                matches!(error, BotError::PollStalled { .. }),
                "the cancellation is the substrate's own typed variant and keeps the \
                 budget that was applied: {error:?}"
            );
            Ok(())
        }
    }
}

/// Wait on a plain OS thread that has no executor to stall, until the caller's
/// budget is spent or the tick it watches has returned.
///
/// The wait is a deadline rather than a fixed sleep, so the sampler returns as
/// soon as the budget is spent instead of rounding up to a multiple of its own
/// interval.
///
/// The sleep is the *fallback*, not the plan. In the ordinary case the caller's
/// tick has already returned and sets `released`, which ends the wait at once —
/// the thread would otherwise outlive the answer by spending the whole ceiling.
/// The case the budget is actually for is a tick that hangs: that is the failure
/// being detected, and the thread is what detects it, so it must keep waiting
/// until it does.
///
/// The workspace's ban on `std::thread::sleep` exists because blocking an
/// *executor* thread stalls every task waiting on it. This is not an executor
/// thread and has nothing to stall: it is the independent sampler, spawned to
/// outlive whatever it measures. `rt::time::sleep` cannot be used because it
/// needs a reactor this thread deliberately does not run — a watchdog sharing the
/// tick's own driver cannot observe that tick failing to make progress.
///
/// A park rather than a sleep: both wait without blocking anything, and the park
/// is the one the workspace names for a synchronous wait. It returns early if
/// this thread is unparked, and nothing unparks it, so the sampling resolution
/// is the slice either way — and the loop condition, not the wait, is what bounds
/// the tick.
fn sample_a_tick(started: std::time::Instant, budget: Duration, released: Arc<AtomicBool>) {
    let slice = Duration::from_millis(2);
    while started.elapsed() < budget && !released.load(Ordering::Acquire) {
        // Never parks past the deadline: `slice` is the polling resolution and
        // the loop condition is the budget, so the overshoot is bounded by a
        // slice rather than by the slice times a whole budget.
        std::thread::park_timeout(slice);
    }
}

// ── T08: a declared failure forces a refresh, not a permanent quiet state ───

/// Each of T08's four causes, as the pair a test needs to arm one.
///
/// Written as a table rather than four hand-written tests because the row is a
/// claim about *all four*: a repair that handles disconnect and silently misses
/// the overflow passes any test that only checks disconnect.
const CAUSES: [(RefreshReason, &str); 4] = [
    (RefreshReason::Disconnected, "disconnected"),
    (RefreshReason::WatchOverflow, "watch-overflow"),
    (RefreshReason::StaleRemoteKey, "stale-remote-key"),
    (RefreshReason::InvalidationFailed, "invalidation-failed"),
];

/// The handles a test needs to arm one declarable source and read what it did.
///
/// A struct rather than a five-tuple: the tuple's shape is written three times
/// across this file, and a named field is what makes `polls` distinguishable
/// from `value` at a call site rather than by position.
struct DeclarableRig {
    /// The bot under test.
    bot: Bot,
    /// The value the source reports.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers.
    reason: Rc<Cell<Option<RefreshReason>>>,
    /// Whether the source refuses to read.
    refusing: Rc<Cell<bool>>,
    /// How many times the source was polled.
    polls: Rc<Cell<u32>>,
    /// Every value the action ran with, in order.
    seen: Rc<RefCell<Vec<u32>>>,
}

/// Build a one-chain bot over a declarable source.
fn declarable_bot(domain: &'static str) -> Result<DeclarableRig, Box<dyn Error>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let source = Declarable::new(1, domain);
    let (value, reason, refusing, polls) = source.handles();
    let bot = Bot::builder(domain)
        .observe(source)
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;
    Ok(DeclarableRig {
        bot,
        value,
        reason,
        refusing,
        polls,
        seen,
    })
}

/// A source whose baseline the substrate is told is unsound is re-read on the
/// next tick, and the tick report names the cause.
///
/// The counterexample this closes: a source reports `Disconnected`, the
/// substrate keeps the value it last saw, and because a re-read would have
/// compared *equal* against that value the substrate never looks again. The bot
/// goes quiet and the only thing a caller can see is zero fired effects, which
/// is indistinguishable from a healthy source that held still.
#[test]
fn a_declared_failure_forces_a_refresh_rather_than_a_permanent_quiet_state() -> TestResult {
    for (cause, spelling) in CAUSES {
        let mut rig = declarable_bot("test::t08_source")?;

        // One clean tick: the source is read, the value commits, the entry runs,
        // and the chain settles. Nothing is armed, so nothing should be forced.
        tick(&mut rig.bot)?;
        let baseline_polls = rig.polls.get();
        assert!(
            baseline_polls >= 1,
            "{spelling}: the baseline tick must have polled the source at least once"
        );
        assert!(
            !rig.bot.tick_report().forced_any(),
            "{spelling}: an undeclared source is not forced — a bot that forces \
             everything would satisfy this row without reading anything"
        );
        let fired_before = ran_with(&rig.seen).len();
        assert!(
            fired_before >= 1,
            "{spelling}: the baseline tick must have fired its entry once"
        );

        // The value has not changed at all. The source says its baseline cannot
        // be trusted, and the substrate must re-read anyway: with the value
        // unchanged, a correct implementation sees "equal" here and fires
        // nothing, which is the right answer — and it only knows it is right
        // because it looked.
        rig.value.set(1);
        rig.reason.set(Some(cause));
        tick(&mut rig.bot)?;

        assert!(
            rig.polls.get() > baseline_polls,
            "{spelling}: a source that declared its cache unsound was not re-read; \
             polls stayed at {}",
            rig.polls.get()
        );
        let report = rig.bot.tick_report();
        let forced = report
            .forced()
            .first()
            .ok_or_else(|| format!("{spelling}: the tick report names no forced refresh"))?;
        assert_eq!(
            forced.reason(),
            cause,
            "{spelling}: the report must name the cause the source declared"
        );
        assert_eq!(
            forced.chain(),
            0,
            "{spelling}: the forced chain is the one whose source declared it"
        );
        assert_eq!(
            forced.domain(),
            "test::t08_source",
            "{spelling}: the report names the source by its own domain identifier"
        );
        assert_eq!(
            ran_with(&rig.seen).len(),
            fired_before,
            "{spelling}: a forced re-read of an unchanged value fires nothing — the \
             value really did not move"
        );
    }
    Ok(())
}

/// A forced refresh commits the value the source reported, not the one it
/// invalidated.
///
/// The half of T08 that is easy to get wrong in the other direction: a repair
/// that re-reads but then compares the fresh read against the baseline it
/// already distrusted would keep the stale value forever and re-observe on every
/// tick without ever converging. Here the value moves *while* the cache is
/// declared unsound, so the forced tick has to both report the forced refresh and
/// commit the newer value.
#[test]
fn a_forced_refresh_commits_the_newer_value_and_then_returns_to_quiet() -> TestResult {
    let mut rig = declarable_bot("test::t08_move")?;

    tick(&mut rig.bot)?;
    assert_eq!(
        ran_with(&rig.seen),
        vec![1],
        "the first generation runs on 1"
    );

    // The baseline is unsound *and* the value moved. The forced tick has to
    // commit 2, not 1: 1 is exactly the value the declaration says cannot be
    // trusted.
    rig.value.set(2);
    rig.reason.set(Some(RefreshReason::WatchOverflow));
    tick(&mut rig.bot)?;

    assert_eq!(
        ran_with(&rig.seen),
        vec![1, 2],
        "the forced refresh must commit the value the source reported, not the one \
         it declared unsound"
    );
    assert!(
        rig.bot.tick_report().forced_any(),
        "the tick that re-read a declared-unsound source reports itself as one"
    );
    let after_move = rig.polls.get();

    // The declaration is spent: the next tick has a sound baseline again, so it
    // is not forced, and a value that has not moved fires nothing. This is the
    // difference between "forced a refresh" and "stayed permanently invalid",
    // and it is the half a test that only checks the first tick would miss.
    rig.reason.set(None);
    tick(&mut rig.bot)?;
    assert!(
        !rig.bot.tick_report().forced_any(),
        "a source that declared nothing is not forced: the previous declaration was \
         spent by the read it forced"
    );
    assert_eq!(
        rig.polls.get(),
        after_move.saturating_add(1),
        "the quiet tick still polls — an unforced tick that skipped the poll would \
         pass this assertion for the wrong reason"
    );
    assert_eq!(
        ran_with(&rig.seen),
        vec![1, 2],
        "a quiet tick fires nothing, and says so rather than looking quiet because \
         it stopped looking"
    );
    Ok(())
}

/// A source whose poll fails keeps its baseline marked, so a *failed* refresh is
/// not mistaken for a completed one.
///
/// The failure mode this closes: the mark is spent by the forced poll, the poll
/// fails, and the next tick compares against the same unsound baseline it was
/// supposed to replace. The bot is quiet again for exactly the reason it was
/// quiet before, and the report says nothing.
#[test]
fn a_failed_refresh_keeps_the_baseline_marked() -> TestResult {
    let mut rig = declarable_bot("test::t08_fail")?;
    tick(&mut rig.bot)?;
    let baseline = rig.polls.get();

    rig.reason.set(Some(RefreshReason::Disconnected));
    tick(&mut rig.bot)?;
    assert!(
        rig.polls.get() > baseline,
        "the armed source must be re-read"
    );
    assert_eq!(
        rig.bot
            .tick_report()
            .forced()
            .first()
            .map(|forced| forced.reason()),
        Some(RefreshReason::Disconnected),
        "the tick report names the cause"
    );

    // The source now refuses, *and* keeps declaring. The forced read never
    // landed, so the mark has to outlive the tick that made it.
    rig.refusing.set(true);
    refused_tick(&mut rig.bot)?;

    // Re-read again: the baseline is still unsound because the forced poll that
    // would have replaced it did not succeed. `declared_refresh` skips a chain
    // that is already marked, so the mark the refusal left standing is what
    // forces this one — not the reason the test happened to re-arm.
    rig.reason.set(None);
    let before_retry = rig.polls.get();
    refused_tick(&mut rig.bot)?;
    assert!(
        rig.polls.get() > before_retry,
        "a refresh that failed must leave the baseline marked, or the bot returns \
         to quiet against the value it just said it could not trust: polls stayed \
         at {}",
        rig.polls.get()
    );
    assert_eq!(
        rig.bot
            .tick_report()
            .forced()
            .first()
            .map(|forced| forced.reason()),
        Some(RefreshReason::Disconnected),
        "and the report still names it — a source that declared nothing on this \
         tick is still carrying the mark its failed refresh left"
    );
    Ok(())
}

/// Two tenants' sources over one bot: one chain's declared failure forces that
/// chain's refresh and leaves the other's quiet.
///
/// The two-tenant case, and the reason the mark is per chain rather than per
/// bot: a bot-wide flag would make one source's transport failure re-read every
/// other source forever, which is the same permanent-quiet defect wearing the
/// opposite face.
#[test]
fn two_tenants_sources_forced_refreshes_stay_attributed_to_their_own_chain() -> TestResult {
    let quiet_value = Rc::new(Cell::new(1));
    let quiet_polls = Rc::new(Cell::new(0));
    let quiet_seen = Rc::new(RefCell::new(Vec::new()));
    let broken_value = Rc::new(Cell::new(1));
    let broken_reason = Rc::new(Cell::new(None));
    let broken_polls = Rc::new(Cell::new(0));
    let broken_seen = Rc::new(RefCell::new(Vec::new()));

    let mut bot = Bot::builder("t08-tenants")
        .observe(Declarable::over(
            Rc::clone(&quiet_value),
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
            Rc::clone(&quiet_polls),
            "test::tenant_quiet",
        ))
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&quiet_seen),
            },
        )
        .observe(Declarable::over(
            Rc::clone(&broken_value),
            Rc::clone(&broken_reason),
            Rc::new(Cell::new(false)),
            Rc::clone(&broken_polls),
            "test::tenant_broken",
        ))
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&broken_seen),
            },
        )
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;

    tick(&mut bot)?;
    assert_eq!(ran_with(&quiet_seen), vec![1]);
    assert_eq!(ran_with(&broken_seen), vec![1]);

    let quiet_before = quiet_polls.get();
    broken_reason.set(Some(RefreshReason::StaleRemoteKey));
    tick(&mut bot)?;

    let report = bot.tick_report();
    assert_eq!(
        report.forced().len(),
        1,
        "exactly the chain that declared is forced, not the whole bot: {:?}",
        report.forced()
    );
    let forced = report.forced().first().ok_or("the broken chain is named")?;
    assert_eq!(
        forced.chain(),
        1,
        "the forced chain is the one whose source declared it"
    );
    assert_eq!(
        forced.domain(),
        "test::tenant_broken",
        "and it is named by its own domain, not the sibling's"
    );
    assert_eq!(forced.reason(), RefreshReason::StaleRemoteKey);
    assert!(
        broken_polls.get() > 1,
        "the declared-unsound source was re-read"
    );
    assert_eq!(
        quiet_polls.get(),
        quiet_before.saturating_add(1),
        "the sibling source is polled exactly once more, like any quiet tick — a \
         per-bot flag would re-read it forever"
    );
    assert_eq!(
        report.superseded(),
        &[],
        "a forced refresh is not a supersession: the value was equal, so nothing \
         was passed over"
    );
    Ok(())
}

// ── T09: event identity and latest-state supersession ──────────────────────

/// The handles a test needs to queue events and read what ran.
struct EventRig {
    /// The bot under test.
    bot: Bot,
    /// The events still to be delivered, in order.
    queue: Rc<RefCell<Vec<EventId<u32>>>>,
    /// Every event id the action ran with, in order.
    seen: Rc<RefCell<Vec<u64>>>,
}

/// Build a one-chain bot over an event feed.
fn event_bot(domain: &'static str) -> Result<EventRig, Box<dyn Error>> {
    let queue = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let bot = Bot::builder(domain)
        .observe(EventFeed {
            queue: Rc::clone(&queue),
            polls: Rc::new(Cell::new(0)),
            domain,
        })
        .on(
            |event: &EventId<u32>| event.id() > 0,
            LandsEvents {
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;
    Ok(EventRig { bot, queue, seen })
}

/// Two events over equal payloads are two events, and a redelivery of one is
/// one.
///
/// The row's own distinction, exercised in both directions in one run because
/// they are the same mechanism read twice: the admitted-input identity of an
/// `EventId` carries its event id, so two ids over equal payloads derive
/// different identities and both execute, while the same id delivered again
/// derives the same one and retires. A bot that keyed dedup on the payload would
/// fire the first and silently drop the second.
#[test]
fn identical_payloads_with_distinct_event_ids_both_execute_and_a_redelivery_does_not() -> TestResult
{
    let mut rig = event_bot("test::t09_feed")?;

    // Event 1 over payload 7, then event 2 over the *same* payload.
    rig.queue.borrow_mut().push(EventId::new(1, 7));
    tick(&mut rig.bot)?;
    assert_eq!(*rig.seen.borrow(), vec![1], "the first event executes");

    rig.queue.borrow_mut().push(EventId::new(2, 7));
    tick(&mut rig.bot)?;
    assert_eq!(
        *rig.seen.borrow(),
        vec![1, 2],
        "a second event id over an identical payload is a different event and must \
         execute; the payload alone cannot decide this"
    );

    // The same event, delivered again. The feed is drained, so the redelivery is
    // queued explicitly.
    rig.queue.borrow_mut().push(EventId::new(2, 7));
    tick(&mut rig.bot)?;
    assert_eq!(
        *rig.seen.borrow(),
        vec![1, 2],
        "a redelivery of the same event must not execute it a second time"
    );

    // And the first event comes round again, to show the dedup is over the whole
    // tagged history rather than over the last event only.
    rig.queue.borrow_mut().push(EventId::new(1, 7));
    tick(&mut rig.bot)?;
    assert_eq!(
        *rig.seen.borrow(),
        vec![1, 2],
        "a redelivery of an *older* event must not execute either — the fence is \
         over the events that have landed, not the most recent one"
    );
    Ok(())
}

/// An intermediate value overtaken before any entry acted on it is reported as
/// superseded, not as a fired effect and not as a retire.
///
/// The situation is a generation held open: the first value's action reports an
/// indeterminate outcome, so the entry is held and the transition keeps its
/// binding. A second value then arrives, and a third before the held one is
/// settled. The substrate admits only the newest once the walk releases the
/// chain, so the second value is passed over entirely — and the row asks for that
/// to be *visible*, because "fired twice" would claim work the bot never did and
/// "fired once" would hide a value the caller never saw.
#[test]
fn an_intermediate_value_is_reported_as_superseded_rather_than_fired_or_retired() -> TestResult {
    let value = Rc::new(Cell::new(1));
    let reason = Rc::new(Cell::new(None));
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("t09-latest")
        .observe(Declarable::over(
            Rc::clone(&value),
            Rc::clone(&reason),
            Rc::new(Cell::new(false)),
            Rc::clone(&polls),
            "test::t09_latest",
        ))
        .on(
            |observed: &u32| *observed > 0,
            NeverSettles {
                seen: Rc::clone(&seen),
            },
        )
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;

    // Generation 1 opens and its action is held: the outcome is unknown, so the
    // entry stays outstanding and the transition keeps value 1 bound to it.
    held_tick(&mut bot)?;
    assert_eq!(
        ran_with(&seen),
        vec![1],
        "the first generation ran its action against value 1"
    );
    assert!(
        !bot.pending().is_empty(),
        "the entry is held: an indeterminate outcome is not a decision"
    );

    // Value 2 commits while generation 1 is still held. It cannot be admitted
    // over the binding, so it waits in the slot, unacted-on. The held generation
    // is still the one the walk reaches, so the tick reports the hold again
    // rather than a clean run.
    value.set(2);
    held_tick(&mut bot)?;
    assert_eq!(
        ran_with(&seen),
        vec![1],
        "the held generation keeps its own binding: value 2 must not reach an entry \
         that was acknowledged against value 1"
    );

    // Value 3 arrives before generation 1 releases. Now value 2 is passed over:
    // the substrate admits only the newest when the chain comes free, and nothing
    // ever acted on 2.
    value.set(3);
    held_tick(&mut bot)?;

    let report = bot.tick_report();
    assert_eq!(
        ran_with(&seen),
        vec![1],
        "nothing new ran: the held entry is still held"
    );
    assert!(
        report.superseded_any(),
        "an intermediate value overtaken before it was acted on must be reported; \
         report was {report:?}"
    );
    let superseded = report
        .superseded()
        .first()
        .ok_or("the pass-over is named")?;
    assert_eq!(
        superseded.chain(),
        0,
        "the pass-over names the chain whose value was overtaken"
    );
    assert_eq!(
        superseded.revision(),
        2,
        "and names the revision the *replaced* value was committed under — the one \
         that will never be acted on"
    );
    assert!(
        !report.forced_any(),
        "a pass-over is not a forced refresh: nothing about the source's caching \
         was in question"
    );
    assert_eq!(
        report.fired(),
        0,
        "a passed-over value is not work done, so it is not counted as a fired effect"
    );

    // Settle the held generation. Settling it admits the newest committed
    // observation — 3, which took the intermediate's place — and that opens the
    // next generation, so this is the tick the action runs for real. It runs
    // with `NeverSettles`, so it holds again; what matters here is the value it
    // ran with, which is the one that survived rather than the one that was
    // passed over.
    let held = bot
        .pending()
        .first()
        .cloned()
        .ok_or("the held entry is reported")?;
    let key = held
        .key()
        .ok_or("a held entry with an attempt carries its key")?;
    bot.resolve_effect(&key, lgwks_bot::spec::EffectEvidence::Applied)?;
    assert_eq!(
        bot.pending().len(),
        0,
        "the settlement decides the generation it names, and reports the chain as \
         free of held work"
    );

    held_tick(&mut bot)?;
    assert_eq!(
        ran_with(&seen),
        vec![1, 3],
        "settling generation 1 admits the newest committed value — 3 — and that is \
         the one that runs. Value 2 never ran, which is what the pass-over reported."
    );
    // No pass-over on the settling tick. Settling generation 1 admits value 3 and
    // runs it, so nothing was overtaken: a pass-over is a value that was never
    // acted on, and 3 was. Reporting one here would be the same defect in the
    // other direction — a healthy bot claiming it skipped a state it did not.
    assert_eq!(
        bot.tick_report().superseded(),
        &[],
        "admitting the newest value is not itself a pass-over: value 3 was admitted \
         here, so nothing unacted was replaced"
    );
    Ok(())
}

// ── T06: a saturated chain does not starve an independent one ──────────────
//
// What this row can and cannot mean here, stated before the tests rather than
// discovered while writing them.
//
// The row asks two things. The first is **saturated task A cannot starve
// independent B** — a chain at its capacity ceiling must not stop an unrelated
// chain from completing. That is about the *walk*: a chain whose entries are all
// held open, and which therefore occupies the transition slot on every tick,
// must not prevent a different chain's entries from being reached and run. Both
// tests below are that claim, and both hold.
//
// The second is **one slow source does not block unrelated ready work** — and
// that is now proved too, by the per-poll deadline this branch added. The gap it
// closed was real: a tick's observation phase joins every source in a wave with
// `lgwks_std::task::join_all_boxed`, which polls them on the calling thread and
// returns only when the whole wave has resolved, so `MAX_IN_FLIGHT_POLLS` bounded
// the fan-out but not the wait, and a source that never resolved held the tick
// with every other chain's action behind it.
//
// The repair is a per-poll deadline rather than a per-wave one, which is the
// narrower of the two: it bounds the one poll that is stuck and leaves its
// siblings free, where a wave deadline would cancel every source in the wave
// whenever any one of them was slow. It is a policy the bot *declares* through
// its own builder (`EcsBuilder::with_poll_deadline`) rather than a second
// admission surface beside `GrantSet`, which is the shape this crate keeps: a
// budget the substrate applies to work it already owns, not one a caller has to
// grant a source permission to declare.
//
// Three tests, and they are three claims rather than three views of one: that the
// tick returns and the independent chain acts inside it
// (`a_slow_source_does_not_block_an_independent_chain`), that the cancelled chain
// is re-polled rather than retired
// (`a_stalled_chain_is_re_polled_and_commits_when_it_answers`), and that the
// bound is a real one rather than a number
// (`a_poll_deadline_that_bounds_nothing_is_refused_at_build`).

/// The handles a test needs to read two chains.
struct PairRig {
    /// The bot under test.
    bot: Bot,
    /// The value the independent chain's source reports.
    ready_value: Rc<Cell<u32>>,
    /// How many times the saturated chain's source was polled.
    saturated_polls: Rc<Cell<u32>>,
    /// How many times the independent chain's source was polled.
    ready_polls: Rc<Cell<u32>>,
    /// Every value the saturated chain's action ran with.
    saturated_seen: Rc<RefCell<Vec<u32>>>,
    /// Every value the independent chain's action ran with.
    ready_seen: Rc<RefCell<Vec<u32>>>,
}

/// Build a two-chain bot: chain 0's action never settles, chain 1's always does.
///
/// Chain 0 is the saturated one and chain 1 the independent one, declared
/// through the same builder call chain — so there is one bot, one world and one
/// schedule step. The point of T06 is that these two live *on one substrate*,
/// not that they are two processes that happen to interleave.
///
/// `NeverSettles` is what holds chain 0 at capacity. Its entry goes to
/// `OutcomeUnknown` and stays there, so the chain has a transition it can never
/// drop: every tick walks it, finds it held, and stops there. That is the
/// saturation, and it is *bounded* — the transition is one state per entry of
/// the spec, so a held chain costs its declared slots and nothing more.
fn saturated_pair(name: &'static str) -> Result<PairRig, Box<dyn Error>> {
    let saturated_value = Rc::new(Cell::new(1));
    let saturated_polls = Rc::new(Cell::new(0));
    let saturated_seen = Rc::new(RefCell::new(Vec::new()));
    let ready_value = Rc::new(Cell::new(2));
    let ready_polls = Rc::new(Cell::new(0));
    let ready_seen = Rc::new(RefCell::new(Vec::new()));

    let bot = Bot::builder(name)
        .observe(Declarable::over(
            Rc::clone(&saturated_value),
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
            Rc::clone(&saturated_polls),
            "test::saturated",
        ))
        .on(
            |observed: &u32| *observed > 0,
            NeverSettles {
                seen: Rc::clone(&saturated_seen),
            },
        )
        .observe(Declarable::over(
            Rc::clone(&ready_value),
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
            Rc::clone(&ready_polls),
            "test::independent",
        ))
        .on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&ready_seen),
            },
        )
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;
    Ok(PairRig {
        bot,
        ready_value,
        saturated_polls,
        ready_polls,
        saturated_seen,
        ready_seen,
    })
}

/// A chain held at capacity does not stop an independent chain from completing.
///
/// The oracle is the log of what ran, not a clock. Chain 0's action reports an
/// indeterminate outcome, so its entry is held for the rest of the run and the
/// walk stops there on every tick — that is the saturation. Chain 1's action
/// lands. If the held chain could starve the independent one, chain 1's log
/// would be empty and this fails on an exact value rather than on a timing.
///
/// The second half is the half a single tick would miss: the held chain is
/// re-walked and re-holds on *every* later tick, and the independent chain runs
/// again on each. Saturation that starves is saturation that persists, so a test
/// that only checks the first tick would pass against a repair that fixed one
/// tick's ordering and left the next one starved.
#[test]
fn a_chain_held_at_capacity_does_not_starve_an_independent_chain() -> TestResult {
    let mut rig = saturated_pair("t06-saturated")?;

    // Three ticks over a still-moving world: the first opens both generations
    // and the next two prove the hold persists without costing chain 1 anything.
    for tick_index in 1..=3_u32 {
        // Chain 0 re-holds, so its own action's error is the first failure in
        // declaration order on every tick the hold persists; chain 1 lands
        // regardless.
        held_tick(&mut rig.bot)?;
        assert_eq!(
            rig.saturated_polls.get(),
            tick_index,
            "the saturated chain's source is polled on every tick: a source that \
             stopped being read would make the saturation vacuous"
        );
        assert_eq!(
            rig.ready_polls.get(),
            tick_index,
            "the independent chain's source is polled on every tick too — a walk \
             that stopped at the held chain would never reach it"
        );
        assert_eq!(
            ran_with(&rig.saturated_seen),
            vec![1],
            "the saturated chain's action ran once across three ticks and its \
             outcome is held throughout: a second run would be a replay of an \
             effect that may already have happened"
        );
        assert_eq!(
            ran_with(&rig.ready_seen),
            vec![2],
            "the independent chain's value held still, so its entry is already \
             settled: a saturated chain cannot starve an independent one, and a \
             quiet one does not re-fire either"
        );
    }

    // The independent chain is the one that moves. It runs a new generation
    // while chain 0 is still held, which is the ordering claim rather than a
    // side effect of it: a walk that stopped at the held chain would leave this
    // log at one entry however ready chain 1 is.
    rig.ready_value.set(3);
    held_tick(&mut rig.bot)?;
    assert_eq!(
        ran_with(&rig.ready_seen),
        vec![2, 3],
        "the independent chain runs a new generation while the other chain is \
         still held at capacity"
    );
    assert_eq!(
        ran_with(&rig.saturated_seen),
        vec![1],
        "and the held chain still has not re-entered its action"
    );

    assert!(
        !rig.bot.pending().is_empty(),
        "the saturated chain's entry is still held after four ticks — this is the \
         capacity being occupied, not a tick that failed"
    );
    let held = rig.bot.pending();
    assert!(
        held.iter().all(|work| work.id().chain() == 0),
        "the only held work is chain 0's: a hold that spread to chain 1 would be \
         starvation wearing a different name — {:?}",
        held
    );
    Ok(())
}

/// A chain held at capacity does not starve an independent chain, at every
/// saturation tier the host can reach.
///
/// 100, 1,000 and 10,000 held chains declared on **one** bot alongside one
/// independent chain. Each held chain occupies a transition that the walk
/// reaches and cannot drop, so the walk's work grows with the mass while the
/// independent chain's share of it does not. The claim is that the independent
/// chain's action still runs, at every tier, and that every held chain is still
/// reported as held rather than silently dropped to make room for it.
///
/// The tier reached is recorded rather than clamped, so a host that reaches only
/// 100 does not read as though it reached 10,000.
#[test]
fn a_saturated_mass_does_not_starve_an_independent_chain_at_every_tier() -> TestResult {
    let mut reached: Vec<usize> = Vec::new();

    for requested in [100_usize, 1_000, 10_000] {
        let mass = Rc::new(RefCell::new(Vec::new()));
        let independent = Rc::new(RefCell::new(Vec::new()));

        // The independent chain is declared first, so it is chain 0 and its
        // effect is the first the walk reaches — the position a starved walk
        // would lose.
        let independent_polls = Rc::new(Cell::new(0));
        let mut builder = Bot::builder("t06-tiers").observe(Declarable::over(
            Rc::new(Cell::new(1)),
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
            Rc::clone(&independent_polls),
            "test::independent",
        ));
        builder = builder.on(
            |observed: &u32| *observed > 0,
            Lands {
                seen: Rc::clone(&independent),
            },
        );

        for _ in 0..requested {
            let seen = Rc::clone(&mass);
            builder = builder
                .observe(Declarable::over(
                    Rc::new(Cell::new(1)),
                    Rc::new(Cell::new(None)),
                    Rc::new(Cell::new(false)),
                    Rc::new(Cell::new(0)),
                    "test::saturated",
                ))
                .on(|observed: &u32| *observed > 0, NeverSettles { seen });
        }

        let mut bot = builder
            .with_effects(memory_scope()?)
            .build(&GrantSet::empty())?;

        // The independent chain's own effect lands, so its entry never holds; the
        // saturated chains each hold and report their own indeterminate outcome,
        // and chain 0 is first in declaration order, so that is what the tick
        // reports. The chains behind it still ran.
        held_tick(&mut bot)?;

        assert_eq!(
            bot.source_domains().len(),
            requested.saturating_add(1),
            "the bot declares one independent chain and {requested} saturated ones"
        );
        assert_eq!(
            ran_with(&independent),
            vec![1],
            "at {requested} saturated chains the independent chain's action still ran"
        );
        assert_eq!(
            ran_with(&mass).len(),
            requested,
            "every saturated chain reached its own action once: a walk that stopped \
             at the first hold would have run one, not {requested}"
        );
        assert_eq!(
            bot.pending().len(),
            requested,
            "and every one of them is still reported as held rather than dropped to \
             make room — a dropped hold is a lost effect nobody would ever see"
        );
        reached.push(requested);
    }

    assert_eq!(
        reached,
        vec![100, 1_000, 10_000],
        "the tiers this run reached, recorded rather than clamped, so a reader is \
         never told a concurrency number nobody ran"
    );
    Ok(())
}
