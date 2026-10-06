//! The durable-step contract a [`Scope`] reaches without naming a host.
//!
//! # Why a trait and a task-local rather than a field on [`Scope`]
//!
//! A scope is a cheap, cloneable handle built by hand by every caller in the
//! crate and by every consumer. Adding a store field would make every
//! `Scope::root` construct a store, and would make a plain local flow carry a
//! file path it never uses. Instead the host *installs* the store for the
//! futures it runs, through the same task-local mechanism
//! [`Host::run`](crate::task::Host) already uses for the held-permit set: the
//! body of a run sees exactly the store that host installed, a step outside a
//! run sees none, and nothing carries a `Store` a caller could not see.
//!
//! The trait is what [`Scope::remember`] calls, so the script module has no
//! dependency on [`lgwks_bot::task`](crate::task) and the task module supplies
//! the one shipped implementation. A caller that wants a store of their own can
//! implement this — it is two methods over archived bytes — but must honour the
//! same three obligations: [`lookup`] distinguishes a read failure from
//! absence, [`append`] returns only after the bytes are durable, and both bind
//! the caller's tenant.

use std::future::Future;
use std::sync::Arc;

use lgwks_std::wire::rkyv::de::pooling::Pool;
use lgwks_std::wire::rkyv::rancor::Strategy;
use lgwks_std::wire::{Archive, Deserialize, Serialize, WireError};

use crate::effect::RunId;
use crate::rt::task_local;
use crate::task::{DefinitionIdentity, Drift};

use super::{FlowError, Scope, StepKey};

/// The bound a step's value must satisfy to be a durable step's result.
///
/// `lgwks_std::wire`'s checked decoder names the deserializer strategy it uses
/// rather than leaving it to inference, and that strategy type is a detail of
/// the facade. Naming the bound once here means the signature of [`remember`]
/// reads as `T: Durable` — something a consumer can see and a doc can explain —
/// instead of four levels of rkyv generics that would change with the release.
pub trait Durable: Sized {
    /// Archive this value the way a run store frames it.
    ///
    /// # Errors
    ///
    /// [`WireError`] when the value does not archive.
    fn to_record(&self) -> Result<Vec<u8>, WireError>;

    /// Rebuild this value from the bytes a run store framed.
    ///
    /// # Errors
    ///
    /// [`WireError`] when the bytes are not this type's archive.
    fn from_record(bytes: &[u8]) -> Result<Self, WireError>;
}

impl<T> Durable for T
where
    T: Archive,
    for<'a> T: Serialize<
        Strategy<
            lgwks_std::wire::rkyv::ser::Serializer<
                lgwks_std::wire::AlignedVec,
                lgwks_std::wire::rkyv::ser::allocator::ArenaHandle<'a>,
                lgwks_std::wire::rkyv::ser::sharing::Share,
            >,
            WireError,
        >,
    >,
    for<'a> <T as Archive>::Archived: Deserialize<T, Strategy<Pool, WireError>>
        + lgwks_std::wire::rkyv::bytecheck::CheckBytes<
            Strategy<
                lgwks_std::wire::rkyv::validation::Validator<
                    lgwks_std::wire::rkyv::validation::archive::ArchiveValidator<'a>,
                    lgwks_std::wire::rkyv::validation::shared::SharedValidator,
                >,
                WireError,
            >,
        >,
{
    fn to_record(&self) -> Result<Vec<u8>, WireError> {
        lgwks_std::wire::to_bytes::<WireError>(self).map(|bytes| bytes.as_ref().to_vec())
    }

    fn from_record(bytes: &[u8]) -> Result<Self, WireError> {
        lgwks_std::wire::from_bytes::<T, WireError>(bytes)
    }
}

task_local! {
    /// The durable record store for the run this future belongs to, if any.
    static RECORDS: Option<Records>;

    /// The definition identity the run above records its values under.
    ///
    /// Shared rather than borrowed for the same reason [`RECORDS`] is: the value
    /// has to outlive the frame that installed it, because a nested run installs
    /// its own and restores this one on the way out. A clone is one reference
    /// count.
    static DEFINITION: Option<Arc<DefinitionIdentity>>;

    /// The authority a step's `require` is checked against, if any.
    static AUTHORITY: Option<Authority>;
}

