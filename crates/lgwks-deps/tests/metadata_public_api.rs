//! Downstream compile coverage for the read-only metadata API.

use std::error::Error;
use std::path::Path;

use lgwks_deps::metadata::{self, DependencySource};

#[test]
fn downstream_can_read_metadata_edges_and_workspace_members() -> Result<(), Box<dyn Error>> {
    let document = r#"{
      "packages":[{"id":"app","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[
        {"name":"serde","source":"registry+https://github.com/rust-lang/crates.io-index","req":"^1","kind":null,"optional":true,"path":null}
      ]}],
      "workspace_members":["app"]
    }"#;
    let edges = metadata::parse(document)?;
    let edge = edges
        .first()
        .ok_or("a downstream consumer should receive the declared edge")?;
    assert_eq!(
        (edge.consumer(), edge.package(), edge.requirement()),
        ("app", "serde", "^1"),
        "edge exposes its identities and requirement immutably"
    );
    assert_eq!(
        edge.source,
        DependencySource::Registry(
            "registry+https://github.com/rust-lang/crates.io-index".to_owned()
        ),
        "edge exposes the source detail without normalizing it"
    );
    assert!(edge.optional, "edge exposes inactive optionality");

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("the crate manifest must be nested in the workspace")?;
    let members = metadata::workspace_members(workspace_root)?;
    let member = members
        .value()
        .iter()
        .find(|member| member.name() == "lgwks_deps")
        .ok_or("the workspace inventory must include lgwks_deps")?;
    assert_eq!(
        member.manifest_dir, manifest_dir,
        "member identity retains its Cargo manifest directory"
    );
    Ok(())
}
