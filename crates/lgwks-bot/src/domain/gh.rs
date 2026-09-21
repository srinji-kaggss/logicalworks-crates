//! `gh` owns the GitHub domain and enforces INV-BOT-CAP-GATED: every
//! GitHub operation requires `bot.net`.

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

// ── pr_status ──────────────────────────────────────────────────────────────

/// Observe the status of a pull request. Supports Observe and Query.
pub struct PrStatus {
    /// The `owner/repo` observed; echoed in the "binding required" diagnostic.
    repo: String,
    /// Forced to `[bot.net]` by the constructor: every GitHub call is an HTTP
    /// call, and no constructor omits the capability.
    caps: Vec<Cap>,
}

/// PR state returned by observation or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct PrState {
    /// Whether any check status changed since last poll.
    pub checks_changed: bool,
    /// Whether all checks are green.
    pub all_green: bool,
    /// Whether the PR is merged.
    pub merged: bool,
    /// Raw status text for custom evaluation.
    pub status: String,
}

impl PrStatus {
    /// Create a PR status observer for `owner/repo`.
    pub fn new(repo: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            caps: vec![Cap::net()],
        }
    }
}

impl verb::Observe for PrStatus {
    type Output = PrState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<PrState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("polling {:?} — binding required", self.repo),
        })
    }

    fn domain_id(&self) -> &str {
        "gh::pr_status"
    }
}

impl verb::Query for PrStatus {
    type Input = ();
    type Output = PrState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<PrState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("querying {:?} — binding required", self.repo),
        })
    }

    fn domain_id(&self) -> &str {
        "gh::pr_status"
    }
}

/// Convenience: create a `PrStatus` observer.
pub fn pr_status(repo: impl Into<String>) -> PrStatus {
    PrStatus::new(repo)
}

// ── ci_run ─────────────────────────────────────────────────────────────────

/// Observe CI run status. Supports Observe, Query.
pub struct CiRun {
    /// The `owner/repo` observed; echoed in the "binding required" diagnostic.
    repo: String,
    /// Forced to `[bot.net]` by the constructor: reading a run's status is an
    /// API call, so there is no ungated path to it.
    caps: Vec<Cap>,
}

/// CI run state returned by observation or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct CiState {
    /// Whether the run failed.
    pub failed: bool,
    /// Whether the run succeeded.
    pub succeeded: bool,
    /// Whether the run is still in progress.
    pub in_progress: bool,
    /// Run identifier.
    pub run_id: String,
}

impl CiRun {
    /// Create a CI run observer for `owner/repo`.
    pub fn new(repo: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            caps: vec![Cap::net()],
        }
    }
}

impl verb::Observe for CiRun {
    type Output = CiState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<CiState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("polling {:?} — binding required", self.repo),
        })
    }

    fn domain_id(&self) -> &str {
        "gh::ci_run"
    }
}

impl verb::Query for CiRun {
    type Input = ();
    type Output = CiState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<CiState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("querying {:?} — binding required", self.repo),
        })
    }

    fn domain_id(&self) -> &str {
        "gh::ci_run"
    }
}

/// Convenience: create a `CiRun` observer.
pub fn ci_run(repo: impl Into<String>) -> CiRun {
    CiRun::new(repo)
}

// ── merge ──────────────────────────────────────────────────────────────────

/// Execute a PR merge. Supports Execute only.
pub struct Merge {
    /// The `owner/repo` merged; echoed in the "binding required" diagnostic.
    repo: String,
    /// Forced to `[bot.net]` by the constructor: a merge is a mutating API
    /// call, so it is gated on the same capability a read is.
    caps: Vec<Cap>,
}

impl Merge {
    /// Create a merge executor for `owner/repo`.
    pub fn new(repo: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            caps: vec![Cap::net()],
        }
    }
}

impl verb::Execute for Merge {
    type Input = PrState;
    type Output = String;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &PrState)) -> Result<String, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("merging {:?} — binding required", self.repo),
        })
    }

    fn domain_id(&self) -> &str {
        "gh::merge"
    }
}

/// Convenience: create a `Merge` executor.
pub fn merge(repo: impl Into<String>) -> Merge {
    Merge::new(repo)
}
