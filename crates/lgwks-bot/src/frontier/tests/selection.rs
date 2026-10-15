//! Selection agrees with admission; ten thousand requests replay identically; release returns every slot.

use super::*;

#[test]
fn selection_skips_hosts_the_gate_will_not_admit() {
    // The filed counterexample. Rules are per-host state that selection
    // never consulted, and because selection takes the first key in a
    // `BTreeMap`, a permanently disallowed lexicographically-first host was
    // returned on every turn and starved the ready host behind it.
    let mut frontier = frontier();
    for host in ["a.example", "b.example"] {
        frontier.observe_resolution(host, Resolved::Origin(host.to_owned()));
    }
    frontier.observe_rules(
        &ConstraintKey::host("a.example"),
        RulesState::Disallowed {
            rule: "Disallow: /".to_owned(),
        },
    );
    frontier.observe_rules(&ConstraintKey::host("b.example"), RulesState::Allowed);

    assert!(
        matches!(
            frontier.admit("a.example", Duration::ZERO),
            Admission::Reject { .. }
        ),
        "the gate refuses the disallowed host"
    );
    assert_eq!(
        frontier.next_admissible(Duration::ZERO).as_deref(),
        Some("b.example"),
        "so selection must not return it: rejection does not remove the \
             host, and a selector with its own predicate returns it forever"
    );
}

#[test]
fn selection_agrees_with_admission_on_every_rules_state() -> TestResult {
    // One mixed backlog, ordered so the first host of each kind is the one a
    // divergent selector would have returned.
    let mut frontier = frontier();
    let hosts = ["a.example", "b.example", "c.example", "d.example"];
    for host in hosts {
        frontier.observe_resolution(host, Resolved::Origin(format!("origin-{host}")));
    }
    frontier.observe_rules(
        &ConstraintKey::host("a.example"),
        RulesState::Disallowed {
            rule: "Disallow: /".to_owned(),
        },
    );
    frontier.observe_rules(&ConstraintKey::host("b.example"), RulesState::NotFetched);
    frontier.observe_rules(
        &ConstraintKey::host("c.example"),
        RulesState::Unreachable {
            since: Duration::ZERO,
        },
    );
    frontier.observe_rules(&ConstraintKey::host("d.example"), RulesState::Allowed);

    let next = frontier.next_admissible(Duration::ZERO);
    assert_eq!(
        next.as_deref(),
        Some("d.example"),
        "disallowed, unfetched and unexpired-unreachable are all states the \
             gate will not admit, so none of them is selectable"
    );

    // The property the acceptance criteria state: for every host selection
    // returns, the gate admits it on unchanged state.
    // Refused rather than defaulted: with no host selected there is nothing
    // to admit, and the round trip this checks cannot be checked at all.
    let Some(selected) = next else {
        return Err("the gate selected no host, so the round trip cannot be \
                        exercised"
            .into());
    };
    let permit = match frontier.admit(&selected, Duration::ZERO) {
        Admission::Admit(permit) => permit,
        other => {
            return Err(format!(
                "selection returned {selected}, which the gate answered with {other:?}"
            )
            .into());
        }
    };
    assert!(permit.holds(&ConstraintKey::host("d.example")));
    Ok(())
}

#[test]
fn a_lone_unpermitted_host_is_not_selected() {
    let mut frontier = frontier();
    frontier.observe_resolution("only.example", Resolved::Origin("origin".to_owned()));
    frontier.observe_rules(&ConstraintKey::host("only.example"), RulesState::NotFetched);

    assert_eq!(
        frontier.next_admissible(Duration::ZERO),
        None,
        "a host whose rules have not been fetched is a prerequisite, not an \
             admission, and returning it as admissible deadlocks the scheduler \
             that trusts the answer"
    );
    assert_eq!(
        frontier.next_prerequisite(Duration::ZERO),
        Some(("only.example".to_owned(), DeferralKind::RulesNotFetched)),
        "the same predicate reports the work that would unblock it"
    );
}

#[test]
fn selection_agrees_with_admission_across_cooldowns_and_saturation() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    ready(&mut frontier, "b.example", "origin-b");
    ready(&mut frontier, "c.example", "origin-c");

    // a is inside a Retry-After the target asked for.
    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    frontier.observe_retry_after(&permit, Duration::ZERO, Duration::from_secs(30))?;

    // b is saturated: admitted, not completed.
    let held = admitted(&mut frontier, "b.example", Duration::ZERO)?;

    assert_eq!(
        frontier.next_admissible(Duration::from_secs(1)).as_deref(),
        Some("c.example"),
        "a host on cooldown and a host at its window are both held, so \
             selection moves past them to one the gate will take"
    );
    assert!(
        matches!(
            frontier.admit("c.example", Duration::from_secs(1)),
            Admission::Admit(_)
        ),
        "and the host selection returned is one the gate admits"
    );

    // c takes the last egress slot; nothing is admissible any more.
    frontier.complete(permit, Outcome::Success)?;
    assert_eq!(
        frontier.next_admissible(Duration::from_secs(1)),
        None,
        "with the egress saturated, nothing is admissible"
    );
    assert_eq!(
        frontier.next_prerequisite(Duration::from_secs(1)),
        None,
        "and nothing is waiting on a prerequisite either: these are waits, \
             not work the run can do"
    );

    frontier.complete(held, Outcome::Success)?;
    assert_eq!(
        frontier.next_admissible(Duration::from_secs(31)).as_deref(),
        Some("a.example"),
        "once the egress frees and the cooldown expires, the backlog is \
             selectable again — no caller-maintained shadow filtering"
    );
    Ok(())
}