/// The authority a run's steps are checked against: the host's grant plus the
/// delta of any repair this run is carrying.
///
/// Object-safe and `Arc`-held for the same reason as [`RunRecords`] — it is
/// installed as a `dyn` behind a task-local and consulted by borrowed methods, so
/// a step body can ask what it may do without the host being named in its
/// signature.
#[derive(Clone)]
pub(crate) struct Authority(pub(crate) Arc<dyn AuthorityCheck>);

/// What a step's `require` asks of the run's authority.
///
/// One method, because there is one question: *is this capability covered right
/// now*. It returns the whole shortfall rather than a `bool`, so a step that is
/// short of three capabilities learns all three at once and the repair ticket
/// built from the answer is written once.
pub(crate) trait AuthorityCheck: Send + Sync {
    /// Every capability in `required` this run's authority does not cover, in
    /// declaration order, each named once.
    fn uncovered(&self, required: &[crate::cap::Cap]) -> Vec<crate::cap::Shortage>;
}

impl Authority {
    /// The shortfall for `required`, or `None` when no authority is installed —
    /// which is a local run outside any host, where a step reaches nothing and so
    /// needs nothing.
    pub(crate) fn shortfall(
        &self,
        required: &[crate::cap::Cap],
    ) -> Option<Vec<crate::cap::Shortage>> {
        let short = self.0.uncovered(required);
        (!short.is_empty()).then_some(short)
    }
}

/// The authority installed for the current future, if any.
pub(crate) fn authority() -> Option<Authority> {
    // A task-local read that fails means the future was polled outside every scope,
    // which is a different fact from a scope that installed the absence on purpose.
    // The access error is recorded rather than dropped, because a caller reading
    // "no authority" from a future that should have had one is looking at a wiring
    // defect, and a silently discarded error is how that stays invisible.
    let installed = AUTHORITY.try_with(Clone::clone);
    if let Err(ref access) = installed {
        lgwks_std::trace::debug!(error = ?access, "authority: polled outside every scope, so no authority is installed");
    }
    installed.ok().flatten()
}

/// Install `authority` for the futures polled inside `body`; `None` installs the
/// absence a run with no authority reports, through the same one composition.
///
/// Crate-private for the same reason as [`within`]: the host is the supported way
/// to make a run checkable, and a caller who installed their own authority would
/// be granting themselves the capability the step is asking about.
pub(crate) async fn with_authority<R>(
    authority: Option<Authority>,
    body: impl Future<Output = R>,
) -> R {
    AUTHORITY.scope(authority, body).await
}

/// The durable store a run's durable steps consult.
///
/// Object-safe, because it is installed as a `dyn` behind a task-local and
/// reached through `Arc<dyn RunRecords>`; it takes only borrowed arguments, so
/// it needs no generic method and no `Box`.
pub trait RunRecords: Send + Sync {
    /// The record committed for `key` under `run`, or `None` when there is
    /// none.
    ///
    /// `Err` is a read failure, never a miss: reporting one as `None` would make
    /// a step that had finished run again (INV-BOT-7).
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the store cannot be read, including when `run` belongs
    /// to a tenant other than the one asking.
    fn lookup(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
    ) -> Result<Option<StoredValue>, FlowError>;

