//! Sync-surface acceptance without the async engine.
//!
//! Compiled only under `--no-default-features`: proves the serializable spec
//! surface (`BotSpec` JSON round-trip) survives with no `tokio` in the tree.
#![cfg(not(feature = "rt"))]

use lgwks_bot::BotSpec;
use lgwks_bot::spec::{ActionSpec, ChainSpec};

/// The tests here cross `json::Error` and `BotError`, so they report
/// `Box<dyn Error>` and propagate each with `?`.
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn no_default_features_preserve_the_sync_spec_surface() -> TestResult {
    // The specs are `#[non_exhaustive]`, so this consumer builds them through
    // `new` rather than a struct literal — the same path a downstream crate
    // must take.
    let spec = BotSpec::new(
        "minimal",
        vec![ChainSpec::new(
            "source::event",
            "owner/repo",
            vec![("changed".into(), ActionSpec::new("notify::log", "stdout"))],
        )],
    );

    let json = spec.to_json()?;
    let decoded = BotSpec::from_json(&json)?;
    assert_eq!(decoded.name, "minimal");
    assert_eq!(decoded.chains[0].on[0].0, "changed");
    Ok(())
}