#[test]
fn a_grace_expiry_is_visible_to_selection_before_anything_acts_on_it() {
    // The rules expiry is time-driven, and a selector that maintained its
    // own approximation would flip at a different instant than the gate.
    let mut frontier = frontier();
    frontier.observe_resolution("a.example", Resolved::Origin("origin-a".to_owned()));
    frontier.observe_rules(
        &ConstraintKey::host("a.example"),
        RulesState::Unreachable {
            since: Duration::ZERO,
        },
    );

    let inside_grace = PolitenessPolicy::DEFAULT
        .unreachable_grace()
        .saturating_sub(Duration::from_secs(1));
    assert_eq!(
        frontier.next_admissible(inside_grace),
        None,
        "inside the grace the complete disallow still holds"
    );
    assert_eq!(
        frontier
            .next_admissible(PolitenessPolicy::DEFAULT.unreachable_grace())
            .as_deref(),
        Some("a.example"),
        "and at the boundary it lifts for selection, exactly where the gate \
             would stop deferring"
    );
    assert_eq!(
        frontier.rules(&ConstraintKey::host("a.example")),
        RulesState::Unreachable {
            since: Duration::ZERO
        },
        "selection is side-effect free: it reports the expiry without \
             writing it down"
    );
    assert!(
        matches!(
            frontier.admit("a.example", PolitenessPolicy::DEFAULT.unreachable_grace()),
            Admission::Admit(_)
        ),
        "and the two agree once the grant is acted on"
    );
}

#[test]
fn selection_makes_progress_through_a_disallowed_prefix() -> TestResult {
    // A selection/admission cycle over a backlog whose first keys can never
    // be admitted. Each cycle must dispatch the next real host and never
    // hand back one the gate will refuse.
    let mut frontier = frontier();
    for host in ["a.example", "b.example", "c.example", "d.example"] {
        frontier.observe_resolution(host, Resolved::Origin(format!("origin-{host}")));
        frontier.observe_rules(
            &ConstraintKey::host(host),
            if host == "a.example" {
                RulesState::Disallowed {
                    rule: "Disallow: /".to_owned(),
                }
            } else {
                RulesState::Allowed
            },
        );
    }

    let mut dispatched = Vec::new();
    let mut held = Vec::new();
    for _ in 0..3 {
        let Some(host) = frontier.next_admissible(Duration::ZERO) else {
            break;
        };
        match frontier.admit(&host, Duration::ZERO) {
            // Held, not completed: each dispatch keeps its origin's single
            // slot, so the next cycle has to move on to another host rather
            // than returning the same one.
            Admission::Admit(permit) => {
                dispatched.push(host);
                held.push(permit);
            }
            other => {
                return Err(format!(
                    "selection returned {host}, which the gate answered with {other:?}"
                )
                .into());
            }
        }
    }

    assert_eq!(
        dispatched,
        vec!["b.example", "c.example", "d.example"],
        "three cycles dispatch three distinct ready hosts behind a host that \
             can never be dispatched"
    );
    assert_eq!(held.len(), 3, "each dispatch is still in flight");
    Ok(())
}