    /// Commit the record for `key` under `run`, durably, and return only once
    /// the bytes are on the device.
    ///
    /// # Errors
    ///
    /// [`FlowError`] for any refusal, including a declared ceiling. A refusal
    /// never truncates a committed record.
    fn append(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
        path: &str,
        definition: &DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> Result<Appended, FlowError>;

    /// [`RunRecords::append`], waited for rather than sat through.
    ///
    /// The door [`remember`] takes, and the reason it is a separate method rather
    /// than an `async fn` on the trait: a durable write is `write_all` plus
    /// `sync_all`, and running that on the thread awaiting it parks every other
    /// task on that executor for the length of an `fsync`. A store that implements
    /// this hands the write to a thread of its own and this future waits for the
    /// answer, so the device's latency is the device's and the runtime keeps
    /// turning (INV-BOT-50).
    ///
    /// The default implementation calls [`RunRecords::append`] on the awaiting
    /// thread, which is correct and is exactly the defect this method exists to
    /// remove — so a store that has no owner thread is a store that blocks, not one
    /// that loses the record.
    ///
    /// # Errors
    ///
    /// Whatever [`RunRecords::append`] reports.
    fn append_async<'a>(
        &'a self,
        record: StagedRecord<'a>,
    ) -> crate::BoxFuture<'a, Result<Appended, FlowError>> {
        Box::pin(async move {
            let (tenant, run, key, path, definition) = (
                record.tenant(),
                record.run(),
                record.key(),
                record.path(),
                record.definition(),
            );
            self.append(tenant, run, key, path, definition, record.into_bytes())
        })
    }

    /// Whether this store's records for `run` were written under `definition`.
    ///
    /// The replay's own pre-flight, asked by every durable step before it returns
    /// a recorded value. A store that holds no record for the run at all answers
    /// `true`: that is a first attempt, and refusing it would make the first
    /// record unreachable.
    ///
    /// The default answers `true` because a store with no identity of its own has
    /// nothing to compare against — but a store that *does* have one must report a
    /// refusal as `Err` and never as `false`. A `false` is a claim that the
    /// records disagree with this definition, and a store that could not read its
    /// own records has established no such thing (INV-BOT-7): collapsing the two
    /// reports an unreadable store as a drifted one, which sends the caller
    /// looking for a change to a definition when the device is what failed.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the store could not answer the question. The caller
    /// treats it as a refusal rather than as a compatibility, and it carries the
    /// store's own typed error rather than a rendering of it.
    fn compatibility(
        &self,
        _run: RunId,
        _definition: &DefinitionIdentity,
    ) -> Result<bool, FlowError> {
        Ok(true)
    }

    /// Which axis of `run`'s records disagrees with `definition`.
    ///
    /// A second door beside [`RunRecords::compatibility`] and not a return value
    /// of it, because the two answer different questions. `compatibility` answers
    /// *whether* to proceed and must be cheap enough to ask on every durable step;
    /// `drift` answers *which axis* and is asked only after a step has already been
    /// told no, where the caller needs a repair — a new run, a decision, a
    /// migration or an edit.
    ///
    /// Only the store knows the identity its records were written under, so the
    /// caller cannot recompute this from the declared identity alone; folding it
    /// into the boolean would have meant returning an `Option<Drift>` in place of
    /// an answer that is also legitimately `None`, and a caller that ignored the
    /// payload would have had no axis to report.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the store holds no record for `run`, and therefore has
    /// no recorded identity to compare against. That is a state a caller cannot
    /// reach through [`RunRecords::compatibility`] — it answers `true` for a run
    /// the store never wrote — so the error is a refusal to invent an answer, not
    /// a path the shipped step takes. A store with no identity of its own does not
    /// need this door: it may report no drift, consistent with its `compatibility`
    /// answering `true`.
    fn drift(&self, _run: RunId, _definition: &DefinitionIdentity) -> Result<Drift, FlowError> {
        Err(FlowError::failed(
            "this store holds no definition identity for the run, so it cannot name a \
             drift axis; refusing to invent one",
        ))
    }

    /// Build the record this store is being asked to commit.
    ///
    /// One constructor for the six fields both doors take, because the awaited door
    /// and the blocking door are otherwise two places that assemble the same thing
    /// and could disagree about what a record is.
    fn stage<'a>(
        &self,
        tenant: &'a str,
        run: RunId,
        key: StepKey,
        path: &'a str,
        definition: &'a DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> StagedRecord<'a> {
        StagedRecord::of(tenant, run, key, path, definition, bytes)
    }
}

