//! Seeded deterministic replay of the `build` input-digest contract.
//!
//! One seed draws a source tree: how many files, how deep, their names and
//! their bytes. The tree is written under two different temporary roots, the
//! way two checkouts of one repository live at two paths, and every family
//! drives the public `InputDigest` and `directive` against them. The property
//! is the one a build script's implementation digest rests on: the digest is a
//! function of the inputs' root-relative names and bytes, its variables and
//! its domain tag, so the same tree at two paths digests equal and any one
//! byte, name or tag changed diverges.
//!
//! Gated on `feature = "build"`, which is what gates `lgwks_std::build`.

#![cfg(feature = "build")]

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::seeded_bytes::{below, next_bytes};
use crate::seeded_sweep::{SWEEP_SEEDS, fold, initial_trace, seeded_stream};

use lgwks_std::build::{BuildError, InputDigest, directive};
use lgwks_std::hash::Digest;
use lgwks_std::seeded::Seeded;

/// What every family returns: a setup failure is a test failure, not a panic.
type Outcome<T = ()> = Result<T, Box<dyn Error>>;

/// Domain tag every family digests under unless it is testing the tag.
const DOMAIN: &str = "lgwks-std/sim-build/v1";

/// A variable no build environment sets, so it digests as empty everywhere.
const UNSET_VARIABLE: &str = "LGWKS_SIM_BUILD_NEVER_SET";

/// One drawn source tree: `(root-relative name, bytes)` for every file.
type Tree = Vec<(String, Vec<u8>)>;

/// Draws a tree of one to twelve files, up to three directories deep, each up
/// to 2 KiB, from `state`. The index in every file name keeps names unique.
fn draw_tree(state: &mut Seeded) -> Tree {
    let count = below(state, 12).saturating_add(1);
    let mut files = Vec::new();
    for index in 0..count {
        let depth = below(state, 4);
        let mut parts = vec![String::from("src")];
        for level in 0..depth {
            parts.push(format!("d{level}_{}", below(state, 3)));
        }
        parts.push(format!("f{index}.rs"));
        let len = below(state, 2_048);
        files.push((parts.join("/"), next_bytes(state, len)));
    }
    files
}

/// Writes `tree` under `root`.
fn write_tree(root: &Path, tree: &Tree) -> Outcome {
    for file in tree {
        let path = root.join(&file.0);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &file.1)?;
    }
    Ok(())
}

/// The digest of `root`'s `src` tree plus one unset variable, with its directives.
fn digest_of(domain: &str, root: &Path) -> Result<(Digest, String), BuildError> {
    let mut inputs = InputDigest::new(domain, root)?;
    inputs.tree("src")?;
    inputs.variable(UNSET_VARIABLE);
    let mut directives = Vec::new();
    let digest = inputs.finish(&mut directives)?;
    Ok((digest, String::from_utf8_lossy(&directives).into_owned()))
}

/// Writes `tree` into a fresh temporary root and digests it.
fn digest_tree(domain: &str, tree: &Tree) -> Outcome<Digest> {
    let root = tempfile::tempdir()?;
    write_tree(root.path(), tree)?;
    Ok(digest_of(domain, root.path())?.0)
}

/// Runs one seed: the same tree at two roots must agree; returns the digest's trace.
fn build_trace(seed: u64) -> Outcome<u64> {
    let mut state = seeded_stream(seed);
    let tree = draw_tree(&mut state);
    let left = digest_tree(DOMAIN, &tree)?;
    let right = digest_tree(DOMAIN, &tree)?;
    assert_eq!(
        left, right,
        "seed {seed:#018x}: one tree at two roots must digest equal"
    );
    let mut trace = initial_trace();
    let (words, _) = left.as_bytes().as_chunks::<8>();
    for word in words {
        fold(&mut trace, u64::from_le_bytes(*word));
    }
    Ok(trace)
}

#[test]
/// The same tree under two different roots digests equal on every named seed.
fn sim_one_tree_at_two_roots_digests_equal() -> Outcome {
    for seed in SWEEP_SEEDS {
        build_trace(seed)?;
    }
    Ok(())
}

#[test]
/// Flipping one drawn byte of one drawn file changes the digest.
fn sim_a_single_byte_change_diverges() -> Outcome {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let mut tree = draw_tree(&mut state);
        let original = digest_tree(DOMAIN, &tree)?;
        let target = below(&mut state, tree.len());
        let picked = tree
            .get_mut(target)
            .ok_or("the drawn index is inside the tree")?;
        if picked.1.is_empty() {
            picked.1.push(0);
        } else {
            let at = below(&mut state, picked.1.len());
            let byte = picked
                .1
                .get_mut(at)
                .ok_or("the drawn offset is inside the file")?;
            *byte ^= 1;
        }
        let changed = digest_tree(DOMAIN, &tree)?;
        assert_ne!(
            original, changed,
            "seed {seed:#018x}: one changed byte must change the digest"
        );
    }
    Ok(())
}

#[test]
/// Moving a file's bytes to another name changes the digest: names are inputs.
fn sim_a_renamed_file_diverges() -> Outcome {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let mut tree = draw_tree(&mut state);
        let original = digest_tree(DOMAIN, &tree)?;
        let first = tree
            .first_mut()
            .ok_or("a drawn tree has at least one file")?;
        first.0.push_str(".renamed");
        let renamed = digest_tree(DOMAIN, &tree)?;
        assert_ne!(
            original, renamed,
            "seed {seed:#018x}: a renamed input must change the digest"
        );
    }
    Ok(())
}

