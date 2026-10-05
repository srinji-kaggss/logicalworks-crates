//! Admitted-capability policy through the public metadata + admission API.
//!
//! Issue #158 A2: adding an upstream feature, turning defaults on, flipping
//! optionality or changing a target scope must move the verdict, not ride an
//! unchanged class/origin approval. These fixtures build the edge through
//! `metadata::parse` and audit it with `audit_direct`, and each refusal names
//! its dimension.

use std::error::Error;

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

type TestResult = Result<(), Box<dyn Error>>;

/// The six authored dimensions, defaulting to the `plain` baseline.
#[derive(Default, Clone)]
struct Dims {
    features: Vec<&'static str>,
    uses_default_features: bool,
    optional: bool,
    target: Option<&'static str>,
}

impl Dims {
    /// The `plain` baseline: no features, defaults on, mandatory, unconditional.
    fn baseline() -> Self {
        Self {
            uses_default_features: true,
            ..Self::default()
        }
    }
}

/// One `app` → `engine` edge with the given dimensions, through the parser.
fn edge(dims: &Dims) -> Result<Vec<DirectEdge>, Box<dyn Error>> {
    let features = dims
        .features
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(",");
    let target = dims
        .target
        .map_or("null".to_owned(), |value| format!("\"{value}\""));
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"engine","source":"registry+https://github.com/rust-lang/crates.io-index","req":"1.0","kind":null,"rename":null,"optional":{optional},"uses_default_features":{default},"features":[{features}],"target":{target},"path":null}}]}},{{"id":"lic-engine","name":"engine","license":"MIT OR Apache-2.0","manifest_path":"/dep/Cargo.toml","dependencies":[]}}],"workspace_members":["app"]}}"#,
        optional = dims.optional,
        default = dims.uses_default_features,
        features = features,
        target = target,
    );
    Ok(metadata::parse(&document)?)
}

/// A register approving `engine` with the given authored policy lines.
fn register(policy: &str) -> Result<Contract, Box<dyn Error>> {
    let text = format!(
        concat!(
            "[policy]\nschema = 2\nenforce = true\n\n",
            "[[approved]]\n",
            "crate = \"engine\"\n",
            "tier = \"boundary\"\n",
            "version = \"1.0\"\n",
            "owner = \"app\"\n",
            "capability = \"engine.core\"\n",
            "license = \"MIT OR Apache-2.0\"\n",
            "source = \"registry\"\n",
            "{policy}",
            "allowed_consumers = \"app\"\n",
            "allowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\n",
            "approved_on = \"2026-09-30\"\n",
            "review = \"tests/feature_policy.rs\"\n",
        ),
        policy = policy,
    );
    Ok(Contract::parse(&text)?)
}

/// The first refusal, or a test error naming the empty admission.
fn first(refusals: &[Refusal]) -> Result<&Refusal, String> {
    refusals
        .first()
        .ok_or_else(|| "expected a refusal, got an empty admission".to_owned())
}

#[test]
fn defaults_off_is_admitted_and_defaults_on_is_refused() -> TestResult {
    let register = register("uses_default_features = \"false\"\n")?;
    let approved = edge(&Dims {
        uses_default_features: false,
        ..Dims::default()
    })?;
    assert!(
        audit_direct(&approved, &register).is_empty(),
        "the exact authored default-features bit must pass"
    );

    let widened = edge(&Dims::baseline())?;
    assert!(
        matches!(
            first(&audit_direct(&widened, &register))?,
            Refusal::DefaultFeaturesDrift {
                approved: false,
                declared: true,
                ..
            }
        ),
        "turning defaults on must be a DefaultFeaturesDrift, not a pass"
    );
    Ok(())
}

