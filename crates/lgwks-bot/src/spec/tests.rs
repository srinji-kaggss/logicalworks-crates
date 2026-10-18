//! Tests of the spec module: the wire contract, the builder entry points and the tick.
//! Split from `spec.rs` (#526).

use super::*;
use crate::error::DispatchCertainty;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// What a test returns when it can fail in more than one error domain.
///
/// Building the scope a bot needs crosses `IdError` and `BrokerError` as
/// well as `BotError`, and none of the three converts into another, so a
/// test that hands over a scope reports through `Box<dyn Error>` and names
/// the failure with `?` rather than flattening it into a variant it is not.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The effect scope a test's bot runs under: the bot's own, so the spec
/// tests and the ECS tests build one scope rather than two that could drift.
///
/// A bot has no dispatch path without one, so every builder in this module
/// hands over a scope; the ones whose subject is spec validation still do,
/// because a refusal produced by "no scope" would be a refusal about the
/// wrong thing and the assertion around it would read as the validation it
/// claims to test.
fn test_effects() -> TestResult<EffectScope> {
    crate::ecs::tests::test_effects()
}

/// The failure a test reports when its precondition did not hold. Tests
/// return `Result` and propagate with `?`, so a mismatch is reported as a
/// named assertion failure with the cause attached rather than as a bare
/// unwind, and the cause names the invariant that was violated, not merely
/// that something was.
fn failed(cause: impl Into<String>) -> BotError {
    BotError::DomainError {
        domain: "spec::tests".into(),
        certainty: DispatchCertainty::NotDelivered,
        cause: cause.into(),
    }
}

/// How long a poll waits for its sibling at the [`Rendezvous`]: far past
/// any scheduling delay a loaded runner adds, far inside
/// [`crate::ecs::DEFAULT_POLL_DEADLINE`], so a tick that polled one source
/// at a time would still answer, with a peak of one.
const SIBLING_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// The polls in flight now, and the most that have been at once.
#[derive(Default)]
struct Overlap {
    in_flight: usize,
    peak: usize,
}

/// A meeting point for the polls of one tick.
///
/// The property under test is that `Bot::tick` has two polls in flight at
/// once. A fixed hold (it was 40 ms) only shows that when the second poll
/// starts before the first one's hold ends, and a loaded CI runner broke that
/// race: the hold expired first and the peak read 1. Here each poll waits
/// for the other instead of for a clock. When both are in flight together,
/// the peak is 2 on every schedule. When tick polls them one after the
/// other, the first waits out its patience alone and the peak is 1. Neither
/// answer depends on how fast the host is.
#[derive(Default)]
struct Rendezvous {
    overlap: Mutex<Overlap>,
    arrived: Condvar,
}

impl Rendezvous {
    /// The counts. A poisoned lock still holds them, because nothing a
    /// panicking holder could have done leaves the two numbers torn.
    fn overlap(&self) -> MutexGuard<'_, Overlap> {
        match self.overlap.lock() {
            Ok(overlap) => overlap,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Arrive, wait until `quorum` polls have been in flight together or
    /// the patience runs out, then leave.
    ///
    /// The wait is on the peak, not the live count: a peak never falls, so
    /// the first poll to leave cannot strand its sibling waiting for a count
    /// that has already dropped back.
    fn meet(&self, quorum: usize) {
        let mut overlap = self.overlap();
        // Bound: the test drives two chains against one rendezvous, so the
        // count stays at most 2.
        overlap.in_flight = overlap.in_flight.saturating_add(1);
        overlap.peak = overlap.peak.max(overlap.in_flight);
        self.arrived.notify_all();
        let mut overlap =
            match self
                .arrived
                .wait_timeout_while(overlap, SIBLING_PATIENCE, |overlap| overlap.peak < quorum)
            {
                Ok((overlap, _waited)) => overlap,
                Err(poisoned) => poisoned.into_inner().0,
            };
        overlap.in_flight = overlap.in_flight.saturating_sub(1);
    }

    /// The most polls that have been in flight at once.
    fn peak(&self) -> usize {
        self.overlap().peak
    }
}

/// An observer that needs `bot.net` and resolves with a value the caller
/// chooses.
///
/// One double for both boundary tests. The callee refuses it under an empty
/// proof and admits it under an issued one; the only thing that differs
/// between the two runs is the value read back, which is a field here rather
/// than a second declaration that could differ in the cap as well.
struct NetSource {
    /// The caps this source requires.
    caps: Vec<Cap>,
    /// What `poll` resolves with.
    value: u32,
}
impl NetSource {
    /// A `bot.net` source resolving with `value`.
    fn net(value: u32) -> Self {
        Self {
            caps: vec![Cap::net()],
            value,
        }
    }
}
impl crate::verb::Observe for NetSource {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(crate::verb::Observe::required_caps(self))?;
        Ok(self.value)
    }
    fn domain_id(&self) -> &str {
        "test::net_source"
    }
}

