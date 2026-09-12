//! `sys` owns the system process domain. Requires `bot.sys`.

use crate::cap::{Auth, Cap};
use crate::error::BotError;
use crate::verb;

/// Observe or execute a system process. Supports Observe, Execute, Query.
pub struct Process {
    command: String,
    caps: Vec<Cap>,
}

/// Process state returned by observation or query.
#[derive(Debug, Clone)]
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
            cause: format!("polling {} — binding required", self.command),
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
            cause: format!("executing {} — binding required", self.command),
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
            cause: format!("querying {} — binding required", self.command),
        })
    }

    fn domain_id(&self) -> &str {
        "sys::process"
    }
}

/// Convenience: create a system process domain.
pub fn process_command(command: impl Into<String>) -> Process {
    Process::new(command)
}
