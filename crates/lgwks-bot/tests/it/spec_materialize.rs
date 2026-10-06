//! `Bot::from_spec`: the materializer that walks a `BotSpec` against a registry.
//!
//! Two claims are held here, and they are the two the contract names. T25: a
//! spec materialized from JSON and a bot built natively from the same domains
//! produce the same normalized operation trace — which domains were polled and
//! which actions executed, in order — because they go through the same
//! `assemble`/`build` path with no second interpreter. T23: a spec with every
//! kind of unmet need returns them all in one attributed `NeedSet`, and nothing
//! is polled or executed on the way.
//!
//! Everything drives the shipped surface: the real `Bot`, the real
//! `DomainRegistry` built with `domains!`, the real `Source`/`Action` handles,
//! and a real `MemoryJournal` behind a real `EffectScope`. The domains here are
//! deliberately tiny and share one thread-local recorder, so the "same trace"
//! assertion compares what a person would see rather than an internal field.

use std::cell::RefCell;
use std::error::Error;

use lgwks_bot::broker::Broker;
use lgwks_bot::domain::eval::{Above, Changed};
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{Bot, BotSpec, EffectIdentity, EffectScope};
use lgwks_bot::{
    Action, Admission, Auth, BotError, Cap, EffectLifetime, Evaluate, Execute, GrantSet, Need,
    NeedSet, Observe, Source, domains,
};

/// A test result that can carry an `Admission`, a `BotError` or an id error.
type TestResult<T> = Result<T, Box<dyn Error>>;

/// A bot's fired count paired with the operations this thread recorded for it.
type Run = (usize, Vec<String>);

// ── The operation trace ────────────────────────────────────────────────────

thread_local! {
    /// The operations the domains performed on this thread, in order.
    ///
    /// A thread-local rather than a captured `Rc`: a registry constructor is a
    /// bare function pointer with no context, so the recorder is the only state
    /// shared between a natively constructed domain and one a registry built
    /// from the same `target`.
    static OPERATIONS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Append one operation to the current thread's trace.
fn record(operation: impl Into<String>) {
    OPERATIONS.with(|trace| trace.borrow_mut().push(operation.into()));
}

/// Take the trace, leaving it empty.
fn take_trace() -> Vec<String> {
    OPERATIONS.with(|trace| std::mem::take(&mut *trace.borrow_mut()))
}

// ── Domains ────────────────────────────────────────────────────────────────

/// Parse a target as a count, or refuse it — the malformed-input case a
/// constructor reports through admission.
///
/// The `ParseIntError` is recorded rather than dropped: `target` is untrusted
/// text, and the parse error's own message ("invalid digit found in string",
/// "cannot parse integer from empty string") is the part that says whether the
/// value was empty or merely non-numeric. `BotError::IncompleteSpec` renders it
/// through `Escaped`, so the untrusted text it quotes cannot forge a log line.
pub(crate) fn parse_count(target: &str) -> Result<u16, BotError> {
    target
        .parse::<u16>()
        .map_err(|not_a_count| BotError::IncompleteSpec {
            field: "target",
            cause: not_a_count.to_string(),
        })
}

/// A source that reports the count its `target` parsed to.
///
/// Shared with `sim_spec_materialize`, whose sweep admits specs over the same
/// source rather than a copy of it.
pub(crate) struct Counter {
    /// The value to report.
    value: u16,
    /// The capabilities this instance requires.
    caps: Vec<Cap>,
}

impl Counter {
    /// Build a cap-free one directly, for a native bot.
    fn new(value: u16) -> Self {
        Self::with_caps(value, Vec::new())
    }

    /// Build one with the given capabilities directly, for a native bot.
    fn with_caps(value: u16, caps: Vec<Cap>) -> Self {
        Self { value, caps }
    }

    /// Build a cap-free one from the `target` its spec names.
    pub(crate) fn from_target(target: &str) -> Result<Source, BotError> {
        Ok(Source::ordered(Self::with_caps(
            parse_count(target)?,
            Vec::new(),
        )))
    }

    /// Build one requiring `bot.net` from the `target` its spec names.
    pub(crate) fn net_from_target(target: &str) -> Result<Source, BotError> {
        Ok(Source::ordered(Self::with_caps(
            parse_count(target)?,
            vec![Cap::net()],
        )))
    }
}

impl Observe for Counter {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&self.caps)?;
        record(format!("poll:{}", self.domain_id()));
        Ok(self.value)
    }

    fn domain_id(&self) -> &str {
        "test::counter"
    }
}