#[test]
fn ten_thousand_requests_replay_identically() {
    /// A verdict without the permit it may carry.
    ///
    /// A permit's identity is per-frontier — that is what makes a foreign
    /// one refusable — so two runs are compared on the verdicts they
    /// produced, one per step, and permits are settled inside the run rather
    /// than recorded.
    #[derive(Debug, PartialEq, Eq)]
    enum Shape {
        /// The frontier took the reservation.
        Admit,
        /// Held, with the instant, the cause and the binding constraint.
        Defer(Duration, DeferralKind, ConstraintKey),
        /// Dropped for good.
        Reject(RejectKind),
    }

    /// What one run produced: the verdict per step, sampled windows, and the
    /// count of completions the ledger refused — zero, or the run is not
    /// evidence about anything.
    struct Replay {
        /// The verdict of each of the ten thousand steps.
        verdicts: Vec<Shape>,
        /// The egress window, sampled every five hundred steps.
        windows: Vec<u32>,
        /// Completions the frontier refused.
        refusals: usize,
    }

    fn run(mut frontier: Frontier) -> Replay {
        use std::collections::VecDeque;

        let hosts: Vec<String> = (0..20_u32)
            .map(|index| format!("h{index}.example"))
            .collect();
        let mut replay = Replay {
            verdicts: Vec::new(),
            windows: Vec::new(),
            refusals: 0,
        };
        // Up to three requests in flight at once, oldest retired first. A
        // run that completed every request before asking for the next one
        // would never saturate anything, and a replay of two runs that never
        // back off is two identical silences.
        let mut parked: VecDeque<InFlightPermit> = VecDeque::new();
        let mut at = Duration::ZERO;
        let mut index = 0_usize;

        for step in 0..10_000_usize {
            let host = &hosts[index];
            let outcome = if step.checked_rem(7) == Some(0) {
                Outcome::Failure
            } else if step.checked_rem(3) == Some(0) {
                Outcome::CacheHit
            } else {
                Outcome::Success
            };
            if parked.len() >= 3 {
                let oldest = parked.pop_front();
                let refused =
                    oldest.is_some_and(|permit| frontier.complete(permit, outcome).is_err());
                if refused {
                    replay.refusals = replay.refusals.saturating_add(1);
                }
            }
            // A refusal has no reservation to settle, so only an admission
            // is parked — which is the exchange the API enforces.
            let shape = match frontier.admit(host, at) {
                Admission::Admit(permit) => {
                    parked.push_back(permit);
                    Shape::Admit
                }
                Admission::Defer {
                    resume_at,
                    kind,
                    constraint,
                } => Shape::Defer(resume_at, kind, constraint),
                Admission::Reject { kind } => Shape::Reject(kind),
            };
            replay.verdicts.push(shape);
            at = at.saturating_add(Duration::from_millis(5));
            if step.checked_rem(500) == Some(0) {
                replay
                    .windows
                    .push(frontier.window(&ConstraintKey::egress("eth0")));
            }
            index = if index.saturating_add(1) >= hosts.len() {
                0
            } else {
                index.saturating_add(1)
            };
        }
        replay
    }

    fn build() -> Frontier {
        let mut frontier = frontier();
        // Three hosts per origin, the shared-infrastructure case at size.
        // Cycled from the table rather than indexed by a remainder, so the
        // grouping is stated rather than recomputed.
        const ORIGINS: [&str; 3] = ["origin-0", "origin-1", "origin-2"];
        for (index, origin) in (0..20_u32).zip(ORIGINS.iter().cycle()) {
            let host = format!("h{index}.example");
            ready(&mut frontier, &host, origin);
        }
        frontier
    }

    let first = run(build());
    let second = run(build());

    assert_eq!(first.verdicts.len(), 10_000);
    assert_eq!(
        first.refusals, 0,
        "every admission in the run was released by the ledger that minted \
             it, so the refusal counter is the run's own consistency check"
    );
    assert!(
        first
            .verdicts
            .iter()
            .any(|shape| matches!(*shape, Shape::Admit))
            && first
                .verdicts
                .iter()
                .any(|shape| matches!(*shape, Shape::Defer(..))),
        "the schedule exercised both dispatch and back-off, so the replay \
             comparison is not two identical silences"
    );
    assert_eq!(
        first.verdicts, second.verdicts,
        "the same schedule on the same policy must produce identical verdicts"
    );
    assert_eq!(first.windows, second.windows);
}

#[test]
fn permits_from_identically_driven_frontiers_compare_equal() -> TestResult {
    // The determinism the virtual clock exists for, stated on the permit
    // itself: two frontiers driven through the same script mint the same
    // reservations in the same order, so their verdicts are comparable.
    let mut first = frontier();
    let mut second = frontier();
    ready(&mut first, "a.example", "origin-a");
    ready(&mut second, "a.example", "origin-a");

    assert_eq!(
        admitted(&mut first, "a.example", Duration::ZERO)?,
        admitted(&mut second, "a.example", Duration::ZERO)?,
        "identical schedules produce identical reservations"
    );
    Ok(())
}

#[test]
fn next_admissible_is_stable_and_skips_the_abandoned() {
    let mut frontier = frontier();
    ready(&mut frontier, "b.example", "origin-b");
    ready(&mut frontier, "a.example", "origin-a");
    ready(&mut frontier, "c.example", "origin-c");

    assert_eq!(
        frontier.next_admissible(Duration::ZERO).as_deref(),
        Some("a.example"),
        "selection is by key order, not by insertion order"
    );

    frontier.abandon("a.example");
    assert_eq!(
        frontier.next_admissible(Duration::ZERO).as_deref(),
        Some("b.example")
    );
    assert!(frontier.is_abandoned("a.example"));
}

#[test]
fn releasing_returns_every_shared_slot() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    ready(&mut frontier, "b.example", "origin-a");

    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    // b is behind the same origin, so the shared window is what blocks it.
    assert_eq!(
        deferral(&frontier.admit("b.example", Duration::ZERO)).map(|parts| parts.1),
        Some(DeferralKind::RateLimited)
    );

    frontier.complete(permit, Outcome::Success)?;
    assert_eq!(frontier.in_flight(&ConstraintKey::origin("origin-a")), 0);
    assert_eq!(frontier.in_flight(&ConstraintKey::egress("eth0")), 0);
    assert_eq!(frontier.outstanding(), 0);
    Ok(())
}