/// An observer that resolves with `42` and needs `bot.net`.
///
/// Declared once for both capability tests: two copies are two chances to
/// change one arm and leave the other asserting the old thing, and the cap it
/// requires *is* what those tests are about.
struct FakeSource(Vec<Cap>);
impl FakeSource {
    fn net() -> Self {
        Self(vec![Cap::net()])
    }
}
impl crate::verb::Observe for FakeSource {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] {
        &self.0
    }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(crate::verb::Observe::required_caps(self))?;
        Ok(42)
    }
    fn domain_id(&self) -> &str {
        "test::source"
    }
}

/// An observer that resolves immediately with `1`.
struct Immediate;
impl crate::verb::Observe for Immediate {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] {
        &[]
    }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(crate::verb::Observe::required_caps(self))?;
        Ok(1)
    }
    fn domain_id(&self) -> &str {
        "test::immediate"
    }
}

/// An observer whose poll always fails at the domain boundary.
struct Failing;
impl crate::verb::Observe for Failing {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] {
        &[]
    }
    async fn poll(&self, _call: (Auth, ())) -> Result<u32, BotError> {
        Err(BotError::DomainError {
            domain: "test::failing".into(),
            certainty: DispatchCertainty::NotDelivered,
            cause: "boom".into(),
        })
    }
    fn domain_id(&self) -> &str {
        "test::failing"
    }
}

/// An observer that meets its siblings at a [`Rendezvous`]. Its body crosses
/// a `spawn_blocking` thread, which is the only way two polls can be in
/// flight at once on a one-thread executor whose `block_on` has no timer.
struct PeakSource {
    rendezvous: Arc<Rendezvous>,
}
impl crate::verb::Observe for PeakSource {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] {
        &[]
    }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(crate::verb::Observe::required_caps(self))?;
        let rendezvous = Arc::clone(&self.rendezvous);
        lgwks_std::task::spawn_blocking(move || rendezvous.meet(2)).await;
        Ok(1)
    }
    fn domain_id(&self) -> &str {
        "test::peak"
    }
}

/// An action that proves the caps, and counts how often it ran when it was
/// given a counter to count into.
///
/// One implementation for both jobs. The counting tests read the counter and
/// the build-shape tests pass [`Action::new`]; two doubles differed only in
/// whether they owned a counter, and that is a field rather than a type — a
/// second impl is a second place for the two to disagree about what an action
/// requires or how long its effect lives.
#[derive(Clone)]
struct Action {
    /// Where each invocation is recorded, when the test cares.
    counter: Option<Arc<AtomicUsize>>,
}
impl Action {
    /// An action that records nothing.
    fn new() -> Self {
        Self { counter: None }
    }

    /// An action that records each invocation into `counter`.
    fn counting(counter: Arc<AtomicUsize>) -> Self {
        Self {
            counter: Some(counter),
        }
    }
}
impl crate::verb::Execute for Action {
    type Input = u32;
    type Output = ();
    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> crate::verb::EffectLifetime {
        crate::verb::EffectLifetime::Local
    }
    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(crate::verb::Execute::required_caps(self))?;
        if let Some(ref counter) = self.counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    fn domain_id(&self) -> &str {
        "test::action"
    }
}

