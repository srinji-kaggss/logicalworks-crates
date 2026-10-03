//! Shared in-memory effect scope for a bot under test or measurement.
//!
//! A bot has no dispatch path without a scope: the scope names the run, the
//! environment the run acts on, and the journal a dispatch is written to before
//! it leaves the process. One definition, because the fixture's only job is to
//! be a valid scope and two copies would be free to drift; an in-memory journal,
//! because these callers are about a different boundary than durability is.
//!
//! Included by path:
//! `#[path = "support/effects.rs"] mod effects;` from a target at the crate's
//! `tests/` root, and `#[path = "../tests/support/effects.rs"] mod effects;`
//! from an example.

use std::error::Error;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{EffectIdentity, EffectScope};

/// The effect scope a bot under test or measurement runs under.
///
/// A fresh identity per call, so no caller inherits another's held attempts.
pub(crate) fn memory_scope() -> Result<EffectScope, Box<dyn Error>> {
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
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
    ))
}