/// An action that records the value it was handed, tagged by its target.
struct Page {
    /// A label the target carried, so two pages in a chain are distinguishable.
    label: String,
    /// The capabilities this instance requires.
    caps: Vec<Cap>,
}

impl Page {
    /// Build a cap-free one directly, for a native bot.
    fn new(label: &str) -> Self {
        Self::with_caps(label, Vec::new())
    }

    /// Build one with the given capabilities directly, for a native bot.
    fn with_caps(label: &str, caps: Vec<Cap>) -> Self {
        Self {
            label: label.to_owned(),
            caps,
        }
    }

    /// Build a cap-free one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Action, BotError> {
        Ok(Action::new(Self::with_caps(target, Vec::new())))
    }

    /// Build one requiring `bot.net` from the `target` its spec names.
    fn net_from_target(target: &str) -> Result<Action, BotError> {
        Ok(Action::new(Self::with_caps(target, vec![Cap::net()])))
    }
}

impl Execute for Page {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&self.caps)?;
        record(format!(
            "execute:{}:{}:{}",
            self.domain_id(),
            self.label,
            call.1
        ));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::page"
    }
}

domains! {
    /// The domains these tests run: two sources and two actions, one of each
    /// requiring `bot.net`.
    pub SPEC_DOMAINS {
        observe {
            "test::counter" => Counter::from_target,
            "test::net_counter" => Counter::net_from_target,
        }
        execute {
            "test::page" => Page::from_target,
            "test::net_page" => Page::net_from_target,
        }
    }
}

domains! {
    /// A deliberately duplicated registry: two source constructors share one
    /// identifier, so admission must refuse it rather than let declaration order
    /// pick a winner.
    pub DUPLICATE_DOMAINS {
        observe {
            "test::counter" => Counter::from_target,
            "test::counter" => Counter::net_from_target,
        }
        execute {}
    }
}

// ── Scopes ─────────────────────────────────────────────────────────────────

/// The flow revision every test scope uses.
const FLOW: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// A fresh effect scope over an in-memory journal.
fn scope() -> TestResult<EffectScope> {
    scope_for_run("0102030405060708090a0b0c0d0e0f10")
}

/// A fresh effect scope whose run is named by `run_hex`.
fn scope_for_run(run_hex: &str) -> TestResult<EffectScope> {
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex(run_hex)?,
            environment,
            FlowRevision::from_tagged("blake3_256", FLOW)?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    ))
}

// ── Materializing, tick, and native builders ───────────────────────────────

/// Materialize a spec from JSON against [`SPEC_DOMAINS`] under `grants`.
fn materialize(json: &str, grants: &GrantSet) -> TestResult<Bot> {
    let spec = BotSpec::from_json(json)?;
    Ok(Bot::from_spec(&spec, &SPEC_DOMAINS, grants, scope()?)?)
}

/// Materialize under `grants`, returning the admission refusal unopened so a
/// test can inspect the `NeedSet`.
fn admit(json: &str, grants: &GrantSet, effects: EffectScope) -> Result<Bot, Admission> {
    match BotSpec::from_json(json) {
        Ok(spec) => Bot::from_spec(&spec, &SPEC_DOMAINS, grants, effects),
        Err(cause) => Err(Admission::Refused(cause)),
    }
}

