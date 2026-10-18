//! The spec module's documented doors, run as tests (#526).
//!
//! Each used to be a doctest that named the crate by path: the typed chain against its source, and
//! the three accessor families of the serializable spec. They are tests here, so each documented
//! claim runs under nextest with the rest of the suite.

use std::error::Error;

use lgwks_bot::domain::eval::Above;
use lgwks_bot::spec::{ActionSpec, ChainSpec};
use lgwks_bot::{
    Auth, Bot, BotError, BotSpec, Cap, EffectLifetime, Evaluate, Execute, GrantSet, Observe,
};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// A source that reports a count.
struct Counter;

impl Observe for Counter {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&[])?;
        Ok(3)
    }

    fn domain_id(&self) -> &str {
        "doc::counter"
    }
}

/// An action whose input is a count.
struct Ring;

impl Execute for Ring {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        Ok(())
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    fn domain_id(&self) -> &str {
        "doc::ring"
    }
}

/// The bound `on` is stated against, independently of the builder: any triple that satisfies it is a
/// chain this crate accepts.
fn assert_chain<S: Observe, C: Evaluate<S::Output>, A: Execute<Input = S::Output>>() {}

#[test]
fn a_chain_typed_against_its_source_builds_under_an_effect_scope() -> TestResult {
    // Instantiated with a shipped condition, so the bound is proven satisfiable by something other
    // than the closure below.
    assert_chain::<Counter, Above<u16>, Ring>();
    let bot = Bot::builder("doc")
        .observe(Counter)
        .on(|ticks: &u16| *ticks >= 3, Ring)
        .with_effects(crate::effects::memory_scope()?)
        .build(&GrantSet::empty())?;
    assert_eq!(bot.name(), "doc");
    assert_eq!(bot.source_domains().len(), 1);
    Ok(())
}

#[test]
fn a_spec_reads_back_its_name_its_version_and_its_chains() -> TestResult {
    let spec = BotSpec::from_json(r#"{"name":"larry","chains":[]}"#)?;
    assert_eq!(spec.name(), "larry");
    assert_eq!(spec.version(), BotSpec::CURRENT_VERSION);
    assert!(spec.chains().is_empty());
    Ok(())
}

#[test]
fn a_chain_reads_back_its_source_its_target_and_its_actions() {
    let chain = ChainSpec::new(
        "gh::pr_status",
        "owner/repo",
        vec![(
            "changed".to_owned(),
            ActionSpec::new("notify::slack", "#deploys"),
        )],
    );
    assert_eq!(chain.source(), "gh::pr_status");
    assert_eq!(chain.target(), "owner/repo");
    assert_eq!(chain.on()[0].1.domain(), "notify::slack");
}

#[test]
fn an_action_reads_back_its_domain_and_its_target() {
    let action = ActionSpec::new("notify::slack", "#deploys");
    assert_eq!(action.domain(), "notify::slack");
    assert_eq!(action.target(), "#deploys");
}
