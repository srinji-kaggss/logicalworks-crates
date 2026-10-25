//! The edge a stale package comes through, on the lock shapes Cargo writes
//! and on seeded graphs checked against a closure computed another way.

use std::collections::BTreeSet;
use std::error::Error;

use lgwks_std::seeded::Seeded;

use super::{Graph, Node, split_entry};
use crate::freshness_cmd::SimError;

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The registry source every fixture package carries.
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// A workspace member or path dependency.
fn local(name: &str, dependencies: &[&str]) -> Node {
    Node {
        name: name.to_owned(),
        version: "0.1.0".to_owned(),
        source: None,
        dependencies: dependencies
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect(),
    }
}

/// A registry package.
fn registry(name: &str, version: &str, dependencies: &[&str]) -> Node {
    Node {
        source: Some(REGISTRY.to_owned()),
        version: version.to_owned(),
        ..local(name, dependencies)
    }
}

#[test]
fn entries_split_into_name_version_and_source() {
    assert_eq!(split_entry("serde"), ("serde", None, None), "bare name");
    assert_eq!(
        split_entry("syn 2.0.1"),
        ("syn", Some("2.0.1"), None),
        "name and version"
    );
    assert_eq!(
        split_entry("syn 2.0.1 (registry+https://example.test/index)"),
        (
            "syn",
            Some("2.0.1"),
            Some("registry+https://example.test/index")
        ),
        "name, version and source"
    );
}

#[test]
fn a_package_a_registry_dependency_pulls_in_comes_through_that_dependency() {
    // Issue #702: bevy_ecs is stale because lgwks_bot pulls it in.
    let graph = Graph::new(vec![
        local("app", &["lgwks_bot", "serde"]),
        registry("bevy_ecs", "0.19.1", &["bevy_ptr"]),
        registry("bevy_ptr", "0.19.1", &[]),
        registry("lgwks_bot", "3.1.0", &["bevy_ecs"]),
        registry("serde", "1.0.0", &[]),
    ]);
    assert!(graph.via(0).is_empty(), "a member comes through nothing");
    assert_eq!(
        graph.via(1),
        ["lgwks_bot"],
        "bevy_ecs comes through lgwks_bot"
    );
    assert_eq!(
        graph.via(2),
        ["lgwks_bot"],
        "so does what bevy_ecs pulls in"
    );
    assert_eq!(
        graph.via(3),
        ["lgwks_bot"],
        "a direct dependency is its own edge"
    );
    assert_eq!(graph.via(4), ["serde"], "serde is declared by the member");
}

#[test]
fn a_package_two_directs_reach_names_both_sorted() {
    let graph = Graph::new(vec![
        local("app", &["zeta", "alpha"]),
        registry("alpha", "1.0.0", &["shared"]),
        registry("shared", "1.0.0", &[]),
        registry("zeta", "1.0.0", &["shared"]),
    ]);
    assert_eq!(graph.via(2), ["alpha", "zeta"], "both directs, sorted");
}

#[test]
fn workspace_members_and_path_dependencies_are_looked_through() {
    let graph = Graph::new(vec![
        local("app", &["lib", "tool"]),
        local("lib", &["serde"]),
        local("tool", &["lib"]),
        registry("serde", "1.0.0", &["serde_derive"]),
        registry("serde_derive", "1.0.0", &[]),
    ]);
    assert_eq!(graph.via(3), ["serde"], "lib declares serde");
    assert_eq!(graph.via(4), ["serde"], "serde_derive comes through serde");
    assert!(graph.via(1).is_empty(), "lib is the reader's own");
}

#[test]
fn two_versions_of_one_name_are_told_apart_by_version() {
    let graph = Graph::new(vec![
        local("app", &["rand 0.8.5", "old"]),
        registry("old", "1.0.0", &["rand 0.7.3"]),
        registry("rand", "0.7.3", &[]),
        registry("rand", "0.8.5", &[]),
    ]);
    assert_eq!(graph.via(2), ["old"], "0.7.3 is reached through old");
    assert_eq!(
        graph.via(3),
        ["rand 0.8.5"],
        "0.8.5 is declared directly, so its version tells it from 0.7.3"
    );
}

#[test]
fn two_directly_declared_versions_of_one_name_each_carry_their_version() {
    let graph = Graph::new(vec![
        local("app", &["rand 0.7.3", "rand 0.8.5"]),
        registry("rand", "0.7.3", &[]),
        registry("rand", "0.8.5", &[]),
    ]);
    assert_eq!(graph.via(1), ["rand 0.7.3"], "the older version");
    assert_eq!(graph.via(2), ["rand 0.8.5"], "the newer version");
}