/// Tick a bot once and return the fired count together with this thread's trace.
fn tick_and_trace(bot: &mut Bot) -> TestResult<Run> {
    let _ = take_trace();
    let fired = bot.tick()?;
    Ok((fired, take_trace()))
}

/// A native bot over one `test::counter` chain.
fn native_one<C>(value: u16, condition: C, action: Page) -> TestResult<Bot>
where
    C: Evaluate<u16> + 'static,
{
    Ok(Bot::builder("native")
        .observe(Counter::new(value))
        .on(condition, action)
        .with_effects(scope()?)
        .build(&GrantSet::empty())?)
}

/// A native bot over one chain with two always-firing entries.
fn native_two_entries(first: Page, second: Page) -> TestResult<Bot> {
    Ok(Bot::builder("native")
        .observe(Counter::new(3))
        .on(|_: &u16| true, first)
        .on(|_: &u16| true, second)
        .with_effects(scope()?)
        .build(&GrantSet::empty())?)
}

/// A native bot over two always-firing chains.
fn native_two_chains(first: Page, second: Page) -> TestResult<Bot> {
    Ok(Bot::builder("native")
        .observe(Counter::new(3))
        .on(|_: &u16| true, first)
        .observe(Counter::new(4))
        .on(|_: &u16| true, second)
        .with_effects(scope()?)
        .build(&GrantSet::empty())?)
}

/// Tick `native` once and the JSON spec once, under no grants.
fn both_runs(native: Bot, json: &str) -> TestResult<(Run, Run)> {
    both_runs_with(native, json, &GrantSet::empty())
}

/// Tick `native` once and the JSON spec once under `grants`, returning both
/// traces.
fn both_runs_with(native: Bot, json: &str, grants: &GrantSet) -> TestResult<(Run, Run)> {
    let mut native = native;
    let native_run = tick_and_trace(&mut native)?;
    let mut made = materialize(json, grants)?;
    let made_run = tick_and_trace(&mut made)?;
    Ok((native_run, made_run))
}

// ── T25: native and materialized traces agree ──────────────────────────────

#[test]
fn one_chain_traces_match() -> TestResult<()> {
    let native = native_one(3, |_: &u16| true, Page::new("a"))?;
    let (native_run, made_run) = both_runs(
        native,
        r#"{"version":1,"name":"made","chains":[
            {"source":"test::counter","target":"3",
             "on":[["always",{"domain":"test::page","target":"a"}]]}]}"#,
    )?;
    assert_eq!(
        native_run, made_run,
        "a materialized bot must poll and execute in the same order as a native one"
    );
    assert_eq!(made_run.0, 1, "the one chain fires once");
    Ok(())
}

#[test]
fn two_entries_in_one_chain_trace_in_declaration_order() -> TestResult<()> {
    let native = native_two_entries(Page::new("a"), Page::new("b"))?;
    let (native_run, made_run) = both_runs(
        native,
        r#"{"version":1,"name":"made","chains":[
            {"source":"test::counter","target":"3","on":[
                ["always",{"domain":"test::page","target":"a"}],
                ["always",{"domain":"test::page","target":"b"}]]}]}"#,
    )?;
    assert_eq!(
        native_run, made_run,
        "entry order must survive materialization"
    );
    assert_eq!(made_run.0, 2, "both entries fire");
    Ok(())
}

#[test]
fn two_chains_poll_in_declaration_order() -> TestResult<()> {
    let native = native_two_chains(Page::new("a"), Page::new("b"))?;
    let (native_run, made_run) = both_runs(
        native,
        r#"{"version":1,"name":"made","chains":[
            {"source":"test::counter","target":"3",
             "on":[["always",{"domain":"test::page","target":"a"}]]},
            {"source":"test::counter","target":"4",
             "on":[["always",{"domain":"test::page","target":"b"}]]}]}"#,
    )?;
    assert_eq!(
        native_run, made_run,
        "two chains must poll and fire in the same order on both paths"
    );
    assert_eq!(made_run.0, 2, "each chain fires its one entry");
    Ok(())
}

