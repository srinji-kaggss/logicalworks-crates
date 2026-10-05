//! `random` owns every source of randomness and enforces
//! INV-RANDOM-ONE-SOURCE: all randomness comes from a single OS CSPRNG backend,
//! and a failure to read it is an error the caller must handle, never a
//! silent fallback to a clock, a counter, or userspace PRNG.
//!
//! Backs [`crate::id`] UUID v4 generation. Uses `getrandom` for OS entropy.
//!
//! # Targets
//!
//! Every target `getrandom` supports, and no others. This module holds no
//! target list of its own, because a second list can only be narrower than the
//! backend's and a narrower list refuses targets whose entropy source exists.
//! The backend is the one authority on where a source exists and refuses a
//! target it has none for from its own dispatch, so that refusal is the single
//! compile-time gate. On a target the backend supports but whose source fails
//! at run time, the call returns [`EntropyError`] and substitutes nothing.
//!
//! The supported set is therefore the FreeBSD, NetBSD, OpenBSD, DragonFly,
//! illumos, Solaris, Haiku, Redox, AIX, NTO, Fuchsia, Android, iOS, watchOS,
//! tvOS, visionOS, UEFI, Hermit, VxWorks, Linux, macOS, Windows and
//! `wasm32-wasip1`/`wasip2` families the backend dispatches, and
//! `scripts/check-target-matrix.sh` is the lane that executes `cargo check`
//! across that matrix rather than inferring it from a `cfg`.
//!
//! `id` reads [`bytes`] for UUID v4, so this set is also the set `lgwks_std`
//! with `random` builds on.
//!
//! [`EntropyError`]: crate::random::EntropyError
//! [`bytes`]: crate::random::bytes

use std::error::Error;
use std::fmt;
use std::io;

/// The entropy backend this module reads, named as a stable short identifier.
const BACKEND: &str = "getrandom";

/// What the entropy backend could not do, at the granularity a caller can act
/// on.
///
/// A caller that cannot proceed without randomness fails; a caller that can
/// retry acts on the kind rather than on the message, which is why the raw OS
/// code is data here and not a rendered string.
///
/// ```rust
/// use lgwks_std::random::{EntropyError, EntropyErrorKind};
///
/// // A caller matches the kind and the code, never the message. The wildcard
/// // arm is required: the enum is `#[non_exhaustive]`, so a new kind is an
/// // additive change rather than a breaking one.
/// fn decision(error: &EntropyError) -> &'static str {
///     match error.kind() {
///         EntropyErrorKind::OsCode { code } if code == 4 => "interrupted: retry",
///         EntropyErrorKind::OsCode { .. } => "an OS refusal: read the code",
///         EntropyErrorKind::UnsupportedTarget => "this host has no entropy source",
///         _ => "the backend failed without a code",
///     }
/// }
/// # let _ = decision;
/// # Ok::<(), EntropyError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntropyErrorKind {
    /// The OS refused the read and reported a code: an `errno` on the POSIX
    /// targets, a Win32 status on Windows, an EFI status on UEFI.
    ///
    /// `4` is `EINTR` on every POSIX target this module builds on, and `19` is
    /// `ENODEV`; a caller distinguishes "interrupted, retry" from "no entropy
    /// device, this host cannot serve randomness" from the code alone.
    OsCode {
        /// The code exactly as the OS reported it.
        code: i32,
    },
    /// This target's backend has no entropy source, so the call failed at run
    /// time rather than falling back to a weaker source.
    UnsupportedTarget,
    /// The backend failed without a code this crate can represent: an `errno`
    /// that was zero or negative where a positive value was required, or a UEFI
    /// status above `i32::MAX`. There is nothing to match on, so a caller
    /// retries or fails.
    BackendInternal,
}

/// The OS entropy source could not be read. There is no second source; a caller
/// that cannot proceed without randomness must fail, not substitute.
///
/// The cause is structured rather than rendered: [`EntropyError::kind`] says
/// what the backend could not do, [`EntropyError::raw_os_error`] carries the
/// OS's own code where there was one, and [`EntropyError::io_error_kind`] maps
/// that code to the portable `std::io` classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntropyError {
    /// The entropy backend that failed, as a stable short name (`"getrandom"`);
    /// callers may match on it, so it is not a message fragment.
    backend: &'static str,
    /// What the backend could not do.
    kind: EntropyErrorKind,
}

