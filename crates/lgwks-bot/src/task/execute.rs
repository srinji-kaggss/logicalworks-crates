//! The one body of a run, shared by every door, and the reports a run the
//! host declined is given.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::cap::{Cap, Deficit, Demand, uncovered};
use crate::effect::RunId;
use crate::gate::GrantSet;
use crate::script::run_store::{Authority as RecordsAuthority, Records};
use crate::script::trail::Trail;
use crate::script::{FlowError, Scope, within};

use super::admit::{Declined, RunAuthority, Terminal, TrailSnapshot, disposition_of};
use super::host::Plan;
use super::name::TaskName;
use super::{
    BODY_STEP, Control, Disposition, HELD_PERMITS, Host, RepairTicket, Report, StoreError, Task,
};

impl Host {
    /// The one body of a run, shared by [`Host::run`], [`Host::resume`] and
    /// [`Host::repair`].
    ///
    /// Split so there is exactly one place a run mints or adopts its identity,
    /// checks its authority, builds its scope, installs the store and assembles
    /// its report — a second copy of any of those is a second definition of what
    /// a resume means. `repair` is the one door that widens authority, and it is a
    /// parameter rather than a branch, so the admission check below cannot be
    /// bypassed by a caller that happens to hold a ticket.
    ///
    /// The root budget is charged for **every** run, repair or not, and it is
    /// charged *before* the body so a run that spends its budget to zero reaches
    /// the refusal rather than running once more. That is what makes a permanent
    /// refusal plus repeated `NotApplied` finite: the budget is carried in the
    /// run's own ledger, so a fresh process finds it where the last one left it.
    ///
    /// `plan` carries the two facts a caller may declare — the run id to resume
    /// and the definition identity it asserts — as one sum type, so the drift
    /// check has one placement rather than one per door.
    pub(super) async fn execute<I, O, F, Fut>(
        &self,
        task: &Task<F>,
        input: I,
        plan: Plan,
        repair: Option<&RepairTicket>,
        grant_delta: &GrantSet,
        spend: u64,
    ) -> Report<O>
    where
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let started = Instant::now();
        let task_name = task.name_owned();
        let resume = plan.run();
        // The definition this run records under. A run that declared none gets a
        // per-run identity derived from the tenant, the task and the run id, which
        // is stable across attempts at one run and therefore resumes exactly as a
        // run under `run` already does — a declared identity is an addition, never
        // a precondition.
        // A resume's identity names the run it is *replacing* only for its first
        // attempt: a caller that runs the same task on the same input twice gets
        // two runs and two identities, which is right — they are two pieces of
        // work. Declaring one identity for both and resuming the first would then
        // be refused, which is the opposite of what a caller means. So the
        // identity a run is recorded under is its own first attempt's, and a
        // resume is measured against that.
        // Read before the match consumes it: the drift check below needs to know
        // whether the caller declared this identity or the host derived it, and
        // only the first is a claim about the run.
        let declared = plan.declared().cloned();
        let definition = match declared.clone() {
            Some(identity) => identity,
            None => self.definition_for(task_name.as_str(), resume.as_ref()),
        };