#[test]
fn changed_condition_materializes_identically() -> TestResult<()> {
    let json = r#"{"version":1,"name":"made","chains":[
        {"source":"test::counter","target":"3",
         "on":[["changed",{"domain":"test::page","target":"a"}]]}]}"#;

    let mut native = native_one(3, Changed::<u16>::new(), Page::new("a"))?;
    let native_first = tick_and_trace(&mut native)?;
    let native_second = tick_and_trace(&mut native)?;
    let mut made = materialize(json, &GrantSet::empty())?;
    let made_first = tick_and_trace(&mut made)?;
    let made_second = tick_and_trace(&mut made)?;

    assert_eq!(
        native_first, made_first,
        "the first tick of a `changed` chain must match"
    );
    assert_eq!(
        native_second, made_second,
        "a `changed` condition must hold its state the same way on both paths"
    );
    assert_eq!(native_first.0, 1, "the first tick fires once");
    assert_eq!(
        native_second.0, 0,
        "a source that holds still fires nothing on the second tick"
    );
    Ok(())
}

#[test]
fn threshold_condition_materializes_identically() -> TestResult<()> {
    let native = native_one(7, Above::<u16>::new(5), Page::new("a"))?;
    let (native_run, made_run) = both_runs(
        native,
        r#"{"version":1,"name":"made","chains":[
            {"source":"test::counter","target":"7",
             "on":[["threshold::above(5)",{"domain":"test::page","target":"a"}]]}]}"#,
    )?;
    assert_eq!(
        native_run, made_run,
        "a parameterized threshold must materialize to the same predicate"
    );
    assert_eq!(made_run.0, 1, "7 is above 5, so the entry fires");
    Ok(())
}

#[test]
fn capability_requiring_chain_materializes_under_the_same_grants() -> TestResult<()> {
    let grants = GrantSet::empty().grant(Cap::net());
    let native = Bot::builder("native")
        .observe(Counter::with_caps(3, vec![Cap::net()]))
        .on(|_: &u16| true, Page::new("a"))
        .with_effects(scope()?)
        .build(&grants)?;
    let (native_run, made_run) = both_runs_with(
        native,
        r#"{"version":1,"name":"made","chains":[
            {"source":"test::net_counter","target":"3",
             "on":[["always",{"domain":"test::page","target":"a"}]]}]}"#,
        &grants,
    )?;
    assert_eq!(
        native_run, made_run,
        "authority comes from the grant set on both paths, so the traces agree"
    );
    Ok(())
}

// ── T23: one complete NeedSet, and nothing runs ────────────────────────────

#[test]
fn admission_reports_all_five_needs_in_one_set() -> TestResult<()> {
    let _ = take_trace();
    let refusal = admit(
        r#"{"version":1,"name":"needs","chains":[
            {"source":"unknown::one","target":"x",
             "on":[["always",{"domain":"test::page","target":"a"}]]},
            {"source":"unknown::two","target":"x",
             "on":[["always",{"domain":"test::page","target":"a"}]]},
            {"source":"test::counter","target":"3",
             "on":[["always",{"domain":"unknown::action","target":"a"}]]},
            {"source":"test::counter","target":"not-a-count",
             "on":[["always",{"domain":"test::page","target":"a"}]]},
            {"source":"test::net_counter","target":"3",
             "on":[["always",{"domain":"test::page","target":"a"}]]}]}"#,
        &GrantSet::empty(),
        scope()?,
    );

    let Err(Admission::Needs(needs)) = refusal else {
        return Err(format!("expected a NeedSet, got {refusal:?}").into());
    };

    assert_eq!(
        needs.needs(),
        [
            Need::UnknownSource {
                chain: 0,
                domain: "unknown::one".into(),
            },
            Need::UnknownSource {
                chain: 1,
                domain: "unknown::two".into(),
            },
            Need::UnknownAction {
                chain: 2,
                action: 0,
                domain: "unknown::action".into(),
            },
            Need::SourceTargetRejected {
                chain: 3,
                domain: "test::counter".into(),
                // Both halves, because the constructor reports both: the field
                // that is missing and the parse failure that made it missing.
                cause: "incomplete bot spec: missing target: invalid digit found in string".into(),
            },
            Need::MissingCapability {
                chain: 4,
                action: None,
                domain: "test::net_counter".into(),
                capability: Cap::net(),
            },
        ],
        "one refusal must carry all five needs, attributed and in declaration order"
    );
    assert_eq!(
        needs.len(),
        5,
        "no more and no fewer than the five injected"
    );

    assert!(
        take_trace().is_empty(),
        "a refused materialization must poll nothing and execute nothing"
    );
    Ok(())
}

