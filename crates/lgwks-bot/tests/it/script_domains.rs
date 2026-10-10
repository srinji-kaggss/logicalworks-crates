//! `observe` and `act`: a registry domain called from a `script!` flow (#388).
//!
//! The flows here are expanded by the real macro and run by a real `Host`
//! whose registry is a real `domains!` static, the same kind of static a
//! `BotSpec` materializes against. The domains share `spec_materialize`'s
//! thread-local recorder, so what a flow polled and executed reads back as the
//! same strings a spec's tick leaves (`poll:test::counter`,
//! `execute:test::page:a:3`); `sim_script_domains` holds the two traces equal
//! over a seeded sweep.
//!
//! The issue's done-when, one test each:
//!
//! | Claim | Test |
//! |---|---|
//! | `observe github::pr_status of "owner/repo"` compiles and runs against the stub | `a_flow_observes_the_stub_pull_request_domain` |
//! | an unknown identifier names the registry and the nearest declared one | `an_unknown_identifier_names_the_registry_and_the_nearest` |
//! | the trace is the `BotSpec` path's | `the_flow_and_the_spec_leave_one_trace` (and the sim) |

#![cfg(feature = "script")]

use std::error::Error;
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope, StepKind};
use lgwks_bot::spec::{Bot, BotSpec};
use lgwks_bot::task::{Host, Report, task};
use lgwks_bot::{
    Action, Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, Need, Observe, Source, domains,
};

use crate::effects::memory_scope;
use crate::spec_materialize::{Counter, Page, record, take_trace};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

// ── Domains ────────────────────────────────────────────────────────────────

/// The open pull-request count the stub reports for each repository it knows.
const OPEN_PULL_REQUESTS: [(&str, u16); 2] = [("acme/widgets", 4), ("acme/gears", 0)];

/// A stub of a GitHub pull-request status source: the count of open pull
/// requests on the `owner/repo` its target names, from a fixed table.
///
/// A stub rather than the `gh` binding because what is under test is the
/// word's path to a registry constructor, not GitHub; the real binding's own
/// contract is `gh_binding`'s.
struct PullRequestStatus {
    /// `owner/repo`, as the target named it.
    repository: String,
}

impl PullRequestStatus {
    /// Build from an `owner/repo` target, refusing any other shape.
    fn from_target(target: &str) -> Result<Source, BotError> {
        match target.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Source::ordered(Self {
                    repository: target.to_owned(),
                }))
            }
            _ => Err(BotError::IncompleteSpec {
                field: "target",
                cause: String::from("a repository is named as owner/repo"),
            }),
        }
    }
}

impl Observe for PullRequestStatus {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&[])?;
        record(format!("poll:{}:{}", self.domain_id(), self.repository));
        Ok(OPEN_PULL_REQUESTS
            .iter()
            .find(|&&(known, _)| known == self.repository)
            .map_or(0, |&(_, open)| open))
    }

    fn domain_id(&self) -> &str {
        "github::pr_status"
    }
}

/// An action whose effect leaves the process, which `act` must refuse before
/// it runs; it records itself if it ever does.
struct Post;

impl Post {
    /// Build from any target.
    fn from_target(_target: &str) -> Result<Action, BotError> {
        Ok(Action::new(Self))
    }
}

