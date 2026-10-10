//! The latency arithmetic the `lgwks_ast` measurement examples share.
//!
//! `parse_budget` and `rust_readers` both report a median and a 99th
//! percentile over per-parse samples, and the two must mean the same thing, so
//! the arithmetic is one definition. Included by path
//! (`#[path = "support/measure.rs"] mod measure;`): a file in a subdirectory of
//! `examples/` is not auto-discovered as an example of its own.

/// The `percent`-th percentile of `samples`, by nearest rank over sorted input.
///
/// A percentile over no samples is a refusal rather than a zero: every caller
/// builds its samples by running at least one round, so an empty list means a
/// caller measured nothing, and a column of zero would read as a measurement
/// that came out at zero.
fn percentile(samples: &[u128], percent: u128) -> Result<u128, String> {
    if samples.is_empty() {
        let refusal = Err(format!(
            "a percentile over no samples is not a measurement; {percent}th asked for one"
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "percentile: returning an error to the caller");
        return refusal;
    }
    let last = samples.len().saturating_sub(1);
    // The rank is computed in the arithmetic's width and clamped into the list,
    // so the answer is one of the samples rather than an interpolation between
    // two. The sample count is a `usize` and the arithmetic a `u128`, because a
    // nanosecond count is one.
    let length = u128::try_from(samples.len()).map_err(|error| {
        format!(
            "{} samples do not fit the arithmetic: {error}",
            samples.len()
        )
    })?;
    let rank = length.saturating_mul(percent).saturating_div(100);
    let rank =
        usize::try_from(rank).map_err(|error| format!("rank {rank} is not an index: {error}"))?;
    let index = rank.saturating_sub(1).min(last);
    samples.get(index).copied().ok_or_else(|| {
        format!(
            "rank {rank} of {} samples names none of them",
            samples.len()
        )
    })
}

/// The median and the 99th percentile of `samples`, which it sorts first.
///
/// `percentile` reads ranks off sorted input, so the sort lives here, beside
/// the only reads, rather than at each caller: a caller that skipped it got a
/// rank over measurement order, which is an arbitrary sample, not a percentile.
pub(crate) fn spread(samples: &mut [u128]) -> Result<(u128, u128), String> {
    samples.sort_unstable();
    let p50 = percentile(samples, 50)?;
    let p99 = percentile(samples, 99)?;
    Ok((p50, p99))
}