#[test]
fn proposed_grants_closes_the_capability_need_without_granting() -> TestResult<()> {
    let json = r#"{"version":1,"name":"needs","chains":[
        {"source":"test::net_counter","target":"3",
         "on":[["always",{"domain":"test::net_page","target":"a"}]]}]}"#;
    let refusal = admit(json, &GrantSet::empty(), scope()?);
    let Err(Admission::Needs(needs)) = refusal else {
        return Err(format!("expected a NeedSet, got {refusal:?}").into());
    };
    assert_eq!(needs.len(), 2, "source and action each need `bot.net`");

    let proposal = needs.proposed_grants();
    assert!(
        proposal.grants(&Cap::net()),
        "the proposal must name the capability that closes the need"
    );
    // The proposal is data; the materializer still refuses the same spec, so it
    // was never a grant.
    let still = admit(json, &GrantSet::empty(), scope()?);
    assert!(
        matches!(still, Err(Admission::Needs(_))),
        "a repair proposal must not admit the bot on its own: {still:?}"
    );
    Ok(())
}

#[test]
fn unknown_condition_is_a_need_not_an_inert_gate() -> TestResult<()> {
    let refusal = admit(
        r#"{"version":1,"name":"cond","chains":[
            {"source":"test::counter","target":"3",
             "on":[["mystery::gate",{"domain":"test::page","target":"a"}]]}]}"#,
        &GrantSet::empty(),
        scope()?,
    );
    let Err(Admission::Needs(needs)) = refusal else {
        return Err(format!("expected a NeedSet, got {refusal:?}").into());
    };
    assert_eq!(
        needs.needs(),
        [Need::UnknownCondition {
            chain: 0,
            action: 0,
            condition: "mystery::gate".into(),
        }],
        "an unknown condition must be reported, never treated as always-true"
    );
    Ok(())
}

// ── Registry and shape refusals ────────────────────────────────────────────

#[test]
fn a_duplicate_registry_is_refused_naming_the_identifier() -> TestResult<()> {
    let spec = BotSpec::from_json(
        r#"{"version":1,"name":"dup","chains":[
            {"source":"test::counter","target":"3",
             "on":[["always",{"domain":"test::page","target":"a"}]]}]}"#,
    )?;
    let refusal = Bot::from_spec(&spec, &DUPLICATE_DOMAINS, &GrantSet::empty(), scope()?);

    match refusal {
        Err(Admission::Refused(BotError::DuplicateDomain {
            domain,
            role,
            first,
            second,
        })) => {
            assert_eq!(domain, "test::counter");
            assert_eq!(role, "source");
            assert_eq!((first, second), (0, 1));
            Ok(())
        }
        other => Err(format!("expected a duplicate-domain refusal, got {other:?}").into()),
    }
}

#[test]
fn malformed_json_is_a_typed_error() {
    assert!(
        matches!(
            BotSpec::from_json("{ this is not json"),
            Err(BotError::MalformedSpec { .. })
        ),
        "malformed JSON must be a typed refusal, never a panic"
    );
}