impl EntropyError {
    /// The backend that could not provide entropy.
    #[must_use]
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// What the backend could not do.
    #[must_use]
    pub fn kind(&self) -> EntropyErrorKind {
        self.kind
    }

    /// The raw OS error code, or `None` when the failure did not come from the
    /// OS — an unsupported target, or a backend failure with no representable
    /// code.
    ///
    /// The width is `i32` because that is the width `errno` has on every POSIX
    /// and Windows target this module builds on. `getrandom` reports a `usize`
    /// status word on UEFI; a status above `i32::MAX` is reported as no code
    /// rather than narrowed into the low 32 bits, because a truncated code
    /// names a different failure than the one that happened.
    #[must_use]
    pub fn raw_os_error(&self) -> Option<i32> {
        match self.kind {
            EntropyErrorKind::OsCode { code } => Some(code),
            EntropyErrorKind::UnsupportedTarget | EntropyErrorKind::BackendInternal => None,
        }
    }

    /// The portable `std::io` classification of [`EntropyError::raw_os_error`],
    /// or `None` when there is no OS code to classify.
    ///
    /// The mapping is `std`'s, not this crate's, so it is whatever `std` says for
    /// the code on the target in question. The one classification this crate
    /// asserts is the one a retry depends on: `EINTR` is
    /// [`io::ErrorKind::Interrupted`]. `ENODEV` is *not* that, so a caller that
    /// retries on an interruption never loops on a host with no entropy device.
    #[must_use]
    pub fn io_error_kind(&self) -> Option<io::ErrorKind> {
        self.raw_os_error()
            .map(|code| io::Error::from_raw_os_error(code).kind())
    }
}

impl fmt::Display for EntropyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not read OS entropy from {}", self.backend)?;
        match self.kind {
            EntropyErrorKind::OsCode { code } => write!(
                f,
                ": {} (OS error {code})",
                io::Error::from_raw_os_error(code)
            ),
            EntropyErrorKind::UnsupportedTarget => {
                f.write_str(": this target's backend has no entropy source")
            }
            EntropyErrorKind::BackendInternal => {
                f.write_str(": the backend failed without a usable OS error code")
            }
        }
    }
}

// The OS cause is the `code` in `kind`, which `Display` renders through
// `io::Error::from_raw_os_error`, so there is no separate error object to chain
// and `source` is `None` rather than a re-wrapped copy of the same fact.
impl Error for EntropyError {}

/// Fills `buf` with cryptographically secure random bytes from the OS.
///
/// On success every byte of `buf` is written. On failure **the contents of
/// `buf` are unspecified**: the backend may have written a prefix, the whole
/// buffer, or none of it, and this module neither reads nor clears it. A caller
/// must not read `buf` after an `Err`; [`bytes`] has no such window because a
/// failed draw returns no array.
///
/// ```rust
/// let mut buf = [0u8; 32];
/// lgwks_std::random::fill_bytes(&mut buf)?;
/// # Ok::<(), lgwks_std::random::EntropyError>(())
/// ```
pub fn fill_bytes(buf: &mut [u8]) -> Result<(), EntropyError> {
    fill_from(buf, getrandom::fill)
}

/// Returns `N` cryptographically secure random bytes from the OS.
///
/// A refusal returns no array, so there is no partially filled value for a
/// caller to read; the error carries the same kind, code and classification as
/// [`fill_bytes`].
pub fn bytes<const N: usize>() -> Result<[u8; N], EntropyError> {
    let mut out = [0u8; N];
    fill_bytes(&mut out)?;
    Ok(out)
}

/// Narrows a backend status to this crate's declared `Option<i32>` width.
///
/// `getrandom` reports `i32` on every target except UEFI, whose status word is
/// a `usize`. An out-of-width status is dropped rather than truncated, so the
/// caller reads [`EntropyErrorKind::BackendInternal`] instead of a code that
/// names some other failure.
fn narrow_status<T: Copy + TryInto<i32>>(status: Option<T>) -> Option<i32> {
    status.and_then(|value| value.try_into().ok())
}

