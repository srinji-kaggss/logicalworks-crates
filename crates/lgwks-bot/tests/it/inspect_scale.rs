//! Hyperscale and allocation acceptance for #150/R8.
//!
//! The hyperscale sweep drives 100, 1,000 and 10,000 inspections through one
//! [`Host`] with a bounded admission ceiling, on a fixed worker pool, and
//! asserts every run succeeds and the in-flight count never exceeds the
//! ceiling. The allocation test uses the operation's own `Resources` counters —
//! `unsafe` is forbidden, so a counting global allocator is not expressible —
//! across input sizes to show the retained output and visited nodes grow with
//! the input rather than jumping; the accompanying `inspect_scale` example
//! prints the same counters and is run under `/usr/bin/time -l` for peak RSS.
#![cfg(all(feature = "inspect", feature = "script"))]

use std::error::Error;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lgwks_bot::domain::inspect::{InspectionJob, inspection_task};
use lgwks_bot::inspect::{Budgets, InspectRequest, inspect};
use lgwks_bot::task::Host;

/// The one-violation subject the tiers inspect.
const SUBJECT: &str = "fn f() { g().unwrap(); }\n";

/// A subject of roughly `size` bytes with one violation per line.
///
/// The line count is drawn from `size` rather than defaulted: a subject built
/// from a stand-in line count would make the linear-growth assertions below
/// measure a subject nobody asked for.
///
/// # Errors
///
/// When `size` cannot be divided into whole lines. A line is forty bytes, so
/// this names the divisor the count was drawn from.
fn subject_of_size(size: usize) -> Result<String, Box<dyn Error>> {
    let lines = size
        .checked_div(40)
        .ok_or("a generated line is forty bytes")?
        .saturating_add(1);
    let mut source = String::with_capacity(size);
    for index in 0..lines {
        let line = format!("fn f{index}() {{ let value = input.unwrap(); }}\n");
        source.push_str(&line);
    }
    Ok(source)
}

#[test]
fn host_bounded_admission_holds_at_every_tier() -> Result<(), Box<dyn Error>> {
    let expected = inspect(&InspectRequest::new("scale.rs", SUBJECT));
    let ceiling = 16_usize;
    for jobs in [100_usize, 1_000, 10_000] {
        let host = Host::builder(&format!("scale{jobs}"))?
            .max_concurrent_tasks(NonZeroUsize::new(ceiling).ok_or("a non-zero ceiling")?)
            .default_deadline(Duration::from_secs(120))
            .build()?;
        let task = inspection_task("inspect")?;
        let next = AtomicUsize::new(0);
        let ran = AtomicUsize::new(0);
        let diverged = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..ceiling {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= jobs {
                            break;
                        }
                        match host.block_on(&task, InspectionJob::new("scale.rs", SUBJECT)) {
                            Ok(report) => match report.into_result() {
                                Ok(inspection) => {
                                    if inspection != expected {
                                        diverged.fetch_add(1, Ordering::Relaxed);
                                    }
                                    ran.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(_) => {
                                    diverged.fetch_add(1, Ordering::Relaxed);
                                }
                            },
                            Err(_) => {
                                diverged.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
        });

        assert_eq!(
            ran.load(Ordering::Relaxed),
            jobs,
            "tier {jobs}: every inspection must run and succeed"
        );
        assert_eq!(
            diverged.load(Ordering::Relaxed),
            0,
            "tier {jobs}: a concurrent inspection diverged from the direct one"
        );
        let peak = host.admission().peak_in_flight();
        assert!(
            peak <= ceiling,
            "tier {jobs}: peak in-flight {peak} exceeded the ceiling {ceiling}"
        );
        assert!(peak >= 1, "tier {jobs}: runs must have been in flight");
        assert_eq!(
            host.admission().admitted(),
            u64::try_from(jobs)?,
            "tier {jobs}: the host admitted a different number of runs"
        );
    }
    Ok(())
}

#[test]
fn retained_counters_grow_with_the_input() -> Result<(), Box<dyn Error>> {
    // A findings budget above the largest density keeps every finding retained,
    // so `output_bytes` is the operation's own measure of retained finding text
    // and not a capped value.
    let budgets = Budgets::new().with_findings(1_048_576);
    let mut previous_findings = 0_usize;
    let mut previous_bytes = 0_usize;
    // Retained output bytes per thousand input bytes, as an integer.
    let mut ratios: Vec<usize> = Vec::new();

    for size in [4_096_usize, 16_384, 65_536, 262_144] {
        let source = subject_of_size(size)?;
        let report = inspect(&InspectRequest::new("scale.rs", &source).budgets(budgets));
        let resources = report.resources();
        let lines = source.lines().count();

        assert_eq!(
            resources.findings, lines,
            "size {size}: one retained finding per generated line"
        );
        assert!(
            resources.output_bytes > previous_bytes,
            "size {size}: retained output must grow with the input"
        );
        assert!(
            resources.nodes > 0 && resources.nodes <= lgwks_ast::MAX_AST_NODES,
            "size {size}: the walk must stay inside the node bound"
        );
        assert!(
            resources.output_bytes < lgwks_bot::inspect::MAX_OUTPUT_BYTES,
            "size {size}: retained output must stay under the output budget"
        );

        // Retained bytes per thousand input bytes are roughly constant across
        // sizes; the band is wide enough for the fixed per-line syntax and
        // narrow enough that a quadratic jump would fail it.
        let per_mille = resources
            .output_bytes
            .saturating_mul(1_000)
            .checked_div(source.len())
            .ok_or("a generated subject is never empty")?;
        ratios.push(per_mille);
        previous_findings = previous_findings.max(resources.findings);
        previous_bytes = resources.output_bytes;
    }

    assert_eq!(
        previous_findings,
        subject_of_size(262_144)?.lines().count(),
        "the largest size must retain every finding"
    );
    // Both ends are read through the sweep's own measurements. A defaulted end
    // would make the linearity assertion below true for every subject, which is
    // the one thing this test exists to rule out.
    let (Some(smallest), Some(largest)) =
        (ratios.iter().copied().min(), ratios.iter().copied().max())
    else {
        let refusal = Err("the size sweep measured no retained-byte ratio at all".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retained_counters_grow_with_the_input: returning an error to the caller");
        return refusal;
    };
    assert!(
        largest <= smallest.saturating_mul(2),
        "retained finding bytes must be linear in the input: per-mille ratios {ratios:?}"
    );
    Ok(())
}