#[test]
fn unsupported_version_is_a_typed_error() {
    let refusal = BotSpec::from_json(r#"{"version":999,"name":"x","chains":[]}"#);
    assert!(
        matches!(
            refusal,
            Err(BotError::UnsupportedSpecVersion {
                found: 999,
                supported: 1
            })
        ),
        "a version this build does not implement must be refused: {refusal:?}"
    );
}

#[test]
fn empty_chains_are_refused_by_the_materializer() -> TestResult<()> {
    let spec = BotSpec::from_json(r#"{"version":1,"name":"empty","chains":[]}"#)?;
    let refusal = Bot::from_spec(&spec, &SPEC_DOMAINS, &GrantSet::empty(), scope()?);
    assert!(
        matches!(
            refusal,
            Err(Admission::Refused(BotError::IncompleteSpec {
                field: "chains",
                ..
            }))
        ),
        "a materialized bot with no chains is a refused document: {refusal:?}"
    );
    Ok(())
}

#[test]
fn a_spec_cannot_grant_itself_authority() -> TestResult<()> {
    // The spec names a `bot.net` source, but the caller holds no grant. The
    // source's identifier buys no reach.
    let refusal = admit(
        r#"{"version":1,"name":"self-grant","chains":[
            {"source":"test::net_counter","target":"3",
             "on":[["always",{"domain":"test::page","target":"a"}]]}]}"#,
        &GrantSet::empty(),
        scope()?,
    );
    assert!(
        matches!(refusal, Err(Admission::Needs(_))),
        "naming a domain must not grant the capability it needs: {refusal:?}"
    );
    Ok(())
}

// ── Concurrency: independent materializations ──────────────────────────────

/// The spec every concurrent materialization builds.
#[cfg(feature = "rt")]
const CONCURRENT_SPEC: &str = r#"{"version":1,"name":"concurrent","chains":[
    {"source":"test::counter","target":"3",
     "on":[["always",{"domain":"test::page","target":"a"}]]}]}"#;

#[cfg(feature = "rt")]
#[test]
fn a_thousand_materializations_across_tenants_are_independent() -> TestResult<()> {
    let runtime = lgwks_bot::Runtime::new()?;

    // Each job owns its run identity and its scope, and builds, ticks and drops
    // its bot entirely on its own thread. Nothing is shared, so nothing can
    // collide; the assertion is that every one of a thousand independent runs
    // reaches the same answer.
    let jobs: Vec<_> = (0..1000_u32)
        .map(|index| {
            lgwks_std::task::spawn_blocking(move || -> Result<usize, String> {
                let spec =
                    BotSpec::from_json(CONCURRENT_SPEC).map_err(|error| error.to_string())?;
                // One-based: a zero run identifier is refused, and every job
                // must still get its own distinct identity.
                let run_hex = format!("{:032x}", index.saturating_add(1));
                let effects = scope_for_run(&run_hex).map_err(|error| error.to_string())?;
                let mut bot = Bot::from_spec(&spec, &SPEC_DOMAINS, &GrantSet::empty(), effects)
                    .map_err(|error| error.to_string())?;
                assert_eq!(bot.name(), "concurrent", "the name must not be shared");
                assert_eq!(
                    bot.source_domains().len(),
                    1,
                    "each bot keeps its own single chain"
                );
                bot.tick().map_err(|error| error.to_string())
            })
        })
        .collect();

    let fired = runtime.block_on(async {
        let mut counts = Vec::with_capacity(jobs.len());
        for job in jobs {
            counts.push(job.await);
        }
        counts
    });

    assert_eq!(fired.len(), 1000, "every job must be collected");
    for (index, outcome) in fired.iter().enumerate() {
        assert_eq!(
            outcome.as_ref().ok(),
            Some(&1_usize),
            "materialization {index} was not independent of the others: {outcome:?}"
        );
    }
    Ok(())
}

