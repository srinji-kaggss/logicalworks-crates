//! The owner thread's loop: stage a batch in arrival order, sync it once, answer every member, and promote or wait for the next.

use std::fs::File;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use super::{
    DEFAULT_QUEUE_DEPTH, Gate, MAX_BATCH_BYTES, MAX_BATCH_RECORDS, Request, Slot, Stage, lock,
    wait_timeout,
};

/// The owner's loop: stage a batch, sync it once, answer everyone in it.
///
/// The file and the caller's state are dropped before `released` is set, so a
/// waiter that observes the release knows everything is already gone rather than
/// about to be. The loop ends when the handle has closed *and* both rings have
/// drained, so a request submitted just before the last handle went is still
/// performed: the bytes it would have written are the ones an acknowledgment may
/// already name.
pub(super) fn serve<S, A>(
    mut file: File,
    mut state: S,
    slot: Arc<Mutex<Slot<S, A>>>,
    gate: Arc<Mutex<Gate>>,
    signal: Arc<Condvar>,
) {
    while run_batch(&mut file, &mut state, &slot, &gate, &signal) {}
    drop(file);
    drop(state);
    let mut held = lock(&slot);
    held.released = true;
    signal.notify_all();
}

/// One staged member of a batch: the request it belongs to, and what it owes.
///
/// A named pair rather than a tuple because the two halves are read apart — the
/// batch counts one, and each member's answer takes both — and a tuple would make
/// a reader remember which element is which at every use.
struct Member<S, A> {
    /// Where this member's answer goes.
    request: Arc<Request<S, A>>,
    /// What its ordered step staged.
    staged: Stage<A, S>,
}

/// Stage one batch, sync it once, and answer it. `false` once there is nothing left.
///
/// Three phases, in this order and never another:
///
/// 1. **Stage.** Take requests from the front of the pending ring and run each
///    ordered step in arrival order. Every byte is on the file by the end of this
///    phase, and every step that owes a flush owes it *to this batch*.
/// 2. **Sync.** One `sync_all` if anything is owed, covering every record staged
///    above. There is no linger before it and none after it.
/// 3. **Answer.** Publish in arrival order: a settled answer as it stands, an
///    unsynced one only after the sync returned, and the folds in the same order.
///
/// The poison latch is written *before* any answer of a failed batch goes out: a
/// caller woken by a failure may append again at once and must find the handle
/// already refusing.
fn run_batch<S, A>(
    file: &mut File,
    state: &mut S,
    slot: &Mutex<Slot<S, A>>,
    gate: &Mutex<Gate>,
    signal: &Condvar,
) -> bool {
    promote(&mut lock(slot));
    let mut batch: Vec<Member<S, A>> = Vec::new();
    let mut staged_bytes = 0usize;
    loop {
        // The stall gate is checked before a request is taken, so a parked device
        // parks the append at the door rather than after the owner has claimed it.
        // Only a *first* record waits: a batch already holding staged bytes owes
        // them a flush, and waiting for company would be exactly the linger this
        // design refuses.
        if batch.is_empty() && !await_work(slot, gate, signal) {
            break;
        }
        if batch.len() >= MAX_BATCH_RECORDS || staged_bytes >= MAX_BATCH_BYTES {
            break;
        }
        // An empty ring means this batch is complete, not that it should be
        // retried: only this thread takes requests out of it, so an empty one
        // cannot fill again under us. An empty *batch* reaching here would mean
        // `await_work` reported work that is not there, which the owner cannot
        // recover from by waiting and must not report as a record.
        let Some(request) = take(slot) else {
            break;
        };
        let outcome = match request.take_job() {
            Err(cause) => Err(cause),
            Ok(job) => {
                let injected = {
                    let mut held = lock(slot);
                    std::mem::take(&mut held.fail_next)
                };
                if injected {
                    Err(std::io::Error::other("the injected device refusal"))
                } else {
                    job(file, state)
                }
            }
        };
        let staged = match outcome {
            Ok(staged) => staged,
            // A failure inside the ordered step may have moved a prefix of the
            // bytes, so the handle cannot say what is on the disk and refuses every
            // later append until a reopen replays the truth. The record still needs
            // an answer, and the answer is the same failure.
            Err(cause) => Stage::Settled(Err(cause)),
        };
        staged_bytes = staged_bytes.saturating_add(staged.staged_bytes());
        {
            let mut held = lock(slot);
            held.staged_records = held.staged_records.saturating_add(1);
        }
        batch.push(Member { request, staged });
    }

    if batch.is_empty() {
        return false;
    }

    // One flush for the batch. A member that owes nothing does not make the batch
    // sync; a member whose flush fails takes the same failure as every other, and
    // none of them is answered.
    let owes = batch.iter().any(|member| member.staged.owes_flush());
    let sync = if owes {
        let injected = {
            let mut held = lock(slot);
            std::mem::take(&mut held.fail_next_flush)
        };
        if injected {
            // A test refusal of the covering flush: the one failure no filesystem
            // produces on demand, and the failure that must answer the whole batch
            // at once. No `sync_all` runs, so no flush is counted.
            Err(std::io::Error::other("the injected flush refusal"))
        } else {
            let outcome = file.sync_all();
            let mut held = lock(slot);
            held.flushes = held.flushes.saturating_add(1);
            outcome
        }
    } else {
        Ok(())
    };

    let mut failed = false;
    let flush_ok = sync.is_ok();
    // A batch that owed no flush has no flush failure to carry; the kind is read
    // only for an `Unsynced` member after a failed sync, so `Other` is never seen.
    let flush_kind = match sync {
        Ok(()) => std::io::ErrorKind::Other,
        Err(ref cause) => cause.kind(),
    };
    for Member { request, staged } in batch {
        // A failure anywhere in the batch poisons the handle, so the latch is
        // raised as each member is answered rather than once at the end: a caller
        // woken by the first failure may append again at once and must find the
        // handle already refusing.
        let answer = match staged {
            Stage::Settled(answer) => answer,
            Stage::Unsynced { answer, settle, .. } if flush_ok => {
                // The fold runs only on a flush that returned, and in submission
                // order, so a store's index cannot name a record the device never
                // took.
                settle(state);
                Ok(answer)
            }
            Stage::Unsynced { .. } => Err(std::io::Error::new(
                flush_kind,
                "the batch's flush failed, so this record's outcome is unknown",
            )),
            // The step flushed its own bytes inside the ordered step, so its answer
            // is ready whether or not this batch syncs.
            #[cfg(feature = "script")]
            Stage::Committed(answer) => Ok(answer),
        };
        if answer.is_err() {
            failed = true;
        }
        // A caller that went away while the bytes were moving leaves the handle
        // unable to say whether they landed, which is the same position a failed
        // write leaves it in: poisoned until a reopen replays the truth.
        if request.was_abandoned() {
            failed = true;
        }
        if failed {
            lock(slot).poisoned = true;
        }
        // The answer goes out whether or not anyone is listening: a caller that has
        // gone cannot be told, but the bytes have moved either way, and the poison
        // above is what makes the next caller read the file back rather than trust
        // a view that may be behind it.
        request.publish(answer);
    }
    true
}

