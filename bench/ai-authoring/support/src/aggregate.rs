//! The `aggregate` task's fixed vocabulary: the sum error.
//!
//! One definition for every arm: the prompt fixes these arms, so a copy per
//! reference file is how the copies drift. New references re-export this;
//! model authors write their own from the prompt.

/// Why an `aggregate` run did not return its sum.
#[derive(Debug)]
pub enum SolveError {
    /// The overall deadline passed before the sum resolved.
    Deadline,
    /// The run was cancelled before it could resolve.
    Cancelled,
    /// A fetch failed, naming the failing id.
    Fetch { id: u32 },
}
