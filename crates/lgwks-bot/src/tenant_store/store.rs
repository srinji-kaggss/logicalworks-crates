//! The store's operations: open, the locked refresh-apply-sync step, and the keyed reads.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::record::{
    Stored, fold, frame_record, full_replay, genesis_head, refresh, trim_to_committed,
};
use super::{
    BUSY_WAIT, HEADER_LEN, Index, KIND_LANE_DISPATCHED, LOCK_FILE, LOCK_SLICE_MS, LaneRow,
    LockGuard, MAX_LISTED, OpenRun, Policy, RELEASES, RunEntry, RunRow, RunStart, STORE_FILE,
    STORE_FORMAT, STORE_MAGIC, StoreInner, TenantStore, TenantStoreError, VERSION_BYTE,
    close_decision, is_open_lane, is_pending, lane_ready, lock_file, lock_index, lock_is_stale,
    now_nanos, tenant_rows, unknown_run,
};
use crate::journal::frame::SaturatingFrom;
use crate::journal::owner::{lock, wait_timeout};

impl TenantStore {
    /// Open (or create) the store under `dir`, waiting up to a minute for
    /// another coordinator's lock.
    ///
    /// The directory is created when absent. A torn final frame is trimmed,
    /// because an interrupted append was never anyone's answer; every earlier
    /// frame is retained.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Storage`] when the device refuses; [`NotAStore`](TenantStoreError::NotAStore)
    /// when the file exists and is not a tenant store;
    /// [`FormatVersion`](TenantStoreError::FormatVersion) when it is one this
    /// build does not read; [`Corrupt`](TenantStoreError::Corrupt) when a
    /// committed frame does not follow from the ones before it.
    pub fn open(dir: &Path) -> Result<Self, TenantStoreError> {
        Self::open_with_busy_timeout(dir, BUSY_WAIT)
    }

