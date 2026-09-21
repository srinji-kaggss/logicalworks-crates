//! The root README quickstart, kept here so the workspace gate compiles it.
//!
//! `README.md` at the repository root is not a crate README, so nothing
//! type-checks its code blocks. This example is that snippet verbatim, and the
//! `README quickstart` job in CI fails if the two drift apart. An API change
//! therefore breaks the build instead of silently invalidating the first code a
//! new user copies.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339();
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: capability-gated actors. Grants are required to build, not only to run.
    // `build` returns a typed error; the caller decides what to do with it.
    let bot = lgwks_bot::Bot::builder("demo").build(&lgwks_bot::GrantSet::empty())?;
    let _ = bot;

    Ok(())
}
