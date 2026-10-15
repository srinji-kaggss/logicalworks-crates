//! Unit tests for `frontier`, moved verbatim out of frontier.rs.

use super::*;

const HOUR: Duration = Duration::from_secs(3_600);
const THIRTY_ONE_DAYS: Duration = Duration::from_secs(2_678_400);

/// A test that cannot fail silently: every helper below returns a value or a
/// message, and `?` carries either out to the harness.
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn frontier() -> Frontier {
    Frontier::new(PolitenessPolicy::DEFAULT, "eth0")
}

/// A host that is resolved, permitted, and otherwise ready to go.
fn ready(frontier: &mut Frontier, host: &str, origin: &str) {
    frontier.observe_resolution(host, Resolved::Origin(origin.to_owned()));
    frontier.observe_rules(&ConstraintKey::host(host), RulesState::Allowed);
}

/// Destructures a deferral into its parts, so a test asserts on real values
/// rather than on a pattern match that could hold for the wrong reason.
fn deferral(admission: &Admission) -> Option<(Duration, DeferralKind, ConstraintKey)> {
    match *admission {
        Admission::Defer {
            resume_at,
            kind,
            ref constraint,
        } => Some((resume_at, kind, constraint.clone())),
        Admission::Admit(_) | Admission::Reject { .. } => None,
    }
}

/// Admits `host`, asserting that the frontier took the reservation, and
/// returns the permit that releases it.
///
/// A refusal comes back as an error naming what arrived instead, so a test
/// that expected an admission fails with the verdict rather than with a
/// panic that says nothing about which arm it got.
fn admitted(
    frontier: &mut Frontier,
    host: &str,
    at: Duration,
) -> Result<InFlightPermit, Box<dyn std::error::Error>> {
    match frontier.admit(host, at) {
        Admission::Admit(permit) => Ok(permit),
        other => Err(format!("expected an admission for {host}, got {other:?}").into()),
    }
}

/// Admits `host` and completes it with `outcome`, asserting both halves of
/// the exchange. The one-line form of "a request went out and came back".
fn completed(
    frontier: &mut Frontier,
    host: &str,
    outcome: Outcome,
) -> Result<(), Box<dyn std::error::Error>> {
    let permit = admitted(frontier, host, Duration::ZERO)?;
    frontier.complete(permit, outcome)?;
    Ok(())
}

#[test]
fn an_unresolved_host_defers_rather_than_rejects() {
    let mut frontier = frontier();
    assert_eq!(
        deferral(&frontier.admit("a.example", Duration::ZERO)),
        Some((
            Duration::ZERO,
            DeferralKind::OriginUnmapped,
            ConstraintKey::host("a.example"),
        )),
        "a crawler discovers hosts as it goes, so an unknown host is a \
             prerequisite to resolve and not a dead item"
    );
}

#[test]
fn nxdomain_rejects_and_servfail_defers() {
    let mut frontier = frontier();
    frontier.observe_resolution("gone.example", Resolved::DoesNotExist);
    frontier.observe_resolution("flaky.example", Resolved::Failed);

    assert_eq!(
        frontier.admit("gone.example", Duration::ZERO),
        Admission::Reject {
            kind: RejectKind::DoesNotExist
        },
        "RFC 8020 makes NXDOMAIN authoritative"
    );

    assert_eq!(
        deferral(&frontier.admit("flaky.example", Duration::ZERO)),
        Some((
            PolitenessPolicy::DEFAULT.retry_after_failure(),
            DeferralKind::ResolutionFailed,
            ConstraintKey::host("flaky.example"),
        )),
        "SERVFAIL is not authoritative, so it must not be terminal"
    );
}

