//! `random` owns every source of randomness and enforces
//! INV-RANDOM-ONE-SOURCE: all randomness comes from a single OS CSPRNG backend,
//! and a failure to read it is an error the caller must handle, never a
//! silent fallback to a clock, a counter, or userspace PRNG.
//!
//! Backs [`crate::id`] UUID v4 generation. Uses `getrandom` for OS entropy.
//!
//! Supported targets: Linux, macOS, and Windows. Unsupported targets fail at
//! compile time.

use std::error::Error;
use std::fmt;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("lgwks_std::random supports linux, macOS, and windows only.");

/// The OS entropy source could not be read. There is no second source; a caller
/// that cannot proceed without randomness must fail, not substitute.
#[derive(Debug)]
pub struct EntropyError {
    /// The entropy backend that failed, as a stable short name (`"getrandom"`);
    /// callers may match on it, so it is not a message fragment.
    backend: &'static str,
    /// The backend's own description of the failure, kept verbatim for
    /// diagnosis and rendered by the `Display` impl below.
    cause: String,
}

impl EntropyError {
    /// The backend that could not provide entropy.
    #[must_use]
    pub fn backend(&self) -> &'static str {
        self.backend
    }
}

impl fmt::Display for EntropyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not read OS entropy from {}: {}",
            self.backend, self.cause
        )
    }
}

impl Error for EntropyError {}

/// Fills `buf` with cryptographically secure random bytes from the OS.
pub fn fill_bytes(buf: &mut [u8]) -> Result<(), EntropyError> {
    if buf.is_empty() {
        return Ok(());
    }
    getrandom::fill(buf).map_err(|cause| EntropyError {
        backend: "getrandom",
        cause: cause.to_string(),
    })
}

/// Returns `N` cryptographically secure random bytes from the OS.
pub fn bytes<const N: usize>() -> Result<[u8; N], EntropyError> {
    let mut out = [0u8; N];
    fill_bytes(&mut out)?;
    Ok(out)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // These tests return `Result` rather than unwrapping: an entropy refusal
    // reports its own `Debug` on failure, which is the same report `.expect`
    // would have panicked with, without an `expect` in the tree.
    #[test]
    fn fills_the_whole_buffer() -> Result<(), EntropyError> {
        // A 64-byte draw leaving the sentinel untouched everywhere would be a
        // short read presented as success.
        let mut buf = [0xAAu8; 64];
        fill_bytes(&mut buf)?;
        assert!(
            buf.iter().any(|&byte_value| byte_value != 0xAA),
            "buffer looks unwritten"
        );
        Ok(())
    }

    #[test]
    fn successive_draws_differ() -> Result<(), EntropyError> {
        let first = bytes::<32>()?;
        let second = bytes::<32>()?;
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn empty_buffer_is_a_no_op() {
        assert!(fill_bytes(&mut []).is_ok());
    }
}
