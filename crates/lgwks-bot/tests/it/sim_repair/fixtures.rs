//! The shapes every repair test builds from: the candidate needs, the seeded plan, hosts, grants, blocked cases and the decision arms.

use super::*;

/// The four capabilities a seeded run can be short of at once.
///
/// Drawn as a prefix, so the seed sweeps T23's "multiple missing permissions" from
/// one capability to all four rather than testing only the one-capability case.
pub(super) fn candidate_needs() -> Vec<Cap> {
    vec![
        Cap::new(Cap::NET),
        Cap::new(Cap::FS),
        Cap::new(Cap::SYS),
        Cap::new(Cap::NOTIFY),
    ]
}

/// One what a scenario's seed decided a caller would do to a ticket.
///
/// A *decision*, not a fault: each arm is one thing an operator or a supervisor
/// might do after reading a repair ticket, and the question the families ask is
/// whether doing the same decision twice, or out of order, changes what the run
/// ends up holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// Answer the ticket with exactly the authority it asks for.
    Repair,
    /// Answer it a second time with the same grant — the redelivery.
    Duplicate,
    /// Answer it with a grant covering only part of what it asks for.
    Deny,
    /// Answer it with a grant that reaches outside the ticket.
    OverWide,
    /// Ask the ledger what the run is at, without applying anything.
    Inspect,
}

impl Action {
    /// The name the trace records.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::Duplicate => "duplicate",
            Self::Deny => "deny",
            Self::OverWide => "over-wide",
            Self::Inspect => "inspect",
        }
    }
}

/// The plan one seed drew: which run, which tenant, and the order of decisions.
pub(super) struct Plan {
    /// The run this scenario is about, drawn so a seed's identity is in the trace.
    pub(super) ordinal: u32,
    /// Which of the four capabilities the run reaches for.
    pub(super) reach: usize,
    /// The decisions, in the order the seed applies them.
    pub(super) actions: Vec<Action>,
}

impl Clone for Plan {
    /// Every field is `Copy` except the action list, so a per-decision copy of the
    /// plan would be free anyway — hand-written so the copy stops compiling the
    /// moment a field stops being `Copy`.
    fn clone(&self) -> Self {
        Self {
            ordinal: self.ordinal,
            reach: self.reach,
            actions: self.actions.clone(),
        }
    }
}

/// A plan drawn from `rng`.
///
/// The reach is cut first and the decision order after, so a change to how a
/// decision is chosen cannot renumber how many capabilities a run reached for in
/// another family: a regression in the denial rules would otherwise be
/// unbisectable against a regression in the need width.
pub(super) fn plan(rng: &mut Rng) -> Result<Plan, Box<dyn Error>> {
    // `1..=3`, not `1..=4`: the seeded families include an over-wide case, and
    // a ticket naming all four shipped capabilities leaves no shipped name outside
    // it to reach for, so the case would have nothing to assert.
    let reach = usize::try_from(rng.between(1, 3))?;
    let count = rng.between(2, 5);
    let actions = (0..count)
        .map(|_| match rng.below(5) {
            0 => Action::Repair,
            1 => Action::Duplicate,
            2 => Action::Deny,
            3 => Action::OverWide,
            _ => Action::Inspect,
        })
        .collect();
    Ok(Plan {
        ordinal: rng.between(1, 1_000),
        reach,
        actions,
    })
}

/// A host for `tenant` over `dir`, granting nothing, with both stores installed.
pub(super) fn blocked_host(tenant: &str, dir: &std::path::Path) -> Result<Host, Box<dyn Error>> {
    shared::repairable_host(tenant, dir, GrantSet::empty())
}

/// The grant a repair door always answers a ticket with.
///
/// `Host::repair` checks the grant against the ticket, so a caller that has a
/// ticket but not its grant would be told the ticket's needs twice. One spelling
/// for "exactly what was asked for" rather than one per test.
pub(super) fn repairable_host(
    tenant: &str,
    dir: &std::path::Path,
    grants: GrantSet,
) -> Result<Host, Box<dyn Error>> {
    shared::repairable_host(tenant, dir, grants)
}

/// The grant that answers a ticket needing the first `reach` capabilities.
pub(super) fn exact_grant(reach: usize) -> GrantSet {
    candidate_needs()
        .iter()
        .take(reach)
        .fold(GrantSet::empty(), |held, cap| held.grant(cap.clone()))
}

/// A grant covering one capability fewer than the ticket asks for.
pub(super) fn short_grant(reach: usize) -> GrantSet {
    candidate_needs()
        .iter()
        .take(reach.saturating_sub(1))
        .fold(GrantSet::empty(), |held, cap| held.grant(cap.clone()))
}

/// A grant covering the ticket's needs *and* one the ticket never asked for.
pub(super) fn wide_grant(reach: usize) -> GrantSet {
    // The next shipped capability past the ticket's own needs, which the
    // over-wide check's candidate list is built to see. `reach` is drawn in
    // `1..=3` for exactly this reason: a ticket naming all four leaves nothing
    // among the shipped names to reach for, and an over-wide grant the check
    // cannot see is not a case this file can assert.
    let beyond = match candidate_needs().get(reach) {
        Some(shipped) => shipped.clone(),
        None => Cap::new("test.beyond"),
    };
    exact_grant(reach).grant(beyond)
}

