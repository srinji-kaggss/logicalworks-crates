//! Matched-workload measurement: bounded vs uncapped fan-out.
//!
//! Runs the same 512 one-millisecond tasks under three concurrency ceilings and
//! prints peak in-flight count and wall time, so the `join_all_bounded` limit is
//! visible rather than asserted. This is a measurement harness, not a pass/fail
//! gate: wall time varies with the machine, the peak does not.
//!
//! ```text
//! cargo run -p lgwks_bot --example bounded_fanout
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time::{Duration, sleep};

const TASKS: u32 = 512;

fn main() {
    let runtime = lgwks_bot::Runtime::new().expect("runtime");
    for &(label, limit) in &[("limit=4", 4usize), ("limit=64", 64), ("uncapped", 1024)] {
        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();

        let futures = (0..TASKS).map(|index| {
            let current = Arc::clone(&current);
            let peak = Arc::clone(&peak);
            async move {
                let now = current.fetch_add(1, SeqCst) + 1;
                peak.fetch_max(now, SeqCst);
                sleep(Duration::from_millis(1)).await;
                current.fetch_sub(1, SeqCst);
                index
            }
        });

        let outputs = runtime.block_on(join_all_bounded(limit, futures));
        assert_eq!(outputs.len(), TASKS as usize);
        assert_eq!(outputs[0], 0, "input order is preserved");
        println!(
            "{label:>8}: peak_in_flight={:>4} elapsed={:?}",
            peak.load(SeqCst),
            started.elapsed()
        );
    }
}
