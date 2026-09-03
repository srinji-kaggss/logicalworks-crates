//! `spec` owns the bot builder and serializable spec, enforcing
//! INV-BOT-SPEC-SERIALIZABLE: every `BotSpec` round-trips through JSON and
//! INV-BOT-TUPLE-WIRE: causal chains are `(condition, action)` tuples.

use lgwks_std::json::{Deserialize, Serialize};

use super::cap::Cap;
use super::error::BotError;
use super::gate::GrantSet;

// ── Serializable spec ──────────────────────────────────────────────────────

/// The serializable bot contract — what an AI emits, what a manifest contains,
/// what `Bot::build()` validates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct BotSpec {
    /// The bot's unique name.
    pub name: String,
    /// Observation chains: each binds a source to condition–action tuples.
    pub chains: Vec<ChainSpec>,
}

/// One observation binding in a serializable spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
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
#[serde(crate = "lgwks_std::json::serde")]
pub struct ActionSpec {
    /// The domain identifier (e.g. `"notify::slack"`).
    pub domain: String,
    /// The target parameter (e.g. `"#deploys"`).
    pub target: String,
}

// ── Live bot ───────────────────────────────────────────────────────────────

/// A built bot — name, typed observation chains, validated capabilities.
/// Constructed via `Bot::builder("name")`.
pub struct Bot {
    name: String,
    chains: Vec<Chain>,
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
    fn poll_any(&self) -> Result<Box<dyn std::any::Any>, BotError>;
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

    fn poll_any(&self) -> Result<Box<dyn std::any::Any>, BotError> {
        self.poll().map(|v| Box::new(v) as Box<dyn std::any::Any>)
    }
}

trait EvaluateAny {
    fn condition_id(&self) -> &str;
    fn check_any(&self, value: &dyn std::any::Any) -> Result<bool, BotError>;
}

trait ExecuteAny {
    fn domain_id(&self) -> &str;
    fn required_caps(&self) -> &[Cap];
    fn run_any(&self, input: &dyn std::any::Any) -> Result<Box<dyn std::any::Any>, BotError>;
}

// ── Builder ────────────────────────────────────────────────────────────────

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

    /// The bot's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The observation chains.
    pub fn chains(&self) -> &[Chain] {
        &self.chains
    }

    /// Tick all observation chains: poll each source, evaluate conditions,
    /// fire matching actions. Returns the count of actions fired.
    pub fn tick(&self) -> Result<usize, BotError> {
        let mut fired = 0;
        for chain in &self.chains {
            let value = chain.source.poll_any()?;
            for entry in &chain.entries {
                if entry.condition.check_any(value.as_ref())? {
                    entry.action.run_any(value.as_ref())?;
                    fired += 1;
                }
            }
        }
        Ok(fired)
    }
}

impl Chain {
    /// The source domain identifier.
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

            fn run_any(
                &self,
                input: &dyn std::any::Any,
            ) -> Result<Box<dyn std::any::Any>, BotError> {
                match input.downcast_ref::<A::Input>() {
                    Some(typed) => self
                        .0
                        .run(typed)
                        .map(|v| Box::new(v) as Box<dyn std::any::Any>),
                    None => Err(BotError::DomainError {
                        domain: self.0.domain_id().into(),
                        cause: "type mismatch in execute input".into(),
                    }),
                }
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
        })
    }
}

// ── Serialization ──────────────────────────────────────────────────────────

impl BotSpec {
    /// Serialize to JSON.
    pub fn to_json(&self) -> Result<String, crate::json::Error> {
        crate::json::to_string_pretty(self)
    }

    /// Deserialize from JSON.
    pub fn from_json(s: &str) -> Result<Self, crate::json::Error> {
        crate::json::from_str(s)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
            fn poll(&self) -> Result<u32, BotError> {
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
            fn run(&self, _: &u32) -> Result<(), BotError> {
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
            fn poll(&self) -> Result<u32, BotError> {
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
            fn run(&self, _: &u32) -> Result<(), BotError> {
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
            fn poll(&self) -> Result<u32, BotError> {
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
            fn run(&self, _: &u32) -> Result<(), BotError> {
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

        let fired = bot.tick().unwrap();
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }
}
