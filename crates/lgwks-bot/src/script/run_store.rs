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
        bytes: Vec<u8>,
    ) -> Result<Appended, FlowError>;
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
    pub(crate) fn record(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), FlowError> {
        match self.0.append(tenant, run, key, path, bytes) {
            Ok(Appended::Recorded) | Ok(Appended::AlreadyRecorded) => Ok(()),
            Ok(Appended::Conflicting) => Err(FlowError::Failed {
                at: std::sync::Arc::from(path),
                reason: format!(
                    "a different value is already recorded for step {path:?} under this run; \
                     refusing to overwrite a committed record"
                ),
            }),
            Err(error) => Err(error.located_at(&std::sync::Arc::from(path))),
        }
    }
}

impl std::fmt::Debug for Records {
    /// The installed store's own `Debug`, so a host's debug output names the
    /// store it installed without the script module importing its type.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RunRecords")
    }
}

/// Install `records` for the futures polled inside `body`.
///
/// Crate-private: the host is the supported way to make a run durable (DX-10),
/// and a caller who installed their own store would be reaching past the one
/// path that also mints the run id the record is keyed by.
pub(crate) async fn within<R>(records: Records, body: impl Future<Output = R>) -> R {
    RECORDS.scope(Some(records), body).await
}

/// The store installed for the current future, if any.
pub(crate) fn installed() -> Option<Records> {
    RECORDS.try_with(Clone::clone).unwrap_or(None)
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
/// let fetched = remember(scope, "fetch", async { Ok::<_, FlowError>(41u32) }).await?;
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
    step_in(&child, body).await
}

/// [`remember`] against a scope the caller already entered.
pub(crate) async fn step_in<T, Fut>(
    scope: &Scope,
    body: impl FnOnce() -> Fut,
) -> Result<T, FlowError>
where
    T: lgwks_std::wire::Archive + Durable,
    Fut: Future<Output = Result<T, FlowError>>,
{
    remember_at(scope, scope.run(), body).await
}

/// [`remember`] against a scope already carrying a run, or `remember_at` for a
/// resume where the caller supplies the run explicitly.
pub(crate) async fn remember_at<T, Fut>(
    scope: &Scope,
    run: Option<RunId>,
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
    let key = scope.key();
    let tenant = scope.tenant().as_str();

    if let Some(stored) = records.lookup(tenant, run, key)? {
        return decode(&stored);
    }

    let value = body().await?;
    let bytes = encode(&value)?;
    records.record(tenant, run, key, scope.path(), bytes)?;
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