#[test]
fn spec_round_trips_json() -> Result<(), BotError> {
    let spec = BotSpec {
        version: BotSpec::CURRENT_VERSION,
        name: "larry".into(),
        chains: vec![ChainSpec {
            source: "gh::pr_status".into(),
            target: "owner/repo".into(),
            on: vec![(
                "checks_changed".into(),
                ActionSpec {
                    domain: "notify::slack".into(),
                    target: "#deploys".into(),
                },
            )],
        }],
    };
    let json = spec
        .to_json()
        .map_err(|error| failed(format!("a well-formed spec must serialize: {error}")))?;
    let back = BotSpec::from_json(&json)?;
    assert_eq!(back.name, "larry");
    assert_eq!(back.chains.len(), 1);
    assert_eq!(back.chains[0].source, "gh::pr_status");
    assert_eq!(back.chains[0].on.len(), 1);
    assert_eq!(back.chains[0].on[0].0, "checks_changed");
    Ok(())
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    // deny_unknown_fields is what makes the from_json doc's "unknown field
    // is rejected" true; without it serde would silently ignore all three.
    let top = r#"{"name":"x","chains":[],"extra":1}"#;
    let chain = r#"{"name":"x","chains":[{"source":"a","target":"b","on":[],"extra":1}]}"#;
    let action = r#"{"name":"x","chains":[{"source":"a","target":"b","on":[["c",{"domain":"d","target":"e","extra":1}]]}]}"#;
    assert!(
        BotSpec::from_json(top).is_err(),
        "an unknown top-level field must be rejected, not ignored"
    );
    assert!(
        BotSpec::from_json(chain).is_err(),
        "an unknown ChainSpec field must be rejected, not ignored"
    );
    assert!(
        BotSpec::from_json(action).is_err(),
        "an unknown ActionSpec field must be rejected, not ignored"
    );
}

#[test]
fn missing_required_fields_are_rejected() {
    assert!(
        BotSpec::from_json(r#"{"chains":[]}"#).is_err(),
        "a spec with no name must be rejected, not defaulted"
    );
}

#[test]
fn spec_size_bound_is_checked_just_above_the_limit() {
    // Limit-adjacent partition for the defensive bound: exactly at the
    // bound the input is not refused for size (it is merely malformed);
    // one byte over is refused as SpecTooLarge and never parsed.
    let at = "x".repeat(MAX_SPEC_BYTES);
    assert!(matches!(
        BotSpec::from_json(&at),
        Err(BotError::MalformedSpec { .. })
    ));
    let over = "x".repeat(MAX_SPEC_BYTES + 1);
    assert!(matches!(
        BotSpec::from_json(&over),
        Err(BotError::SpecTooLarge { bytes, limit })
            if bytes == MAX_SPEC_BYTES + 1 && limit == MAX_SPEC_BYTES
    ));
}

#[test]
fn malformed_spec_diagnostic_escapes_control_characters() -> Result<(), BotError> {
    // An unknown field name is attacker-chosen; a newline in it must not
    // survive into the diagnostic as a log-forging byte.
    let Err(error) = BotSpec::from_json("{\"name\":\"x\",\"chains\":[],\"a\\nb\":1}") else {
        return Err(failed(
            "a field name containing a raw newline must be rejected as malformed",
        ));
    };
    let cause = match error {
        BotError::MalformedSpec { cause } => cause,
        other => return Err(failed(format!("expected MalformedSpec, got {other:?}"))),
    };
    assert!(
        !cause.contains('\n') && !cause.contains('\r'),
        "cause must not carry raw control bytes: {cause:?}"
    );
    Ok(())
}

#[test]
fn both_builder_entry_points_apply_the_same_admission() -> TestResult<()> {
    // `assemble` is shared, so the no-chains path and the with-chains path
    // must agree on both the empty-name rejection and the capability check.
    struct NeedsNet(Vec<Cap>);
    impl crate::verb::Observe for NeedsNet {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &self.0
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            Ok(0)
        }
        fn domain_id(&self) -> &str {
            "test::needs_net"
        }
    }
    // No-chains entry point (`BotBuilder::build`).
    assert!(
        matches!(
            Bot::builder("")
                .with_effects(test_effects()?)
                .build(&GrantSet::empty()),
            Err(BotError::IncompleteSpec {
                field: "name",
                cause: _,
            })
        ),
        "the no-chains entry point must reject an empty name"
    );
    // With-chains entry point (`ObserveBuilder::build`) rejects the same.
    assert!(
        matches!(
            Bot::builder("")
                .observe(NeedsNet(vec![]))
                .with_effects(test_effects()?)
                .build(&GrantSet::empty()),
            Err(BotError::IncompleteSpec {
                field: "name",
                cause: _,
            })
        ),
        "the with-chains entry point must reject the same empty name"
    );
    // And both admit capabilities the same way.
    let denied = Bot::builder("x")
        .observe(NeedsNet(vec![Cap::net()]))
        .on(|_: &u32| true, Action::new())
        .with_effects(test_effects()?)
        .build(&GrantSet::empty());
    assert!(
        matches!(denied, Err(BotError::CapabilityDenied { .. })),
        "a source whose cap is not in the grant set must be denied at build"
    );
    Ok(())
}

