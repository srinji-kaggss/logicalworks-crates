//! The edge extraction's tests: identity, membership, scope and the schema refusals (INV-DEP-7, INV-DEP-10).

use std::path::{Path, PathBuf};

use super::collect::manifest_path;
use super::edges::lexical_join;
use super::faults::TestResult;
use super::*;

#[test]
fn missing_workspace_package_record_is_a_schema_refusal() {
    let input = r#"{"packages":[],"workspace_members":["path+file:///repo#app@0.1.0"]}"#;
    assert!(
        matches!(
            parse(input),
            Err(MetadataError::Schema(message)) if message.contains("has no package record")
        ),
        "a missing workspace member record must be a schema refusal"
    );
}

#[test]
fn a_valid_empty_workspace_remains_valid() -> TestResult {
    assert!(
        parse(r#"{"packages":[],"workspace_members":[]}"#)?.is_empty(),
        "an empty virtual workspace is a valid metadata subject"
    );
    Ok(())
}

#[test]
fn duplicate_package_ids_and_manifest_directories_are_schema_refusals() {
    let duplicate_id = r#"{"packages":[
      {"id":"same","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[]},
      {"id":"same","name":"other","manifest_path":"/repo/other/Cargo.toml","dependencies":[]}
    ],"workspace_members":["same"]}"#;
    assert!(
        matches!(
            parse(duplicate_id),
            Err(MetadataError::Schema(message)) if message.contains("duplicate Cargo package id")
        ),
        "duplicate package ids must be refused before graph extraction"
    );

    let duplicate_dir = r#"{"packages":[
      {"id":"first","name":"first","manifest_path":"/repo/Cargo.toml","dependencies":[]},
      {"id":"second","name":"second","manifest_path":"/repo/Cargo.toml","dependencies":[]}
    ],"workspace_members":["first","second"]}"#;
    assert!(
        matches!(
            parse(duplicate_dir),
            Err(MetadataError::Schema(message)) if message.contains("multiple workspace packages")
        ),
        "two members cannot claim the same manifest directory"
    );
}

#[test]
fn duplicate_workspace_ids_and_missing_member_manifests_are_refused() {
    let duplicate_member = r#"{"packages":[
      {"id":"app","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[]}
    ],"workspace_members":["app","app"]}"#;
    assert!(
        matches!(
            parse(duplicate_member),
            Err(MetadataError::Schema(message)) if message.contains("duplicate Cargo workspace member id")
        ),
        "a repeated workspace member id must be refused"
    );

    let missing_manifest = r#"{"packages":[
      {"id":"app","name":"app","dependencies":[]}
    ],"workspace_members":["app"]}"#;
    assert!(
        matches!(
            parse(missing_manifest),
            Err(MetadataError::Schema(message)) if message.contains("has no manifest_path")
        ),
        "a workspace member without a manifest identity must be refused"
    );
}

#[test]
fn a_member_path_with_a_different_package_identity_is_refused() {
    let input = r#"{
      "packages": [
        {"id":"app","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[
          {"name":"renamed","source":null,"req":"*","kind":null,"optional":false,"path":"helper"}
        ]},
        {"id":"helper","name":"helper","manifest_path":"/repo/helper/Cargo.toml","dependencies":[]}
      ],"workspace_members":["app","helper"]
    }"#;
    assert!(
        matches!(
            parse(input),
            Err(MetadataError::Schema(message)) if message.contains("resolves to workspace package")
        ),
        "a path target's package identity must agree with its member record"
    );
}

/// The manifest path must be absolute before it reaches the child.
///
/// `read_metadata` spawns Cargo with its working directory set to the
/// repository root, and Cargo resolves `--manifest-path` against *that*
/// directory. A relative manifest path is therefore applied twice — once
/// against this process's working directory and once against the child's —
/// and names a manifest that does not exist, so the gate reports a
/// repository the operator named as missing. The check is lexical: it holds
/// whatever the working directory happens to be, and for the same reason a
/// root that is already absolute must reach Cargo unchanged.
#[test]
fn a_manifest_path_is_resolved_before_cargo_can_reinterpret_it() -> TestResult {
    let relative = manifest_path(Path::new("crates/lgwks-deps"))?;
    assert!(
        relative.is_absolute(),
        "a relative root must be resolved here: {}",
        relative.display()
    );
    assert!(
        relative.ends_with("crates/lgwks-deps/Cargo.toml"),
        "the resolved path is the root's own manifest: {}",
        relative.display()
    );
    let root = std::env::current_dir()?.join("crates/lgwks-deps");
    assert_eq!(
        manifest_path(&root)?,
        root.join("Cargo.toml"),
        "an absolute root must reach Cargo as the same manifest"
    );
    Ok(())
}

