//! The `pipeline` task's fixed vocabulary: the stage error.
//!
//! One definition for every arm: the prompt fixes these arms, so a copy per
//! reference file is how the copies drift. New references re-export this;
//! model authors write their own from the prompt.

use crate::StageName;

/// Why a `pipeline` run did not publish its artifact.
#[derive(Debug)]
pub enum PipelineError {
    /// The overall deadline passed before the run resolved.
    Deadline,
    /// The run was cancelled before it could resolve.
    Cancelled,
    /// A stage failed, naming the stage.
    Stage { name: StageName },
}