impl Execute for Post {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::External
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(&[])?;
        record(format!("execute:{}:{}", self.domain_id(), call.1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::post"
    }
}

domains! {
    /// What these flows name: the stub, the spec oracle's counter and page,
    /// a counter that needs `bot.net`, and an external action.
    pub(crate) SCRIPT_DOMAINS {
        observe {
            "github::pr_status" => PullRequestStatus::from_target,
            "test::counter" => Counter::from_target,
            "test::net_counter" => Counter::net_from_target,
        }
        execute {
            "test::page" => Page::from_target,
            "test::post" => Post::from_target,
        }
    }
}

// ── Flows ──────────────────────────────────────────────────────────────────

lgwks_bot::script! {
    /// How many pull requests are open on `repo`.
    flow open_pull_requests(repo: &str) -> u16:
        let open: u16 = observe github::pr_status of repo
        give back open

    /// Page `label` with the count `target` reads as, when it is over `over`.
    pub(crate) flow page_when_over(target: &str, over: u16, labels: &[String]) -> u16:
        let count: u16 = observe test::counter of target
        if count > over:
            for label in labels:
                act test::page on label with count
        give back count

    /// A page that would run first, then a misspelled source.
    pub(crate) flow page_then_misspelled_source(target: &str) -> u16:
        act test::page on "first" with 1_u16
        let count: u16 = observe test::countr of target
        give back count

    /// A misspelled source and a misspelled action in one flow.
    pub(crate) flow two_misspellings(target: &str) -> u16:
        let count: u16 = observe test::countr of target
        act test::pager on "first" with count
        give back count

    /// A counter read as text, which it is not.
    flow counter_as_text(target: &str) -> String:
        let text: String = observe test::counter of target
        give back text

    /// An action whose effect leaves the process.
    pub(crate) flow post(target: &str, value: u16):
        act test::post on target with value

    /// A counter that needs `bot.net`.
    pub(crate) flow reach(target: &str) -> u16:
        let count: u16 = observe test::net_counter of target
        give back count

    /// A page whose output is asked for as `T`.
    flow page_output(label: &str) -> ():
        let paged = act test::page on label with 5_u16
        give back paged.into_output::<()>()?

    /// A page whose output is asked for as the wrong type.
    flow page_output_as_count(label: &str) -> u16:
        let paged = act test::page on label with 5_u16
        give back paged.into_output::<u16>()?
}

// ── Hosts and runs ─────────────────────────────────────────────────────────

/// A host for `tenant` resolving against [`SCRIPT_DOMAINS`] under `grants`.
pub(crate) fn host(tenant: &str, grants: GrantSet) -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder(tenant)?
        .domains(&SCRIPT_DOMAINS)
        .grants(grants)
        .default_deadline(Duration::from_secs(30))
        .build()?)
}

/// A host with no registry installed.
fn bare_host() -> Result<Host, Box<dyn Error>> {
    Ok(Host::builder("acme")?
        .default_deadline(Duration::from_secs(30))
        .build()?)
}

/// The `BotError` a flow failed with, or why it did not fail with one.
pub(crate) fn bot_error<O: std::fmt::Debug>(report: &Report<O>) -> Result<&BotError, String> {
    match *refusal(report)? {
        FlowError::Bot { ref source, .. } => Ok(&**source),
        ref other => Err(format!("expected a bot error, got {other:?}")),
    }
}

/// The capabilities a blocked run was short of, or why it was not blocked.
pub(crate) fn blocked_on<O: std::fmt::Debug>(report: &Report<O>) -> Result<Vec<Cap>, String> {
    match *refusal(report)? {
        FlowError::Blocked { ref deficit, .. } => Ok(deficit
            .shortages()
            .map(|short| short.required().clone())
            .collect()),
        ref other => Err(format!("expected a blocked run, got {other:?}")),
    }
}

/// The error a run failed with, or what it returned instead.
fn refusal<O: std::fmt::Debug>(report: &Report<O>) -> Result<&FlowError, String> {
    match report.result() {
        Err(error) => Ok(error),
        Ok(value) => Err(format!("expected a refusal, got {value:?}")),
    }
}

/// The needs of an `UndeclaredDomains` refusal.
pub(crate) fn undeclared<O: std::fmt::Debug>(report: &Report<O>) -> Result<Vec<Need>, String> {
    match *bot_error(report)? {
        BotError::UndeclaredDomains { ref needs } => Ok(needs.needs().to_vec()),
        ref other => Err(format!("expected undeclared domains, got {other:?}")),
    }
}

