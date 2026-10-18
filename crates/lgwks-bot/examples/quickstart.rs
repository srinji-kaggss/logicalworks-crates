//! The root README quickstart, kept here so the workspace gate compiles it.
//!
//! `README.md` at the repository root is not a crate README, so nothing
//! type-checks its code blocks. This example is that snippet verbatim, and the
//! `README quickstart` job in CI fails if the two drift apart. An API change
//! therefore breaks the build instead of silently invalidating the first code a
//! new user copies. The orchestration is a `script!` flow, the supported
//! interface (#526).

use std::error::Error;
use std::io::Write;

use lgwks_bot::script::{Scope, Tenant};

lgwks_bot::script! {
    /// Square every number, as many at once as the machine sustains; each
    /// attempt gets a second and a failure worth retrying is retried under
    /// the same key.
    flow squares(numbers: Vec<u64>) -> u64:
        let squared = each number in numbers:
            retry up to 3 times, waiting 10ms:
                within 1s:
                    number.saturating_mul(number)
        give back squared.iter().sum()
}

fn main() -> Result<(), Box<dyn Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339()?;
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: orchestration is a flow. Every flow takes the scope of the tenant it
    // runs for; the fan-out is bounded, every attempt has a deadline, and a
    // retry spends from a budget the whole run shares.
    let scope = Scope::root(Tenant::new("demo")?);
    let total = lgwks_bot::block_on(squares(&scope, vec![1, 2, 3]))?;
    writeln!(out, "sum of squares: {total}")?;
    assert_eq!(total, 14, "1 + 4 + 9");

    Ok(())
}