/// One record a store is being asked to commit, borrowed rather than assembled at
/// each call site.
///
/// The trait's two doors take the same six facts, and a store's implementation
/// wants them as one value; a struct the caller fills once through
/// [`StagedRecord::of`] is what keeps those two doors from being two copies of the
/// same parameter list.
///
/// Fields private and reached through accessors, because the invariants a store
/// checks — the tenant that owns the run, the key the record is filed under, the
/// definition it was written under — are the store's to make, not a caller's to
/// have bypassed by writing the field directly.
#[derive(Debug, Clone)]
pub struct StagedRecord<'a> {
    /// The tenant the run belongs to.
    tenant: &'a str,
    /// The run the record is for.
    run: RunId,
    /// The step key it is filed under.
    key: StepKey,
    /// The step's path, for attribution.
    path: &'a str,
    /// The definition the value is recorded under.
    definition: &'a DefinitionIdentity,
    /// The archived value.
    bytes: Vec<u8>,
}

impl<'a> StagedRecord<'a> {
    /// The six facts a record is, assembled once.
    fn of(
        tenant: &'a str,
        run: RunId,
        key: StepKey,
        path: &'a str,
        definition: &'a DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            tenant,
            run,
            key,
            path,
            definition,
            bytes,
        }
    }

    /// The tenant the run belongs to.
    #[must_use]
    pub const fn tenant(&self) -> &'a str {
        self.tenant
    }

    /// The run this stored record was filed under, which names the run that wrote it.
    #[must_use]
    pub const fn run(&self) -> RunId {
        self.run
    }

    /// The step key the record is filed under.
    #[must_use]
    pub const fn key(&self) -> StepKey {
        self.key
    }

    /// The step's path, for attribution.
    #[must_use]
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// The definition the value is recorded under.
    ///
    /// Borrowed rather than cloned because the store copies it once, onto its own
    /// storage thread, where the frame head is computed — the same place the
    /// tenant and path are copied, so a store cannot record one without the other.
    #[must_use]
    pub const fn definition(&self) -> &'a DefinitionIdentity {
        self.definition
    }

    /// The archived value, by reference.
    ///
    /// A borrow rather than a `take`, so reading a staged record cannot empty it
    /// behind a store that is about to write it.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The archived value, by value, for the one store that moves it onto its own
    /// storage thread.
    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// A committed record, as the script module receives it: the step's path and its
/// archived bytes.
///
/// The bytes rather than the decoded value because decoding needs the caller's
/// type `T`, which the store does not know. Holding the path beside the value is
/// what lets a decode failure be attributed to the step rather than to the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredValue {
    /// The step path this record is about.
    path: String,
    /// The archived value.
    bytes: Vec<u8>,
}

impl StoredValue {
    /// Build a record from the path and archived bytes the store holds.
    ///
    /// Crate-private because a `StoredValue` never escapes the crate except
    /// through [`super::remember`], which decodes it under the step's own type;
    /// a public field would be a value a caller could hand to a step of the
    /// wrong type.
    pub(crate) fn new(path: String, bytes: Vec<u8>) -> Self {
        Self { path, bytes }
    }

    /// The archived bytes this record holds, for a host that stores a fixed-width
    /// reserved value (a request digest, a terminal record) under a step key.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The archived bytes, by value.
    ///
    /// The reader's door, and the counterpart to the borrow
    /// [`StagedRecord::bytes`] takes on the writer's side. `pub(crate)` for the same
    /// reason [`StoredValue::new`] is: the store hands these out and
    /// [`crate::task::RunStore::lookup`] is the one place a caller may read them,
    /// so no public field can hand a step of the wrong type a value it will decode.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// What a durable append did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Appended {
    /// The record is committed and durable.
    Recorded,
    /// An identical record was already committed; nothing new was written and
    /// the step is known durable.
    AlreadyRecorded,
    /// A different record is committed under this key. Nothing was written; the
    /// caller must not treat the step as re-recorded.
    Conflicting,
}

/// The erased store handle a run installs.
#[derive(Clone)]
pub(crate) struct Records(pub(crate) Arc<dyn RunRecords>);

impl Records {
    /// The typed lookup this module performs, or `None` when no store is
    /// installed.
    pub(crate) fn lookup(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
    ) -> Result<Option<StoredValue>, FlowError> {
        self.0.lookup(tenant, run, key)
    }

