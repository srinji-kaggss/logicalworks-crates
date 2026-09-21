//! The root README quickstart, kept here so the workspace gate compiles it.
//!
//! `README.md` at the repository root is not a crate README, so nothing
//! type-checks its code blocks. This example is that snippet verbatim, and the
//! `README quickstart` job in CI fails if the two drift apart. An API change
//! therefore breaks the build instead of silently invalidating the first code a
//! new user copies.

use std::io::Write;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{EffectIdentity, EffectScope};
use lgwks_bot::{Bot, GrantSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339();
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: capability-gated actors. Grants are required to build, not only to
    // run, and so is an effect scope: it names the run, the environment that run
    // acts on, and the journal a dispatch is written to before the effect leaves
    // the process. `build` returns a typed error; the caller decides what to do
    // with it.
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    let effects = EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
            environment,
            FlowRevision::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    );

    let bot = Bot::builder("demo")
        .with_effects(effects)
        .build(&GrantSet::empty())?;
    let _ = bot;

    Ok(())
}
