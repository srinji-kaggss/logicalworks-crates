//! `spec` owns the bot builder and serializable spec, enforcing
//! INV-BOT-SPEC-SERIALIZABLE: every `BotSpec` round-trips through JSON and
//! INV-BOT-TUPLE-WIRE: causal chains are `(condition, action)` tuples.

use lgwks_std::json::{Deserialize, Serialize};

use super::cap::{Auth, Cap};
use super::error::BotError;
use super::gate::GrantSet;

// ── Serializable spec ──────────────────────────────────────────────────────

/// The serializable bot contract — what an AI emits and what a manifest
/// contains. `from_json` validates its shape; capability validation happens at
/// build time from a [`GrantSet`], not from a spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
pub struct BotSpec {
    /// The bot's unique name.
    pub name: String,
    /// Observation chains: each binds a source to condition–action tuples.
    pub chains: Vec<ChainSpec>,
}

/// One observation binding in a serializable spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
pub struct ChainSpec {
    /// The domain identifier of the observed source (e.g. `"gh::pr_status"`).
    pub source: String,
    /// The target parameter for the source (e.g. `"owner/repo"`).
    pub target: String,
    /// Condition–action pairs: `[condition_id, { domain: target }]`.
    pub on: Vec<(String, ActionSpec)>,
}

/// A serializable action reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
pub struct ActionSpec {
    /// The domain identifier (e.g. `"notify::slack"`).
    pub domain: String,
    /// The target parameter (e.g. `"#deploys"`).
    pub target: String,
}

// ── Live bot ───────────────────────────────────────────────────────────────

/// A built bot — name, typed observation chains, validated capabilities.
/// Constructed via `Bot::builder("name")`. Holds the grant set so every
/// `tick` mints fresh [`Auth`] proofs per domain instead of trusting
/// build-time admission alone.
pub struct Bot {
    name: String,
    chains: Vec<Chain>,
    grants: GrantSet,
}

/// A typed observation chain: source → `[(condition, action)]`.
pub struct Chain {
    source: Box<dyn ObserveAny>,
    entries: Vec<ChainEntry>,
}

/// One `(condition, action)` tuple in a chain.
pub struct ChainEntry {
    condition: Box<dyn EvaluateAny>,
    action: Box<dyn ExecuteAny>,
}

// ── Type-erased verb wrappers ──────────────────────────────────────────────

trait ObserveAny {
    fn domain_id(&self) -> &str;
    fn required_caps(&self) -> &[Cap];
    fn poll_any<'a>(
        &'a self,
        grants: &'a GrantSet,
    ) -> crate::BoxFuture<'a, Result<Box<dyn std::any::Any>, BotError>>;
}

impl<T: super::verb::Observe + 'static> ObserveAny for T
where
    T::Output: 'static,
{
    fn domain_id(&self) -> &str {
        super::verb::Observe::domain_id(self)
    }

    fn required_caps(&self) -> &[Cap] {
        super::verb::Observe::required_caps(self)
    }

    fn poll_any<'a>(
        &'a self,
        grants: &'a GrantSet,
    ) -> crate::BoxFuture<'a, Result<Box<dyn std::any::Any>, BotError>> {
        Box::pin(async move {
            let auth: Auth = grants.issue(super::verb::Observe::required_caps(self))?;
            let value = self.poll((auth, ())).await?;
            Ok(Box::new(value) as Box<dyn std::any::Any>)
        })
    }
}

trait EvaluateAny {
    fn condition_id(&self) -> &str;
    fn check_any(&self, value: &dyn std::any::Any) -> Result<bool, BotError>;
}

trait ExecuteAny {
    fn domain_id(&self) -> &str;
    fn required_caps(&self) -> &[Cap];
    fn run_any<'a>(
        &'a self,
        grants: &'a GrantSet,
        input: &'a dyn std::any::Any,
    ) -> crate::BoxFuture<'a, Result<Box<dyn std::any::Any>, BotError>>;
}

// ── Builder ────────────────────────────────────────────────────────────────

/// Upper bound on sources polled simultaneously by one `tick`. A source poll
/// may occupy one `spawn_blocking` thread, so this caps tick's blocking-thread
/// fan-out regardless of how many chains a spec declares. Chains beyond the
/// cap are polled in additional waves.
const MAX_IN_FLIGHT_POLLS: usize = 32;

