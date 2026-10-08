//! The `capture` task's fixed vocabulary: the error and the bounded result.
//!
//! One definition for every arm's solution: the prompt fixes these items, so a
//! copy per reference file is how the copies drift. Reference solutions
//! re-export these; model authors write their own from the prompt.

/// Why a capture did not return its result.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// The program could not be started at all, naming why. A failure after
    /// the child started is never reported here.
    Spawn(String),
    /// The child did not exit before the deadline; the whole group was stopped.
    Deadline,
    /// The supervisor was cancelled before or while the child ran.
    Cancelled,
}

/// What one capture observed: the retained head, the exact total, and whether
/// anything was cut.
#[derive(Debug, PartialEq, Eq)]
pub struct CaptureResult {
    head: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

impl CaptureResult {
    /// The retained head bytes, never more than the declared ceiling.
    #[must_use]
    pub fn new(head: Vec<u8>, total_bytes: u64, truncated: bool) -> Self {
        Self { head, total_bytes, truncated }
    }

    /// The retained head bytes.
    #[must_use]
    pub fn head(&self) -> &[u8] {
        &self.head
    }

    /// The exact total the child wrote.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Whether the child wrote more than was retained.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}