        // A resume this store knows belongs to another tenant is refused before
        // admission, so the refusal is a `Refused` with no body and no step rather
        // than a record written under the wrong tenant. A run the store has
        // *never seen* is not refused: a process killed before its first record
        // left no evidence at all, and refusing that resume would make the
        // exactly-at-least-once step unrecoverable. The store still refuses to
        // *read* another tenant's records; this only governs whether the run id
        // may be adopted.
        if let Some(run) = resume
            && let Some(store) = self.inner.store.as_ref()
            && let Some(owner) = store.tenant_of(run)
            && owner != self.inner.tenant.as_str()
        {
            let error = StoreError::ForeignTenant {
                owner,
                asked: self.inner.tenant.as_str().to_owned(),
            };
            return self.refuse(started, task_name, error.to_string());
        }
        // What this call's records will be *written* under, decided before
        // admission. The run a call adopts is not always the run it records
        // against — a fresh run mints one of its own below, and it is *that*
        // run's records this call writes — so a resume reads the identity its own
        // records already carry rather than the one its derivation would name.
        // The steps need that: a durable step must be able to *find* the records
        // it wrote under the identity the run was admitted with, whichever
        // declaration the caller makes now.
        let recorded_definition = match resume {
            Some(adopted) => self.identity_for(adopted, &definition),
            None => definition.clone(),
        };
        // The drift check, before admission and before the body is ever
        // constructed. Replaying a value the current definition never produced and
        // re-running a step whose effect already landed are the two answers on
        // either side of this, so the run is refused rather than picked between,
        // and the refusal names the axis that disagrees so the caller knows which
        // repair it needs. Nothing is written and no permit is taken.
        //
        // Only a *declared* identity is checked. The recorded one is what the
        // steps below write and look up under, so comparing a run's records
        // against it would agree by construction and fence nothing.
        if let (Some(run), Some(store), Some(declared)) =
            (resume, self.inner.store.as_ref(), declared.as_ref())
            && let Err(error) = store.check_definition(run, declared)
        {
            // Every refusal is refused; only the *kind* differs, and the kind is
            // the store's own. A drift is the flow's typed `Incompatible` carrying
            // its axis; every other store refusal — an unreadable device, a
            // ceiling, a foreign tenant — reaches the caller as `FlowError::Store`
            // wrapping the store's own error. Matching on `Incompatible` alone
            // once let a read failure through as though the check had passed,
            // which is INV-BOT-7 in the host's own pre-flight: a store that could
            // not be read admitted the run and let the step below discover it,
            // having already told the caller nothing.
            let located = match error {
                StoreError::Incompatible { ref drift, .. } => {
                    FlowError::incompatible(task_name.as_str(), drift.clone())
                }
                other => FlowError::Store {
                    at: Arc::from(task_name.as_str()),
                    source: Box::new(other),
                },
            };
            self.inner.refused.fetch_add(1, Ordering::Relaxed);
            return self.report(Terminal {
                started,
                task: task_name,
                disposition: Disposition::Refused,
                output: None,
                error: Some(located),
                trail: TrailSnapshot::Empty,
                run: Some(run),
                needs: None,
                repair: None,
            });
        }
        // A resume on a host with nothing to replay from would run every step
        // again under a caller's belief that finished steps are kept. That is the
        // silent downgrade a resume exists to rule out, so it is refused
        // before any step runs.
        if let Some(run) = resume
            && self.inner.store.is_none()
        {
            return self.refuse(
                started,
                task_name,
                format!(
                    "this host has no run store, so run {} has nothing to resume from",
                    run.id().to_hex()
                ),
            );
        }

        // The run identity, decided before admission so a refused run still has
        // the identity a caller can ask about — a run refused at admission ran
        // nothing, and a run id attached to it names a run with no records.
        let run = self.adopt_run(resume);
        // A host given a store promised durable steps. A build that cannot mint
        // the identity those steps are keyed by refuses rather than running
        // them unrecorded and reporting a run nobody can resume.
        if run.is_none() && self.inner.store.is_some() {
            return self.refuse(
                started,
                task_name,
                String::from(
                    "this host has a run store but could not mint a run identity \
                     (the `ephemeral` feature is off or its entropy source failed); \
                     no step ran unrecorded",
                ),
            );
        }

        // The authority check, at the admission boundary and as one complete pass.
        // Only the task's own declaration is checked here, and it is the blunt
        // form: a task that reaches in its *first* step declares it with
        // [`Task::requiring`] and is refused before its body, so nothing runs and
        // nothing is recorded. A task that reaches later declares nothing here and
        // asks at the step that reaches, through [`Scope::require`], so the work
        // before the block survives to be replayed by a repair.
        //
        // The base grant and this run's repair delta are checked *together* and
        // never one replacing the other: a repair adds authority for one run, and
        // every capability the host already had still holds.
        let covers = |cap: &Cap| self.inner.grants.grants(cap) || grant_delta.grants(cap);
        let shortfall = Deficit::from_shortages(uncovered(
            task.needs(),
            covers,
            Some(&Demand::new(task_name.as_str())),
        ));
        if let Some(deficit) = shortfall {
            // Blocked before admission and before the body: nothing was polled and
            // nothing executed, and the report names *every* unmet need plus the
            // repair ticket that closes exactly those. Both come from this one
            // `deficit`, so they cannot disagree about what the run was missing.
            return self.blocked(started, task_name, run, deficit);
        }

