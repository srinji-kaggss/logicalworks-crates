//! The append path: the RunRecords doors a durable step takes, and the ordered decide-and-write that runs on the storage owner's thread.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex};

use lgwks_std::hash::Digest;

use crate::effect::RunId;
use crate::journal::frame::SaturatingFrom;
use crate::journal::owner::{self, Stage, SubmitError};
use crate::script::run_store::{Appended, RunRecords, StagedRecord, StoredValue};
use crate::script::{FlowError, StepKey};

use super::record::{Stored, frame, hex_of, store_full};
use super::{
    DefinitionIdentity, Drift, Held, Index, MAX_RECORDS_PER_RUN, MAX_STORE_BYTES, RunIndex,
    RunStore, StoreError, StoreLimitKind,
};

/// The store as the script module reaches it: a typed lookup and a typed append
/// over archived bytes, with no `lgwks_bot::task` in its own surface.
///
/// The implementation is here; the trait lives in
/// [`script`] because the task module is a *consumer* of a
/// durable step, and a step must be markable without the host that runs it.
impl RunRecords for RunStore {
    fn lookup(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
    ) -> Result<Option<StoredValue>, FlowError> {
        self.step_readable()?;
        let index = self.index();
        let Some(held) = index.runs.get(&run) else {
            return Ok(None);
        };
        if held.tenant != tenant {
            let refusal = Err(StoreError::ForeignTenant {
                owner: held.tenant.clone(),
                asked: tenant.to_owned(),
            }
            .into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "lookup: returning an error to the caller");
            return refusal;
        }
        Ok(held
            .steps
            .get(&key.to_hex())
            .map(|step| StoredValue::new(step.path.clone(), step.bytes.clone())))
    }

    fn append(
        &self,
        tenant: &str,
        run: RunId,
        key: StepKey,
        path: &str,
        definition: &DefinitionIdentity,
        bytes: Vec<u8>,
    ) -> Result<Appended, FlowError> {
        let staged = self.owned(RunRecords::stage(
            self, tenant, run, key, path, definition, bytes,
        ));
        self.commit(staged.record, staged.key)
            .map_err(FlowError::from)
    }

    /// Whether the records for `run` were written under `definition`.
    ///
    /// The check every durable step makes before it returns a recorded value. A
    /// run this store holds nothing for is compatible by definition: that is a
    /// first attempt, and refusing it would make the first record unreachable.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::check_definition`] reports, unchanged: a drift as
    /// [`FlowError::Incompatible`], and a device refusal as itself. Neither is ever
    /// collapsed into the other's answer.
    fn compatibility(
        &self,
        run: RunId,
        definition: &DefinitionIdentity,
    ) -> Result<bool, FlowError> {
        self.step_readable()?;
        self.check_definition(run, definition)
            .map(|()| true)
            .map_err(FlowError::from)
    }

    /// Which axis of `run`'s recorded identity disagrees with `definition`.
    ///
    /// The same comparison [`Self::compatibility`] made, returning the axis rather
    /// than the boolean. Two doors rather than one returning a richer type because
    /// the boolean is asked on every durable step — including the ones that pass,
    /// where the axis is never wanted — and building a [`Drift`] that names two
    /// digests or two schema ids on a step that will replay cleanly is work spent
    /// only to be thrown away.
    ///
    /// # Errors
    ///
    /// [`FlowError::Failed`] when this store holds no record for `run`, so there is
    /// no recorded identity to compare against, or when the two doors disagree —
    /// `compatibility` refused this pair while this comparison finds nothing to
    /// refuse. Naming an axis in either case would mean inventing one.
    fn drift(&self, run: RunId, definition: &DefinitionIdentity) -> Result<Drift, FlowError> {
        let Some(recorded) = self.definition_of(run) else {
            let refusal = Err(FlowError::failed(format!(
                "this store holds no definition identity for run {}, so it cannot name a drift \
             axis; refusing to invent one",
                run.id().to_hex()
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "drift: returning an error to the caller");
            return refusal;
        };
        match definition.drift_from(&recorded) {
            Some(drift) => Ok(drift),
            None => Err(FlowError::failed(format!(
                "run {} was refused as incompatible and yet agrees with the declared \
                 definition; refusing to name an axis for a disagreement that is not there",
                run.id().to_hex()
            ))),
        }
    }

    /// The awaited door, which is the one a durable step takes.
    ///
    /// The `sync_all` behind it is the device's cost, and this is what keeps that
    /// cost off the executor: the step's future waits for the owner's answer
    /// instead of sitting through the flush (INV-BOT-50).
    fn append_async<'a>(
        &'a self,
        record: StagedRecord<'a>,
    ) -> crate::BoxFuture<'a, Result<Appended, FlowError>> {
        let staged = self.owned(record);
        Box::pin(async move {
            self.commit_async(staged.record, staged.key)
                .await
                .map_err(FlowError::from)
        })
    }
}

