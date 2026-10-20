// File: crates/lgwks-bot/examples/open_issue.rs (rust)
//! Open a GitHub issue from a `script!` flow, once per key.
//!
//! ```text
//! # the write is granted by name; without it the run is refused naming it
//! cargo run -p lgwks_bot --features process,ephemeral --example open_issue -- \
//!     acme/widgets harness-2026-10-20-1 "Title" --body "Body" --grant github.issue.create
//!
//! # the same key again opens nothing: it prints the first run's issue
//! open_issue acme/widgets harness-2026-10-20-1 "Title" --grant github.issue.create --json
//! ```
//!
//! The body is the flow below and nothing else: one `act` line. The host
//! grants running `gh` and reaching the network; the write, the capability
//! `github.issue.create`, only when the operator names it with `--grant`. The
//! key is what makes a rerun safe: the action reads GitHub back for it before
//! it creates — the listing, then every issue number past it — so a rerun
//! after a crash or a lost answer finds the first run's issue without waiting
//! for GitHub's listing to catch up. A new key costs three `gh` calls: the
//! listing, one number read and the create.
//!
//! Exit status: 0 an issue is open; 2 refused before anything was written;
//! 3 a write may or may not have landed (rerun with the same key to settle
//! it); 1 anything else, a create GitHub rejected included. `gh` and its
//! credentials are the operator's; this program never reads a token.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use lgwks_bot::domain::gh::{ISSUE_CREATE, IssueDraft, OpenIssue, OpenedIssue};
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, task};
use lgwks_bot::{BotError, Cap, GrantSet, domains};
use lgwks_std::json::{self, Serialize};

/// What the command line takes.
const USAGE: &str = "usage: open_issue <owner/repo> <key> <title> [--body TEXT | --body-file PATH] \
                     [--grant CAPABILITY]... [--json]";

/// The most bytes a body file may hold before it is read: four bytes for each
/// of GitHub's 65,536 characters.
const MAX_BODY_FILE_BYTES: u64 = 4 * 65_536;

domains! {
    /// The one domain this command reaches.
    pub(crate) ISSUES {
        observe {}
        execute {
            "gh::issue" => OpenIssue::from_target,
        }
    }
}

lgwks_bot::script! {
    /// Open `draft` on `repo`, once for its key.
    flow open_issue(repo: &str, draft: IssueDraft) -> OpenedIssue:
        let opened = act gh::issue on repo with draft
        give back opened.into_output::<OpenedIssue>()?
}

/// The parsed command line.
struct Args {
    /// `owner/repo`.
    repo: String,
    /// The caller key.
    key: String,
    /// The title.
    title: String,
    /// The body, possibly empty.
    body: String,
    /// The capabilities the operator granted beyond running `gh`.
    grants: Vec<String>,
    /// Whether to answer as one JSON object.
    json: bool,
}

/// The answer as one JSON object, for an agent.
#[derive(Serialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct Answer<'run> {
    /// `opened`, `refused`, `indeterminate` or `failed`.
    outcome: &'run str,
    /// The issue number, when one is open.
    number: Option<u64>,
    /// The issue page, when one is open.
    url: Option<&'run str>,
    /// `created`, `found` or `reconciled`, when one is open.
    settlement: Option<&'run str>,
    /// What went wrong, when nothing is open.
    error: Option<String>,
}

fn main() -> ExitCode {
    let mut err = std::io::stderr().lock();
    match run() {
        Ok(code) => code,
        Err(failure) => {
            // A closed stderr leaves nothing else to tell.
            let _unreported = writeln!(err, "open_issue: {failure}\n{USAGE}");
            ExitCode::from(1)
        }
    }
}

/// Parse, run the flow once, report.
fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let args = parse(std::env::args().skip(1))?;
    let draft = IssueDraft::new(args.key.as_str(), args.title.as_str(), args.body.as_str())?;
    let grants = args.grants.iter().fold(
        GrantSet::empty().grant(Cap::sys()).grant(Cap::net()),
        |granted, name| granted.grant(Cap::new(name.clone())),
    );
    let host = Host::builder("harness")?
        .domains(&ISSUES)
        .grants(grants)
        .default_deadline(Duration::from_secs(120))
        .build()?;
    let opening = task(
        "open-issue",
        |scope: Scope, (repo, draft): (String, IssueDraft)| async move {
            open_issue(&scope, &repo, draft).await
        },
    )?;
    let report = lgwks_bot::block_on(host.run(&opening, (args.repo.clone(), draft)));
    let mut out = std::io::stdout().lock();
    let (answer, code) = match report.result() {
        Ok(issue) => (
            Answer {
                outcome: "opened",
                number: Some(issue.number()),
                url: Some(issue.url()),
                settlement: Some(issue.settlement().as_str()),
                error: None,
            },
            0,
        ),
        Err(failure) => {
            let (outcome, code) = classify(failure);
            (
                Answer {
                    outcome,
                    number: None,
                    url: None,
                    settlement: None,
                    error: Some(failure.to_string()),
                },
                code,
            )
        }
    };
    if args.json {
        writeln!(out, "{}", json::to_string(&answer)?)?;
    } else if let (Some(url), Some(settlement)) = (answer.url, answer.settlement) {
        writeln!(out, "{settlement} {url}")?;
    } else if let Some(ref error) = answer.error {
        writeln!(out, "{}: {error}", answer.outcome)?;
        if answer.outcome == "refused" && !args.grants.iter().any(|name| name == ISSUE_CREATE) {
            writeln!(
                out,
                "  to allow the write, rerun with --grant {ISSUE_CREATE}"
            )?;
        }
    }
    Ok(ExitCode::from(code))
}

/// The outcome word and exit status a failed run reports.
fn classify(failure: &FlowError) -> (&'static str, u8) {
    match *failure {
        FlowError::Blocked { .. } => ("refused", 2),
        FlowError::Bot { ref source, .. } => match **source {
            BotError::EffectIndeterminate { .. } => ("indeterminate", 3),
            BotError::DomainError { .. } | BotError::CredentialRejected { .. } => ("failed", 1),
            _ => ("refused", 2),
        },
        _ => ("failed", 1),
    }
}

/// Read the command line, refusing anything it does not name.
fn parse(mut words: impl Iterator<Item = String>) -> Result<Args, Box<dyn std::error::Error>> {
    let (Some(repo), Some(key), Some(title)) = (words.next(), words.next(), words.next()) else {
        lgwks_std::trace::debug!("open_issue: a positional argument is missing");
        return Err("owner/repo, key and title are required".into());
    };
    let mut args = Args {
        repo,
        key,
        title,
        body: String::new(),
        grants: Vec::new(),
        json: false,
    };
    while let Some(word) = words.next() {
        match word.as_str() {
            "--json" => args.json = true,
            "--body" => args.body = words.next().ok_or("--body takes the text")?,
            "--body-file" => {
                let path = words.next().ok_or("--body-file takes a path")?;
                if std::fs::metadata(&path)?.len() > MAX_BODY_FILE_BYTES {
                    lgwks_std::trace::debug!(path = %path, "open_issue: the body file is over its bound");
                    return Err(format!("{path} is larger than any issue body GitHub holds").into());
                }
                args.body = std::fs::read_to_string(&path)?;
            }
            "--grant" => args
                .grants
                .push(words.next().ok_or("--grant takes a capability")?),
            other => {
                lgwks_std::trace::debug!(argument = other, "open_issue: an unknown argument");
                return Err(format!("{other:?} is not an argument this command takes").into());
            }
        }
    }
    Ok(args)
}
