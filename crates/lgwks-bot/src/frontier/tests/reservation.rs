//! The reservation is bound to the admission, not to the name.

use super::*;

// ── The reservation is bound to the admission, not to the name ─────────

#[test]
fn releasing_after_reresolution_preserves_other_requests() -> TestResult {
    // The filed counterexample. One-request origin window, four-request
    // egress window: A and B are admitted against two origins, A's name is
    // then re-resolved onto B's origin while A's request is still running,
    // and A completes.
    let mut frontier = frontier();
    for (host, origin) in [
        ("a.example", "old"),
        ("b.example", "new"),
        ("c.example", "new"),
    ] {
        frontier.observe_resolution(host, Resolved::Origin(origin.to_owned()));
        frontier.observe_rules(&ConstraintKey::host(host), RulesState::Allowed);
    }
    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    assert!(
        matches!(
            frontier.admit("b.example", Duration::ZERO),
            Admission::Admit(_)
        ),
        "two hosts on two origins are bounded by the egress, not by each other"
    );

    frontier.observe_resolution("a.example", Resolved::Origin("new".to_owned()));
    frontier.complete(permit, Outcome::Success)?;

    // Physical ownership invariants, not implementation counters.
    assert_eq!(
        frontier.in_flight(&ConstraintKey::origin("old")),
        0,
        "A's request was admitted against the old origin, so completing it \
             must return that origin's slot — not one derived from the name's \
             new topology"
    );
    assert_eq!(
        frontier.in_flight(&ConstraintKey::origin("new")),
        1,
        "B is still physically running against the new origin and still owns \
             its only slot"
    );
    assert!(
        matches!(
            frontier.admit("c.example", Duration::ZERO),
            Admission::Defer { .. }
        ),
        "the default origin window of one must never admit C while B owns it"
    );
    assert_eq!(
        frontier.outstanding(),
        1,
        "one request is still in flight, so the ledger holds exactly one \
             reservation"
    );
    Ok(())
}

#[test]
fn a_resolution_failure_during_flight_returns_the_original_slots() -> TestResult {
    // A transient SERVFAIL while A is running removes the host from the
    // topology map. The permit still names the origin the request was
    // admitted against, so the release lands where the slots were taken.
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "old");
    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    assert_eq!(frontier.in_flight(&ConstraintKey::origin("old")), 1);

    frontier.observe_resolution("a.example", Resolved::Failed);
    frontier.complete(permit, Outcome::Failure)?;

    assert_eq!(
        frontier.in_flight(&ConstraintKey::origin("old")),
        0,
        "a resolution failure is an observation about the name, not evidence \
             that the request stopped holding its origin's slot"
    );
    assert_eq!(
        frontier.in_flight(&ConstraintKey::egress("eth0")),
        0,
        "and the egress slot comes back with it"
    );
    assert_eq!(frontier.outstanding(), 0, "the ledger is empty again");
    Ok(())
}

#[test]
fn several_in_flight_across_topology_generations_conserve_every_slot() -> TestResult {
    // Three requests admitted, the first host's name re-resolved twice while
    // all three are still running, and the completions issued out of order.
    // Every slot taken is returned exactly once, against the origin it was
    // taken from.
    let mut frontier = frontier();
    for (host, origin) in [
        ("a.example", "origin-a"),
        ("b.example", "origin-b"),
        ("c.example", "origin-c"),
    ] {
        ready(&mut frontier, host, origin);
    }
    let first = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    let second = admitted(&mut frontier, "b.example", Duration::ZERO)?;
    let third = admitted(&mut frontier, "c.example", Duration::ZERO)?;
    assert_eq!(
        frontier.in_flight(&ConstraintKey::egress("eth0")),
        3,
        "three requests are physically running"
    );

    // a.example moves twice: an edge cache, a failover, a second failover.
    frontier.observe_resolution("a.example", Resolved::Origin("gen2".to_owned()));
    frontier.observe_resolution("a.example", Resolved::Origin("gen3".to_owned()));
    assert_eq!(
        frontier.in_flight(&ConstraintKey::origin("origin-a")),
        1,
        "the request holds the origin it was admitted against however many \
             times its name has moved since"
    );

    // Out of order, and against a topology the names have left behind.
    frontier.complete(second, Outcome::Success)?;
    frontier.complete(third, Outcome::Failure)?;
    frontier.complete(first, Outcome::Success)?;

    for origin in ["origin-a", "origin-b", "origin-c", "gen2", "gen3"] {
        assert_eq!(
            frontier.in_flight(&ConstraintKey::origin(origin)),
            0,
            "every origin's occupancy returns to zero once its requests have \
                 completed"
        );
    }
    assert_eq!(frontier.in_flight(&ConstraintKey::egress("eth0")), 0);
    assert_eq!(frontier.outstanding(), 0, "and no reservation is left over");

    // The other half of the same rule: a *new* request for the same name
    // keys on where that name points now, so re-resolution still moves the
    // origin the run shares — it just cannot move one that is in flight.
    let moved = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    assert!(
        moved.holds(&ConstraintKey::origin("gen3")),
        "a new admission keys on the current topology"
    );
    assert!(
        !moved.holds(&ConstraintKey::origin("origin-a")),
        "and not on the one an earlier request for the same name used"
    );
    frontier.complete(moved, Outcome::Success)?;
    Ok(())
}