/// Map one owner refusal onto this store's vocabulary.
///
/// One conversion rather than a match per call site, because every `?` on an owner
/// refusal wraps the same fact — the device, or the handle, said no — and a literal
/// per call site would be four chances to name a different variant for one event.
/// The refusal's own message distinguishes a full queue from a poison from an
/// undelivered answer, so nothing is lost by carrying it as the device's error.
impl From<SubmitError> for StoreError {
    fn from(cause: SubmitError) -> Self {
        Self::Storage {
            cause: cause.into_io(),
        }
    }
}

/// One staged append: the record to write and the step key it is for.
///
/// Its own type so the two append doors build the record once between them, and
/// the key travels with it rather than being threaded beside it at each call site.
pub(super) struct Staged {
    /// The record as it will be archived.
    pub(super) record: Stored,
    /// The step key the already-recorded and conflict checks are on.
    pub(super) key: StepKey,
}

/// The in-file chain head of one run, held with the index.
impl RunStore {
    /// The chain step of `stored`: the head of the whole file, which every run
    /// shares, so a frame's head cannot be valid under one run's history and
    /// invalid under the file's.
    ///
    /// One chain for the file rather than one per run, deliberately. A per-run
    /// chain would need the head to be re-found by replaying only that run's
    /// frames, and a caller that read one run's records could then not verify
    /// them without reading every other tenant's records too. One file, one
    /// chain, and the tenant check above is what keeps the reads separated.
    ///
    /// The whole of this runs on the storage owner's thread — the in-memory checks,
    /// the fence, the write, the `sync_all` and the fold into the index — so two
    /// appends to this store are two ordered steps rather than two racing ones,
    /// and none of them parks the executor the `sync_all` does. INV-BOT-50.
    fn commit(&self, stored: Stored, key: StepKey) -> Result<Appended, StoreError> {
        self.inner
            .owner
            .submit(move |file, index| append_on_owner(file, index, &stored, key))
            .map_err(StoreError::from)
    }

    /// The same append, awaited rather than sat through.
    ///
    /// The one a durable step takes, because a step's body is a future and the
    /// `sync_all` its record costs is the device's, not the executor's.
    fn commit_async<'a>(
        &'a self,
        stored: Stored,
        key: StepKey,
    ) -> crate::BoxFuture<'a, Result<Appended, StoreError>> {
        Box::pin(async move {
            self.inner
                .owner
                .submit_async(move |file, index| append_on_owner(file, index, &stored, key))
                .await
                .map_err(StoreError::from)
        })
    }
}

/// The ordered append, over the file and the index it folds into.
///
/// A free function taking the shared state rather than a method on `&self`, because
/// it runs on the storage owner's thread with a closure that must own everything it
/// touches: the `Arc` the store's state lives in and the record itself.
///
/// The lock is taken for the stage half only. The settle half is a fold into the
/// same index, and it runs on this same thread under the owner's ordering — after
/// the batch's one `sync_all`, never before it — so no append can observe a
/// half-committed index and no `.await` is ever reached holding it.
///
/// # Errors
///
/// Every [`StoreError`] an append can produce, carried as the device's own error so
/// the owner's one reply channel serves both stores.
fn append_on_owner(
    file: &mut File,
    shared: &Arc<Mutex<Index>>,
    stored: &Stored,
    key: StepKey,
) -> std::io::Result<Stage<Appended, Arc<Mutex<Index>>>> {
    let mut index = owner::lock(shared);
    decide_and_write(file, &mut index, stored, key)
}