        // Admission first, so a run that never starts is never counted as one
        // and never leaves a step in the trail.
        let permit = match self.admit(started, &task_name).await {
            Ok(permit) => permit,
            Err(failure) => {
                let disposition = failure.disposition();
                if disposition == Disposition::Refused {
                    self.inner.refused.fetch_add(1, Ordering::Relaxed);
                }
                return self.report(Terminal {
                    started,
                    task: task_name,
                    disposition,
                    output: None,
                    error: Some(failure.into_error()),
                    trail: TrailSnapshot::Empty,
                    run,
                    needs: None,
                    repair: None,
                });
            }
        };
        // Dropped on every exit path from here on, including a future the caller
        // drops while this run is suspended: the budget returns to what it was,
        // the in-flight count falls, and no body outlives the call.
        let _admission = self.charge(permit);

        // The root budget is charged here, after the run was admitted and confirmed
        // to have authority, but before the body runs. A repair is charged exactly
        // like any other attempt — that is what makes an authorized repair *distinct*
        // rather than a reset (T13). The refusal is reported as a terminal report
        // rather than an error, because the run genuinely happened and its state is
        // exactly this.
        if let Some(run) = run
            && let Some(ledger) = self.inner.ledger.as_ref()
            && let Err(refusal) = ledger
                .charge(
                    self.inner.tenant.as_str(),
                    run,
                    repair.map(RepairTicket::stamp).as_ref(),
                    spend,
                    self.inner.repair_attempts,
                    self.inner.repair_spend,
                )
                .await
        {
            return self.report(Terminal {
                started,
                task: task_name,
                disposition: Disposition::Refused,
                output: None,
                error: Some(FlowError::failed(format_args!(
                    "the run's root budget refused this attempt: {refusal}"
                ))),
                trail: TrailSnapshot::Empty,
                run: Some(run),
                needs: None,
                repair: None,
            });
        }

        let trail = Trail::new(self.inner.limits.progress_capacity());
        // The host's clock, not a fresh one: a run's admission wait and its
        // body's `within` budget are two readings of the same timeline, and
        // minting a second clock here would make a run that is admitted late
        // look to its own body as if it had started on time. The policy is the
        // host's for the same reason: one shared retry budget, not one per run.
        let root = Scope::rooted(
            self.inner.tenant.clone(),
            self.inner.token.child_token(),
            Arc::clone(&trail),
            self.inner.clock.clone(),
            run,
            Arc::clone(&self.inner.policy),
        );
        let scope = match root.enter(task_name.as_str()) {
            Ok(scope) => scope,
            // The task's own step could not be entered: the name validated at
            // declaration, so this is the depth ceiling or a stop that arrived
            // in the instant between admission and entry. Either way the body
            // never ran, and the report says so rather than inventing a trail.
            Err(error) => {
                return self.report(Terminal {
                    started,
                    task: task_name,
                    disposition: disposition_of(&error),
                    output: None,
                    error: Some(error),
                    trail: TrailSnapshot::Empty,
                    run,
                    needs: None,
                    repair: None,
                });
            }
        };

        // A nested run's identity joins the set its parent established, so a
        // child two levels down sees every ancestor's host and is charged the
        // same way. Sibling runs have their own scopes and their own sets.
        //
        // Outside a scope there is no held set, and an empty one is the correct
        // reading: this run holds nothing it inherited, so it is charged for
        // everything it does itself. `Vec::new()` rather than a discard of the
        // not-entered error, which carries no permit information at all.
        let held = match HELD_PERMITS.try_with(Clone::clone) {
            Ok(held) => held,
            Err(not_entered) => {
                lgwks_std::trace::debug!(
                    ?not_entered,
                    "submit: no enclosing scope holds a permit set"
                );
                Vec::new()
            }
        };
        let charged = if held.contains(&self.inner.identity) {
            held
        } else {
            let mut charged = held;
            charged.push(self.inner.identity);
            charged
        };
        let body = (task.body)(scope.clone(), input);
        let deadline = self.inner.limits.default_deadline();
        // Everything from here to the report is one `Box`: this frame is on the
        // stack of every *nested* run, and depth-four nesting at an admission
        // ceiling of one is a shipped property (T04). Holding the body future,
        // both task-local scopes, the deadline wrapper and the report assembly
        // inline makes each level's frame large enough that the default test
        // thread's stack overflows at the depths the invariant names. One
        // pointer per level is the difference between that and not.
        // The store is installed for exactly this future and its descendants, so
        // a durable step records against *this* run and a nested run under the
        // same host inherits both. With no store the scope is entered with no
        // run at all, which is what makes "no durability" the default rather
        // than a claim.
        // One composition for all three task-local scopes rather than one per
        // combination of "has a store" and "has a repair": two alternative
        // nestings in one future carry both in its frame, which is what pushed a
        // two-review test past a 2 MiB thread stack, and a third would have
        // widened the same frame again. Boxed, so a run nested in another run's
        // body costs the parent a pointer rather than its frame.
        let outcome = Box::pin(crate::script::run_store::with_authority(
            Some(self.authority(grant_delta)),
            crate::script::run_store::within(
                self.records(run),
                &recorded_definition,
                HELD_PERMITS.scope(charged, within(&scope, BODY_STEP, deadline, body)),
            ),
        ))
        .await;