#[test]
fn includes_inactive_optional_and_dev_edges() -> TestResult {
    let input = r#"{
      "packages": [{
        "id": "path+file:///repo#app@0.1.0",
        "name": "app",
        "repository": "https://example.invalid/app",
        "manifest_path": "/repo/Cargo.toml",
        "dependencies": [
          {"name":"serde","source":"registry+https://github.com/rust-lang/crates.io-index","req":"^1","kind":null,"optional":true,"path":null},
          {"name":"proptest","source":"registry+https://github.com/rust-lang/crates.io-index","req":"^1.6","kind":"dev","optional":false,"path":null}
        ]
      }],
      "workspace_members": ["path+file:///repo#app@0.1.0"]
    }"#;
    let edges = parse(input)?;
    assert_eq!(edges.len(), 2);
    let serde = edges
        .iter()
        .find(|edge| edge.package == "serde")
        .ok_or("the serde edge should have been extracted")?;
    let proptest = edges
        .iter()
        .find(|edge| edge.package == "proptest")
        .ok_or("the proptest edge should have been extracted")?;
    assert!(serde.optional);
    assert!(!serde.workspace);
    assert_eq!(proptest.kind, DependencyKind::Dev);
    Ok(())
}

/// A sparse-registry source names the same registry class as the Git
/// index, so an admission comparing registry identity is not defeated by
/// Cargo's transport spelling.
#[test]
fn a_sparse_registry_source_is_classified_as_a_registry() -> TestResult {
    let input = r#"{
      "packages": [{
        "id": "app",
        "name": "app",
        "repository": null,
        "manifest_path": "/repo/Cargo.toml",
        "dependencies": [
          {"name":"serde","source":"sparse+https://index.crates.io/","req":"^1","kind":null,"optional":false,"path":null}
        ]
      }],
      "workspace_members": ["app"]
    }"#;
    let edges = parse(input)?;
    let edge = edges
        .first()
        .ok_or("the sparse registry edge must be extracted")?;
    assert_eq!(
        edge.source.class(),
        "registry",
        "a sparse+ source is a registry, not an unknown scheme"
    );
    assert_eq!(edge.source.detail(), "sparse+https://index.crates.io/");
    Ok(())
}

#[test]
fn ignores_non_workspace_transitives() -> TestResult {
    let input = r#"{
      "packages": [
        {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
         "manifest_path":"/repo/Cargo.toml","dependencies":[]},
        {"id":"registry+x#serde@1.0.0","name":"serde","repository":null,"dependencies":[]}
      ],
      "workspace_members": ["path+file:///repo#app@0.1.0"]
    }"#;
    assert!(parse(input)?.is_empty());
    Ok(())
}

/// An outside path package sharing a member's name is not that member
/// (issue #143 R13).
///
/// `app` depends on a `helper` that lives outside the workspace while the
/// workspace has its own member also called `helper`. Name membership
/// alone would mark the outside edge internal and let `audit_direct`
/// skip external admission for it — a gate bypass. The edge must be
/// external, carry its path source, and inherit no member repository.
#[test]
fn an_outside_path_package_with_a_member_name_is_not_a_member() -> TestResult {
    let input = r#"{
      "packages": [
        {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
         "manifest_path":"/repo/Cargo.toml",
         "dependencies":[
           {"name":"helper","source":null,"req":"*","kind":null,"optional":false,"path":"../outside/helper"}
         ]},
        {"id":"path+file:///repo/helper#helper@0.1.0","name":"helper",
         "repository":"https://example.invalid/helper",
         "manifest_path":"/repo/helper/Cargo.toml","dependencies":[]},
        {"id":"path+file:///outside/helper#helper@0.2.0","name":"helper",
         "repository":null,
         "manifest_path":"/outside/helper/Cargo.toml","dependencies":[]}
      ],
      "workspace_members": ["path+file:///repo#app@0.1.0","path+file:///repo/helper#helper@0.1.0"]
    }"#;
    let edges = parse(input)?;
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.consumer, "app");
    assert_eq!(edge.package, "helper");
    assert!(
        !edge.workspace,
        "an outside path target must not inherit membership from its name"
    );
    assert_eq!(
        edge.source,
        DependencySource::Path("../outside/helper".to_owned())
    );
    assert_eq!(
        edge.target_repository, None,
        "an outside target must not inherit the member's repository"
    );
    Ok(())
}