#[test]
fn rules_states_follow_rfc_9309() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    let key = ConstraintKey::host("a.example");

    frontier.observe_rules(&key, RulesState::NotFetched);
    assert_eq!(
        deferral(&frontier.admit("a.example", Duration::ZERO)).map(|parts| parts.1),
        Some(DeferralKind::RulesNotFetched),
        "unfetched rules are a prerequisite, not a wait"
    );

    // 4xx: unavailable, MAY access any resources.
    frontier.observe_rules(&key, RulesState::NoRules);
    let permit = admitted(&mut frontier, "a.example", HOUR)?;
    frontier.complete(permit, Outcome::Success)?;

    // 5xx: unreachable, MUST assume complete disallow.
    frontier.observe_rules(
        &key,
        RulesState::Unreachable {
            since: Duration::ZERO,
        },
    );
    assert_eq!(
        deferral(&frontier.admit("a.example", HOUR)).map(|parts| parts.1),
        Some(DeferralKind::OriginPolicyUnreachable),
        "RFC 9309 2.3.1.4 requires assuming complete disallow"
    );

    // ...but only until a fresh file is obtained, which that section bounds.
    assert!(
        matches!(
            frontier.admit("a.example", THIRTY_ONE_DAYS),
            Admission::Admit(_)
        ),
        "RFC 9309 2.3.1.4 lets a long-unreachable file be treated as unavailable"
    );
    assert_eq!(
        frontier.rules(&key),
        RulesState::NoRules,
        "taking the admission is what records the expiry the verdict rested on"
    );
    Ok(())
}

#[test]
fn disallowed_is_terminal_and_unreachable_is_not() {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    let key = ConstraintKey::host("a.example");

    frontier.observe_rules(
        &key,
        RulesState::Disallowed {
            rule: "Disallow: /private".to_owned(),
        },
    );
    assert_eq!(
        frontier.admit("a.example", Duration::ZERO),
        Admission::Reject {
            kind: RejectKind::Disallowed {
                rule: "Disallow: /private".to_owned()
            }
        }
    );

    // The same request under an unreachable file must NOT be rejected:
    // that would turn a transient 5xx into a permanent deny-list.
    frontier.observe_rules(
        &key,
        RulesState::Unreachable {
            since: Duration::ZERO,
        },
    );
    assert_eq!(
        deferral(&frontier.admit("a.example", HOUR)).map(|parts| parts.1),
        Some(DeferralKind::OriginPolicyUnreachable),
        "unreachability is provisional and must never be spelled Reject"
    );
}

#[test]
fn hosts_sharing_an_origin_share_one_window() {
    let mut frontier = frontier();
    // Eight Shopify-shaped tenants: distinct names, one proxy.
    for index in 0..8_u32 {
        ready(
            &mut frontier,
            &format!("shop{index}.example"),
            "shopify-edge",
        );
    }

    let mut admitted = 0_u32;
    for index in 0..8_u32 {
        if matches!(
            frontier.admit(&format!("shop{index}.example"), Duration::ZERO),
            Admission::Admit(_)
        ) {
            admitted = admitted.saturating_add(1);
        }
    }

    assert_eq!(
        admitted,
        PolitenessPolicy::DEFAULT.min_window(),
        "eight 'independent' hosts behind one proxy must share one window, \
             or the run is a Layer 7 flood with a polite-looking config"
    );
    assert_eq!(
        frontier.in_flight(&ConstraintKey::origin("shopify-edge")),
        PolitenessPolicy::DEFAULT.min_window()
    );
}

#[test]
fn distinct_origins_are_limited_by_egress_alone() {
    let mut frontier = frontier();
    for index in 0..4_u32 {
        let host = format!("h{index}.example");
        ready(&mut frontier, &host, &format!("origin-{index}"));
    }

    let mut admitted = 0_u32;
    for index in 0..4_u32 {
        if matches!(
            frontier.admit(&format!("h{index}.example"), Duration::ZERO),
            Admission::Admit(_)
        ) {
            admitted = admitted.saturating_add(1);
        }
    }
    assert_eq!(
        admitted,
        PolitenessPolicy::DEFAULT.egress_window(),
        "four hosts on four origins are bounded by the egress, not by each other"
    );
    assert_eq!(
        deferral(&frontier.admit("h0.example", Duration::ZERO)).map(|parts| parts.2),
        Some(ConstraintKey::egress("eth0")),
        "and the binding constraint is the egress"
    );
}

