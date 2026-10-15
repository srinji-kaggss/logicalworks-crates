//! The four verbs over the gh binding: a pull-request query and a snapshot source.

use super::{Gh, GhError, GhOutcome, PrSnapshot, PullRequest};
use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

// ── The four verbs over the `gh` binding ────────────────────────────────────

/// The GitHub binding as a query surface: a read-only `gh api` call.
///
/// A `Query` here is a real, bounded, supervised `gh api` invocation whose
/// stdout the caller parses. It exists so the same adapter serves an ordinary
/// caller through the verbs rather than through a private helper, which is what
/// makes a verb-level capability denial observable on the real path.
#[derive(Debug)]
pub struct GhQuery {
    /// The binding every call runs through.
    gh: Gh,
    /// The argument vector after `api`.
    rest: Vec<String>,
    /// Forced by the constructor: the same two capabilities [`Gh`] requires.
    caps: Vec<Cap>,
}

impl GhQuery {
    /// Query `gh` with these arguments.
    #[must_use]
    pub fn new(gh: Gh, rest: impl IntoIterator<Item = String>) -> Self {
        Self {
            caps: gh.required_caps().to_vec(),
            gh,
            rest: rest.into_iter().collect(),
        }
    }
}

impl verb::Query for GhQuery {
    type Input = ();
    type Output = GhOutcome;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<GhOutcome, BotError> {
        // Checked before anything is started, so a denial is a refusal and not
        // a child process that ran and whose output nobody wanted.
        call.0.check(self.required_caps())?;
        self.run().await
    }

    fn domain_id(&self) -> &str {
        "gh::api"
    }
}

impl GhQuery {
    /// The call itself, shared by every build of this surface.
    #[cfg(not(feature = "process"))]
    async fn run(&self) -> Result<GhOutcome, BotError> {
        let rest: Vec<&str> = self.rest.iter().map(String::as_str).collect();
        let args = self.gh.args_for(&rest);
        match self.gh.call(&args).await {
            // Nothing ran, so a retry cannot duplicate anything.
            Err(GhError::NoRunner) | Ok(GhOutcome { .. }) => Err(BotError::DomainError {
                domain: String::from("gh::api"),
                certainty: DispatchCertainty::Refused,
                cause: GhError::NoRunner.to_string(),
            }),
            Err(source) => Err(BotError::DomainError {
                domain: String::from("gh::api"),
                certainty: DispatchCertainty::NotDelivered,
                cause: source.to_string(),
            }),
        }
    }

    /// The call itself, shared by every build of this surface.
    #[cfg(feature = "process")]
    async fn run(&self) -> Result<GhOutcome, BotError> {
        let rest: Vec<&str> = self.rest.iter().map(String::as_str).collect();
        let args = self.gh.args_for(&rest);
        match self.gh.call(&args).await {
            Ok(outcome) => Ok(outcome),
            Err(GhError::Process(crate::rt::process::ProcessRunError::Refused))
            | Err(GhError::Process(crate::rt::process::ProcessRunError::NotStarted { .. })) => {
                // Nothing ran, so a retry cannot duplicate.
                Err(BotError::DomainError {
                    domain: String::from("gh::api"),
                    certainty: DispatchCertainty::Refused,
                    cause: format!("the GitHub client did not start: {}", self.gh.program),
                })
            }
            // The child started: whatever it was about to do may have happened,
            // so this is indeterminate rather than a clean failure.
            Err(GhError::Process(crate::rt::process::ProcessRunError::AfterStart { source })) => {
                Err(BotError::EffectIndeterminate {
                    domain: String::from("gh::api"),
                    cause: format!("the GitHub client started but did not settle: {source}"),
                })
            }
            Err(source) => Err(BotError::DomainError {
                domain: String::from("gh::api"),
                certainty: DispatchCertainty::NotDelivered,
                cause: source.to_string(),
            }),
        }
    }
}

/// The GitHub binding as an observe surface: repeated snapshots of a pull
/// request's head.
///
/// Each poll is one supervised `gh` call, so an observer that fires repeatedly
/// is a bounded sequence of real children rather than a cached value. The state
/// it reports is the head sha, which is the fact a freshness check turns on.
#[derive(Debug)]
pub struct PrSnapshotSource {
    /// The binding every poll runs through.
    gh: Gh,
    /// The pull request observed.
    pull: PullRequest,
    /// Forced by the constructor: the same two capabilities [`Gh`] requires.
    caps: Vec<Cap>,
}

impl PrSnapshotSource {
    /// Observe `pull` through `gh`.
    #[must_use]
    pub fn new(gh: Gh, pull: PullRequest) -> Self {
        Self {
            caps: gh.required_caps().to_vec(),
            gh,
            pull,
        }
    }
}

impl verb::Observe for PrSnapshotSource {
    type Output = PrSnapshot;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<PrSnapshot, BotError> {
        call.0.check(self.required_caps())?;
        match self.gh.snapshot(&self.pull).await {
            Ok(snapshot) => Ok(snapshot),
            Err(source) => Err(match source.credential_rejection(self.domain_id()) {
                Some(rejection) => rejection,
                None => BotError::DomainError {
                    domain: self.domain_id().into(),
                    certainty: DispatchCertainty::NotDelivered,
                    cause: source.to_string(),
                },
            }),
        }
    }

    fn domain_id(&self) -> &str {
        "gh::pr_snapshot"
    }
}
