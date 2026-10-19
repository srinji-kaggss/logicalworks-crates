//! The consumer check's decisions, proven without the network: how a README page
//! becomes a program, what manifest a consumer gets, and how versions are read.
//!
//! Source: the four failures of the first run against the 3.0.0 / 4.0.0 / 2.0.0
//! releases (#526), and rustdoc's documentation-test rules for hidden lines,
//! crate attributes and `extern crate`.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;

use crate::consumer::{DEPS_IMPORTS, ROWS, manifest};
use crate::program::{first_rust_block, main_rs};
use crate::{info_version, manifest_version, tag_version};

type TestResult = Result<(), Box<dyn Error>>;

/// The text of a file at the repository root.
fn repo_file(name: &str) -> Result<String, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    Ok(std::fs::read_to_string(path)?)
}

/// The program the README page at `name` makes.
fn program_of(name: &str) -> Result<String, Box<dyn Error>> {
    let page = repo_file(name)?;
    let block = first_rust_block(&page).ok_or("the page has no runnable block")?;
    Ok(main_rs(&block))
}

#[test]
fn hidden_lines_lose_their_marker_and_attributes_keep_theirs() {
    let block = "# use std::fmt;\n#\n##[allow(unused)]\n#[derive(Debug)]\n#![allow(dead_code)]\nextern crate foo;\nlet value = 1;";
    let expected = "#![allow(dead_code)]\nextern crate foo;\nfn main() -> Result<(), Box<dyn std::error::Error>> {\nuse std::fmt;\n\n#[allow(unused)]\n#[derive(Debug)]\nlet value = 1;\nOk(())\n}\n";
    assert_eq!(
        main_rs(block),
        expected,
        "`# ` and a lone `#` hide, `##` is a `#`, `#[..]` is code, crate-level lines are hoisted"
    );
}

#[test]
fn a_block_that_returns_its_own_ok_gets_no_second_one() {
    let program = main_rs("let value = Ok::<(), std::io::Error>(());");
    assert!(
        !program.contains("Ok(())\n}"),
        "the block already ends in a turbofish `Ok::<`: {program}"
    );
}

#[test]
fn a_block_that_declares_main_is_the_program() {
    assert_eq!(
        main_rs("fn main() {}"),
        "fn main() {}\n",
        "a block with its own `fn main` is not wrapped"
    );
}

#[test]
fn only_the_first_closed_rust_fence_counts() {
    let page = "```text\nno\n```\n```rustscript\nno\n```\n```rust,no_run\nyes\n```\n```rust\nsecond\n```\n";
    assert_eq!(
        first_rust_block(page).as_deref(),
        Some("yes"),
        "a fence named `rustscript` is not Rust, `rust,no_run` is"
    );
    assert_eq!(
        first_rust_block("```rust\nnever closed\n"),
        None,
        "an unclosed fence is not a block"
    );
    assert_eq!(
        first_rust_block("```rust\n\n```\n"),
        None,
        "an empty block is not a program"
    );
}

#[test]
fn the_ast_readme_keeps_its_derive_attribute_above_a_hoisted_extern_crate() -> TestResult {
    let program = program_of("crates/lgwks-ast/README.md")?;
    assert!(
        program.contains("\n#[derive(thiserror::Error, Debug)]\n"),
        "the derive line lost its `#`: {program}"
    );
    let extern_at = program.find("extern crate lgwks_ast as thiserror;");
    let main_at = program.find("fn main()");
    assert!(
        matches!((extern_at, main_at), (Some(first), Some(second)) if first < second),
        "`extern crate` must sit above the wrapping main: {program}"
    );
    Ok(())
}

#[test]
fn the_bot_script_page_is_the_program_as_written() -> TestResult {
    let page = repo_file("crates/lgwks-bot/README-script.md")?;
    let block = first_rust_block(&page).ok_or("README-script.md has no runnable block")?;
    assert!(
        block.contains("fn main() -> Result<(), FlowError>"),
        "the page's flow declares its own main"
    );
    assert_eq!(
        main_rs(&block),
        format!("{block}\n"),
        "a block with its own main passes through unchanged"
    );
    Ok(())
}

#[test]
fn no_row_page_leaves_a_hiding_marker_in_its_program() -> TestResult {
    for row in &ROWS {
        let page = repo_file(&format!("{}/{}", row.dir, row.page))?;
        if let Some(block) = first_rust_block(&page) {
            let program = main_rs(&block);
            assert!(
                program
                    .lines()
                    .all(|line| !line.trim_start().starts_with("# ")),
                "{} {}: a hidden line stayed hidden: {program}",
                row.name,
                row.label
            );
        }
    }
    Ok(())
}

#[test]
fn the_deps_minimal_program_names_only_modules_the_root_keeps() {
    assert!(
        !DEPS_IMPORTS.contains("scan"),
        "`scan` moved to lgwks_ast in lgwks_deps 4.0.0 (#408)"
    );
}

#[test]
fn eight_consumers_each_their_own_package() {
    let packages: BTreeSet<(&str, &str)> = ROWS.iter().map(|row| (row.name, row.label)).collect();
    assert_eq!(
        packages.len(),
        ROWS.len(),
        "a crate and feature set name one consumer"
    );
    assert_eq!(ROWS.len(), 8, "std x3, ast, deps x2, bot x2");
}

#[test]
fn a_manifest_pins_the_exact_published_version_with_no_path() -> TestResult {
    let row = ROWS
        .iter()
        .find(|row| row.name == "lgwks_ast")
        .ok_or("no lgwks_ast row")?;
    let text = manifest(row, "2.0.0");
    assert!(
        text.contains("lgwks_ast = { version = \"=2.0.0\", default-features = true }"),
        "the dependency is the registry's exact version: {text}"
    );
    assert!(
        text.contains("name = \"consumer-lgwks-ast-default\""),
        "the package is named for crate and feature set: {text}"
    );
    assert!(
        !text.contains("path ="),
        "a consumer has no path into this tree: {text}"
    );
    Ok(())
}

#[test]
fn versions_are_read_from_cargo_info_tags_and_manifests() {
    assert_eq!(
        info_version("name: lgwks_std\nversion: 3.0.0 (latest 3.1.0)\nlicense: Apache-2.0\n")
            .as_deref(),
        Some("3.0.0"),
        "the first word after `version: `"
    );
    assert_eq!(info_version("name: lgwks_std\n"), None, "no version line");
    assert_eq!(
        tag_version("lgwks_std-v3.0.0", "lgwks_std"),
        Some("3.0.0"),
        "the tag names its version"
    );
    assert_eq!(
        tag_version("lgwks_std-v3.0.0", "lgwks_ast"),
        None,
        "another crate's tag names nothing"
    );
    assert_eq!(
        manifest_version("[package]\nname = \"x\"\nversion = \"4.0.0\"\n").as_deref(),
        Some("4.0.0"),
        "the package version"
    );
}