/// Builds the typed error from what the backend reported.
///
/// The one conversion point: [`fill_from`] is its only production caller, and a
/// test drives it with an OS code a working host never produces. `getrandom`
/// offers no way to do that from outside its own crate — `Error::new_custom` is
/// its only public constructor and yields a code with no OS meaning — so without
/// this seam the round trip through [`EntropyError::raw_os_error`] would be
/// unobservable until a host's entropy source failed for real.
fn from_backend(raw_os_error: Option<i32>, unsupported: bool) -> EntropyError {
    let kind = if let Some(code) = raw_os_error {
        EntropyErrorKind::OsCode { code }
    } else if unsupported {
        EntropyErrorKind::UnsupportedTarget
    } else {
        EntropyErrorKind::BackendInternal
    };
    EntropyError {
        backend: BACKEND,
        kind,
    }
}

/// Fills `buf` from `source`, turning a backend failure into the typed error.
///
/// [`fill_bytes`] is the only production caller. The indirection is the test
/// seam for the failure path: a source that refuses is the only way to observe
/// what this module does with a failure, and an empty buffer must reach no
/// source at all.
fn fill_from<F>(buf: &mut [u8], source: F) -> Result<(), EntropyError>
where
    F: FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
{
    if buf.is_empty() {
        return Ok(());
    }
    source(buf).map_err(|failure| {
        from_backend(
            narrow_status(failure.raw_os_error()),
            failure == getrandom::Error::UNSUPPORTED,
        )
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // `EINTR` is the one OS code whose portable classification this file
    // asserts, so a change to `std`'s mapping shows up here rather than in a
    // caller's retry loop.
    const EINTR: i32 = 4;

    /// `ENODEV`: the OS is healthy and has no entropy device, which is the
    /// failure a retry must not loop on.
    const ENODEV: i32 = 19;

    /// The bound a caller's own signature can state about this error type.
    ///
    /// A caller that propagates a refusal into a `Box<dyn Error>`, across a
    /// thread, or through a supervisor needs these bounds to hold; they are
    /// asserted at compile time because nothing at run time would notice their
    /// absence until a generic parameter stopped accepting this type.
    const fn assert_propagates<T: Error + Send + Sync + 'static>() {}

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

    #[test]
    fn an_empty_buffer_reaches_no_entropy_source() {
        // The early return is the observable claim: an empty draw costs no
        // syscall, so a caller can ask for zero bytes on a target with no
        // entropy source at all and still succeed.
        let mut called = false;
        let result = fill_from(&mut [], |_buf| {
            called = true;
            Err(getrandom::Error::UNSUPPORTED)
        });
        assert!(
            result.is_ok(),
            "an empty draw must succeed without a source"
        );
        assert!(!called, "an empty draw must not reach the entropy source");
    }

    #[test]
    fn an_injected_os_code_round_trips_through_the_typed_error() {
        assert_propagates::<EntropyError>();
        for code in [1, EINTR, 13, ENODEV, 5, i32::MAX] {
            let error = from_backend(Some(code), false);
            assert_eq!(
                error.raw_os_error(),
                Some(code),
                "OS error {code} must survive the typed error unchanged"
            );
            assert_eq!(
                error.kind(),
                EntropyErrorKind::OsCode { code },
                "OS error {code} must be reported as the OS-code kind"
            );
            assert_eq!(
                error.backend(),
                "getrandom",
                "the backend name is the stable identifier callers match on"
            );
            assert!(
                error.to_string().contains(&code.to_string()),
                "the rendered message must name OS error {code}, got {error}"
            );
            // A caller's log line is the artifact a user reads, and it is the
            // `Debug` rendering, so the code has to survive both.
            assert!(
                format!("{error:?}").contains(&code.to_string()),
                "the debug rendering of OS error {code} must name the code"
            );
        }
    }

    #[test]
    fn an_injected_interruption_is_distinguished_from_a_missing_device() {
        let interrupted = from_backend(Some(EINTR), false);
        let missing = from_backend(Some(ENODEV), false);
        assert_eq!(
            interrupted.io_error_kind(),
            Some(io::ErrorKind::Interrupted),
            "an interruption must classify as Interrupted so a caller retries it"
        );
        assert_ne!(
            interrupted.kind(),
            missing.kind(),
            "two different OS codes are two different failures"
        );
        assert_ne!(
            interrupted.raw_os_error(),
            missing.raw_os_error(),
            "a caller reading the code must tell EINTR from ENODEV"
        );
    }

    #[test]
    fn the_os_classification_is_present_exactly_where_a_code_is() {
        for code in [EINTR, ENODEV, 1, 13] {
            let error = from_backend(Some(code), false);
            assert_eq!(
                error.io_error_kind().is_some(),
                error.raw_os_error().is_some(),
                "OS error {code} must classify exactly when it carries a code"
            );
        }
    }

    #[test]
    fn a_backend_with_no_source_is_not_an_os_code() {
        let error = from_backend(None, true);
        assert_eq!(
            error.kind(),
            EntropyErrorKind::UnsupportedTarget,
            "a backend with no source on this target is its own kind"
        );
        assert_eq!(
            error.raw_os_error(),
            None,
            "an unsupported target has no OS code to report"
        );
        assert_eq!(
            error.io_error_kind(),
            None,
            "an unsupported target has no io classification either"
        );
        assert!(
            error.to_string().contains("no entropy source"),
            "the rendered message must name the missing source, got {error}"
        );
    }

    #[test]
    fn a_backend_failure_without_a_source_is_reported_as_internal() {
        let error = from_backend(None, false);
        assert_eq!(
            error.kind(),
            EntropyErrorKind::BackendInternal,
            "a failure with no code and no missing source is the internal bucket"
        );
        assert_eq!(error.raw_os_error(), None, "and carries no code");
    }

    #[test]
    fn a_status_wider_than_the_declared_code_is_not_truncated() {
        // The UEFI status word is a `usize`, so this is the width boundary the
        // `i32` contract can lose information at. Narrowing to the low bits
        // would report a code that names a different failure.
        assert_eq!(
            narrow_status(Some(4_i64)),
            Some(4),
            "a representable status must narrow to its own value"
        );
        assert_eq!(
            narrow_status(Some(i64::from(i32::MAX))),
            Some(i32::MAX),
            "the boundary status must narrow to its own value"
        );
        assert_eq!(
            narrow_status(Some(4_294_967_296_i64)),
            None,
            "a status above the declared width must be dropped, not truncated"
        );
        assert_eq!(
            narrow_status::<i64>(None),
            None,
            "a backend failure with no status must narrow to no code"
        );
        assert_eq!(
            from_backend(narrow_status(Some(4_294_967_296_i64)), false).kind(),
            EntropyErrorKind::BackendInternal,
            "a dropped width leaves the internal bucket rather than a wrong code"
        );
    }

    #[test]
    fn a_refused_fill_returns_the_typed_error_and_leaves_the_buffer_alone() {
        // The documented promise is that the buffer is unspecified after a
        // failure. What this module owns is the narrower half of it: it does
        // not write, clear or read the buffer on the way out, so whatever the
        // backend left is exactly what the caller still holds.
        let mut buf = [0xAAu8; 32];
        let result = fill_from(&mut buf, |_buf| Err(getrandom::Error::UNSUPPORTED));
        assert_eq!(
            result,
            Err(EntropyError {
                backend: BACKEND,
                kind: EntropyErrorKind::UnsupportedTarget,
            }),
            "a refusing source must surface as the typed error, not a panic or a short fill"
        );
        assert_eq!(
            buf, [0xAAu8; 32],
            "this module must not write or clear the buffer on a refusal"
        );
    }

    #[test]
    fn a_refused_fill_is_observable_through_the_typed_error_it_returns() {
        // `getrandom::Error::UNSUPPORTED` is the one backend failure a caller
        // can construct, so this drives the real conversion in `fill_from`
        // rather than only the classification it ends in.
        let mut buf = [0u8; 8];
        let observed = fill_from(&mut buf, |_buf| Err(getrandom::Error::UNSUPPORTED)).err();
        assert_eq!(
            observed,
            Some(from_backend(None, true)),
            "the real backend failure must classify exactly as the seam says"
        );
        assert_eq!(
            observed.and_then(|error| error.raw_os_error()),
            None,
            "an unsupported backend reports no OS code"
        );
    }

    #[test]
    fn a_successful_fill_through_the_seam_still_writes_the_whole_buffer() {
        // The negative control: a seam that always refuses would pass the
        // failure tests, so the same seam has to produce a fully written buffer
        // when its source succeeds.
        let mut buf = [0u8; 16];
        let result = fill_from(&mut buf, |buf| {
            buf.fill(0x5A);
            Ok(())
        });
        assert!(result.is_ok(), "a succeeding source must report success");
        assert_eq!(
            buf, [0x5Au8; 16],
            "a successful fill must leave the source's bytes in the buffer"
        );
    }
}