/// The need an undeclared identifier in `SCRIPT_DOMAINS` reports.
pub(crate) fn unknown_in_registry(role: &'static str, domain: &str, nearest: &str) -> Need {
    Need::UnknownDomain {
        role,
        domain: domain.to_owned(),
        registry: Some(String::from("SCRIPT_DOMAINS")),
        installed: true,
        nearest: Some(nearest.to_owned()),
    }
}

// ── The issue's done-when ──────────────────────────────────────────────────

#[test]
fn a_flow_observes_the_stub_pull_request_domain() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let checking = task("checking", |scope: Scope, repo: &'static str| async move {
        open_pull_requests(&scope, repo).await
    })?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&checking, "acme/widgets"));
    assert_eq!(report.result().ok().copied(), Some(4));
    assert_eq!(take_trace(), ["poll:github::pr_status:acme/widgets"]);

    // The step is the script's own line, entered under its identifier's last
    // segment, so the trail reads back as the word that ran.
    let step = report
        .trail()
        .iter()
        .find(|entry| entry.path().ends_with("/pr_status"))
        .ok_or("the observation is a step of its own")?;
    let site = step.site().ok_or("a script line entered the observation")?;
    assert_eq!(site.kind(), StepKind::Observe);
    assert_eq!(site.text(), "observe github::pr_status of repo");
    Ok(())
}

#[test]
fn an_unknown_identifier_names_the_registry_and_the_nearest() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let checking = task(
        "checking",
        |scope: Scope, target: &'static str| async move { two_misspellings(&scope, target).await },
    )?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&checking, "3"));
    assert_eq!(
        undeclared(&report)?,
        [
            unknown_in_registry("source", "test::countr", "test::counter"),
            unknown_in_registry("action", "test::pager", "test::page"),
        ],
        "both unknown identifiers at once, each with its registry and nearest"
    );
    assert!(take_trace().is_empty(), "nothing polled or executed");
    let rendered = bot_error(&report)?.to_string();
    assert!(
        rendered.contains(
            "source `test::countr` is not declared in registry `SCRIPT_DOMAINS`; the nearest declared is `test::counter`"
        ),
        "the refusal reads as its repair: {rendered}"
    );
    Ok(())
}

#[test]
fn the_flow_and_the_spec_leave_one_trace() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let paging = task("paging", |scope: Scope, labels: Vec<String>| async move {
        page_when_over(&scope, "3", 2, &labels).await
    })?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&paging, vec![String::from("a")]));
    assert_eq!(report.result().ok().copied(), Some(3));
    let script_trace = take_trace();

    let spec = BotSpec::from_json(
        r#"{"version":1,"name":"made","chains":[
            {"source":"test::counter","target":"3",
             "on":[["threshold::above(2)",{"domain":"test::page","target":"a"}]]}]}"#,
    )?;
    let mut bot = Bot::from_spec(&spec, &SCRIPT_DOMAINS, &GrantSet::empty(), memory_scope()?)?;
    bot.tick()?;
    assert_eq!(script_trace, take_trace());
    assert_eq!(
        script_trace,
        ["poll:test::counter", "execute:test::page:a:3"]
    );
    Ok(())
}

// ── The other refusals ─────────────────────────────────────────────────────

#[test]
fn admission_runs_before_the_first_step() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let checking = task(
        "checking",
        |scope: Scope, target: &'static str| async move {
            page_then_misspelled_source(&scope, target).await
        },
    )?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&checking, "3"));
    assert_eq!(
        undeclared(&report)?,
        [unknown_in_registry(
            "source",
            "test::countr",
            "test::counter"
        )]
    );
    assert!(
        take_trace().is_empty(),
        "the page written before the misspelling never ran"
    );
    Ok(())
}