/// Drive one blocked run for `tenant` over `dir` and hand back its ticket.
///
/// The one place a scenario makes a blocked run, so every family starts from the
/// same state and a family that forgot a step would be visibly different rather
/// than quietly different.
pub(super) fn block(
    tenant: &str,
    dir: &std::path::Path,
    reach: usize,
) -> Result<Case, Box<dyn Error>> {
    let host = blocked_host(tenant, dir)?;
    block_with(&host, reach)
}

/// Drive one blocked run for `tenant` on a host the caller already built.
///
/// [`block`] with the host supplied, because a family that needs its own budget
/// bounds cannot also open a *second* ledger over the same directory: two handles
/// on one chain each hold a length fence the other's writes invalidate, so the
/// second charge is refused as corrupt and the family's fixture — not the property
/// under test — is what fails.
pub(super) fn block_with(host: &Host, reach: usize) -> Result<Case, Box<dyn Error>> {
    let declared = shared::journey_task()?;
    let polls = Polls::shared();
    let needs: Vec<Cap> = candidate_needs().into_iter().take(reach).collect();
    let report =
        lgwks_bot::block_on(host.run(&declared, input_needing(Arc::clone(&polls), 7, needs)));
    if report.disposition() != Disposition::Blocked {
        let refusal = Err(format!(
            "the run must block before any decision: {:?}",
            report.error()
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block_with: returning an error to the caller");
        return refusal;
    }
    let ticket = report
        .repair()
        .ok_or_else(|| shared::missing("a blocked run on a repairable host carries a ticket"))?
        .clone();
    let run = report.run_id().ok_or("a stored run names its run id")?;
    Ok(Case {
        host: host.clone(),
        ticket,
        run,
        declared,
        polls,
    })
}

/// One tenant's run, after its own ticket has been answered.
pub(super) struct Answered {
    /// The tenant that owns the run.
    pub(super) tenant: &'static str,
    /// The run that was blocked and then repaired.
    pub(super) run: RunId,
    /// The ticket it was repaired against.
    pub(super) ticket: RepairTicket,
    /// The host that repaired it.
    pub(super) host: Host,
    /// The task the run used.
    pub(super) declared: Journey,
    /// The counters its attempts share.
    pub(super) polls: Arc<Polls>,
}

/// One blocked run and everything a decision about it needs.
///
/// A named type rather than a five-tuple, because the tuple is read by index at
/// every call site and an index is a fact about the order the fields happen to be
/// in rather than about which field is which.
pub(super) struct Case {
    /// The host the run was attempted on.
    pub(super) host: Host,
    /// The ticket the blocked report handed back.
    pub(super) ticket: RepairTicket,
    /// The run that is blocked.
    pub(super) run: RunId,
    /// The task the run used.
    pub(super) declared: Journey,
    /// The counters this run's attempts share.
    pub(super) polls: Arc<Polls>,
}

/// Apply one decision to a blocked run and record what it did.
///
/// The single decision point every family shares, so "what does a duplicate do"
/// and "what does a stale do" are answered by the same code that runs them in a
/// seeded order rather than by two test files' private copies.
pub(super) fn decide(
    host: &Host,
    declared: &Journey,
    ticket: &RepairTicket,
    polls: &Arc<Polls>,
    action: Action,
    reach: usize,
) -> Result<&'static str, Box<dyn Error>> {
    let (grant, expected) = match action {
        Action::Repair | Action::Duplicate => (exact_grant(reach), "succeeded"),
        Action::Deny => (short_grant(reach), "refused"),
        Action::OverWide => (wide_grant(reach), "refused"),
        Action::Inspect => {
            let ledger = host.run_ledger().ok_or("this host keeps a ledger")?;
            let control = ledger
                .control(ticket.run())
                .ok_or("the blocked run was charged")?;
            return Ok(match (control.epoch(), control.applied()) {
                (0, 0) => "at-zero",
                (1, 1) => "at-one",
                (epoch, applied) => {
                    let refusal = Err(format!(
                        "a run that has had one ticket applied is at epoch 1 with one \
                     applied; got epoch {epoch} with {applied}"
                    )
                    .into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide: returning an error to the caller");
                    return refusal;
                }
            });
        }
    };
    match lgwks_bot::block_on(host.repair(ticket, &grant, declared, input(Arc::clone(polls), 7), 1))
    {
        Ok(report) => Ok(match report.disposition() {
            Disposition::Succeeded => "succeeded",
            other => {
                let refusal = Err(format!(
                    "an authorized repair must succeed, got {other}: {:?}",
                    report.error()
                )
                .into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide: returning an error to the caller");
                return refusal;
            }
        }),
        Err(_) => Ok(expected),
    }
}

/// The epoch and applied count a run should hold after `applied` successful
/// repairs: one, however many deliveries were attempted.
pub(super) fn converged(applied: usize) -> (u64, usize) {
    if applied == 0 { (0, 0) } else { (1, 1) }
}

/// The control state `run` currently holds, or an error naming why it has none.
///
/// One accessor rather than four call sites spelling the same `.ok_or(..)`: a
/// family that forgot the missing case would report "no control state" as an
/// `Option::None` it then silently treated as zero, which is how a run that was
/// never charged reads as a run charged nothing.
pub(super) fn charged(host: &Host, run: RunId) -> Result<lgwks_bot::task::Control, Box<dyn Error>> {
    shared::ledger_of(host)?
        .control(run)
        .ok_or_else(|| missing("the attempt charged its run"))
}

/// The ticket's needs as names, for comparing against a refusal or a report.
pub(super) fn names(caps: &[Cap]) -> Vec<&str> {
    caps.iter().map(Cap::as_str).collect()
}

/// A host for `tenant` over `dir` with its root budget bounded by
/// (`attempts`, `spend`).
///
/// The builder spelled once because T13's arms are all about those two ceilings
/// and three families that each spelled the chain would be three places for a
/// missing `.repair_ledger` to hide.
pub(super) fn bounded_host(
    tenant: &str,
    dir: &std::path::Path,
    attempts: u64,
    spend: u64,
) -> Result<Host, Box<dyn Error>> {
    let stored = Host::builder(tenant)?
        .grants(GrantSet::empty())
        .run_store(dir)?;
    Ok(stored
        .repair_ledger(dir)?
        .repair_bounds(attempts, spend)
        .build()?)
}

/// A task that reaches for `needs` in its **first** step, declared with
/// [`Task::requiring`] rather than reached from inside the body.
///
/// The counterpart to the journey: the journey reaches after its analysis, so it
/// blocks with the analysis already recorded, and a repair has something to
/// replay. This one reaches before any work, so it is refused at the admission
/// boundary and the store holds nothing. Both are the reach mechanism the front
/// door offers, and only the second has been swept.
pub(super) fn first_step_task(needs: Vec<Cap>) -> Result<Journey, Box<dyn Error>> {
    // The binding is what turns the fn item into the fn pointer the journey type
    // names; `journey_task` above gets the same coercion from its own signature.
    let body: fn(Scope, JourneyInput) -> BodyFuture = reach_first;
    Ok(task("reach-first", body)?.requiring(&needs))
}

/// The body [`first_step_task`] declares: it reaches, then counts one poll.
///
/// The `require` is belt and braces — [`Task::requiring`] has already refused the
/// run at the admission boundary — and it is here so the task is honest about the
/// capability it uses if it is ever run under a grant that covers it.
fn reach_first(scope: Scope, input: JourneyInput) -> BodyFuture {
    Box::pin(async move {
        scope.enter("reach")?.require(input.needs())?;
        input.polls().published();
        Ok::<_, FlowError>(input.analysis())
    })
}

/// Report the byte length of `host`'s ledger, or an error naming why it has none.
///
/// Bytes rather than the control state, because "a refused repair left the ledger
/// byte-identical" is a claim about the file and reading the *index* back would
/// confirm only that the handle agrees with itself.
pub(super) fn ledger_bytes(host: &Host) -> Result<u64, Box<dyn Error>> {
    Ok(std::fs::metadata(shared::ledger_of(host)?.path())?.len())
}

/// A description of the arm a decision hit, for the trace and for failures.
///
/// One `&'static str` rather than the `Debug` of an error: a refusal that arrives
/// as `Err(RepairError::AlreadyApplied)` and the same refusal that arrives as a
/// `Refused` report would otherwise print differently under the same label, and
/// the trace hash would depend on which door reported it.
pub(super) fn arm_of(outcome: &Result<Report<u32>, RepairError>) -> &'static str {
    match *outcome {
        Ok(ref report) => disposition_arm(report.disposition()),
        Err(RepairError::AlreadyApplied) => "already-applied",
        Err(RepairError::StaleEpoch { .. }) => "stale-epoch",
        Err(RepairError::NotAuthorized { .. }) => "not-authorized",
        Err(RepairError::OverWide { .. }) => "over-wide",
        Err(RepairError::BudgetSpent { .. }) => "budget-spent",
        Err(RepairError::ForeignTenant { .. }) => "foreign-tenant",
        Err(RepairError::LedgerForeignTenant { .. }) => "ledger-foreign-tenant",
        Err(RepairError::UnknownRun { .. }) => "unknown-run",
        Err(RepairError::NoLedger) => "no-ledger",
        Err(RepairError::Store { .. }) => "store",
        Err(_) => "unrecognized",
    }
}

/// The arm a report's disposition names, without the repair door's error arm.
///
/// A [`Host::resume`](lgwks_bot::task::Host::resume) is not a repair and has no
/// `RepairError` to classify, so this is how the two doors are labelled in one
/// vocabulary — which is the only way a family can say that the budget refused a
/// resume and a repair the same way.
pub(super) fn disposition_arm(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Succeeded => "succeeded",
        Disposition::Blocked => "blocked",
        Disposition::Refused => "refused-at-budget",
        _ => "other-report",
    }
}
