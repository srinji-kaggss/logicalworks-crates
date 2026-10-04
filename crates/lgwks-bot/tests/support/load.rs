//! Shared load instrument for the task front-door test targets.
//!
//! The saturation, isolation and drop tests all need the same body: one that
//! marks itself live, records the high-water mark, yields a chosen number of
//! times so runs genuinely overlap, then marks itself out. One definition, so
//! the `task_front_door` and `sim_task_axes` targets measure the same thing and
//! the live/peak arithmetic cannot drift between them.
//!
//! Included by path so both targets share it:
//! `#[path = "../support/load.rs"] mod load;` (or `"support/load.rs"` from a
//! target at the crate's `tests/` root).
#![allow(
    dead_code,
    reason = "each including test target uses the same instrument but a different yield count"
)]

/// Drive one future to completion on a fresh current-thread runtime.
pub(crate) fn drive<T>(future: impl std::future::Future<Output = T>) -> T {
    lgwks_bot::block_on(future)
}

/// Poll `future` once and report whether it is still pending.
///
/// Used to park a run inside its body without awaiting it to completion.
pub(crate) fn pending_once<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) -> bool {
    drive(std::future::poll_fn(|context| {
        std::task::Poll::Ready(future.as_mut().poll(context).is_pending())
    }))
}

/// Assert a batch of load reports all succeeded and each kept its own input.
///
/// The inputs are the indices `0..reports.len()`, which is how every load sweep
/// in this suite drives the host, so association is proven rather than assumed.
pub(crate) fn assert_runs_succeeded(reports: &[lgwks_bot::task::Report<usize>], label: &str) {
    for (index, report) in reports.iter().enumerate() {
        assert_eq!(
            report.disposition(),
            lgwks_bot::task::Disposition::Succeeded,
            "{label}: run {index} completed: {:?}",
            report.error()
        );
        assert_eq!(
            report.output().copied(),
            Some(index),
            "{label}: run {index} kept its own output"
        );
    }
}

/// Assert a host's budget is whole: every permit returned, nothing in flight.
pub(crate) fn assert_budget_whole(host: &lgwks_bot::task::Host, ceiling: usize, label: &str) {
    assert_eq!(
        host.admission().available_permits(),
        ceiling,
        "{label}: every permit came back"
    );
    assert_eq!(
        host.admission().in_flight(),
        0,
        "{label}: nothing is left in flight"
    );
}

/// Assert a dropped or cancelled run released everything it held.
pub(crate) fn assert_permits_released(host: &lgwks_bot::task::Host, ceiling: usize, label: &str) {
    assert_budget_whole(host, ceiling, label);
    assert_eq!(
        host.admission().refused(),
        0,
        "{label}: the run was admitted, so releasing it is not a refusal"
    );
}

/// Assert the admission invariants a saturated load run must satisfy.
///
/// One definition, so the ceiling, the high-water mark and the permit count are
/// checked the same way in every target that drives a host under load.
pub(crate) fn assert_admission_conserved(
    host: &lgwks_bot::task::Host,
    observed_peak: usize,
    ceiling: usize,
    label: &str,
) {
    assert!(
        observed_peak <= ceiling,
        "{label}: the bodies observed {observed_peak} concurrent against a ceiling of {ceiling}"
    );
    assert_eq!(
        host.admission().peak_in_flight(),
        observed_peak,
        "{label}: the host's high-water mark agrees with what the bodies saw"
    );
    assert_eq!(
        host.admission().available_permits(),
        ceiling,
        "{label}: every permit came back"
    );
    assert_eq!(
        host.admission().in_flight(),
        0,
        "{label}: nothing is left in flight"
    );
}

/// Declare the shared load body: mark live, record the peak, yield `$yields`
/// times, mark out, and return the input.
///
/// Absolute paths throughout so a call site needs only the macro in scope, not
/// the instrument's imports. The closure is one concrete type, so the returned
/// `Task<F>` keeps its body unerased.
macro_rules! ticking_task {
    ($live:expr, $peak:expr, $yields:expr) => {{
        let live = ::std::sync::Arc::clone(&$live);
        let peak = ::std::sync::Arc::clone(&$peak);
        lgwks_bot::task::task(
            "tick",
            move |_scope: lgwks_bot::script::Scope, value: usize| {
                let live = ::std::sync::Arc::clone(&live);
                let peak = ::std::sync::Arc::clone(&peak);
                async move {
                    let now = live
                        .fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
                        .saturating_add(1);
                    let _previous = peak.try_update(
                        ::std::sync::atomic::Ordering::Relaxed,
                        ::std::sync::atomic::Ordering::Relaxed,
                        |high| (now > high).then_some(now),
                    );
                    for _ in 0..$yields {
                        lgwks_bot::rt::task::yield_now().await;
                    }
                    let _previous = live.try_update(
                        ::std::sync::atomic::Ordering::Relaxed,
                        ::std::sync::atomic::Ordering::Relaxed,
                        |count| count.checked_sub(1),
                    );
                    Ok::<usize, lgwks_bot::script::FlowError>(value)
                }
            },
        )
    }};
}

pub(crate) use ticking_task;