#[test]
fn the_binding_constraint_is_reported() {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");

    assert!(
        matches!(
            frontier.admit("a.example", Duration::ZERO),
            Admission::Admit(_)
        ),
        "a resolved, permitted host on a free origin and egress admits"
    );
    assert_eq!(
        deferral(&frontier.admit("a.example", Duration::ZERO)).map(|parts| parts.1),
        Some(DeferralKind::RateLimited),
        "a window of one admits exactly once"
    );
}

#[test]
fn retry_after_binds_the_origin_not_just_the_host() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    ready(&mut frontier, "b.example", "origin-a");

    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    frontier.observe_retry_after(&permit, Duration::ZERO, Duration::from_secs(30))?;
    frontier.complete(permit, Outcome::Success)?;

    assert_eq!(
        deferral(&frontier.admit("b.example", Duration::from_secs(1))),
        Some((
            Duration::from_secs(30),
            DeferralKind::RateLimited,
            ConstraintKey::origin("origin-a"),
        )),
        "a pause asked of the client binds its sibling on the same origin"
    );
    assert!(
        matches!(
            frontier.admit("b.example", Duration::from_secs(31)),
            Admission::Admit(_)
        ),
        "and it lifts when the instant it named arrives"
    );
    Ok(())
}

#[test]
fn a_retry_after_does_not_bind_the_egress() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    ready(&mut frontier, "unrelated.example", "origin-z");

    let permit = admitted(&mut frontier, "a.example", Duration::ZERO)?;
    frontier.observe_retry_after(&permit, Duration::ZERO, Duration::from_secs(30))?;

    assert!(
        matches!(
            frontier.admit("unrelated.example", Duration::from_secs(1)),
            Admission::Admit(_)
        ),
        "one server asking for a pause is not evidence about our network's \
             capacity; binding the egress would let a single target halt every \
             unrelated host in the run"
    );
    assert_eq!(
        deferral(&frontier.admit("a.example", Duration::from_secs(1))).map(|parts| parts.2),
        Some(ConstraintKey::origin("origin-a")),
        "but it does bind the server-side keys the target actually shares"
    );
    Ok(())
}

#[test]
fn the_window_shrinks_on_failure_and_grows_on_success() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    let key = ConstraintKey::origin("origin-a");

    for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
        completed(&mut frontier, "a.example", Outcome::Success)?;
    }
    assert_eq!(
        frontier.window(&key),
        2,
        "eight successes add one to a window of one"
    );

    completed(&mut frontier, "a.example", Outcome::Failure)?;
    assert_eq!(frontier.window(&key), 1, "2 x 70% floors at min_window");

    for _ in 0..5 {
        completed(&mut frontier, "a.example", Outcome::Failure)?;
    }
    assert_eq!(
        frontier.window(&key),
        PolitenessPolicy::DEFAULT.min_window(),
        "a window of zero would admit nothing forever"
    );
    Ok(())
}

#[test]
fn a_cache_hit_does_not_grow_the_window() -> TestResult {
    let mut frontier = frontier();
    ready(&mut frontier, "a.example", "origin-a");
    let key = ConstraintKey::origin("origin-a");

    for _ in 0..PolitenessPolicy::DEFAULT.increase_after() {
        completed(&mut frontier, "a.example", Outcome::CacheHit)?;
    }
    assert_eq!(
        frontier.window(&key),
        PolitenessPolicy::DEFAULT.min_window(),
        "an edge cache answering fast says nothing about origin capacity, and \
             counting it is how a latency controller speeds up under load"
    );
    assert_eq!(
        frontier.in_flight(&key),
        0,
        "a cache hit still returns the slots it held; only the growth signal \
             is excluded"
    );
    Ok(())
}

mod selection;

mod reservation;
