//! Black-box acceptance for the fenced dispatch path (#319).
//!
//! The [`Coordinator`](lgwks_bot::dist_lease::Coordinator) commits every
//! dispatch row to the durable
//! [`TenantStore`](lgwks_bot::tenant_store::TenantStore) before the lane
//! starts, and every grant is fenced by the lease epoch first. Four facts are
//! pinned here against the real store, the real authority, and the real
//! queue:
//!
//! - **A stale coordinator cannot grant (AT07).** A successor's acquire stales
//!   the predecessor's lease, and the predecessor's dispatch, finish, and
//!   reconcile are refused as stale with nothing written — while the
//!   successor's grants commit.
//! - **A revoked lease grants nothing.** Revocation moves the epoch with no
//!   successor, and the revoked lease is refused everywhere.
//! - **The queue refuses past its bound.** A full queue hands the item back;
//!   the oldest item pops first; zero capacity is refused at construction.
//! - **A dead coordinator's runs survive on disk.** A coordinator dropped with
//!   runs open is succeeded by a fresh coordinator over the same directory,
//!   which abandons exactly the open runs with unknown outcomes, and a second
//!   recovery closes nothing.
//! - **A partition withholds grants.** A partitioned channel answers nothing
//!   and grants nothing; healed, it validates again — and a stale lease still
//!   validates false.

use std::error::Error;

use lgwks_bot::dist_lease::{
    AuthorityHandle, Coordinator, DispatchError, GrantChannel, LeaseError, LoopbackChannel,
    QueueError, WorkQueue,
};
use lgwks_bot::tenant_store::{RunStart, TenantStore};

use crate::scratch::Scratch;

/// The test result: fixtures bubble with `?`, like the rest of the estate.
type TestResult = Result<(), Box<dyn Error>>;

/// The lanes the acceptance runs plan.
const LANES: [&str; 2] = ["fmt", "clippy"];

/// Whether the refusal is the fencing refusal: a stale epoch naming both
/// sides. Any other error — a store refusal, a wrong shape — fails the test
/// with its own message rather than passing as a fence.
fn is_stale_refusal(error: &DispatchError) -> Result<bool, String> {
    match *error {
        DispatchError::Lease(LeaseError::StaleEpoch { presented, current }) => {
            Ok(presented < current)
        }
        DispatchError::Store(ref cause) => {
            Err(format!("the fence must refuse, not the store: {cause}"))
        }
        _ => Err("an unknown dispatch refusal".to_owned()),
    }
}

/// A stale coordinator cannot grant: the successor's acquire stales the
/// predecessor's lease, and the predecessor's dispatch, finish, and reconcile
/// are refused with nothing written while the successor's grants commit.
#[test]
fn a_stale_coordinator_cannot_grant() -> TestResult {
    let scratch = Scratch::new("dist-lease-stale")?;
    let store = TenantStore::open(scratch.path())?;
    let coordinator = Coordinator::new(store, "host");
    let first = coordinator.acquire("coord-a");
    coordinator.begin_run(
        &first,
        &RunStart::new(
            "run-fence-001",
            "acme",
            "/repo",
            "host",
            1,
            "lock",
            "t-000001",
        ),
        &LANES,
    )?;
    let second = coordinator.acquire("coord-b");
    let refused = match coordinator.dispatch_lane(&first, "run-fence-001", 0) {
        Err(error) => error,
        Ok(()) => return Err("the stale dispatch must be refused".into()),
    };
    assert!(
        is_stale_refusal(&refused)?,
        "the predecessor's dispatch must be stale, not merely refused"
    );
    coordinator.dispatch_lane(&second, "run-fence-001", 0)?;
    let refused =
        match coordinator.finish_run(&first, "run-fence-001", "GO", "t-2", "record", "seal") {
            Err(error) => error,
            Ok(_) => return Err("the stale finish must be refused".into()),
        };
    assert!(
        is_stale_refusal(&refused)?,
        "the predecessor's finish must be stale, not merely refused"
    );
    let refused = match coordinator.recover(&first, "host", "t-3") {
        Err(error) => error,
        Ok(_) => return Err("the stale reconcile must be refused".into()),
    };
    assert!(
        is_stale_refusal(&refused)?,
        "a stale coordinator cannot declare another dead"
    );
    assert!(
        coordinator.finish_run(&second, "run-fence-001", "GO", "t-4", "record", "seal")?,
        "the current grant must finish the run"
    );
    Ok(())
}

/// A revoked lease grants nothing: revocation moves the epoch with no
/// successor, and the revoked lease is refused at the check and at every
/// grant.
#[test]
fn a_revoked_lease_grants_nothing() -> TestResult {
    let scratch = Scratch::new("dist-lease-revoke")?;
    let store = TenantStore::open(scratch.path())?;
    let coordinator = Coordinator::new(store, "host");
    let lease = coordinator.acquire("coord-a");
    coordinator.handle().revoke();
    match coordinator.handle().check(&lease) {
        Err(LeaseError::StaleEpoch { .. }) => Ok::<(), Box<dyn Error>>(()),
        Err(_) => return Err("a revoked lease must be refused as stale".into()),
        Ok(()) => return Err("a revoked lease must not check out".into()),
    }?;
    let refused = match coordinator.begin_run(
        &lease,
        &RunStart::new("run-revoke-001", "acme", "/repo", "host", 1, "lock", "t-1"),
        &LANES,
    ) {
        Err(error) => error,
        Ok(()) => return Err("the revoked begin must be refused".into()),
    };
    assert!(
        is_stale_refusal(&refused)?,
        "the revoked begin must be stale, not merely refused"
    );
    Ok(())
}

