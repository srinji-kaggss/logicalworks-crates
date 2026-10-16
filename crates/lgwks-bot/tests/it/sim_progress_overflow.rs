//! Simulation family: a progress trail that overflows (acceptance row T36).
//!
//! Row T36 of `docs/orchestration-acceptance.spec.md`: "Progress buffer overflow
//! loses only declared detail and increments counters; authoritative
//! terminal/Unknown state is still retrievable. A quiet client never requires a
//! task-owned reporting loop."
//!
//! `t_rows::an_overflowing_progress_buffer_keeps_the_terminal_state_t36` pins one
//! walk at one capacity. This family draws the capacity, the page count and the
//! failing page from the seed and checks every walk against the same walk on a
//! host whose trail cannot overflow. That is differential testing: two
//! implementations of one contract, an unbounded log and a bounded ring, must
//! agree on everything the ring did not declare it would drop. The ring is a
//! `VecDeque`-style ring buffer, so what it keeps is exactly the newest
//! `capacity` entries and what it loses is counted, never hidden.
//!
//! | Property | What a seed checks |
//! |---|---|
//! | the trail is the suffix | the bounded report retains exactly the last `min(capacity, total)` step paths of the reference trail |
//! | the loss is counted | `dropped_steps` is the number of paths the ring let go, and `progress_capacity` is the capacity declared |
//! | the terminal state is whole | disposition, typed output and located failure equal the reference's, whatever the ring dropped |
//! | a quiet client is enough | the run is driven to its terminal report and nothing reads progress while it runs |
//!
//! # What is real and what is seeded
//!
//! The host, the task, `each` and the report are the shipped types. The seed
//! draws only the shape of the walk, never a clock reading, and the trace holds
//! counts and step paths, never elapsed time, so the hash is a replay receipt.

#![cfg(feature = "script")]

use crate::band_family;
use crate::sim;

use std::error::Error;
use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::script::{FlowError, Scope, each};
use lgwks_bot::task::{Disposition, Host, Report, task};

use sim::Band;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The trail capacity of the reference host: far above the longest walk below,
/// so the reference run retains every step it entered.
const REFERENCE_CAPACITY: usize = 512;

/// The largest trail capacity a seed draws for the bounded host.
const MOST_CAPACITY: u32 = 12;

/// The most pages one walk visits.
const MOST_PAGES: u32 = 48;

/// The message of the one page a seed may refuse.
const REFUSAL: &str = "page refused";

/// Walk `0..pages` one page at a time on a host whose trail holds `capacity`
/// step paths, refusing page `failing` when a seed names one.
///
/// One function for the reference and the bounded run, so the two differ in the
/// capacity alone: a second spelling of the walk could drift into a different
/// run while the comparison still read as a comparison.
fn walk(
    capacity: usize,
    pages: u32,
    failing: Option<u32>,
) -> Result<Report<Vec<u32>>, Box<dyn Error>> {
    let host = Host::builder("t36-sim")?
        .progress_capacity(capacity)
        .default_deadline(Duration::from_secs(30))
        .build()?;
    let walker = task("walk", move |scope: Scope, items: Vec<u32>| async move {
        each::<_, u32, _, _>(
            &scope,
            "page",
            NonZeroUsize::new(1),
            items,
            |step, item| async move {
                step.checkpoint()?;
                if Some(item) == failing {
                    Err(FlowError::failed(REFUSAL))
                } else {
                    Ok(item)
                }
            },
        )
        .await
    })?;
    Ok(lgwks_bot::block_on(host.run(&walker, (0..pages).collect())))
}

