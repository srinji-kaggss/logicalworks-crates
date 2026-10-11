//! The request-key doors: `Host::submit`, how a receipt already on the disk
//! is reattached or settled, and `Host::repair`.

use std::sync::Arc;
use std::time::Instant;

use crate::effect::{InputIdentity, RunId};
use crate::gate::GrantSet;
use crate::script::run_store::Records;
use crate::script::scope::step_key as reserved_step_key;
use crate::script::{Appended, Durable, FlowError, Scope};

use super::admit::{Terminal, TrailSnapshot, disposition_of};
use super::host::Plan;
use super::name::TaskName;
use super::request::{
    BINDING_STEP, TERMINAL_STEP, TerminalRecord, derive_run as derive_request_run, digest_of_record,
};
use super::{
    Disposition, Existing, Host, InFlight, InputDigest, RepairError, RepairTicket, Report,
    RequestConflict, RequestError, RequestKey, Submission, Task, recorded_receipt, terminal_for,
};

impl Host {
    /// Submit `task` over `input` under a caller-supplied [`RequestKey`],
    /// durably.
    ///
    /// The idempotent-request form of [`Host::run`]: the run identity is derived
    /// from this host's tenant and `key`, the input's canonical [`InputDigest`]
    /// is recorded as a durable receipt *before* any step runs, and the
    /// outcome is:
    ///
    /// - [`Submission::Executed`] for a first submission (the body ran or
    ///   resumed under the derived run, and a terminal outcome is now recorded);
    /// - [`Submission::Reattached`] when the same key with the same input had
    ///   already reached a recorded outcome: the recorded report comes back with
    ///   the body never re-run;
    /// - [`Submission::InFlight`] when a previous client submitted the key and
    ///   went away before the run reached a recorded outcome, so an effect may be
    ///   live (see [`InFlight`]).
    ///
    /// A *different* input under the same key is [`RequestError::Conflict`],
    /// carrying both digests. Distinct keys derive distinct runs and are never
    /// collapsed.
    ///
    /// # Which outcomes are recorded as the request's own
    ///
    /// A terminal outcome is recorded under `@terminal` for exactly the
    /// dispositions that are **the request's verdict**: [`Disposition::Succeeded`],
    /// [`Disposition::Failed`] and [`Disposition::DeadlineExceeded`] — the three
    /// the declared task produces, the deadline included because the same
    /// declaration that fixed the key fixed the run's budget.
    ///
    /// [`Disposition::Cancelled`] (the host's stop arrived after admission) and
    /// [`Disposition::Refused`] (the host declined before admission) are **not**
    /// recorded. They are this host declining to finish the request, not the
    /// request failing, and a key whose terminal record says either is a key no
    /// later submission can ever complete — the key *is* the request's identity,
    /// so its body runs at most once and a stop recorded as the verdict ends it
    /// for good. This call still returns [`Submission::Executed`] with the stop in
    /// the report, so the caller learns that the run was interrupted; what it
    /// does not do is turn a restart into a request that can never succeed. The
    /// receipt and every durable step record survive, which is what makes a later
    /// submission [`Submission::InFlight`] and a [`Host::resume`] under that run
    /// able to settle it with each finished step not re-run.
    ///
    /// Enforced by `tests/request_key.rs`
    /// (`a_host_stop_mid_run_leaves_the_request_resumable`,
    /// `a_refusal_before_admission_is_not_recorded_as_the_outcome`,
    /// `a_failed_run_is_the_requests_recorded_outcome`) and
    /// `tests/sim_request_key.rs` (`host_stops_never_poison_a_key_band_00..03`,
    /// `same_seed_replays_host_stops_band_00..03`).
    ///
    /// # Store required
    ///
    /// A durable submission is defined by its receipt, so a host with no store
    /// refuses with [`RequestError::NoStore`] rather than silently degrading to a
    /// plain run.
    ///
    /// # Errors
    ///
    /// [`RequestError::NoStore`], [`RequestError::Conflict`],
    /// [`RequestError::Record`], [`RequestError::Store`] or
    /// [`RequestError::IdCollision`].
    pub async fn submit<I, O, F, Fut>(
        &self,
        key: &RequestKey,
        task: &Task<F>,
        input: I,
    ) -> Result<Submission<O>, RequestError>
    where
        I: InputIdentity,
        O: Durable,
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let Some(store) = self.inner.store.as_ref() else {
            let refusal = Err(RequestError::NoStore);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "submit: returning an error to the caller");
            return refusal;
        };
        let tenant = self.inner.tenant.as_str();
        let run = derive_request_run(tenant, key)?;
        let records = self.records(Some(run)).ok_or(RequestError::NoStore)?;
        let digest = InputDigest::of(&input);
        let binding_key = reserved_step_key(tenant, BINDING_STEP);
        // The identity the request's own records are written under, derived the
        // same way `execute` derives it for this run, so the binding and terminal
        // records replay under the identity the body's steps were recorded with.
        let definition = self.definition_for(task.name(), Some(&run));

        // A receipt already on the disk decides everything: a repeat of the same
        // input reattaches, and a different input is a conflict.
        if let Some(held) = records.lookup(tenant, run, binding_key)? {
            let existing = Existing {
                store,
                records: &records,
                tenant,
                run,
                key,
                held: &held,
                digest,
            };
            return self.observe_existing(existing, task);
        }

        // No receipt yet: the claim decides which submitter may run the body.
        let claimed = records
            .claim(
                tenant,
                run,
                binding_key,
                BINDING_STEP,
                &definition,
                digest.as_bytes().to_vec(),
            )
            .await?;
        match claimed {
            Appended::Recorded => {}
            Appended::AlreadyRecorded => {
                let held = recorded_receipt(&records, tenant, run, binding_key)?;
                let existing = Existing {
                    store,
                    records: &records,
                    tenant,
                    run,
                    key,
                    held: &held,
                    digest,
                };
                return self.observe_existing(existing, task);
            }
            Appended::Conflicting => {
                let held = recorded_receipt(&records, tenant, run, binding_key)?;
                let existing = digest_of_record(held.bytes())?;
                let refusal = Err(RequestError::Conflict(RequestConflict::new(
                    key.clone(),
                    existing,
                    digest,
                )));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "submit: returning an error to the caller");
                return refusal;
            }
        }

        // The body runs under the derived run, so its durable steps replay on a
        // reattach exactly as a resume's do.
        let report = self
            .execute(task, input, Plan::Resume(run), None, &GrantSet::empty(), 1)
            .await;
        // `None` for a disposition that is the host declining to finish the run,
        // and a receipt with no terminal record is precisely what a later
        // submission of the same key needs in order to be able to run it.
        if let Some(terminal) = terminal_for(&report)? {
            records
                .record(
                    tenant,
                    run,
                    reserved_step_key(tenant, TERMINAL_STEP),
                    TERMINAL_STEP,
                    &definition,
                    terminal.encode()?,
                )
                .await?;
        }
        Ok(Submission::Executed(report))
    }

    /// Observe a request whose receipt is already on the disk.
    ///
    /// The two facts this returns are the whole of T30 and T17: a recorded
    /// terminal outcome is a [`Submission::Reattached`] report reproduced
    /// without re-running the body, and a receipt with no terminal record is a
    /// [`Submission::InFlight`] report that keeps the uncertainty and the
    /// cleanup handle apart.
    pub(super) fn observe_existing<O, F>(
        &self,
        existing: Existing<'_>,
        task: &Task<F>,
    ) -> Result<Submission<O>, RequestError>
    where
        O: Durable,
    {
        let Existing {
            store,
            records,
            tenant,
            run,
            key,
            held,
            digest,
        } = existing;
        let recorded = digest_of_record(held.bytes())?;
        if recorded != digest {
            let refusal = Err(RequestError::Conflict(RequestConflict::new(
                key.clone(),
                recorded,
                digest,
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "observe_existing: returning an error to the caller");
            return refusal;
        }
        let terminal_key = reserved_step_key(tenant, TERMINAL_STEP);
        if let Some(head) = records.lookup(tenant, run, terminal_key)? {
            let (disposition, output, error) =
                TerminalRecord::decode(head.bytes())?.parts::<O>()?;
            let report = self.report(Terminal {
                started: Instant::now(),
                task: task.name_owned(),
                disposition,
                output,
                error,
                trail: TrailSnapshot::Empty,
                run: Some(run),
                needs: None,
                repair: None,
            });
            return Ok(Submission::Reattached(report));
        }
        Ok(Submission::InFlight(InFlight::new(
            run,
            task.name().to_owned(),
            store.record_count(run),
        )))
    }

    /// Settle a request a resume just completed, if `run` is one.
    ///
    /// The counterpart to [`Host::submit`]'s recording: `submit` records a
    /// terminal outcome for the run it just drove, and this records it for a run
    /// a resume drove, so a request interrupted by a host stop (or by a client
    /// that walked away) can reach a recorded verdict at all. Without it the key
    /// would report `InFlight` forever, which is the same permanent outcome the
    /// stop caused in the first place.
    ///
    /// A run that is **not** a request is left alone: the receipt is what makes
    /// a run a request, and an ordinary `Host::resume` of a caller-minted run id
    /// gains no reserved record from having been resumed. A run whose receipt
    /// already holds a terminal record is left alone too, so a second resume
    /// cannot overwrite a verdict already recorded.
    ///
    /// The report is returned unchanged, except that a store that refuses the
    /// record turns the run into a located failure. Recording an outcome and
    /// reporting success are one fact; a host that could not record the verdict
    /// must not hand back the value as though it had.
    ///
    /// A store refusal here is reported as a disposition rather than returned as
    /// an error because the door is [`Host::resume`], whose signature answers a
    /// `Report` for every outcome — including admission refusals, which are
    /// already reported rather than returned.
    pub(super) async fn settle_request<O>(
        &self,
        run: RunId,
        task: TaskName,
        started: Instant,
        report: Report<O>,
    ) -> Report<O>
    where
        O: Durable,
    {
        // A host stop records nothing, so there is nothing here to settle, and
        // the report that says the host declined is the answer the caller needs.
        // A store that cannot be *read* while checking an outcome nobody is
        // going to write must not turn a `Refused` into a `Failed` — which is
        // also what keeps a resume of another tenant's run `Refused`: reading its
        // records is refused by the store itself, and that refusal is about the
        // caller, not about the request.
        if matches!(
            report.disposition(),
            Disposition::Refused | Disposition::Cancelled
        ) {
            return report;
        }
        let Some(records) = self.records(Some(run)) else {
            return report;
        };
        let tenant = self.inner.tenant.as_str();
        match self.unsettled(&records, tenant, run).await {
            // Either this run is not a request, or its verdict is already
            // recorded: nothing here has an opinion about either.
            Ok(false) => report,
            Err(error) => self.report(Self::unrecorded(task, started, run, error)),
            Ok(true) => {
                self.settle(records, tenant, run, task, started, report)
                    .await
            }
        }
    }

    /// Whether `run` is a request whose recorded outcome is still missing.
    ///
    /// A store read failure is `Err` rather than a `false` (INV-BOT-7): absence of
    /// evidence and a failed read are different facts, and answering "nothing to
    /// settle" to a failed read would skip the record the caller came for.
    pub(super) async fn unsettled(
        &self,
        records: &Records,
        tenant: &str,
        run: RunId,
    ) -> Result<bool, FlowError> {
        // Not a request: an ordinary resume of a caller-minted run id writes no
        // reserved record, because the receipt is what makes a run a request.
        if records
            .lookup(tenant, run, reserved_step_key(tenant, BINDING_STEP))?
            .is_none()
        {
            return Ok(false);
        }
        // Already settled. A resume that repeats must not overwrite a verdict a
        // client may already have reattached to.
        Ok(records
            .lookup(tenant, run, reserved_step_key(tenant, TERMINAL_STEP))?
            .is_none())
    }

    /// Write the verdict `report` states for an unsettled request.
    ///
    /// `O: Durable` is required here rather than on [`Host::resume`] because it
    /// is required only to *archive a success*: a resume whose output is not
    /// archivable has no recorded verdict to write even when the run is a
    /// request, and saying that is this run's own answer. A stopped or failed
    /// resume records nothing at all and never reaches the archive, so the bound
    /// costs a caller nothing unless it actually submits a success.
    pub(super) async fn settle<O>(
        &self,
        records: Records,
        tenant: &str,
        run: RunId,
        task: TaskName,
        started: Instant,
        report: Report<O>,
    ) -> Report<O>
    where
        O: Durable,
    {
        let Some(terminal) = terminal_for(&report).ok().flatten() else {
            // A host stop, which is not the request's verdict.
            return report;
        };
        // The identity the verdict is written under, derived as `execute` derives
        // it for this run, so the terminal record sits under the same definition
        // the body's steps were recorded with.
        let definition = self.definition_for(task.as_str(), Some(&run));
        let bytes = match terminal.encode() {
            Ok(bytes) => bytes,
            Err(error) => return self.report(Self::unrecorded(task, started, run, error)),
        };
        let stored = records
            .record(
                tenant,
                run,
                reserved_step_key(tenant, TERMINAL_STEP),
                TERMINAL_STEP,
                &definition,
                bytes,
            )
            .await;
        match stored {
            Ok(()) => report,
            Err(error) => self.report(Self::unrecorded(task, started, run, error)),
        }
    }

    /// A run whose outcome the store refused to record, located at its task.
    pub(super) fn unrecorded<O>(
        task: TaskName,
        started: Instant,
        run: RunId,
        cause: FlowError,
    ) -> Terminal<O> {
        let at = Arc::from(task.as_str());
        let error = FlowError::Failed {
            at,
            reason: format!("the request's outcome was not recorded: {cause}"),
        };
        Terminal {
            started,
            task,
            disposition: disposition_of(&error),
            output: None,
            error: Some(error),
            trail: TrailSnapshot::Empty,
            run: Some(run),
            needs: None,
            repair: None,
        }
    }

    /// Repair a run blocked on unmet authority, and resume it.
    ///
    /// The door a [`RepairTicket`] is answered at. It
    /// resumes the run under the run's own id, so every step recorded before the
    /// block replays without its body being polled — the prior analysis is not
    /// rerun — and only the blocked remainder runs. That is the whole of what a
    /// repair costs and the reason it is safe to hand out more authority: the
    /// work already done is not done again.
    ///
    /// # What is refused, and in what order
    ///
    /// Nothing is applied until every check has passed, and each check leaves the
    /// run blocked with its authority unchanged:
    ///
    /// 1. [`RepairError::NoLedger`] — a host with no repair ledger has no epoch,
    ///    no root budget and no record of applied tickets, so there is nothing to
    ///    decide this repair against. Applying it unchecked would be exactly the
    ///    widen-on-replay this door exists to prevent.
    /// 2. [`RepairError::ForeignTenant`] — the ticket names another tenant's run.
    /// 3. [`RepairError::NotAuthorized`] — the grant does not cover every
    ///    capability the ticket names, so the run would stay blocked.
    /// 4. [`RepairError::OverWide`] — the grant carries any capability the ticket
    ///    does not name, shipped or custom. Refused rather than narrowed, so what
    ///    the caller believes was granted cannot exceed what was asked.
    /// 5. [`RepairError::StaleEpoch`] / [`RepairError::AlreadyApplied`] /
    ///    [`RepairError::BudgetSpent`] — decided by the ledger's one ordered step,
    ///    against the run's *current* epoch, so two repairs delivered together
    ///    cannot both apply.
    ///
    /// Only after all five does the run resume, and it resumes with exactly the
    /// ticket's needs added — the authority applied is built from the ticket, not
    /// taken from `grant`, so it cannot exceed the request even in principle. The
    /// repair is charged against the run's root budget exactly as any other
    /// attempt is, so an authorized repair consumes budget rather than resetting
    /// it (T13).
    ///
    /// # Errors
    ///
    /// Every [`RepairError`] above. On success the returned [`Report`] is the run's
    /// own report — it is the same run, resumed, not a second run — so its output,
    /// its disposition and its step trail describe what the repaired run did.
    ///
    /// # A repaired request is settled
    ///
    /// A run [`Host::submit`] started and that blocked records no verdict
    /// (`Blocked` is not the request's outcome: a repair can still move it), so
    /// the repair is the attempt that reaches one, and it settles the request
    /// exactly as [`Host::resume`] does. Without that, a blocked request repaired
    /// to success would still answer [`Submission::InFlight`] to every later
    /// submission of its key. `O: Durable` for the same reason as on
    /// [`Host::resume`]: a recorded verdict is made of an archived output.
    pub async fn repair<I, O, F, Fut>(
        &self,
        ticket: &RepairTicket,
        grant: &GrantSet,
        task: &Task<F>,
        input: I,
        spend: u64,
    ) -> Result<Report<O>, RepairError>
    where
        O: Durable,
        F: Fn(Scope, I) -> Fut,
        Fut: Future<Output = Result<O, FlowError>>,
    {
        let started = Instant::now();
        let ledger = self.inner.ledger.as_ref().ok_or(RepairError::NoLedger)?;
        if ticket.tenant() != self.inner.tenant.as_str() {
            let refusal = Err(RepairError::ForeignTenant {
                ticket: ticket.tenant().to_owned(),
                host: self.inner.tenant.as_str().to_owned(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "repair: returning an error to the caller");
            return refusal;
        }
        // The grant is checked against the ticket *before* the run is admitted and
        // before the ledger is charged, so a denied repair costs nothing at all:
        // no budget, no epoch, no step.
        ticket.check_grant(grant)?;
        // A run the ledger has never seen has no epoch and no budget, so its
        // authority cannot be attributed; refused rather than minted.
        let control = ledger
            .control(ticket.run())
            .ok_or_else(|| RepairError::UnknownRun {
                run: ticket.run().id().to_hex(),
            })?;
        if control.epoch() != ticket.epoch() {
            let refusal = Err(RepairError::StaleEpoch {
                current: control.epoch(),
                offered: ticket.epoch(),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "repair: returning an error to the caller");
            return refusal;
        }
        let delta = ticket.delta();
        let report = self
            .execute(
                task,
                input,
                Plan::Resume(ticket.run()),
                Some(ticket),
                &delta,
                spend,
            )
            .await;
        Ok(self
            .settle_request(ticket.run(), task.name_owned(), started, report)
            .await)
    }
}