#[test]
fn a_host_with_no_registry_refuses_naming_the_absence() -> TestResult {
    let host = bare_host()?;
    let checking = task("checking", |scope: Scope, repo: &'static str| async move {
        open_pull_requests(&scope, repo).await
    })?;
    let report = lgwks_bot::block_on(host.run(&checking, "acme/widgets"));
    assert_eq!(
        undeclared(&report)?,
        [Need::UnknownDomain {
            role: "source",
            domain: String::from("github::pr_status"),
            registry: None,
            installed: false,
            nearest: None,
        }]
    );
    let rendered = bot_error(&report)?.to_string();
    assert!(
        rendered.contains("install one with `HostBuilder::domains`"),
        "the refusal names the repair: {rendered}"
    );
    Ok(())
}

#[test]
fn a_target_the_constructor_refuses_is_its_refusal() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let checking = task("checking", |scope: Scope, repo: &'static str| async move {
        open_pull_requests(&scope, repo).await
    })?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&checking, "not-a-repository"));
    assert!(
        matches!(
            *bot_error(&report)?,
            BotError::IncompleteSpec {
                field: "target",
                ..
            }
        ),
        "{:?}",
        report.result()
    );
    assert!(take_trace().is_empty());
    Ok(())
}

#[test]
fn a_value_bound_as_another_type_is_a_type_mismatch() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let reading = task("reading", |scope: Scope, target: &'static str| async move {
        counter_as_text(&scope, target).await
    })?;
    let report = lgwks_bot::block_on(host.run(&reading, "3"));
    assert!(
        matches!(
            *bot_error(&report)?,
            BotError::TypeMismatch {
                site: "script::observe",
                expected: "alloc::string::String",
                observed: "u16",
                ..
            }
        ),
        "{:?}",
        report.result()
    );
    Ok(())
}

#[test]
fn an_action_output_reads_back_as_its_own_type_only() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let paging = task("paging", |scope: Scope, label: &'static str| async move {
        page_output(&scope, label).await
    })?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&paging, "b"));
    assert!(report.result().is_ok(), "{:?}", report.result());
    assert_eq!(take_trace(), ["execute:test::page:b:5"]);

    let miscounting = task(
        "miscounting",
        |scope: Scope, label: &'static str| async move { page_output_as_count(&scope, label).await },
    )?;
    let report = lgwks_bot::block_on(host.run(&miscounting, "b"));
    assert!(
        matches!(
            *bot_error(&report)?,
            BotError::TypeMismatch {
                site: "script::act",
                expected: "u16",
                ..
            }
        ),
        "{:?}",
        report.result()
    );
    Ok(())
}

#[test]
fn an_external_action_is_refused_naming_the_effect_ledger() -> TestResult {
    let host = host("acme", GrantSet::empty())?;
    let posting = task("posting", |scope: Scope, value: u16| async move {
        post(&scope, "channel", value).await
    })?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host.run(&posting, 7));
    let refused = bot_error(&report)?;
    assert!(
        matches!(*refused, BotError::UnjournaledEffect { ref domain } if domain == "test::post"),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("effect ledger"), "{refused}");
    assert!(take_trace().is_empty(), "the action never ran");
    Ok(())
}

#[test]
fn a_source_short_of_authority_blocks_naming_the_capability() -> TestResult {
    let reaching = task(
        "reaching",
        |scope: Scope, target: &'static str| async move { reach(&scope, target).await },
    )?;
    let _ = take_trace();
    let report = lgwks_bot::block_on(host("acme", GrantSet::empty())?.run(&reaching, "3"));
    assert_eq!(blocked_on(&report)?, [Cap::net()]);
    assert!(take_trace().is_empty(), "the source was never polled");

    let granted = host("acme", GrantSet::empty().grant(Cap::net()))?;
    let report = lgwks_bot::block_on(granted.run(&reaching, "3"));
    assert_eq!(report.result().ok().copied(), Some(3));
    assert_eq!(take_trace(), ["poll:test::counter"]);
    Ok(())
}
