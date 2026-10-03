//! Shared fixtures for the seeded dependency-policy simulations.
//!
//! Included (via `#[path]`) by `sim_dependency_policy.rs` and
//! `sim_policy_properties.rs`, so the edge/register builders, the verdict model
//! and the generator are written once rather than copied between suites.

#[path = "sim.rs"]
mod sim;

use std::error::Error;

use lgwks_deps::contract::Contract;
use lgwks_deps::metadata::{self, DirectEdge};
use lgwks_deps::{Refusal, audit_direct};

pub use sim::Rng;

/// The registry source the fixture approvals and edges name.
pub const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// What every test in the including suites returns.
pub type TestResult = Result<(), Box<dyn Error>>;

/// An `aliases = "…"` line for an approval, or empty when there is no alias.
pub fn alias_line(alias: Option<&str>) -> String {
    alias.map_or(String::new(), |value| format!("aliases = \"{value}\"\n"))
}

/// A coin from a high bit. The LCG's low bit alternates every step, so a
/// low-bit coin would make every derived set phase-locked.
pub fn coin(rng: &mut Rng) -> bool {
    (rng.next_u64() >> 40) & 1 == 1
}

/// One edge with every authored dimension spelled out, through the parser.
pub fn edge(
    package: &str,
    source: Option<&str>,
    features: &[&str],
    uses_default_features: bool,
    optional: bool,
    target: Option<&str>,
    rename: Option<&str>,
) -> Result<DirectEdge, Box<dyn Error>> {
    let source = source.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let features = features
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(",");
    let target = target.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let rename = rename.map_or("null".to_owned(), |value| format!("\"{value}\""));
    let document = format!(
        r#"{{"packages":[{{"id":"app","name":"app","repository":null,"manifest_path":"/repo/Cargo.toml","dependencies":[{{"name":"{package}","source":{source},"req":"1.0","kind":null,"rename":{rename},"optional":{optional},"uses_default_features":{uses_default_features},"features":[{features}],"target":{target},"path":null}}]}}],"workspace_members":["app"]}}"#
    );
    let mut edges = metadata::parse(&document)?;
    edges
        .pop()
        .ok_or_else(|| "the fixture edge must parse".into())
}

/// A register with one approval for `package` owned by `app`, carrying the given
/// extra lines (an alias and any policy keys).
pub fn register(package: &str, source: &str, extra: &str) -> Result<Contract, Box<dyn Error>> {
    let text = format!(
        concat!(
            "[policy]\nschema = 2\nenforce = true\n\n",
            "[[approved]]\n",
            "crate = \"{package}\"\ntier = \"boundary\"\nversion = \"1.0\"\nowner = \"app\"\n",
            "capability = \"engine.core\"\nsource = \"{source}\"\n{extra}",
            "allowed_consumers = \"app\"\nallowed_kinds = \"normal\"\n",
            "reason = \"The engine supplies a capability the standard library cannot express.\"\n",
            "approved_by = \"reviewer\"\napproved_on = \"2026-09-30\"\nreview = \"tests/support/deps_sim.rs\"\n",
        ),
        package = package,
        source = source,
        extra = extra,
    );
    Ok(Contract::parse(&text)?)
}

/// 0 admitted, 1 any refusal.
fn verdict(refusals: &[Refusal]) -> u8 {
    u8::from(!refusals.is_empty())
}

/// Audit one edge and return its verdict code.
pub fn code_for(register: &Contract, edge: DirectEdge) -> u8 {
    verdict(&audit_direct(&[edge], register))
}