/// The decision, the write, and the fold this record owes its batch's flush.
///
/// Every check that can refuse in memory runs before a byte moves, and the length
/// fence runs inside the owner's ordered step where no other append can overtake it
/// — which is why the file's `metadata` is read here rather than through the
/// read-only view: the authoritative answer is the one taken at the moment of the
/// write.
///
/// The three ceilings and the tenant and duplicate checks are decided *here*, at
/// stage time, exactly as they were before group commit: a refusal is the same
/// refusal and refuses the same single record. The fold into the index is what the
/// settle half does, after the batch's one `sync_all` returns, so an index never
/// names a record the device did not take.
///
/// # Errors
///
/// Every [`StoreError`] an append can produce. Nothing is written by any of them,
/// and a refusal leaves the index and the file byte-identical.
fn decide_and_write(
    file: &mut File,
    index: &mut Index,
    stored: &Stored,
    key: StepKey,
) -> std::io::Result<Stage<Appended, Arc<Mutex<Index>>>> {
    let owned = index.runs.get(&stored.run);
    if let Some(owned) = owned {
        if owned.tenant != stored.tenant {
            let refusal = Err(refusal(StoreError::ForeignTenant {
                owner: owned.tenant.clone(),
                asked: stored.tenant.clone(),
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_and_write: returning an error to the caller");
            return refusal;
        }
        if let Some(step) = owned.steps.get(&key.to_hex()) {
            // Already recorded. An identical record is the same fact seen
            // twice — a step whose value was returned and whose caller
            // recorded it again — and a different one is a conflict. Neither
            // writes a byte.
            return Ok(Stage::Settled(Ok(if step.bytes == stored.value {
                Appended::AlreadyRecorded
            } else {
                Appended::Conflicting
            })));
        }
        if u64::saturating_from(owned.steps.len()) >= MAX_RECORDS_PER_RUN {
            let refusal = Err(refusal(StoreError::Limit {
                kind: StoreLimitKind::Records,
                requested: u64::saturating_from(owned.steps.len()).saturating_add(1),
                limit: MAX_RECORDS_PER_RUN,
            }));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_and_write: returning an error to the caller");
            return refusal;
        }
    }

    let previous = tail_head(index);
    let (framed, head) = frame(stored, &previous).map_err(refusal)?;

    let staged = u64::saturating_from(framed.len());
    let next = index
        .committed
        .checked_add(staged)
        .ok_or_else(|| refusal(store_full(u64::MAX)))?;
    if next > MAX_STORE_BYTES {
        let refusal = Err(refusal(store_full(next)));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_and_write: returning an error to the caller");
        return refusal;
    }

    // The fence, checked before a byte is written: the length this handle
    // indexed must be the length on the disk, so a file another writer moved
    // under it is refused here rather than forked. It runs on the owner thread,
    // so the check and the write it guards cannot be split by another append.
    let on_disk = file
        .metadata()
        .map_err(StoreError::storage)
        .map_err(refusal)?
        .len();
    if on_disk != index.committed {
        // The ordinal names how far this run got: a run with no record yet is at
        // its first frame.
        let recorded = match index.runs.get(&stored.run) {
            Some(run) => run.steps.len(),
            None => 0,
        };
        let refusal = Err(refusal(StoreError::Corrupt {
            at: u64::saturating_from(recorded),
        }));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decide_and_write: returning an error to the caller");
        return refusal;
    }

    // The chain head and the committed length move *before* the write, and are the
    // batch's own bookkeeping rather than a claim the device took anything: the next
    // member of this batch must chain over this record, and the next batch's length
    // fence must expect these bytes. A head that lagged a write would fork the chain.
    index.committed = next;
    index.tail = head;

    file.write_all(&framed)
        .map_err(StoreError::storage)
        .map_err(refusal)?;

    // The readable half of the fold waits for the flush: a record a lookup can find
    // is a record whose bytes the device has acknowledged, so publishing it earlier
    // would let a concurrent reader see a value a crash could take back.
    let held = Held {
        path: stored.path.clone(),
        bytes: stored.value.clone(),
    };
    let key = hex_of(&stored.key);
    let run = stored.run;
    let tenant = stored.tenant.clone();
    let definition = stored.definition.to_identity();
    Ok(Stage::Unsynced {
        answer: Appended::Recorded,
        bytes: framed.len(),
        settle: Box::new(move |index: &mut Arc<Mutex<Index>>| {
            let mut index = owner::lock(index);
            let entry = index.runs.entry(run).or_insert_with(|| RunIndex {
                tenant: tenant.clone(),
                definition: definition.clone(),
                steps: HashMap::new(),
            });
            entry.steps.insert(key, held);
        }),
    })
}

/// A store refusal as the device error the owner's one reply channel carries.
///
/// One conversion rather than a `map_err` per arm, because every `?` on a store
/// refusal wraps the same fact — this store said no — and a literal per site would
/// be seven chances to name a different message for one event. The refusal's own
/// `Display` carries the ceiling or the tenant that names it.
fn refusal(cause: StoreError) -> std::io::Error {
    std::io::Error::other(cause.to_string())
}

/// The chain head the next frame in this file follows.
///
/// The index carries the tail head beside the committed length, because the head
/// cannot be recomputed from the index: a frame's head is over the *archived*
/// record, and re-archiving a value to hash it would be a second encoding of the
/// same bytes with its own chance of disagreeing with what is on the disk.
fn tail_head(index: &Index) -> Digest {
    index.tail
}