#[test]
fn empty_name_is_rejected() -> TestResult<()> {
    let result = Bot::builder("")
        .with_effects(test_effects()?)
        .build(&GrantSet::all_shipped());
    assert!(
        result.is_err(),
        "an empty name must be rejected even when every cap is granted"
    );
    Ok(())
}

#[test]
fn capability_denied_without_grant() -> TestResult<()> {
    let result = Bot::builder("test")
        .observe(FakeSource::net())
        .on(|_: &u32| true, Action::new())
        .with_effects(test_effects()?)
        .build(&GrantSet::empty());
    assert!(
        result.is_err(),
        "`bot.net` must be denied when the grant set is empty"
    );
    Ok(())
}

#[test]
fn capability_granted_builds_ok() -> TestResult<()> {
    let grants = GrantSet::empty().grant(Cap::net());
    let bot = Bot::builder("test")
        .observe(FakeSource::net())
        .on(|_: &u32| true, Action::new())
        .with_effects(test_effects()?)
        .build(&grants)?;
    assert_eq!(bot.name(), "test");
    assert_eq!(bot.source_domains().len(), 1);
    Ok(())
}

#[test]
fn tick_fires_matching_actions() -> TestResult<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountSource;
    impl crate::verb::Observe for CountSource {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            Ok(10)
        }
        fn domain_id(&self) -> &str {
            "test::count"
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));

    let mut bot = Bot::builder("ticker")
        .observe(CountSource)
        .on(
            |seen: &u32| *seen > 5,
            Action::counting(Arc::clone(&counter)),
        )
        .on(
            |seen: &u32| *seen > 100,
            Action::counting(Arc::clone(&counter)),
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    let fired = bot.tick()?;
    assert_eq!(fired, 1);
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    Ok(())
}

#[test]
fn issue_denies_what_was_never_granted() -> Result<(), BotError> {
    let grants = GrantSet::empty();
    match grants.issue(&[Cap::net()]) {
        Err(BotError::CapabilityDenied { deficit }) => {
            assert_eq!(deficit.first().required(), &Cap::net());
            Ok(())
        }
        other => Err(failed(format!("expected denial, got {other:?}"))),
    }
}

#[test]
fn call_with_empty_proof_is_denied_at_the_callee() -> Result<(), BotError> {
    use crate::verb::Observe;

    let vacuous = GrantSet::empty().issue(&[])?;
    match lgwks_std::task::block_on(NetSource::net(1).poll((vacuous, ()))) {
        Err(BotError::CapabilityDenied { deficit }) => {
            assert_eq!(deficit.first().required(), &Cap::net());
            Ok(())
        }
        other => Err(failed(format!(
            "a proof covering nothing must be denied by a capped callee, got {other:?}"
        ))),
    }
}