/// Move waiting requests into the ring the owner drains, in arrival order.
///
/// Bounded by the pending ring's own ceiling: a promotion that would overfill it is
/// deferred to the next drain rather than raising the bound.
fn promote<S, A>(held: &mut MutexGuard<'_, Slot<S, A>>) {
    while held.pending.len() < DEFAULT_QUEUE_DEPTH {
        let Some(request) = held.waiting.pop_front() else {
            break;
        };
        request.wake();
        held.pending.push_back(request);
    }
}

/// Take one request from the front of the pending ring, or `None` if it is empty.
fn take<S, A>(slot: &Mutex<Slot<S, A>>) -> Option<Arc<Request<S, A>>> {
    lock(slot).pending.pop_front()
}

/// Wait until there is work, or report that there never will be again.
///
/// The wait is a poll rather than a blocking receive, and that is deliberate: a
/// blocking receive would hold the slot across the wait, which would make the stall
/// gate — the one thing an operator reaches for while a flush is parked —
/// invisible to the thread that has to answer it. A one-millisecond poll costs
/// nothing next to an `fsync` and keeps every state change observable.
fn await_work<S, A>(slot: &Mutex<Slot<S, A>>, gate: &Mutex<Gate>, signal: &Condvar) -> bool {
    let mut held = lock(slot);
    loop {
        // A parked device holds the request back, and the gate is what releases
        // it. The wait is timed rather than indefinite so a handle that closes
        // during a stall still ends the loop, and so a release arriving between
        // batches is not missed.
        while lock(gate).stalled && !held.closed {
            let (guard, _timed_out) = wait_timeout(signal, held, Duration::from_millis(1));
            held = guard;
        }
        promote(&mut held);
        if held.has_work() {
            return true;
        }
        if held.closed {
            // Closed and empty: drained. A request admitted just before the last
            // handle went is the one `take` above took, so nothing is stranded on
            // the way out.
            return false;
        }
        let (guard, _timed_out) = wait_timeout(signal, held, Duration::from_millis(1));
        held = guard;
    }
}