/// Intermediate builder for attaching `(condition, action)` tuples to an
/// observed source.
pub struct ObserveBuilder {
    name: String,
    prior_chains: Vec<Chain>,
    source: Box<dyn ObserveAny>,
    entries: Vec<ChainEntry>,
}

impl Bot {
    /// Start building a named bot.
    pub fn builder(name: impl Into<String>) -> BotBuilder {
        BotBuilder {
            name: name.into(),
            chains: Vec::new(),
        }
    }

    /// The name the built [`Bot`] will report; it is set once by
    /// [`Bot::builder`] and never derived from the chains.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The chains in declaration order. [`Bot::tick`] polls and fires in this
    /// order, so reordering changes which side effects run before an error.
    pub fn chains(&self) -> &[Chain] {
        &self.chains
    }

    /// Tick all observation chains: poll every source concurrently, evaluate
    /// conditions, and fire matching actions in declaration order. Returns the
    /// count of actions fired.
    ///
    /// Async and concurrent: sources are polled in bounded waves of
    /// `MAX_IN_FLIGHT_POLLS` via `lgwks_std::task::join_all`, so a set of
    /// slow observers overlaps without unbounded blocking threads. Actions run
    /// sequentially in chain order so side effects stay deterministic. Every
    /// poll and `execute_action` carries a freshly issued `Auth` proof — a
    /// grant revoked after build cannot fire.
    ///
    /// Error ordering: sources are all polled before any action runs; the first
    /// error in declaration order is returned, and chains declared before it
    /// have already fired.
    pub async fn tick(&self) -> Result<usize, BotError> {
        let mut values: Vec<Result<Box<dyn std::any::Any>, BotError>> =
            Vec::with_capacity(self.chains.len());
        for wave in self.chains.chunks(MAX_IN_FLIGHT_POLLS) {
            let batch = lgwks_std::task::join_all(
                wave.iter().map(|chain| chain.source.poll_any(&self.grants)),
            )
            .await;
            values.extend(batch);
        }

        let mut fired = 0;
        for (chain, value) in self.chains.iter().zip(values) {
            let value = value?;
            for entry in &chain.entries {
                if entry.condition.check_any(value.as_ref())? {
                    entry.action.run_any(&self.grants, value.as_ref()).await?;
                    fired += 1;
                }
            }
        }
        Ok(fired)
    }

    /// Blocking convenience for [`Bot::tick`]: drive it to completion on the
    /// current thread with `lgwks_std::task::block_on`. Callers already in an
    /// async context should `tick().await` the same computation.
    pub fn block_on_tick(&self) -> Result<usize, BotError> {
        lgwks_std::task::block_on(self.tick())
    }
}

impl Chain {
    /// The `domain_id()` reported by this chain's source observer, for
    /// diagnostics.
    pub fn source_domain(&self) -> &str {
        self.source.domain_id()
    }

    /// The condition–action entries.
    pub fn entries(&self) -> &[ChainEntry] {
        &self.entries
    }
}

impl ChainEntry {
    /// The condition identifier (e.g. `"changed"`, `"<closure>"`).
    pub fn condition_id(&self) -> &str {
        self.condition.condition_id()
    }

    /// The action's domain identifier (e.g. `"notify::slack"`).
    pub fn action_domain(&self) -> &str {
        self.action.domain_id()
    }
}

/// Builder for `Bot`. Collects observation chains before validation.
pub struct BotBuilder {
    name: String,
    chains: Vec<Chain>,
}

impl BotBuilder {
    /// Bind a source to observe. Returns an `ObserveBuilder` to attach
    /// `(condition, action)` tuples.
    pub fn observe<S>(self, source: S) -> ObserveBuilder
    where
        S: super::verb::Observe + 'static,
        S::Output: 'static,
    {
        ObserveBuilder {
            name: self.name,
            prior_chains: self.chains,
            source: Box::new(source),
            entries: Vec::new(),
        }
    }

    /// Build with no observation chains — a bot that only supports direct
    /// `query()` and `execute()` calls.
    pub fn build(self, grants: &GrantSet) -> Result<Bot, BotError> {
        if self.name.is_empty() {
            return Err(BotError::IncompleteSpec { field: "name" });
        }
        for chain in &self.chains {
            grants.admit(chain.source.required_caps())?;
            for entry in &chain.entries {
                grants.admit(entry.action.required_caps())?;
            }
        }
        Ok(Bot {
            name: self.name,
            chains: self.chains,
            grants: grants.clone(),
        })
    }
}

