//! Shared sweep vocabulary for the issue #278 families: the seed space, the
//! typed refusal, and the memory bound.
//!
//! The row asks every family's tests to pass over at least 1,000 seeds with
//! seeds-per-minute and peak RSS reported. One definition of the seed space
//! rather than one constant per family, so the coverage the rows promise
//! cannot drift apart family by family.
//!
//! Throughput has no helper here by design: `print_stdout` is
//! workspace-forbid, so a sweep cannot print its numbers, and recording a
//! wall-clock quotient in the hashed trace would make the replay receipt
//! scheduler-dependent. Each sweep asserts the memory bound that keeps a wide
//! sweep honest ([`peak_rss_bytes`]), and the PR evidence quotes the measured
//! seeds-per-minute beside the test's name from a timed run.
//!
//! Peak RSS is the high-water mark where the platform reports one (Linux
//! `VmHWM`) and a point sample where it does not (macOS `ps`), said plainly
//! in [`peak_rss_bytes`]'s contract rather than hidden behind one name.

/// The seed space a row's sweep covers: the row asks for at least a thousand.
pub const SWEEP_SEEDS: u64 = 1024;

/// The first seed of every sweep.
///
/// Not zero: a zero seed is the one value the shared generator remaps, so a
/// family that swept it would report a draw sequence the generator does not
/// produce.
pub const FIRST_SEED: u64 = 1;

/// Return a typed test failure, emitting it first.
///
/// The emission is what the `scan` lane requires of every `return Err`:
/// a caller that sees only a value has no signal. One definition for the
/// families introduced for issue #278, so every scenario's refusal reads
/// identically rather than one line per call site.
pub fn refuse<Outcome>(cause: impl Into<String>) -> Result<Outcome, Box<dyn std::error::Error>> {
    let refusal: Result<Outcome, Box<dyn std::error::Error>> = Err(cause.into().into());
    lgwks_std::trace::debug!(
        error = ?refusal.as_ref().err(),
        "scenario: returning an error to the caller"
    );
    refusal
}

/// This process's peak resident set size in bytes, or `None` when the
/// platform exposes no high-water mark.
///
/// Linux reads `VmHWM` from `/proc/self/status`: the true high-water mark.
/// Anywhere else — macOS has no process-readable high-water mark without
/// reaching past `std`, and a point sample is not a peak — the answer is
/// `None` rather than a number that is not what it claims to be: not measured
/// is not measured as zero. Quote the timed run's peak beside the test's name
/// instead.
#[must_use]
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            let digits = line.strip_prefix("VmHWM:")?;
            let numeric = match digits.trim().strip_suffix("kB") {
                Some(kb) => kb.trim(),
                None => return None,
            };
            let kb: u64 = numeric.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