#[test]
fn a_permit_holds_its_admission_time_keys() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "old");
    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;

    assert!(
        permit.holds(&ConstraintKey::host("a.example")),
        "the permit holds the host it was admitted for"
    );
    assert!(
        permit.holds(&ConstraintKey::origin("old")),
        "and the origin the name resolved to at that instant"
    );
    assert!(
        permit.holds(&ConstraintKey::egress("eth0")),
        "and the egress every request in the run shares"
    );
    assert_eq!(permit.host(), "a.example");

    frontier.observe_resolution("a.example", Resolved::Origin("new".to_owned()));
    assert!(
        !permit.holds(&ConstraintKey::origin("new")),
        "a resolution observed while the request was in flight does not \
             change what the reservation holds"
    );
    Ok(())
}

#[test]
fn response_attribution_follows_the_admission_not_the_current_name() -> TestResult {
    // The response's failure signal is evidence about the infrastructure the
    // request actually reached. After a remap it must still shrink that
    // origin's window, and must not touch the origin the name now names.
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "old");
    ready(&mut frontier, "b.example", "new");
    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    frontier.observe_resolution("a.example", Resolved::Origin("new".to_owned()));

    for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
        completed(&mut frontier, "b.example", Outcome::Success)?;
    }
    let new_before = frontier.window(&ConstraintKey::origin("new"));
    let old_before = frontier.window(&ConstraintKey::origin("old"));

    frontier.complete(permit, Outcome::Failure)?;

    assert_eq!(
        frontier.window(&ConstraintKey::origin("old")),
        PolitenessPolicy::DEFAULT.decrease(old_before),
        "the failure shrank the origin the request was admitted against"
    );
    assert_eq!(
        frontier.window(&ConstraintKey::origin("new")),
        new_before,
        "and left the origin the name moved to untouched"
    );
    Ok(())
}

#[test]
fn a_permit_from_another_frontier_is_refused() -> TestResult {
    let mut issuer = frontier();
    let mut stranger = frontier();
    ready(&mut issuer, "a.example", "origin-a");
    let permit = admitted(&mut issuer, "a.example", Duration::ZERO)?;

    assert_eq!(
        stranger.complete(permit, Outcome::Success),
        Err(CompletionError::ForeignPermit),
        "a permit is evidence only against the accounting that minted it, \
             and a second frontier holds a different ledger"
    );
    assert_eq!(
        issuer.outstanding(),
        1,
        "the refusal changed nothing: the issuer still holds the slot the \
             request occupies"
    );
    assert_eq!(
        issuer.in_flight(&ConstraintKey::origin("origin-a")),
        1,
        "and a foreign completion must not decrement it"
    );
    Ok(())
}

#[test]
fn a_permit_with_no_live_reservation_is_refused() -> TestResult {
    // The same ledger, after the reservation it named has been retired by
    // the completion that consumed it. Nothing in the current public API can
    // hand back a spent permit — `complete` consumes it — so this drives the
    // ledger directly, which is the point: the check is enforced rather than
    // assumed, and the arm a future path would hit is the one a saturating
    // subtraction would have swallowed.
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;

    let ordinal = permit.ordinal;
    frontier.reservations.remove(&ordinal);
    assert_eq!(
        frontier.verify(&permit),
        Err(CompletionError::UnknownPermit),
        "a permit this ledger does not hold is a typed refusal, not a \
             subtraction that quietly does nothing"
    );
    assert_eq!(
        frontier.complete(permit, Outcome::Success),
        Err(CompletionError::UnknownPermit),
        "and completing it is refused before any window is touched"
    );
    Ok(())
}