impl ObserveBuilder {
    /// Add a `(condition, action)` tuple to this observation chain.
    pub fn on<C, A, T>(mut self, condition: C, action: A) -> Self
    where
        T: 'static,
        C: super::verb::Evaluate<T> + 'static,
        A: super::verb::Execute + 'static,
    {
        struct TypedEval<C, T> {
            inner: C,
            _marker: std::marker::PhantomData<T>,
        }

        impl<C: super::verb::Evaluate<T>, T: 'static> EvaluateAny for TypedEval<C, T> {
            fn condition_id(&self) -> &str {
                self.inner.condition_id()
            }

            fn check_any(&self, value: &dyn std::any::Any) -> Result<bool, BotError> {
                match value.downcast_ref::<T>() {
                    Some(typed) => self.inner.check(typed),
                    None => Err(BotError::EvaluateError {
                        cause: "type mismatch in evaluate".into(),
                    }),
                }
            }
        }

        struct TypedExec<A>(A);

        impl<A: super::verb::Execute> ExecuteAny for TypedExec<A>
        where
            A::Input: 'static,
            A::Output: 'static,
        {
            fn domain_id(&self) -> &str {
                self.0.domain_id()
            }

            fn required_caps(&self) -> &[Cap] {
                self.0.required_caps()
            }

            fn run_any<'a>(
                &'a self,
                grants: &'a GrantSet,
                input: &'a dyn std::any::Any,
            ) -> crate::BoxFuture<'a, Result<Box<dyn std::any::Any>, BotError>> {
                Box::pin(async move {
                    match input.downcast_ref::<A::Input>() {
                        Some(typed) => {
                            let auth: Auth = grants.issue(self.0.required_caps())?;
                            let value = self.0.execute_action((auth, typed)).await?;
                            Ok(Box::new(value) as Box<dyn std::any::Any>)
                        }
                        None => Err(BotError::DomainError {
                            domain: self.0.domain_id().into(),
                            cause: "type mismatch in execute input".into(),
                        }),
                    }
                })
            }
        }

        self.entries.push(ChainEntry {
            condition: Box::new(TypedEval {
                inner: condition,
                _marker: std::marker::PhantomData::<T>,
            }),
            action: Box::new(TypedExec(action)),
        });
        self
    }

    /// Finish this observation chain and start another.
    pub fn observe<S>(mut self, source: S) -> ObserveBuilder
    where
        S: super::verb::Observe + 'static,
        S::Output: 'static,
    {
        self.prior_chains.push(Chain {
            source: self.source,
            entries: self.entries,
        });
        ObserveBuilder {
            name: self.name,
            prior_chains: self.prior_chains,
            source: Box::new(source),
            entries: Vec::new(),
        }
    }

    /// Build the bot, validating all capabilities against the grant set.
    pub fn build(mut self, grants: &GrantSet) -> Result<Bot, BotError> {
        self.prior_chains.push(Chain {
            source: self.source,
            entries: self.entries,
        });
        if self.name.is_empty() {
            return Err(BotError::IncompleteSpec { field: "name" });
        }
        for chain in &self.prior_chains {
            grants.admit(chain.source.required_caps())?;
            for entry in &chain.entries {
                grants.admit(entry.action.required_caps())?;
            }
        }
        Ok(Bot {
            name: self.name,
            chains: self.prior_chains,
            grants: grants.clone(),
        })
    }
}

// ── Serialization ──────────────────────────────────────────────────────────

impl BotSpec {
    /// The spec as pretty-printed JSON; field order follows the struct
    /// declaration and enum variants serialize by name.
    pub fn to_json(&self) -> Result<String, crate::json::Error> {
        crate::json::to_string_pretty(self)
    }