/// The queue refuses past its bound: the third push into capacity two hands
/// the item back, the oldest pops first, and zero capacity never builds.
#[test]
fn the_queue_refuses_past_its_bound() -> TestResult {
    let mut queue =
        WorkQueue::new(2).map_err(|error| format!("capacity two must build: {error}"))?;
    queue
        .try_push("first")
        .map_err(|error| format!("the first push must fit: {error}"))?;
    queue
        .try_push("second")
        .map_err(|error| format!("the second push must fit: {error}"))?;
    assert!(queue.is_full(), "two items in capacity two must read full");
    match queue.try_push("third") {
        Err(refused) => {
            assert_eq!(refused.capacity(), 2, "the refusal must name the bound");
            assert_eq!(
                refused.into_item(),
                "third",
                "the refusal must hand the item back"
            );
        }
        Ok(()) => return Err("the third push must be refused".into()),
    }
    assert_eq!(queue.pop(), Some("first"), "the oldest item must pop first");
    assert!(!queue.is_full(), "a pop must reopen the bound");
    queue
        .try_push("third")
        .map_err(|error| format!("the reopened bound must admit: {error}"))?;
    assert_eq!(queue.len(), 2, "the queue must hold exactly its bound");
    match WorkQueue::<&str>::new(0) {
        Err(QueueError::ZeroCapacity) => Ok::<(), Box<dyn Error>>(()),
        Err(_) => return Err("zero capacity must be ZeroCapacity".into()),
        Ok(_) => return Err("zero capacity must not build".into()),
    }?;
    assert!(queue.pop().is_some(), "a pop must hand back work");
    assert!(queue.pop().is_some(), "a second pop must hand back work");
    assert_eq!(
        queue.pop(),
        None,
        "an empty queue pops nothing, and refuses nothing"
    );
    assert!(queue.is_empty(), "a drained queue must read empty");
    Ok(())
}

/// A dead coordinator's runs survive on disk: a fresh coordinator over the
/// same directory abandons exactly the open runs with unknown outcomes, and
/// a second recovery closes nothing.
#[test]
fn a_dead_coordinator_run_survives_on_disk() -> TestResult {
    let scratch = Scratch::new("dist-lease-dead")?;
    let dir = scratch.path();
    {
        let store = TenantStore::open(dir)?;
        let coordinator = Coordinator::new(store, "host");
        let lease = coordinator.acquire("coord-a");
        coordinator.begin_run(
            &lease,
            &RunStart::new(
                "run-dead-001",
                "acme",
                "/repo",
                "host",
                1,
                "lock",
                "t-000001",
            ),
            &LANES,
        )?;
        coordinator.dispatch_lane(&lease, "run-dead-001", 0)?;
        coordinator.begin_run(
            &lease,
            &RunStart::new(
                "run-dead-002",
                "acme",
                "/repo",
                "host",
                1,
                "lock",
                "t-000002",
            ),
            &LANES,
        )?;
    }
    let store = TenantStore::open(dir)?;
    let successor = Coordinator::new(store, "host");
    let lease = successor.acquire("coord-b");
    let closed = successor.recover(&lease, "host", "t-000003")?;
    assert_eq!(
        closed, 2,
        "the successor must close exactly the two open runs"
    );
    assert_eq!(
        successor.recover(&lease, "host", "t-000004")?,
        0,
        "a second recovery must close nothing"
    );
    let store = TenantStore::open(dir)?;
    let lanes = store.lanes("run-dead-001")?;
    assert_eq!(
        lanes[0].outcome.as_deref(),
        Some("outcome-unknown"),
        "the dispatched lane must read unknown, never a pass"
    );
    assert_eq!(
        lanes[1].outcome.as_deref(),
        Some("unmeasured"),
        "the undispatched lane must read unmeasured"
    );
    let row = store.run("acme", "run-dead-002")?;
    assert_eq!(
        row.verdict(),
        Some("UNMEASURED"),
        "the second run must read abandoned"
    );
    Ok(())
}

/// A partition withholds grants: a partitioned channel answers nothing and
/// grants nothing, and healed it validates again — with a stale lease still
/// validating false.
#[test]
fn a_partition_withholds_grants() -> TestResult {
    let authority = AuthorityHandle::new();
    let channel = LoopbackChannel::new(authority.clone());
    let first = authority.acquire("coord-a");
    assert!(
        channel.validate(&first)?,
        "a connected channel must validate the grant"
    );
    channel.set_partitioned(true);
    assert!(channel.partitioned(), "the partition flag must read back");
    match channel.validate(&first) {
        Err(_) => Ok::<(), Box<dyn Error>>(()),
        Ok(_) => return Err("a partitioned channel must not answer".into()),
    }?;
    channel.set_partitioned(false);
    let second = authority.acquire("coord-b");
    assert!(
        channel.validate(&second)?,
        "the healed channel must validate the new grant"
    );
    assert!(
        !channel.validate(&first)?,
        "the healed channel must still refuse the stale grant"
    );
    Ok(())
}