#[test]
fn wrong_scope_proof_is_denied_confused_deputy() -> Result<(), BotError> {
    use crate::verb::Observe;

    let fs_only = GrantSet::empty().grant(Cap::fs()).issue(&[Cap::fs()])?;
    match lgwks_std::task::block_on(NetSource::net(1).poll((fs_only, ()))) {
        Err(BotError::CapabilityDenied { deficit }) => {
            assert_eq!(deficit.first().required(), &Cap::net());
            Ok(())
        }
        other => Err(failed(format!(
            "a proof scoped to `bot.fs` must not authorize `bot.net`, got {other:?}"
        ))),
    }
}

#[test]
fn issued_proof_authorizes_the_call() -> Result<(), BotError> {
    use crate::verb::Observe;

    let auth = GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])?;
    assert_eq!(
        lgwks_std::task::block_on(NetSource::net(7).poll((auth, ())))?,
        7
    );
    Ok(())
}

#[test]
fn tick_drives_sources_in_one_step() -> TestResult<()> {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut bot = Bot::builder("direct")
        .observe(Immediate)
        .on(|_: &u32| true, Action::counting(Arc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    let fired = bot.tick()?;
    assert_eq!(fired, 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn tick_polls_sources_concurrently() -> TestResult<()> {
    // Each poll waits on a pool thread for its sibling, so the peak reaches
    // 2 only if tick has both sources in flight at the same time.
    let rendezvous = Arc::new(Rendezvous::default());
    let source = || PeakSource {
        rendezvous: Arc::clone(&rendezvous),
    };
    let mut bot = Bot::builder("concurrent")
        .observe(source())
        .on(
            |_: &u32| true,
            Action::counting(Arc::new(AtomicUsize::new(0))),
        )
        .observe(source())
        .on(
            |_: &u32| true,
            Action::counting(Arc::new(AtomicUsize::new(0))),
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    assert_eq!(bot.tick()?, 2);
    let observed = rendezvous.peak();
    assert_eq!(observed, 2, "sources overlapped only {observed} at a time");
    Ok(())
}

#[test]
fn tick_waves_more_chains_than_the_in_flight_cap() -> TestResult<()> {
    // 40 chains exceed MAX_IN_FLIGHT_POLLS (32), so this only passes if
    // the wave loop polls every chain, not just the first wave.
    let counter = Arc::new(AtomicUsize::new(0));
    let mut builder = Bot::builder("waves")
        .observe(Immediate)
        .on(|_: &u32| true, Action::counting(Arc::clone(&counter)));
    for _ in 0..39 {
        builder = builder
            .observe(Immediate)
            .on(|_: &u32| true, Action::counting(Arc::clone(&counter)));
    }
    let mut bot = builder
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    assert_eq!(bot.source_domains().len(), 40);
    assert_eq!(bot.tick()?, 40);
    assert_eq!(counter.load(Ordering::SeqCst), 40);
    Ok(())
}

#[test]
fn a_failing_poll_fires_nothing_and_returns_the_first_error() -> TestResult<()> {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut bot = Bot::builder("ordered")
        .observe(Immediate)
        .on(|_: &u32| true, Action::counting(Arc::clone(&counter)))
        .observe(Failing)
        .on(|_: &u32| true, Action::counting(Arc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;
    match bot.tick() {
        Err(BotError::DomainError { domain, .. }) => assert_eq!(domain, "test::failing"),
        other => {
            return Err(
                failed(format!("expected the failing chain's error, got {other:?}")).into(),
            );
        }
    }
    // All-or-nothing per tick: the observe system polls every source before
    // any effect runs, so a tick that errors commits nothing. The earlier
    // chain does not fire. That is stronger than the previous contract,
    // where chains declared before the failure had already acted.
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    Ok(())
}