#[test]
fn a_source_on_an_entry_picks_between_equal_versions() {
    let mut mirror = registry("dup", "1.0.0", &[]);
    mirror.source = Some("registry+https://mirror.test/index".to_owned());
    let graph = Graph::new(vec![
        local("app", &["dup 1.0.0 (registry+https://mirror.test/index)"]),
        registry("dup", "1.0.0", &[]),
        mirror,
    ]);
    assert!(graph.via(1).is_empty(), "the other source is not declared");
    assert_eq!(graph.via(2), ["dup 1.0.0"], "the named source is");
}

#[test]
fn an_edge_to_nowhere_and_a_position_outside_the_graph_reach_nothing() {
    let graph = Graph::new(vec![
        local("app", &["ghost", "real"]),
        registry("real", "1.0.0", &["missing 9.9.9"]),
    ]);
    assert_eq!(graph.via(1), ["real"], "the real edge survives the ghost");
    assert!(graph.via(7).is_empty(), "no such position");
}

#[test]
fn a_cycle_among_registry_packages_terminates() {
    let graph = Graph::new(vec![
        local("app", &["left"]),
        registry("left", "1.0.0", &["right"]),
        registry("right", "1.0.0", &["left"]),
    ]);
    assert_eq!(graph.via(1), ["left"], "left");
    assert_eq!(graph.via(2), ["left"], "right");
}

/// A drawn lock: locals first, then registry packages with edges only to
/// others of the lock, in no particular direction.
fn draw(stream: &mut Seeded) -> Result<Vec<Node>, SimError> {
    let locals = stream.below(3)?.saturating_add(1);
    let registries = stream.below(12)?.saturating_add(1);
    let total = locals.saturating_add(registries);
    let mut nodes = Vec::new();
    for ordinal in 0..total {
        let mut dependencies = Vec::new();
        let fan_out = stream.below(4)?;
        for _ in 0..fan_out {
            let target = stream.below(total)?;
            dependencies.push(format!("pkg{target}"));
        }
        nodes.push(if ordinal < locals {
            local(&format!("pkg{ordinal}"), &[])
        } else {
            registry(&format!("pkg{ordinal}"), "1.0.0", &[])
        });
        if let Some(node) = nodes.last_mut() {
            node.dependencies = dependencies;
        }
    }
    Ok(nodes)
}

/// The reference: for each registry package, the directs from which a walk
/// over registry edges reaches it, found by closing the relation to a fixed
/// point instead of by a search from each direct.
fn reference(nodes: &[Node]) -> Vec<BTreeSet<String>> {
    let count = nodes.len();
    let position = |entry: &str| nodes.iter().position(|node| node.name == entry);
    let mut reaches = vec![BTreeSet::new(); count];
    for (at, node) in nodes.iter().enumerate() {
        if node.source.is_some() {
            reaches[at].insert(at);
        }
    }
    loop {
        let mut changed = false;
        for (at, node) in nodes.iter().enumerate() {
            if node.source.is_none() {
                continue;
            }
            for entry in &node.dependencies {
                let Some(target) = position(entry) else {
                    continue;
                };
                if nodes[target].source.is_none() {
                    continue;
                }
                let inherited: Vec<usize> = reaches[at].iter().copied().collect();
                for origin in inherited {
                    changed |= reaches[target].insert(origin);
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut directs = BTreeSet::new();
    for node in nodes.iter().filter(|node| node.source.is_none()) {
        for entry in &node.dependencies {
            if let Some(target) = position(entry)
                && nodes[target].source.is_some()
            {
                directs.insert(target);
            }
        }
    }
    (0..count)
        .map(|at| {
            reaches[at]
                .iter()
                .filter(|origin| directs.contains(*origin))
                .map(|origin| nodes[*origin].name.clone())
                .collect()
        })
        .collect()
}

#[test]
fn the_graph_matches_a_fixed_point_closure_over_seeds() -> TestResult {
    for seed in 0..300_u64 {
        let mut stream = Seeded::from_seed(seed);
        let nodes = draw(&mut stream)?;
        let graph = Graph::new(nodes.clone());
        let expected = reference(&nodes);
        for (at, expected_labels) in expected.iter().enumerate() {
            let got: BTreeSet<String> = graph.via(at).iter().cloned().collect();
            assert_eq!(
                &got, expected_labels,
                "seed {seed}: package {at} comes through the wrong directs"
            );
            assert!(
                graph.via(at).windows(2).all(|pair| pair[0] < pair[1]),
                "seed {seed}: package {at}'s directs are not sorted and distinct"
            );
        }
    }
    Ok(())
}