        // The task's own step settles before the ring is read, so the trail's
        // first entry says how the run ended as every later entry does.
        scope.settle(&outcome);
        let snapshot = trail.snapshot();
        let (disposition, output, error, needs) = match outcome {
            Ok(value) => (Disposition::Succeeded, Some(value), None, None),
            // A step that reached for authority it does not have blocks the run,
            // and its own shortfall rides out with it: the report's needs and the
            // repair ticket are both built from this one value, so they cannot
            // disagree about what the run was missing. Every other failure is an
            // ordinary located error.
            Err(error) => {
                let located = error.located(&scope);
                let deficit = located.deficit().cloned();
                match deficit {
                    Some(deficit) => (Disposition::Blocked, None, Some(located), Some(deficit)),
                    None => (disposition_of(&located), None, Some(located), None),
                }
            }
        };
        // A blocked run mints its repair ticket from the same shortfall the report
        // carries, and only when the host holds a ledger to decide a later repair
        // against. A blocked run *inside* a repair mints none: a repair that could
        // not close the shortfall hands back the run it was given rather than a
        // fresh ticket for the same block, which would be a way to retry a denied
        // repair by asking again.
        let repair = match (
            needs.as_ref(),
            run,
            self.inner.ledger.as_ref(),
            repair.is_some(),
        ) {
            (Some(deficit), Some(run), Some(ledger), false) => {
                let epoch = ticket_epoch(ledger.control(run));
                Some(RepairTicket::of_deficit(
                    run,
                    self.inner.tenant.as_str(),
                    epoch,
                    deficit,
                ))
            }
            _ => None,
        };
        self.report(Terminal {
            started,
            task: task_name,
            disposition,
            output,
            error,
            trail: TrailSnapshot::Taken(snapshot),
            run,
            needs,
            repair,
        })
    }

    /// A run whose body never ran and whose step trail is therefore empty.
    ///
    /// The one assembly point for the two reports that describe a run the host
    /// declined before a body could start — [`refuse`](Self::refuse) and
    /// [`blocked`](Self::blocked) — because the parts they share are facts rather
    /// than style: no output exists, no step was entered, and the failure is
    /// located at the task because there is no step to locate it in. The two
    /// differ only in their disposition, their run id, and whether unmet
    /// authority rides along.
    pub(super) fn not_admitted<O>(
        &self,
        started: Instant,
        task: TaskName,
        declined: Declined,
    ) -> Report<O> {
        let Declined {
            disposition,
            reason,
            run,
            needs,
            repair,
        } = declined;
        let at = Arc::from(task.as_str());
        self.report(Terminal {
            started,
            task,
            disposition,
            output: None,
            error: Some(FlowError::Failed { at, reason }),
            trail: TrailSnapshot::Empty,
            run,
            needs,
            repair,
        })
    }

    /// A run refused before admission: `Refused`, no body, no step, no run id,
    /// and the reason located at the task.
    pub(super) fn refuse<O>(&self, started: Instant, task: TaskName, reason: String) -> Report<O> {
        self.inner.refused.fetch_add(1, Ordering::Relaxed);
        self.not_admitted(started, task, Declined::plain(Disposition::Refused, reason))
    }

    /// A run blocked on unmet authority: `Blocked`, no body, no step, every unmet
    /// need, and — when this host has a ledger to decide a repair against — the
    /// repair ticket naming exactly those needs.
    ///
    /// One assembly point for the two facts a caller acts on, because they come
    /// from the same [`Deficit`] and a report that named a different set in its
    /// needs and its ticket would be the one thing a caller could not detect.
    pub(super) fn blocked<O>(
        &self,
        started: Instant,
        task: TaskName,
        run: Option<RunId>,
        deficit: Deficit,
    ) -> Report<O> {
        // A ticket is minted only for a run whose epoch the ledger knows and for a
        // run that is *not* already carrying a repair: a repair that could not
        // close the shortfall hands back the run it was given rather than minting a
        // fresh ticket for the same block, which would be a way to retry a denied
        // repair by asking again.
        let repair = match (run, self.inner.ledger.as_ref()) {
            (Some(run), Some(_ledger)) => {
                let epoch = ticket_epoch(
                    self.inner
                        .ledger
                        .as_ref()
                        .and_then(|ledger| ledger.control(run)),
                );
                Some(RepairTicket::of_deficit(
                    run,
                    self.inner.tenant.as_str(),
                    epoch,
                    &deficit,
                ))
            }
            _ => None,
        };
        self.not_admitted(
            started,
            task,
            Declined {
                disposition: Disposition::Blocked,
                reason: String::from("this host's authority does not cover what this task needs"),
                run,
                needs: Some(deficit),
                repair,
            },
        )
    }

    /// The run identity for a run, given the one a resume supplied.
    ///
    /// A resume keeps its own identity. A *fresh* run mints one only when a store
    /// is installed **and** the crate was built with the `ephemeral` feature,
    /// which is the estate's one entropy source. Without a store there is
    /// nothing to key a record by, so a minted id would be an identity nothing
    /// is stored under; without `ephemeral` there is no source to mint from, and
    /// INV-RANDOM-ONE-SOURCE refuses a cheaper substitute. Without a store the
    /// run proceeds with no run id; with a store and no identity, `execute`
    /// refuses rather than run steps the store promised to keep.
    pub(super) fn adopt_run(&self, resume: Option<RunId>) -> Option<RunId> {
        match (self.inner.store.is_some(), resume) {
            (true, Some(run)) => Some(run),
            (true, None) => self.mint_run(),
            (false, _) => None,
        }
    }

    /// A fresh run identity, or `None` when this build cannot mint one.
    ///
    /// The `ephemeral` gate is not incidental: it is the only feature that turns
    /// on the estate's entropy source, and a run id derived from a clock or a
    /// counter would make two runs indistinguishable — exactly what
    /// `lgwks_std::random`'s invariant exists to refuse.
    #[cfg(feature = "ephemeral")]
    pub(super) fn mint_run(&self) -> Option<RunId> {
        RunId::mint().ok()
    }

    /// Without the entropy feature there is no source to mint from.
    ///
    /// A build that wants a durable fresh run enables `ephemeral`; a build that
    /// only resumes supplied run ids needs nothing. Returning `None` keeps the
    /// report honest rather than minting an identity the crate cannot vouch for.
    #[cfg(not(feature = "ephemeral"))]
    pub(super) fn mint_run(&self) -> Option<RunId> {
        None
    }

    /// The authority this run's steps consult: the host's grant plus the delta of
    /// any repair this run carries.
    ///
    /// Always installed, including for a host that grants nothing. A step outside
    /// any host has *no* authority and so requires nothing; a step under a host
    /// that grants nothing has an authority covering nothing and is refused for
    /// whatever it asked about. That distinction — nobody checked versus checked
    /// and refused — is the whole reason this is installed unconditionally rather
    /// than left absent when the grant set is empty.
    pub(super) fn authority(&self, grant_delta: &GrantSet) -> RecordsAuthority {
        RecordsAuthority(Arc::new(RunAuthority {
            base: self.inner.grants.clone(),
            delta: grant_delta.clone(),
            domains: self.inner.domains,
        }))
    }

    /// The erased record store for this run, when there is one.
    pub(super) fn records(&self, run: Option<RunId>) -> Option<Records> {
        // A store with no run cannot key a record, and a run with no store has
        // nowhere to put one; both pairs mean the same thing — this run is not
        // durable — so they are refused here rather than at the first step.
        let store = self.inner.store.as_ref()?;
        run?;
        Some(Records(Arc::new(store.clone())))
    }
}

/// The repair epoch a ticket for a run names: the run's current epoch, or zero
/// for a run the ledger has never seen, because zero is the epoch of a run no
/// repair has touched (`Control::epoch`).
pub(super) fn ticket_epoch(control: Option<Control>) -> u64 {
    match control {
        Some(control) => control.epoch(),
        None => 0,
    }
}