    /// Parse a spec previously produced by [`BotSpec::to_json`]; a missing or
    /// unknown field is rejected rather than defaulted.
    pub fn from_json(s: &str) -> Result<Self, crate::json::Error> {
        crate::json::from_str(s)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
                cause: "boom".into(),
            })
        }
        fn domain_id(&self) -> &str {
            "test::failing"
        }
    }

    /// An observer that records how many polls overlap. Its body crosses a
    /// `spawn_blocking` thread, which is the only way two polls can run at the
    /// same wall-clock time on a one-thread executor.
    struct PeakSource {
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }
    impl crate::verb::Observe for PeakSource {
        type Output = u32;
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
            call.0.check(crate::verb::Observe::required_caps(self))?;
            let in_flight = Arc::clone(&self.in_flight);
            let peak = Arc::clone(&self.peak);
            lgwks_std::task::spawn_blocking(move || {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(40));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
            Ok(1)
        }
        fn domain_id(&self) -> &str {
            "test::peak"
        }
    }

    /// A counting action.
    #[derive(Clone)]
    struct Counting(Arc<AtomicUsize>);
    impl crate::verb::Execute for Counting {
        type Input = u32;
        type Output = ();
        fn required_caps(&self) -> &[Cap] {
            &[]
        }
        async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
            call.0.check(crate::verb::Execute::required_caps(self))?;
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn domain_id(&self) -> &str {
            "test::counting"
        }
    }

    #[test]
    fn spec_round_trips_json() {
        let spec = BotSpec {
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
        let json = spec.to_json().unwrap();
        let back = BotSpec::from_json(&json).unwrap();
        assert_eq!(back.name, "larry");
        assert_eq!(back.chains.len(), 1);
        assert_eq!(back.chains[0].source, "gh::pr_status");
        assert_eq!(back.chains[0].on.len(), 1);
        assert_eq!(back.chains[0].on[0].0, "checks_changed");
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        // deny_unknown_fields is what makes the from_json doc's "unknown field
        // is rejected" true; without it serde would silently ignore all three.
        let top = r#"{"name":"x","chains":[],"extra":1}"#;
        let chain = r#"{"name":"x","chains":[{"source":"a","target":"b","on":[],"extra":1}]}"#;
        let action = r#"{"name":"x","chains":[{"source":"a","target":"b","on":[["c",{"domain":"d","target":"e","extra":1}]]}]}"#;
        assert!(BotSpec::from_json(top).is_err());
        assert!(BotSpec::from_json(chain).is_err());
        assert!(BotSpec::from_json(action).is_err());
    }

    #[test]
    fn missing_required_fields_are_rejected() {
        assert!(BotSpec::from_json(r#"{"chains":[]}"#).is_err());
    }

    #[test]
    fn empty_name_is_rejected() {
        let result = Bot::builder("").build(&GrantSet::all_shipped());
        assert!(result.is_err());
    }

    #[test]
    fn capability_denied_without_grant() {
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

        struct FakeAction;
        impl crate::verb::Execute for FakeAction {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::action"
            }
        }

        let result = Bot::builder("test")
            .observe(FakeSource::net())
            .on(|_: &u32| true, FakeAction)
            .build(&GrantSet::empty());
        assert!(result.is_err());
    }

    #[test]
    fn capability_granted_builds_ok() {
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

        struct FakeAction;
        impl crate::verb::Execute for FakeAction {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::action"
            }
        }

        let grants = GrantSet::empty().grant(Cap::net());
        let bot = Bot::builder("test")
            .observe(FakeSource::net())
            .on(|_: &u32| true, FakeAction)
            .build(&grants)
            .unwrap();
        assert_eq!(bot.name(), "test");
        assert_eq!(bot.chains().len(), 1);
    }

    #[test]
    fn tick_fires_matching_actions() {
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

        #[derive(Clone)]
        struct CountAction(Arc<AtomicUsize>);
        impl crate::verb::Execute for CountAction {
            type Input = u32;
            type Output = ();
            fn required_caps(&self) -> &[Cap] {
                &[]
            }
            async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
                call.0.check(crate::verb::Execute::required_caps(self))?;
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            fn domain_id(&self) -> &str {
                "test::count_action"
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));

        let bot = Bot::builder("ticker")
            .observe(CountSource)
            .on(|v: &u32| *v > 5, CountAction(counter.clone()))
            .on(|v: &u32| *v > 100, CountAction(counter.clone()))
            .build(&GrantSet::empty())
            .unwrap();

        let fired = bot.block_on_tick().unwrap();
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn issue_denies_what_was_never_granted() {
        let grants = GrantSet::empty();
        match grants.issue(&[Cap::net()]) {
            Err(BotError::CapabilityDenied { required }) => {
                assert_eq!(required, Cap::net());
            }
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn call_with_empty_proof_is_denied_at_the_callee() {
        use crate::verb::Observe;

        struct NetSource([Cap; 1]);
        impl Observe for NetSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(1)
            }
            fn domain_id(&self) -> &str {
                "test::net"
            }
        }

        let vacuous = GrantSet::empty().issue(&[]).expect("empty coverage issues");
        match lgwks_std::task::block_on(NetSource([Cap::net()]).poll((vacuous, ()))) {
            Err(BotError::CapabilityDenied { required }) => {
                assert_eq!(required, Cap::net());
            }
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn wrong_scope_proof_is_denied_confused_deputy() {
        use crate::verb::Observe;

        struct NetSource([Cap; 1]);
        impl Observe for NetSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(1)
            }
            fn domain_id(&self) -> &str {
                "test::net"
            }
        }

        let fs_only = GrantSet::empty()
            .grant(Cap::fs())
            .issue(&[Cap::fs()])
            .expect("fs granted");
        match lgwks_std::task::block_on(NetSource([Cap::net()]).poll((fs_only, ()))) {
            Err(BotError::CapabilityDenied { required }) => {
                assert_eq!(required, Cap::net());
            }
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn issued_proof_authorizes_the_call() {
        use crate::verb::Observe;

        struct NetSource([Cap; 1]);
        impl Observe for NetSource {
            type Output = u32;
            fn required_caps(&self) -> &[Cap] {
                &self.0
            }
            async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
                call.0.check(crate::verb::Observe::required_caps(self))?;
                Ok(7)
            }
            fn domain_id(&self) -> &str {
                "test::net"
            }
        }

        let auth = GrantSet::empty()
            .grant(Cap::net())
            .issue(&[Cap::net()])
            .expect("net granted");
        assert_eq!(
            lgwks_std::task::block_on(NetSource([Cap::net()]).poll((auth, ()))).unwrap(),
            7
        );
    }

    #[test]
    fn tick_is_directly_awaitable() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bot = Bot::builder("direct")
            .observe(Immediate)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)))
            .build(&GrantSet::empty())
            .expect("builds");
        let fired = lgwks_std::task::block_on(bot.tick()).expect("tick");
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tick_polls_sources_concurrently() {
        // Both polls block on a dedicated thread, so the peak in-flight count
        // can only reach 2 if tick drives the two sources at the same time.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let source = || PeakSource {
            in_flight: Arc::clone(&in_flight),
            peak: Arc::clone(&peak),
        };
        let bot = Bot::builder("concurrent")
            .observe(source())
            .on(|_: &u32| true, Counting(Arc::new(AtomicUsize::new(0))))
            .observe(source())
            .on(|_: &u32| true, Counting(Arc::new(AtomicUsize::new(0))))
            .build(&GrantSet::empty())
            .expect("builds");
        assert_eq!(bot.block_on_tick().expect("tick"), 2);
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed >= 2,
            "sources overlapped only {observed} at a time"
        );
    }

    #[test]
    fn tick_waves_more_chains_than_the_in_flight_cap() {
        // 40 chains exceed MAX_IN_FLIGHT_POLLS (32), so this only passes if
        // the wave loop polls every chain, not just the first wave.
        let counter = Arc::new(AtomicUsize::new(0));
        let mut builder = Bot::builder("waves")
            .observe(Immediate)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)));
        for _ in 0..39 {
            builder = builder
                .observe(Immediate)
                .on(|_: &u32| true, Counting(Arc::clone(&counter)));
        }
        let bot = builder.build(&GrantSet::empty()).expect("builds");
        assert_eq!(bot.chains().len(), 40);
        assert_eq!(bot.block_on_tick().expect("tick"), 40);
        assert_eq!(counter.load(Ordering::SeqCst), 40);
    }

    #[test]
    fn tick_fires_earlier_chains_then_returns_first_error() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bot = Bot::builder("ordered")
            .observe(Immediate)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)))
            .observe(Failing)
            .on(|_: &u32| true, Counting(Arc::clone(&counter)))
            .build(&GrantSet::empty())
            .expect("builds");
        match bot.block_on_tick() {
            Err(BotError::DomainError { domain, .. }) => assert_eq!(domain, "test::failing"),
            other => panic!("expected the failing chain's error, got {other:?}"),
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
