//! The root README quickstart, kept here so the workspace gate compiles it.
//!
//! `README.md` at the repository root is not a crate README, so nothing
//! type-checks its code blocks. This example is that snippet verbatim (the
//! `//!` lines stripped), and the `README quickstart` job in CI fails if the
//! two drift apart: an API change breaks the build instead of silently
//! invalidating the first code a new user copies. The orchestration is a
//! `script!` flow, the supported interface (#526).
//!
//! The retry guarantee fires here, printed and asserted: a failure worth
//! retrying is retried under the same key. The deadline overrun, the
//! capability refusal and the kill-and-resume fire in
//! `examples/guarantees.rs`, the complete first-run demo, which needs the
//! `ephemeral` feature to mint the durable run's identity.

use lgwks_bot::script::{FlowError, Scope, Tenant};
use std::error::Error;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

/// A stand-in for a backend having a bad minute: the first two calls answer
/// "busy", the third succeeds. The count is shared state the flow reaches
/// through a reference, the way a real client handle rides in.
struct Flaky(AtomicU8);

impl Flaky {
    /// Square `number`, or refuse transiently twice before succeeding.
    async fn call(&self, number: u64) -> Result<u64, FlowError> {
        if self.0.fetch_add(1, Ordering::Relaxed) < 2 {
            lgwks_std::trace::debug!("the backend was busy; the flow will retry");
            return Err(FlowError::transient("the backend was busy"));
        }
        Ok(number.saturating_mul(number))
    }
}

lgwks_bot::script! {
    /// Square every number; a failure worth retrying is retried under the
    /// same key, every attempt holding a one-second budget.
    flow squares(flaky: &Flaky, numbers: Vec<u64>) -> u64:
        let squared = each number in numbers:
            retry up to 3 times, waiting 10ms:
                within 1s:
                    flaky.call(number).await?
        give back squared.iter().sum()
}

fn main() -> Result<(), Box<dyn Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    let flaky = Flaky(AtomicU8::new(0));
    let scope = Scope::root(Tenant::new("demo")?);
    let total = lgwks_bot::block_on(squares(&scope, &flaky, vec![3]))?;
    writeln!(out, "sum of squares: {total}")?;
    assert_eq!(total, 9, "3 squared after two transient refusals");
    assert_eq!(flaky.0.load(Ordering::Relaxed), 3, "three attempts");

    Ok(())
}