/// A bounded walk keeps the newest `capacity` steps, counts the rest, and ends in
/// the same terminal state as the walk that dropped nothing.
fn overflow_keeps_the_terminal_state(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let seed = sim.seed;
        let capacity = usize::try_from(sim.rng().between(1, MOST_CAPACITY))?;
        let pages = sim.rng().between(1, MOST_PAGES);
        let failing = sim
            .rng()
            .chance(500)
            .then(|| sim.rng().between(0, pages.saturating_sub(1)));

        let reference = walk(REFERENCE_CAPACITY, pages, failing)?;
        assert_eq!(
            reference.dropped_steps(),
            0,
            "seed {seed}: the reference host retains every step, or it is no reference"
        );
        let bounded = walk(capacity, pages, failing)?;

        let total = reference.steps().len();
        let kept = capacity.min(total);
        let lost = total.saturating_sub(kept);
        assert_eq!(
            bounded.progress_capacity(),
            capacity,
            "seed {seed}: the report states the capacity it could have retained"
        );
        assert_eq!(
            bounded.steps().len(),
            kept,
            "seed {seed}: the trail holds min(capacity {capacity}, steps {total}) paths"
        );
        assert_eq!(
            bounded.trail().len(),
            kept,
            "seed {seed}: the script trail is the same entries as the step paths"
        );
        assert_eq!(
            bounded.dropped_steps(),
            lost,
            "seed {seed}: every path the ring let go is counted"
        );
        let suffix = reference
            .steps()
            .get(lost..)
            .ok_or("the dropped count lies inside the reference trail")?;
        assert_eq!(
            bounded.steps(),
            suffix,
            "seed {seed}: what the ring kept is the newest {kept} paths, in order"
        );

        let expected = if failing.is_some() {
            Disposition::Failed
        } else {
            Disposition::Succeeded
        };
        assert_eq!(
            bounded.disposition(),
            expected,
            "seed {seed}: the terminal disposition is the walk's own, however full the ring: {bounded:?}"
        );
        assert_eq!(
            bounded.output(),
            reference.output(),
            "seed {seed}: the typed output does not depend on how much trail was kept"
        );
        let last = bounded
            .steps()
            .last()
            .ok_or("a truncated trail still ends where the run stopped")?
            .to_string();
        match failing {
            None => {
                assert_eq!(
                    bounded.output(),
                    Some(&(0..pages).collect::<Vec<u32>>()),
                    "seed {seed}: a finished walk returns every page"
                );
                assert!(
                    last.ends_with(&format!("page#{}", pages.saturating_sub(1))),
                    "seed {seed}: the kept trail ends at the last page: {last}"
                );
            }
            Some(page) => {
                assert!(
                    bounded.output().is_none(),
                    "seed {seed}: a failed walk has no output"
                );
                let located = bounded
                    .error()
                    .map(ToString::to_string)
                    .ok_or("a failed run names its failure")?;
                assert!(
                    located.contains(REFUSAL),
                    "seed {seed}: the failure is located in the report, not only in the trail: {located}"
                );
                assert_eq!(
                    Some(located),
                    reference.error().map(ToString::to_string),
                    "seed {seed}: the located failure is the same with or without the dropped detail"
                );
                assert!(
                    last.ends_with(&format!("page#{page}")),
                    "seed {seed}: the kept trail ends at the page that failed: {last}"
                );
            }
        }

        sim.record(&format!(
            "capacity={capacity} pages={pages} failing={failing:?} total={total} kept={kept} lost={lost} end={}",
            bounded.disposition()
        ));
        for path in bounded.steps() {
            sim.record(path);
        }
        Ok(())
    })
}

band_family::band_family! {
    overflow_keeps_the_terminal_state_band_00_t36 => overflow_keeps_the_terminal_state, 0;
    overflow_keeps_the_terminal_state_band_01_t36 => overflow_keeps_the_terminal_state, 1;
    overflow_keeps_the_terminal_state_band_02_t36 => overflow_keeps_the_terminal_state, 2;
    overflow_keeps_the_terminal_state_band_03_t36 => overflow_keeps_the_terminal_state, 3;
    overflow_keeps_the_terminal_state_band_04_t36 => overflow_keeps_the_terminal_state, 4;
    overflow_keeps_the_terminal_state_band_05_t36 => overflow_keeps_the_terminal_state, 5;
    overflow_keeps_the_terminal_state_band_06_t36 => overflow_keeps_the_terminal_state, 6;
    overflow_keeps_the_terminal_state_band_07_t36 => overflow_keeps_the_terminal_state, 7;
    overflow_keeps_the_terminal_state_band_08_t36 => overflow_keeps_the_terminal_state, 8;
    overflow_keeps_the_terminal_state_band_09_t36 => overflow_keeps_the_terminal_state, 9;
    overflow_keeps_the_terminal_state_band_10_t36 => overflow_keeps_the_terminal_state, 10;
    overflow_keeps_the_terminal_state_band_11_t36 => overflow_keeps_the_terminal_state, 11;
    overflow_keeps_the_terminal_state_band_12_t36 => overflow_keeps_the_terminal_state, 12;
    overflow_keeps_the_terminal_state_band_13_t36 => overflow_keeps_the_terminal_state, 13;
    overflow_keeps_the_terminal_state_band_14_t36 => overflow_keeps_the_terminal_state, 14;
    overflow_keeps_the_terminal_state_band_15_t36 => overflow_keeps_the_terminal_state, 15;
}