#[test]
fn need_set_helpers_are_total() {
    let empty = NeedSet::new(Vec::new());
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    let one = NeedSet::new(vec![Need::UnknownSource {
        chain: 2,
        domain: "x".into(),
    }]);
    assert_eq!(one.needs()[0].chain(), 2);
    assert_eq!(one.needs()[0].action(), None);
}

// ── A source whose output is a struct ──────────────────────────────────────

/// A structured observation: not ordered and not parseable from text, which is
/// the common shape of a real domain's state (`sys::ProcessState`, a PR status).
#[derive(Clone, Debug, PartialEq)]
struct Reading {
    /// What was read.
    label: String,
}

impl lgwks_bot::effect::InputIdentity for Reading {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.test.reading";

    fn write_identity(&self, hasher: &mut lgwks_std::hash::Hasher) {
        hasher.write_framed(self.label.as_bytes());
    }
}

/// A source producing [`Reading`]s, registered through plain `Source::new`.
struct Reader(String);

impl Reader {
    /// Build one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Source, BotError> {
        Ok(Source::new(Self(target.to_owned())))
    }
}

impl Observe for Reader {
    type Output = Reading;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<Reading, BotError> {
        call.0.check(&[])?;
        record(format!("poll:{}", self.domain_id()));
        Ok(Reading {
            label: self.0.clone(),
        })
    }

    fn domain_id(&self) -> &str {
        "test::reader"
    }
}

/// An action that takes a [`Reading`].
struct Note;

impl Note {
    /// Build one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Action, BotError> {
        let _ = target;
        Ok(Action::new(Self))
    }
}

impl Execute for Note {
    type Input = Reading;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &Reading)) -> Result<(), BotError> {
        call.0.check(&[])?;
        record(format!("execute:{}:{}", self.domain_id(), call.1.label));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::note"
    }
}

domains! {
    /// A registry holding a struct-output source.
    pub STRUCT_DOMAINS {
        observe { "test::reader" => Reader::from_target, }
        execute { "test::note" => Note::from_target, }
    }
}

/// Materialize against [`STRUCT_DOMAINS`].
fn admit_struct(json: &str) -> TestResult<Result<Bot, Admission>> {
    let spec = BotSpec::from_json(json)?;
    Ok(Bot::from_spec(
        &spec,
        &STRUCT_DOMAINS,
        &GrantSet::empty(),
        scope()?,
    ))
}

#[test]
fn a_struct_output_source_registers_and_answers_the_order_free_conditions() -> TestResult<()> {
    let mut bot = admit_struct(
        r#"{"version":1,"name":"struct","chains":[
            {"source":"test::reader","target":"r",
             "on":[["changed",{"domain":"test::note","target":""}]]}]}"#,
    )??;
    let first = tick_and_trace(&mut bot)?;
    let second = tick_and_trace(&mut bot)?;
    assert_eq!(
        first,
        (
            1,
            vec![
                "poll:test::reader".to_owned(),
                "execute:test::note:r".to_owned()
            ]
        ),
        "a struct output needs no ordering to be observed and to fire on change"
    );
    assert_eq!(second.0, 0, "an unchanged struct does not fire again");
    Ok(())
}

#[test]
fn a_threshold_on_an_unordered_source_is_an_unknown_condition_need() -> TestResult<()> {
    let refusal = admit_struct(
        r#"{"version":1,"name":"struct","chains":[
            {"source":"test::reader","target":"r",
             "on":[["threshold::above(1)",{"domain":"test::note","target":""}]]}]}"#,
    )?;
    let Err(Admission::Needs(needs)) = refusal else {
        return Err(format!("expected a NeedSet, got {refusal:?}").into());
    };
    assert_eq!(
        needs.needs(),
        [Need::UnknownCondition {
            chain: 0,
            action: 0,
            condition: "threshold::above(1)".into(),
        }],
        "a source that is not ordered has no threshold vocabulary"
    );
    Ok(())
}