/// The positive control: a path edge that resolves into a member's
/// directory stays internal and keeps that member's repository.
#[test]
fn a_path_edge_resolving_into_a_member_directory_is_a_member() -> TestResult {
    let input = r#"{
      "packages": [
        {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
         "manifest_path":"/repo/Cargo.toml",
         "dependencies":[
           {"name":"helper","source":null,"req":"*","kind":null,"optional":false,"path":"helper"}
         ]},
        {"id":"path+file:///repo/helper#helper@0.1.0","name":"helper",
         "repository":"https://example.invalid/helper",
         "manifest_path":"/repo/helper/Cargo.toml","dependencies":[]}
      ],
      "workspace_members": ["path+file:///repo#app@0.1.0","path+file:///repo/helper#helper@0.1.0"]
    }"#;
    let edges = parse(input)?;
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert!(
        edge.workspace,
        "a path target inside a member directory is that member"
    );
    assert_eq!(
        edge.target_repository,
        Some("https://example.invalid/helper".to_owned())
    );
    Ok(())
}

/// Issue #207: a dependency declared only under
/// `[target.'cfg(unix)'.dependencies]` is an edge like any other, carrying
/// its scope, so the register can approve or refuse it.
#[test]
fn a_target_scoped_dependency_is_classified_as_an_edge() -> TestResult {
    let input = r#"{
      "packages": [
        {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
         "manifest_path":"/repo/Cargo.toml",
         "dependencies":[
           {"name":"nix","source":"registry+https://github.com/rust-lang/crates.io-index",
            "req":"0.29","kind":null,"optional":false,"path":null,"target":"cfg(unix)"}
         ]}
      ],
      "workspace_members": ["path+file:///repo#app@0.1.0"]
    }"#;
    let edges = parse(input)?;
    let edge = edges.first().ok_or("the target-scoped edge is visible")?;
    assert_eq!(
        (edge.package(), edge.target()),
        ("nix", Some("cfg(unix)")),
        "the edge keeps the target scope it was declared under"
    );
    Ok(())
}

/// A relative path that escapes the root it resolves against has no
/// location to compare: the join refuses rather than naming a directory
/// outside the filesystem.
#[test]
fn a_path_escaping_its_root_has_no_join() -> TestResult {
    assert_eq!(
        lexical_join(Path::new("/repo"), "sub/../helper"),
        Some(PathBuf::from("/repo/helper"))
    );
    assert_eq!(lexical_join(Path::new("/repo"), "../../escape"), None);
    // Cargo currently reports path keys absolute: the key stands alone.
    assert_eq!(
        lexical_join(Path::new("/repo/app"), "/repo/helper"),
        Some(PathBuf::from("/repo/helper"))
    );
    Ok(())
}

/// A declaring member with no manifest directory has nothing to resolve a
/// path edge against. Before #158 the edge was classified external; now
/// the member itself is an identity defect, so the whole document is
/// refused rather than any edge being classified from it.
#[test]
fn a_path_edge_whose_declarer_has_no_manifest_is_refused() {
    let input = r#"{
      "packages": [
        {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
         "manifest_path":null,
         "dependencies":[
           {"name":"helper","source":null,"req":"*","kind":null,"optional":false,"path":"helper"}
         ]},
        {"id":"path+file:///repo/helper#helper@0.1.0","name":"helper",
         "repository":"https://example.invalid/helper",
         "manifest_path":"/repo/helper/Cargo.toml","dependencies":[]}
      ],
      "workspace_members": ["path+file:///repo#app@0.1.0","path+file:///repo/helper#helper@0.1.0"]
    }"#;
    assert!(
        matches!(
            parse(input),
            Err(MetadataError::Schema(message)) if message.contains("has no manifest_path")
        ),
        "a member without a manifest must be refused, not classified"
    );
}