    /// [`open`](Self::open) with an explicit lock-wait bound.
    ///
    /// The bound a test waits under: production waits the full minute, and a
    /// test that held a lock for a minute to prove the refusal would be
    /// proving patience rather than fencing.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub fn open_with_busy_timeout(
        dir: &Path,
        busy_wait: Duration,
    ) -> Result<Self, TenantStoreError> {
        std::fs::create_dir_all(dir).map_err(TenantStoreError::storage)?;
        let path = dir.join(STORE_FILE);
        let existed = path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(TenantStoreError::storage)?;
        if !existed || file.metadata().map_err(TenantStoreError::storage)?.len() == 0 {
            file.write_all(&Self::header())
                .and_then(|()| file.sync_all())
                .map_err(TenantStoreError::storage)?;
        }
        let store = Self {
            inner: Arc::new(StoreInner {
                file: Mutex::new(file),
                dir: dir.to_path_buf(),
                index: Mutex::new(Index {
                    policies: BTreeMap::new(),
                    runs: BTreeMap::new(),
                    frames: 0,
                    committed: u64::saturating_from(HEADER_LEN),
                    tail: genesis_head(),
                }),
                busy_wait,
                holder_seq: AtomicU64::new(0),
            }),
        };
        let guard = store.acquire()?;
        {
            let mut inner_file = lock_file(&store.inner.file);
            let mut index = lock_index(&store.inner.index);
            full_replay(&mut inner_file, &mut index)?;
            trim_to_committed(&mut inner_file, &index)?;
        }
        drop(guard);
        Ok(store)
    }

    /// The file header as this build writes it: the constant prefix with the
    /// version in the last byte, built once so the writer and the reader
    /// cannot disagree about which byte is which.
    fn header() -> [u8; HEADER_LEN] {
        let mut header = *STORE_MAGIC;
        header[VERSION_BYTE] = STORE_FORMAT;
        header
    }

    /// Where the lock file lives.
    fn lock_path(&self) -> PathBuf {
        self.inner.dir.join(LOCK_FILE)
    }

    /// Hold the host lock across one step, waiting up to the busy timeout.
    ///
    /// Stale locks are stolen by age: a live holder keeps the lock for one
    /// small write and one sync, so a lock older than [`STALE_LOCK_AFTER`]
    /// was orphaned by a crash. The wait is bounded by the busy timeout, and
    /// exhaustion is a typed refusal, never a hang. A release in this process
    /// wakes the waiter at once; one in another process is noticed within
    /// [`LOCK_SLICE_MS`].
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] when the lock stayed held past the bound;
    /// [`TenantStoreError::Storage`] when the device refuses.
    fn acquire(&self) -> Result<LockGuard, TenantStoreError> {
        let path = self.lock_path();
        let started = Instant::now();
        let slice = Duration::from_millis(u64::saturating_from(LOCK_SLICE_MS));
        loop {
            // Read before the claim, so a release that lands between a refused
            // claim and the wait below is seen as a change and retried at once.
            let seen = *lock(&RELEASES.generation);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut claim) => {
                    let seq = self.inner.holder_seq.fetch_add(1, Ordering::SeqCst);
                    let holder = format!("{}-{seq}", now_nanos());
                    if let Err(error) = claim.write_all(holder.as_bytes()) {
                        drop(std::fs::remove_file(&path));
                        lgwks_std::trace::warn!(
                            path = %path.display(),
                            %error,
                            "tenant_store: lock claim write refused; released the claim"
                        );
                        return Err(TenantStoreError::storage(error));
                    }
                    return Ok(LockGuard { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path)? {
                        drop(std::fs::remove_file(&path));
                    } else if started.elapsed() >= self.inner.busy_wait {
                        let waited = started.elapsed();
                        lgwks_std::trace::warn!(
                            path = %path.display(),
                            waited_ms = waited.as_millis(),
                            "tenant_store: lock stayed held past the busy bound; refusing as busy"
                        );
                        return Err(TenantStoreError::Busy { waited });
                    } else {
                        // A release in this process wakes the wait at once. A
                        // release by another process sends no signal here, so the
                        // slice bounds how late that one is noticed.
                        let held = lock(&RELEASES.generation);
                        if *held == seen {
                            drop(wait_timeout(&RELEASES.released, held, slice));
                        }
                    }
                }
                Err(error) => {
                    lgwks_std::trace::warn!(
                        path = %path.display(),
                        %error,
                        "tenant_store: lock claim refused by the device"
                    );
                    return Err(TenantStoreError::storage(error));
                }
            }
        }
    }

    /// Run `apply` under the host lock: refresh the index past every frame
    /// another coordinator committed, apply the mutation, and return its
    /// answer. A mutation's frame is synced before the lock is released, so a
    /// returned answer survives power loss.
    ///
    /// # Errors
    ///
    /// Whatever the device, the lock, the replay or the mutation reports.
    /// A refused mutation writes nothing.
    fn transact<T>(
        &self,
        apply: impl FnOnce(&mut Index) -> Result<(Option<Stored>, T), TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        let _guard = self.acquire()?;
        let mut file = lock_file(&self.inner.file);
        let mut index = lock_index(&self.inner.index);
        refresh(&mut file, &mut index)?;
        let (record, answer) = apply(&mut index)?;
        if let Some(stored) = record {
            let (framed, head) = frame_record(&stored, &index.tail)?;
            file.write_all(&framed).map_err(TenantStoreError::storage)?;
            file.sync_all().map_err(TenantStoreError::storage)?;
            index.committed = index
                .committed
                .saturating_add(u64::saturating_from(framed.len()));
            index.tail = head;
            index.frames = index.frames.saturating_add(1);
            fold(&mut index, &stored);
        }
        Ok(answer)
    }

    /// Refresh the index past committed frames, then read through `read`.
    ///
    /// Reads take the lock for the same reason writes do: without it a read
    /// could answer from an index another coordinator has already moved past.
    /// The lock is held for a memory probe, never across an `await` — this
    /// surface is synchronous.
    ///
    /// # Errors
    ///
    /// Whatever the device, the lock or the replay reports.
    fn observe<T>(
        &self,
        read: impl FnOnce(&Index) -> Result<T, TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        let _guard = self.acquire()?;
        let mut file = lock_file(&self.inner.file);
        let mut index = lock_index(&self.inner.index);
        refresh(&mut file, &mut index)?;
        read(&index)
    }

    /// Run a row-addressed update under the host lock: refresh, resolve the
    /// run, and commit the frame the update builds, or refuse.
    ///
    /// One lookup for the four row-addressed updates — dispatch, end, finish,
    /// abandon — so a run the store never began is refused once, in one
    /// place, rather than once per update with four chances to name a
    /// different refusal for it.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun, and
    /// whatever the device, the lock, the replay or the update reports.
    fn transact_on_run<T>(
        &self,
        run_id: &str,
        update: impl FnOnce(&RunEntry) -> Result<(Option<Stored>, T), TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        self.transact(|index| {
            let Some(entry) = index.runs.get(run_id) else {
                lgwks_std::trace::warn!(
                    run_id,
                    "tenant_store: row-addressed update refused: the run was never begun"
                );
                return Err(unknown_run(run_id));
            };
            update(entry)
        })
    }

    /// Read one run under the host lock: refresh, resolve the run, and answer
    /// through `read`.
    ///
    /// The read half of [`transact_on_run`](Self::transact_on_run): one
    /// lookup, one refusal, shared with the lane listing.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun, and
    /// whatever the device, the lock, the replay or the read reports.
    fn observe_run<T>(
        &self,
        run_id: &str,
        read: impl FnOnce(&RunEntry) -> Result<T, TenantStoreError>,
    ) -> Result<T, TenantStoreError> {
        self.observe(|index| {
            let Some(entry) = index.runs.get(run_id) else {
                lgwks_std::trace::warn!(
                    run_id,
                    "tenant_store: row-addressed read refused: the run was never begun"
                );
                return Err(unknown_run(run_id));
            };
            read(entry)
        })
    }

    /// Record `tenant`'s policy. A second call for the same tenant is refused
    /// and changes nothing: a policy is written once and never rewritten.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::AlreadyInitialised`] when the tenant has a policy;
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn init_policy(
        &self,
        tenant: &str,
        gates_json: &str,
        hardness: u8,
        now: &str,
    ) -> Result<(), TenantStoreError> {
        self.transact(|index| {
            if index.policies.contains_key(tenant) {
                lgwks_std::trace::warn!(
                    tenant,
                    "tenant_store: policy init refused: the tenant already has one"
                );
                return Err(TenantStoreError::AlreadyInitialised {
                    tenant: tenant.to_owned(),
                });
            }
            let stored = Stored::init_policy(tenant, gates_json, hardness, now);
            Ok((Some(stored), ()))
        })
    }

    /// `tenant`'s policy, if it ran `init`.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn policy(&self, tenant: &str) -> Result<Option<Policy>, TenantStoreError> {
        self.observe(|index| {
            Ok(index.policies.get(tenant).map(|held| {
                Policy::new(
                    held.gates_json.clone(),
                    held.hardness,
                    held.created_at.clone(),
                )
            }))
        })
    }

    /// Record a run and its lanes as pending, before any lane starts.
    ///
    /// One frame for the run row and every pending lane row, so the begin is
    /// atomic: a coordinator killed inside it left either the whole run or no
    /// run, never a run with half its lanes.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::DuplicateRun`] when the run id is already begun;
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// or [`TenantStoreError::Limit`] when the device or the ceiling refuses.
    pub fn begin_run(&self, run: &RunStart<'_>, lanes: &[&str]) -> Result<(), TenantStoreError> {
        self.transact(|index| {
            if index.runs.contains_key(run.run_id) {
                lgwks_std::trace::warn!(
                    run = run.run_id,
                    "tenant_store: begin refused: the run id is already begun"
                );
                return Err(TenantStoreError::DuplicateRun {
                    run: run.run_id.to_owned(),
                });
            }
            let stored = Stored::begin_run(run, lanes);
            Ok((Some(stored), ()))
        })
    }

    /// Durable intent: the lane is about to start. Committed before the
    /// process exists, so a coordinator killed at any instant leaves a record
    /// the next run can reconcile.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::LaneLost`] when the row is gone or already past
    /// dispatch; [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn lane_dispatched(&self, run_id: &str, lane: usize) -> Result<(), TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            lane_ready(entry, run_id, lane, "dispatch", is_pending)?;
            let stored = Stored::lane_op(KIND_LANE_DISPATCHED, run_id, lane);
            Ok((Some(stored), ()))
        })
    }

    /// The lane is terminal. A lane ends once; a row already ended (for
    /// example by reconciliation) is never overwritten.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::LaneLost`] when the row is gone or already ended;
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn lane_ended(
        &self,
        run_id: &str,
        lane: usize,
        outcome: &str,
        detail: &str,
        tail: &[u8],
    ) -> Result<(), TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            lane_ready(entry, run_id, lane, "end", is_open_lane)?;
            let stored = Stored::lane_ended(run_id, lane, outcome, detail, tail);
            Ok((Some(stored), ()))
        })
    }

    /// Store the sealed record. Returns `false`, changing nothing, when the
    /// run already ended (for example because it was reconciled as
    /// abandoned): the closer's record stands.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn finish_run(
        &self,
        run_id: &str,
        verdict: &str,
        finished_at: &str,
        record: &str,
        seal: &str,
    ) -> Result<bool, TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            Ok(close_decision(
                entry,
                Stored::finish_run(run_id, verdict, finished_at, record, seal),
            ))
        })
    }

    /// Runs on `host` that have not finished: the reconciliation set a
    /// successor works after a coordinator died.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn open_runs(&self, host: &str) -> Result<Vec<OpenRun>, TenantStoreError> {
        self.observe(|index| {
            Ok(index
                .runs
                .iter()
                .filter(|&(_, entry)| entry.host == host && entry.close.is_none())
                .map(|(run_id, entry)| OpenRun::new(run_id.clone(), entry.coordinator_pid))
                .collect())
        })
    }

    /// Close an unfinished run whose coordinator died, in one frame.
    ///
    /// A lane that was dispatched but never recorded an end may or may not
    /// have run: its outcome is unknown, never a pass and never a failure. A
    /// lane never dispatched did not run. Returns whether the run was open;
    /// an already-closed run keeps its closer's record untouched.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn abandon(&self, run_id: &str, now: &str) -> Result<bool, TenantStoreError> {
        self.transact_on_run(run_id, |entry| {
            Ok(close_decision(entry, Stored::abandon_run(run_id, now)))
        })
    }

    /// The coordinator lock `run_id` was begun under, relative to the state
    /// directory: the name a successor uses to tell whether the coordinator
    /// that minted the run is still alive.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn coordinator_lock(&self, run_id: &str) -> Result<String, TenantStoreError> {
        self.observe_run(run_id, |entry| Ok(entry.coordinator_lock.clone()))
    }

    /// `tenant`'s most recent runs, newest first, capped at 200.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::Busy`] under contention; [`TenantStoreError::Storage`]
    /// when the device refuses.
    pub fn runs(&self, tenant: &str, limit: u32) -> Result<Vec<RunRow>, TenantStoreError> {
        self.observe(|index| {
            let mut rows: Vec<RunRow> = tenant_rows(index, tenant).collect();
            rows.sort_by(|left, right| {
                right
                    .started_at()
                    .cmp(left.started_at())
                    .then_with(|| right.run_id().cmp(left.run_id()))
            });
            rows.truncate(usize::saturating_from(limit.min(MAX_LISTED)));
            Ok(rows)
        })
    }

    /// The one run of `tenant` whose id starts with `prefix`.
    ///
    /// A prefix match, not a pattern: no escape character, no wildcard, so a
    /// run id cannot smuggle syntax into the lookup.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when nothing matches;
    /// [`TenantStoreError::Ambiguous`] when more than one run matches.
    pub fn run(&self, tenant: &str, prefix: &str) -> Result<RunRow, TenantStoreError> {
        self.observe(|index| {
            let mut matches =
                tenant_rows(index, tenant).filter(|row| row.run_id().starts_with(prefix));
            match (matches.next(), matches.next()) {
                (Some(row), None) => Ok(row),
                (Some(_), Some(_)) => Err(TenantStoreError::Ambiguous {
                    run: prefix.to_owned(),
                }),
                (None, _) => Err(TenantStoreError::NoSuchRun {
                    tenant: tenant.to_owned(),
                    run: prefix.to_owned(),
                }),
            }
        })
    }

    /// The lanes of `run_id`, in plan order.
    ///
    /// # Errors
    ///
    /// [`TenantStoreError::NoSuchRun`] when the run was never begun.
    pub fn lanes(&self, run_id: &str) -> Result<Vec<LaneRow>, TenantStoreError> {
        self.observe_run(run_id, |entry| {
            Ok(entry
                .lanes
                .iter()
                .map(|slot| {
                    LaneRow::new(
                        slot.id.clone(),
                        slot.state.clone(),
                        slot.outcome.clone(),
                        slot.detail.clone(),
                        slot.tail.clone(),
                    )
                })
                .collect())
        })
    }
}