#[test]
fn an_unapproved_feature_is_refused_and_a_required_feature_must_be_present() -> TestResult {
    let allowed = register("features = \"std\"\n")?;
    let admitted = edge(&Dims {
        features: vec!["std"],
        ..Dims::baseline()
    })?;
    assert!(audit_direct(&admitted, &allowed).is_empty());

    let widened = edge(&Dims {
        features: vec!["std", "extra"],
        ..Dims::baseline()
    })?;
    assert!(
        matches!(
            first(&audit_direct(&widened, &allowed))?,
            Refusal::FeatureDrift { declared, .. } if declared == "std,extra"
        ),
        "an enabled feature outside the admitted set is a FeatureDrift"
    );

    let required = register("features = \"std,extra\"\nrequired_features = \"std\"\n")?;
    let missing = edge(&Dims {
        features: vec!["extra"],
        ..Dims::baseline()
    })?;
    assert!(
        matches!(
            first(&audit_direct(&missing, &required))?,
            Refusal::FeatureDrift { .. }
        ),
        "a missing required feature must be a FeatureDrift"
    );
    Ok(())
}

#[test]
fn optionality_and_target_scope_are_policed() -> TestResult {
    let mandatory = register("optional = \"false\"\n")?;
    let optional = edge(&Dims {
        optional: true,
        ..Dims::baseline()
    })?;
    assert!(
        matches!(
            first(&audit_direct(&optional, &mandatory))?,
            Refusal::OptionalityDrift {
                approved: false,
                declared: true,
                ..
            }
        ),
        "an optional edge against a mandatory policy is an OptionalityDrift"
    );

    let scoped = register("target = \"cfg(unix)\"\n")?;
    let unix = edge(&Dims {
        target: Some("cfg(unix)"),
        ..Dims::baseline()
    })?;
    assert!(audit_direct(&unix, &scoped).is_empty());
    let unconditional = edge(&Dims::baseline())?;
    assert!(
        matches!(
            first(&audit_direct(&unconditional, &scoped))?,
            Refusal::TargetDrift { .. }
        ),
        "an unconditional declaration against a scoped policy is a TargetDrift"
    );

    let unconditional_policy = register("target = \"\"\n")?;
    assert!(
        matches!(
            first(&audit_direct(&unix, &unconditional_policy))?,
            Refusal::TargetDrift { .. }
        ),
        "a scoped declaration against an unconditional policy is a TargetDrift"
    );
    Ok(())
}

/// The dimension the entry does not constrain is grandfathered rather than
/// silently refused: an entry with no feature key admits the baseline features.
#[test]
fn an_unauthored_dimension_is_grandfathered() -> TestResult {
    let register = register("")?;
    let any = edge(&Dims {
        features: vec!["extra"],
        optional: true,
        target: Some("cfg(windows)"),
        uses_default_features: false,
    })?;
    assert!(
        audit_direct(&any, &register).is_empty(),
        "an absent dimension must not refuse a value it never named"
    );
    Ok(())
}

/// The committed Bevy delta: the real `lgwks_deps` manifest declaration is
/// admitted, and widening it (defaults on or an extra feature) is refused.
#[test]
fn the_intended_bevy_delta_is_admitted_and_a_widening_is_refused() -> TestResult {
    let register = register("uses_default_features = \"false\"\nfeatures = \"std\"\n")?;
    let real = edge(&Dims {
        features: vec!["std"],
        uses_default_features: false,
        ..Dims::default()
    })?;
    assert!(
        audit_direct(&real, &register).is_empty(),
        "the manifest's default-features=false, features=[std] must be admitted"
    );

    let widened_feature = edge(&Dims {
        features: vec!["std", "multi_threaded"],
        uses_default_features: false,
        ..Dims::default()
    })?;
    assert!(matches!(
        first(&audit_direct(&widened_feature, &register))?,
        Refusal::FeatureDrift { .. }
    ));

    let widened_defaults = edge(&Dims {
        features: vec!["std"],
        uses_default_features: true,
        ..Dims::default()
    })?;
    assert!(matches!(
        first(&audit_direct(&widened_defaults, &register))?,
        Refusal::DefaultFeaturesDrift { .. }
    ));
    Ok(())
}
