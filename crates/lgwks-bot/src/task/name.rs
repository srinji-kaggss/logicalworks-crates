//! The validated logical name of a task.
//!
//! A task name is part of every step path and every error location the run
//! produces, so it is bounded and restricted to characters that are safe in a
//! path, a log line and a file name. Validating it once, at declaration, means
//! no run can reach an identity nobody checked.

use std::fmt;
use std::sync::Arc;

use crate::script::FlowError;
use crate::script::scope::{NameFault, name_fault};

/// The longest task name, in bytes.
pub(crate) const MAX_TASK_NAME_BYTES: usize = 128;

/// A validated task name: 1 to [`MAX_TASK_NAME_BYTES`] bytes of ASCII letters,
/// digits, `-`, `_`, `.` and `:`.
///
/// Shared, cheap to clone, and never mutable: every run of one task enters the
/// same first step path, which is what makes that path a stable identity rather
/// than a line number.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TaskName(Arc<str>);

impl TaskName {
    /// Validate `name` and hold it.
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidName`] naming what is wrong with it.
    pub(crate) fn new(name: &str) -> Result<Self, FlowError> {
        let reason = match name_fault(name, MAX_TASK_NAME_BYTES) {
            None => return Ok(Self(Arc::from(name))),
            Some(NameFault::Empty) => "the task name is empty",
            Some(NameFault::TooLong) => "the task name is longer than MAX_TASK_NAME_BYTES",
            Some(NameFault::BadChar) => {
                "the task name may hold only ASCII letters, digits, '-', '_', '.' and ':'"
            }
        };
        Err(FlowError::InvalidName { reason })
    }

    /// The validated name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskName {
    /// The name itself.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
