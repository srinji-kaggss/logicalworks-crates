#![cfg(not(feature = "rt"))]

use lgwks_bot::BotSpec;
use lgwks_bot::spec::{ActionSpec, ChainSpec};

#[test]
fn no_default_features_preserve_the_sync_spec_surface() {
    let spec = BotSpec {
        name: "minimal".into(),
        chains: vec![ChainSpec {
            source: "source::event".into(),
            target: "owner/repo".into(),
            on: vec![(
                "changed".into(),
                ActionSpec {
                    domain: "notify::log".into(),
                    target: "stdout".into(),
                },
            )],
        }],
    };

    let json = spec.to_json().expect("spec JSON");
    let decoded = BotSpec::from_json(&json).expect("round trip");
    assert_eq!(decoded.name, "minimal");
    assert_eq!(decoded.chains[0].on[0].0, "changed");
}