#[test]
/// The domain tag separates two digests over the same inputs.
fn sim_the_domain_tag_separates_digests() -> Outcome {
    let mut state = seeded_stream(SWEEP_SEEDS[0]);
    let tree = draw_tree(&mut state);
    let first = digest_tree(DOMAIN, &tree)?;
    let second = digest_tree("lgwks-std/sim-build/v2", &tree)?;
    assert_ne!(
        first, second,
        "two domain tags over one tree must digest apart"
    );
    Ok(())
}

#[test]
/// Cargo is told about the walked tree, every file and every variable.
fn sim_directives_name_the_tree_every_file_and_every_variable() -> Outcome {
    let mut state = seeded_stream(SWEEP_SEEDS[1]);
    let tree = draw_tree(&mut state);
    let root = tempfile::tempdir()?;
    write_tree(root.path(), &tree)?;
    let (_, directives) = digest_of(DOMAIN, root.path())?;
    let canonical = root.path().canonicalize()?;
    let lines: Vec<&str> = directives.lines().collect();
    let src_line = format!("cargo:rerun-if-changed={}", canonical.join("src").display());
    assert!(
        lines.contains(&src_line.as_str()),
        "the walked tree is announced: {directives}"
    );
    for file in &tree {
        let line = format!(
            "cargo:rerun-if-changed={}",
            canonical.join(&file.0).display()
        );
        assert!(
            lines.contains(&line.as_str()),
            "{} is announced: {directives}",
            file.0
        );
    }
    let variable_line = format!("cargo:rerun-if-env-changed={UNSET_VARIABLE}");
    assert!(
        lines.contains(&variable_line.as_str()),
        "the variable is announced: {directives}"
    );
    assert_eq!(
        lines.len(),
        tree.len().saturating_add(2),
        "one line per file, one for the tree, one for the variable"
    );
    Ok(())
}

#[test]
/// An input outside the root has no portable name and is refused by name.
fn sim_an_input_outside_the_root_is_refused() -> Outcome {
    let root = tempfile::tempdir()?;
    let elsewhere = tempfile::tempdir()?;
    let stray = elsewhere.path().join("stray.rs");
    fs::write(&stray, b"fn stray() {}")?;
    let mut inputs = InputDigest::new(DOMAIN, root.path())?;
    inputs.file(&stray);
    let outcome = inputs.finish(&mut Vec::new());
    assert!(
        matches!(outcome, Err(BuildError::OutsideRoot { .. })),
        "an outside input is refused by name: {outcome:?}"
    );
    Ok(())
}

#[test]
/// A `..` that climbs out of the root is outside it, though the joined path
/// still starts with the root: the digest never names an input `../stray.rs`.
fn sim_a_parent_component_is_outside_the_root() -> Outcome {
    let parent = tempfile::tempdir()?;
    let root = parent.path().join("crate");
    fs::create_dir(&root)?;
    fs::write(parent.path().join("stray.rs"), b"fn stray() {}")?;
    fs::write(root.join("lib.rs"), b"fn lib() {}")?;
    let mut inputs = InputDigest::new(DOMAIN, &root)?;
    inputs.file("../stray.rs");
    let outcome = inputs.finish(&mut Vec::new());
    assert!(
        matches!(outcome, Err(BuildError::OutsideRoot { .. })),
        "a climbing input is refused by name: {outcome:?}"
    );
    let mut inside = InputDigest::new(DOMAIN, &root)?;
    inside.file("sub/../lib.rs");
    let mut plain = InputDigest::new(DOMAIN, &root)?;
    plain.file("lib.rs");
    assert!(
        matches!(
            inside.finish(&mut Vec::new()),
            Err(BuildError::OutsideRoot { .. })
        ),
        "a `..` is refused even where it lands back inside the root"
    );
    plain.finish(&mut Vec::new())?;
    Ok(())
}

#[test]
/// A missing input file is a read refusal carrying the OS error.
fn sim_a_missing_input_is_a_read_refusal() -> Outcome {
    let root = tempfile::tempdir()?;
    let mut inputs = InputDigest::new(DOMAIN, root.path())?;
    inputs.file("absent.rs");
    let outcome = inputs.finish(&mut Vec::new());
    let Err(refusal) = outcome else {
        return Err("a missing input digested instead of being refused".into());
    };
    assert!(
        matches!(refusal, BuildError::Read { .. }),
        "the refusal is a read failure: {refusal}"
    );
    assert!(
        Error::source(&refusal).is_some(),
        "the OS error is kept as the source"
    );
    Ok(())
}

/// A writer whose every write fails, standing in for a closed stdout.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
/// A closed pipe is a typed directive error, never a panic.
fn sim_a_closed_pipe_is_a_directive_error() -> Outcome {
    let outcome = directive(&mut ClosedPipe, "rustc-link-lib=framework=Security");
    assert!(
        matches!(outcome, Err(BuildError::Directive(ref source)) if source.kind() == io::ErrorKind::BrokenPipe),
        "the refusal keeps the pipe's error: {outcome:?}"
    );
    let mut written = Vec::new();
    directive(&mut written, "rustc-link-lib=framework=Security")?;
    assert_eq!(
        written, b"cargo:rustc-link-lib=framework=Security\n",
        "one cargo: line"
    );
    Ok(())
}

#[test]
/// The same seed replays to the same digest trace.
fn sim_the_same_seed_replays_to_the_same_build_trace() -> Outcome {
    for seed in SWEEP_SEEDS {
        assert_eq!(
            build_trace(seed)?,
            build_trace(seed)?,
            "seed {seed:#018x} must replay"
        );
    }
    Ok(())
}

#[test]
/// Two seeds draw two trees and two digests.
fn sim_distinct_build_seeds_diverge_in_their_trace() -> Outcome {
    assert_ne!(
        build_trace(SWEEP_SEEDS[0])?,
        build_trace(SWEEP_SEEDS[1])?,
        "two seeds must not draw one digest, or the sweep ignores its seed"
    );
    Ok(())
}
