//! `sys` owns the system process domain. Requires `bot.sys`.

use crate::cap::{Auth, Cap};
use crate::error::BotError;
use crate::verb;

/// Observe or execute a system process. Supports Observe, Execute, Query.
pub struct Process {
    /// The command name. Retained for the "binding required" diagnostic and for
    /// the eventual process binding; the domain performs no parsing of it here.
    command: String,
    /// Forced to `[bot.sys]` by the constructor: process control is the
    /// capability this domain exists to gate, and no constructor omits it.
    caps: Vec<Cap>,
}

/// Process state returned by observation or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct ProcessState {
    /// Whether the process is running.
    pub running: bool,
    /// Exit code if finished.
    pub exit_code: Option<i32>,
    /// Standard output (truncated).
    pub stdout: String,
}

impl Process {
    /// Create a system process observer/executor.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            caps: vec![Cap::sys()],
        }
    }
}

impl verb::Observe for Process {
    type Output = ProcessState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<ProcessState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("polling {:?} — binding required", self.command),
        })
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}

impl verb::Execute for Process {
    type Input = ();
    type Output = ProcessState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &())) -> Result<ProcessState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("executing {:?} — binding required", self.command),
        })
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}

impl verb::Query for Process {
    type Input = ();
    type Output = ProcessState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<ProcessState, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("querying {:?} — binding required", self.command),
        })
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}