    /// Whether this store's records for `run` agree with `definition`, or the
    /// store's own refusal.
    ///
    /// The check every durable step makes before it replays a value, and the
    /// reason a store that was handed a run from elsewhere cannot answer from
    /// another definition's records. `Ok(())` when the store holds no record for
    /// the run at all, which is the same answer it gives a first attempt.
    ///
    /// Both failures leave as `Err`, and they are *different* errors. A store
    /// refusal is propagated as itself, because it says nothing about the
    /// definition: folding it into a boolean would tell the caller "recorded under
    /// a different definition" — a specific, actionable claim — about a store
    /// whose answer nobody knows, and would make the one fault an operator must
    /// see indistinguishable from a genuine drift (INV-BOT-7). A disagreement
    /// leaves as the typed [`FlowError::Incompatible`] carrying the axis, so the
    /// caller learns whether it needs a new run, a decision, a migration or an
    /// edit rather than having to parse a rendered sentence to find out.
    ///
    /// The two are raised from one call rather than from a boolean plus a second
    /// question, because a store that answers "no" and then cannot name the axis
    /// would otherwise leave the caller with a refusal it cannot repair — and
    /// inventing an axis at that point is exactly the guess the typed arm rules
    /// out.
    ///
    /// # Errors
    ///
    /// [`FlowError::Incompatible`] carrying the drift, or whatever typed error the
    /// store itself reported.
    pub(crate) fn agrees(
        &self,
        run: RunId,
        definition: &DefinitionIdentity,
    ) -> Result<(), FlowError> {
        match self.0.compatibility(run, definition) {
            Ok(true) => Ok(()),
            Ok(false) => Err(FlowError::incompatible("", self.0.drift(run, definition)?)),
            Err(error) => Err(error),
        }
    }

    /// The durable append, refusing a conflicting record as an error.
    ///
    /// A conflict is turned into a [`FlowError`] here rather than being left as
    /// an [`Appended`] arm the caller must check, because the one correct
    /// response to a conflicting record is to refuse the step, and a caller who
    /// forgets to check the arm would silently keep a rewritten history.
    ///
    /// # Errors
    ///
    /// [`FlowError`] on a refusal or on a conflict.
    pub(crate) fn record<'a>(
        &'a self,
        tenant: &'a str,
        run: RunId,
        key: StepKey,
        path: &'a str,
        definition: &'a DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> crate::BoxFuture<'a, Result<(), FlowError>> {
        let conflict = std::sync::Arc::from(path);
        Box::pin(async move {
            match self
                .0
                .append_async(self.0.stage(tenant, run, key, path, definition, bytes))
                .await
            {
                Ok(Appended::Recorded) | Ok(Appended::AlreadyRecorded) => Ok(()),
                Ok(Appended::Conflicting) => Err(FlowError::Failed {
                    at: conflict,
                    reason: format!(
                        "a different value is already recorded for step {path:?} under this run; \
                         refusing to overwrite a committed record"
                    ),
                }),
                Err(error) => Err(error.located_at(&conflict)),
            }
        })
    }

    /// The durable append, reporting which [`Appended`] arm it landed on.
    ///
    /// The door a request key's first submission takes: `Recorded` is the caller
    /// that may run the body, `AlreadyRecorded` is one that must reattach, and
    /// `Conflicting` is a different payload under the same key. [`Self::record`]
    /// collapses the first two because a durable *step* treats them the same;
    /// a submission cannot, so it needs the arm itself.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the store refuses the read or the write.
    pub(crate) async fn claim<'a>(
        &'a self,
        tenant: &'a str,
        run: RunId,
        key: StepKey,
        path: &'a str,
        definition: &'a DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> Result<Appended, FlowError> {
        self.0
            .append_async(self.0.stage(tenant, run, key, path, definition, bytes))
            .await
    }
}

impl std::fmt::Debug for Records {
    /// The installed store's own `Debug`, so a host's debug output names the
    /// store it installed without the script module importing its type.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RunRecords")
    }
}

