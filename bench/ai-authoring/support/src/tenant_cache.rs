//! The `tenant-cache` task's fixed vocabulary: the refusal.
//!
//! One definition for every arm's solution: the prompt fixes this item, so a
//! copy per reference file is how the copies drift. Reference solutions
//! re-export it; model authors write their own from the prompt.

/// Why a cache read did not return its value.
#[derive(Debug, PartialEq, Eq)]
pub enum CacheError {
    /// The token is not the requested tenant's token. Nothing was recorded,
    /// cached or returned for the call.
    Refused,
}
