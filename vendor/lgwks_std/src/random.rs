//! `random` owns every source of randomness and enforces
//! INV-RANDOM-ONE-SOURCE: all randomness comes from a single OS CSPRNG backend,
//! and a failure to read it is an error the caller must handle — never a
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
    backend: &'static str,
    cause: String,
}

impl EntropyError {
    /// The backend that could not provide entropy.
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

impl Error for EntropyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

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

    #[test]
    fn fills_the_whole_buffer() {
        // A 64-byte draw leaving the sentinel untouched everywhere would be a
        // short read presented as success.
        let mut buf = [0xAAu8; 64];
        fill_bytes(&mut buf).expect("OS entropy unavailable");
        assert!(buf.iter().any(|&b| b != 0xAA), "buffer looks unwritten");
    }

    #[test]
    fn successive_draws_differ() {
        let a = bytes::<32>().expect("OS entropy unavailable");
        let b = bytes::<32>().expect("OS entropy unavailable");
        assert_ne!(a, b);
    }

    #[test]
    fn empty_buffer_is_a_no_op() {
        assert!(fill_bytes(&mut []).is_ok());
    }
}