/// Install `records` and `definition` for the futures polled inside `body`;
/// `None` installs the absence a storeless run reports, through the same one
/// composition.
///
/// Crate-private: the host is the supported way to make a run durable (DX-10),
/// and a caller who installed their own store would be reaching past the one
/// path that also mints the run id the record is keyed by.
pub(crate) async fn within<R>(
    records: Option<Records>,
    definition: &DefinitionIdentity,
    body: impl Future<Output = R>,
) -> R {
    RECORDS
        .scope(
            records,
            DEFINITION.scope(Some(Arc::new(definition.clone())), body),
        )
        .await
}

/// The definition the current run records under, when one was installed.
///
/// `None` for a step outside a run, which is what a hand-built [`Scope`] with no
/// host above it is. The caller then derives its own, so the two doors — a
/// host-installed identity and a bare scope — cannot disagree about whether a run
/// declared one.
pub(crate) fn installed_definition() -> Option<Arc<DefinitionIdentity>> {
    // Recorded, then dropped: a scope that installed no identity is the ordinary
    // case, a failed access is a wiring defect, and only one of the two should read
    // as absence.
    let installed = DEFINITION.try_with(Clone::clone);
    if let Err(ref access) = installed {
        lgwks_std::trace::debug!(error = ?access, "installed_definition: polled outside every scope, so no identity is installed");
    }
    installed.ok().flatten()
}

/// The store installed for the current future, if any.
pub(crate) fn installed() -> Option<Records> {
    // As `installed_definition`: a scope that installed no store is ordinary, and a
    // failed access is recorded so it cannot be read as one.
    let installed = RECORDS.try_with(Clone::clone);
    if let Err(ref access) = installed {
        lgwks_std::trace::debug!(error = ?access, "installed: polled outside every scope, so no store is installed");
    }
    installed.ok().flatten()
}

/// The definition a durable step records under.
///
/// The run's host-installed identity when one is installed, otherwise one derived
/// from `scope` and `step`. One resolution point, so a refusal [`remember_at`]
/// writes and a value [`remember`] writes cannot disagree about the definition
/// their run is using.
pub(crate) fn step_definition(scope: &Scope, step: &str) -> Arc<DefinitionIdentity> {
    match installed_definition() {
        Some(installed) => installed,
        None => Arc::new(definition_of(scope, step)),
    }
}

/// Run `body` once and remember its value across runs of this step.
///
/// The public spelling of the durable step. `step` is the step's name and enters
/// a child scope for the key and the location; `body` is the work, and is
/// polled only when no completed record for this step is committed under the
/// current run. Without a host that installed a store, this is exactly `body`:
/// the value is returned and nothing is claimed to be durable.
///
/// `T` must be archivable through [`lgwks_std::wire`], which is what makes a
/// recorded value readable after the process that produced it is gone.
///
/// # Errors
///
/// Whatever `body` returns, or [`FlowError`] when the store refuses to read or
/// write the record.
///
/// # Example
///
/// ```
/// use lgwks_bot::script::{FlowError, Scope, remember};
///
/// # async fn run(scope: &Scope) -> Result<(), FlowError> {
/// let fetched = remember(scope, "fetch", || async { Ok::<_, FlowError>(41u32) }).await?;
/// assert_eq!(fetched, 41);
/// # Ok(())
/// # }
/// ```
pub async fn remember<T, Fut>(
    scope: &Scope,
    step: &str,
    body: impl FnOnce() -> Fut,
) -> Result<T, FlowError>
where
    T: lgwks_std::wire::Archive + Durable,
    Fut: Future<Output = Result<T, FlowError>>,
{
    let child = scope.enter(step)?;
    // The host-installed identity, or one derived from this scope's own facts when
    // no run above declared one. Derived rather than defaulted so two runs on one
    // host cannot claim one definition, and so a hand-built scope with no host
    // above it still records something that is stable for its own run.
    let definition = step_definition(scope, step);
    step_in(&child, &definition, body).await
}

