//! The crate README's documented examples, run as tests (#526).
//!
//! Each used to be a doctest that named the crate by path: the quick start, the spec's JSON round
//! trip and its refusal against a registry that lacks its domains. They are tests here, so each
//! documented claim runs under nextest with the rest of the suite and the README says what a test
//! holds instead of printing a program.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::verb::{Execute, Observe};
use lgwks_bot::{
    Admission, Auth, Bot, BotError, BotSpec, Cap, DomainRegistry, EffectLifetime, GrantSet,
};

use crate::effects::memory_scope;

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// A source whose value is the queue depth the test last stored, read each tick.
///
/// Shared with `sim_readme_front_door`, which drives it through a seeded sequence of depths.
pub(crate) struct QueueDepth {
    caps: Vec<Cap>,
    depth: Arc<AtomicU32>,
}

impl QueueDepth {
    /// A source of `depth`, which requires the network capability to be polled.
    pub(crate) fn over(depth: &Arc<AtomicU32>) -> Self {
        Self {
            caps: vec![Cap::net()],
            depth: Arc::clone(depth),
        }
    }
}

impl Observe for QueueDepth {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(self.depth.load(Ordering::SeqCst))
    }

    fn domain_id(&self) -> &str {
        "queue::depth"
    }
}

/// An effect that runs only when its chain's condition holds, and counts each run.
pub(crate) struct PageOnCall {
    pages: Arc<AtomicU32>,
}

impl PageOnCall {
    /// An effect that adds one to `pages` every time it runs.
    pub(crate) fn counting(pages: &Arc<AtomicU32>) -> Self {
        Self {
            pages: Arc::clone(pages),
        }
    }
}

impl Execute for PageOnCall {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        self.pages.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    // The journal under this test only promises `Ephemeral`, so the effect says it is local.
    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    fn domain_id(&self) -> &str {
        "notify::page"
    }
}

#[test]
fn a_chain_fires_once_on_the_tick_its_source_moves_past_the_condition() -> TestResult {
    let grants = GrantSet::empty().grant(Cap::net());
    let depth = Arc::new(AtomicU32::new(42));
    let pages = Arc::new(AtomicU32::new(0));
    let mut bot = Bot::builder("queue-watch")
        .observe(QueueDepth::over(&depth))
        .on(|depth: &u32| *depth > 10, PageOnCall::counting(&pages))
        .with_effects(memory_scope()?)
        .build(&grants)?;
    // One tick polls every source and fires every chain whose condition now holds.
    assert_eq!(bot.tick()?, 1, "the depth 42 passes the condition 10");
    assert_eq!(pages.load(Ordering::SeqCst), 1, "the effect ran once");
    Ok(())
}

#[test]
fn a_bot_spec_round_trips_through_json() -> TestResult {
    // `r##"…"##`, not `r#"…"#`: the payload contains `"#deploys"`, and the `"#` in that channel
    // name would otherwise terminate the raw string early.
    let spec = BotSpec::from_json(
        r##"{
    "name": "larry",
    "chains": [{
        "source": "gh::pr_status",
        "target": "owner/repo",
        "on": [["checks_changed", {"domain": "notify::slack", "target": "#deploys"}]]
    }]
}"##,
    )?;
    let json = spec.to_json()?;
    // The two halves of the round trip have different error types, deliberately: `from_json`
    // reports typed field and size errors (`BotError`) because untrusted input can be wrong in
    // domain ways, while `to_json` can only fail in the serializer.
    let again = BotSpec::from_json(&json)?;
    assert_eq!(again.name(), "larry", "the name reads back");
    assert_eq!(again.chains().len(), 1, "the one chain reads back");
    assert_eq!(
        again.to_json()?,
        json,
        "what was written reads back as the same document"
    );
    Ok(())
}

#[test]
fn a_spec_naming_a_domain_the_registry_lacks_is_refused_with_a_need_set() -> TestResult {
    let spec = BotSpec::from_json(
        r##"{"version":1,"name":"larry","chains":[
            {"source":"gh::pr_status","target":"owner/repo",
             "on":[["always",{"domain":"notify::slack","target":"#deploys"}]]}]}"##,
    )?;
    let refusal: Result<Bot, Admission> = Bot::from_spec(
        &spec,
        &DomainRegistry::empty(),
        &GrantSet::empty(),
        memory_scope()?,
    );
    // Nothing is polled and nothing runs: the document is all-or-nothing, and what it needs
    // comes back as a report, never as a grant.
    assert!(
        matches!(refusal, Err(Admission::Needs(ref needs)) if !needs.is_empty()),
        "an empty registry cannot supply the document's domains: {refusal:?}"
    );
    Ok(())
}