/// The definition a step records under when no host installed one.
///
/// The per-step fallback, and the reason it is a digest rather than a fixed
/// constant: the run id and the step's own path are both already facts this
/// store holds, so naming them together gives a stable identity for a flow that
/// never declared a revision — and it is deliberately *not* stable across two
/// different run ids, so a caller that resumes a run it does not hold cannot
/// inherit a definition from wherever it looked.
///
/// `pub(crate)` because it is derived once here rather than at each call site:
/// a caller that stages or appends a record by hand — the storage owner's own
/// tests do — needs the same identity a `remember` would have written, or the
/// record it commits is one no replay would recognise.
pub(crate) fn definition_of(scope: &Scope, step: &str) -> DefinitionIdentity {
    let mut hasher = lgwks_std::hash::Hasher::new();
    hasher.write_framed(b"lgwks.bot.definition.v1");
    hasher.write_framed(scope.tenant().as_str().as_bytes());
    hasher.write_framed(step.as_bytes());
    if let Some(run) = scope.run() {
        hasher.write_framed(run.id().to_hex().as_bytes());
    } else {
        hasher.write_framed(b"no-run");
    }
    DefinitionIdentity::new(scope.path(), u64::MAX, hasher.finalize(), 1)
}

/// [`remember`] against a scope the caller already entered.
pub(crate) async fn step_in<T, Fut>(
    scope: &Scope,
    definition: &DefinitionIdentity,
    body: impl FnOnce() -> Fut,
) -> Result<T, FlowError>
where
    T: lgwks_std::wire::Archive + Durable,
    Fut: Future<Output = Result<T, FlowError>>,
{
    remember_at(scope, scope.run(), definition, body).await
}

/// [`remember`] against a scope already carrying a run, or `remember_at` for a
/// resume where the caller supplies the run explicitly.
pub(crate) async fn remember_at<T, Fut>(
    scope: &Scope,
    run: Option<RunId>,
    definition: &DefinitionIdentity,
    body: impl FnOnce() -> Fut,
) -> Result<T, FlowError>
where
    T: lgwks_std::wire::Archive + Durable,
    Fut: Future<Output = Result<T, FlowError>>,
{
    let Some(records) = installed() else {
        return body().await;
    };
    let Some(run) = run else {
        return body().await;
    };
    // Before the lookup, not after it. A record found under another definition is
    // a value this build never produced, and returning it is the failure T15
    // names; asking first means the refusal costs a hash-map probe rather than a
    // step body having already been trusted.
    //
    // Both failures arrive as themselves. A genuine drift is the typed
    // `Incompatible` arm carrying its axis, so a caller can tell whether it needs
    // a new run, a decision, a migration or an edit; a store that could not read
    // its own records arrives as the store's own error rather than as a claim
    // about the definition. Neither is a `Failed` reason string: that form could
    // express neither, and the caller had no way to repair a drift from it.
    //
    // `located_at` fills the step path on the refusal. The typed arm is built
    // with an empty location because it does not yet know the step, and a refusal
    // that keeps its constructor's empty path would name the run boundary instead
    // of the step that was about to replay.
    records
        .agrees(run, definition)
        .map_err(|error| error.located_at(scope.shared_path()))?;
    let key = scope.key();
    let tenant = scope.tenant().as_str();

    if let Some(stored) = records.lookup(tenant, run, key)? {
        return decode(&stored);
    }

    // The body's own failure is located at *this* step rather than at the run's
    // boundary, so a ticket names the step a caller must look at rather than the
    // task that happened to contain it.
    let value = body()
        .await
        .map_err(|error| error.located_at(scope.shared_path()))?;
    let bytes = encode(&value).map_err(|error| error.located_at(scope.shared_path()))?;
    records
        .record(tenant, run, key, scope.path(), definition, bytes)
        .await?;
    Ok(value)
}

/// Decode a recorded value, locating a failure at the step it belongs to.
fn decode<T>(stored: &StoredValue) -> Result<T, FlowError>
where
    T: Durable,
{
    T::from_record(&stored.bytes).map_err(|cause| FlowError::Failed {
        at: std::sync::Arc::from(stored.path.as_str()),
        reason: format!("the value recorded for this step is not the type it returns: {cause}"),
    })
}

/// Archive a value the way the store frames it, so encode and decode agree.
fn encode<T>(value: &T) -> Result<Vec<u8>, FlowError>
where
    T: Durable,
{
    value.to_record().map_err(|cause| FlowError::Failed {
        at: std::sync::Arc::from(""),
        reason: format!("this step's value could not be archived: {cause}"),
    })
}
