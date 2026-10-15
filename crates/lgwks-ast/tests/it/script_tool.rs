//! `lgwks-ast script map|check`, driven as a user runs it (#384).
//!
//! Every test runs the shipped binary on a scratch tree and asserts what the
//! user sees: the map, the refusal with its `path:line:column`, the summary,
//! the JSON document an agent parses, and the exit status a gate reads. The
//! trees hold what a repository holds: scripts in nested directories, build
//! output that must not be read, a file that is not Rust tokens, and paths
//! that do not exist.

#![cfg(feature = "tool")]

use std::error::Error;

use lgwks_std::json::Value;

use crate::cli::{plant, text, tool};
use crate::scratch::Scratch;

type TestResult = Result<(), Box<dyn Error>>;

/// A script the language accepts, at the top of its file.
const ACCEPTED: &str =
    "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        within 20ms:\n            x\n}\n";

/// A script the language refuses at its file's line 3, column 23: the `exit`.
const REFUSED: &str = "lgwks_bot::script! {\n    flow stop():\n        std::process::exit(1)\n}\n";

#[test]
fn map_prints_every_script_and_skips_build_output() -> TestResult {
    let tree = Scratch::new("script-tool-map")?;
    plant(tree.path(), "src/deep/flows.rs", ACCEPTED)?;
    plant(tree.path(), "target/debug/build/out.rs", REFUSED)?;
    plant(tree.path(), "vendor/dep/lib.rs", REFUSED)?;
    let output = tool(tree.path(), &["script", "map"])?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.starts_with(
            "src/deep/flows.rs:1:12: script!\nflow add(x: u32) -> u32  @2\n  within 20ms  @3\n"
        ),
        "the map names the invocation at its macro name (`script`, column 12) and renders its flow and step at their lines:\n{out}"
    );
    assert!(
        out.contains("OK  script map — 1 script! in 1 files, nothing refused"),
        "the summary counts one file, so target/ and vendor/ were never read:\n{out}"
    );
    Ok(())
}

#[test]
fn check_reports_the_refusal_where_the_compiler_does_and_exits_two() -> TestResult {
    let tree = Scratch::new("script-tool-check")?;
    plant(tree.path(), "src/good.rs", ACCEPTED)?;
    plant(tree.path(), "src/bad.rs", REFUSED)?;
    let output = tool(tree.path(), &["script", "check", "src"])?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.contains("src/bad.rs:3:23: refused: ")
            && out.contains("ending the process from a flow"),
        "the refusal is located at the exit and says what it refuses:\n{out}"
    );
    assert!(
        !out.contains("flow add"),
        "check prints refusals only, not the accepted script's map:\n{out}"
    );
    assert!(
        err.contains("REFUSED  script check: 1 refused across 2 script! in 2 files"),
        "the summary goes to stderr with the counts:\n{err}"
    );
    Ok(())
}

#[test]
fn json_is_one_document_an_agent_can_read() -> TestResult {
    let tree = Scratch::new("script-tool-json")?;
    plant(tree.path(), "a.rs", ACCEPTED)?;
    plant(tree.path(), "b.rs", REFUSED)?;
    let output = tool(tree.path(), &["script", "map", "--json", "a.rs", "b.rs"])?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    let document: Value = lgwks_std::json::from_str(&out)?;
    assert_eq!(document["verb"], "map");
    assert_eq!(document["files"], 2);
    assert_eq!(document["refused"], 1);
    let accepted = &document["invocations"][0];
    assert_eq!(accepted["path"], "a.rs");
    assert_eq!(accepted["map"]["flows"][0]["name"], "add");
    assert_eq!(accepted["map"]["flows"][0]["line"], 2);
    assert_eq!(accepted["map"]["flows"][0]["steps"][0]["kind"], "Within");
    assert_eq!(accepted["map"]["flows"][0]["steps"][0]["line"], 3);
    let refused = &document["invocations"][1];
    assert_eq!(refused["refusal"]["line"], 3);
    assert_eq!(refused["refusal"]["column"], 23);
    assert!(
        err.is_empty(),
        "--json writes nothing beside the document:\n{err}"
    );
    Ok(())
}

#[test]
fn a_file_that_is_not_rust_tokens_is_refused_not_skipped() -> TestResult {
    let tree = Scratch::new("script-tool-broken")?;
    plant(tree.path(), "ok.rs", ACCEPTED)?;
    plant(
        tree.path(),
        "broken.rs",
        "fn f() {}\nconst S: &str = \"unterminated;\n",
    )?;
    let output = tool(tree.path(), &["script", "check"])?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        err.contains("broken.rs:2:17: this file is not Rust tokens"),
        "the unreadable file is named at its stray quote:\n{err}"
    );
    Ok(())
}

#[test]
fn a_missing_path_and_an_unknown_word_refuse_with_the_usage() -> TestResult {
    let tree = Scratch::new("script-tool-usage")?;
    let missing = tool(tree.path(), &["script", "check", "no/such/dir"])?;
    let (_, err) = text(&missing);
    assert_eq!(missing.status.code(), Some(2), "{err}");
    assert!(err.contains("cannot read no/such/dir"), "{err}");
    for args in [
        &["script", "draw"][..],
        &["map"][..],
        &[][..],
        &["script", "map", "--yaml"][..],
        &["script", "map", "--top", "3"][..],
        &["script", "census", "--json"][..],
        &["script", "census", "--top"][..],
        &["script", "census", "--top", "0"][..],
        &["script", "census", "--top", "many"][..],
    ] {
        let output = tool(tree.path(), args)?;
        let (_, err) = text(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {err}");
        assert!(
            err.contains("usage: lgwks-ast script map|check"),
            "{args:?}: {err}"
        );
    }
    let help = tool(tree.path(), &["--help"])?;
    assert_eq!(help.status.code(), Some(0));
    assert!(
        text(&help)
            .0
            .starts_with("usage: lgwks-ast script map|check")
    );
    Ok(())
}

/// Two flows whose plain Rust shares one shape (`let _ = _.get(0)`, at lines 3
/// and 7), and one line of another.
const RECURRING: &str = "lgwks_bot::script! {\n    flow first(site: u32) -> u32:\n        let seen = site.get(1)\n        site.note(seen).await?\n        give back site\n    flow second(site: u32) -> u32:\n        let other = site.get(2)\n        give back site\n}\n";

#[test]
fn census_prints_the_recurring_shapes_and_never_gates() -> TestResult {
    let tree = Scratch::new("script-tool-census")?;
    plant(tree.path(), "src/flows.rs", RECURRING)?;
    plant(tree.path(), "src/bad.rs", REFUSED)?;
    plant(tree.path(), "target/debug/build/out.rs", RECURRING)?;
    let output = tool(tree.path(), &["script", "census"])?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a census measures and never gates:\nstdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.starts_with(
            "2 `script!` in 1 files, 2 flows; 2 word steps and 3 passthrough lines \
             (passthrough share 60.0%); 1 refused or unreadable and not read; 2 distinct shapes.\n"
        ),
        "the refused script is counted and not read, each flow's `give back` is a word step, and target/ is never walked:\n{out}"
    );
    assert!(
        out.contains(
            "| 1 | 2 | 1 | `let _ = _.get(0)` | `src/flows.rs:3`, `src/flows.rs:7` |\n\
             | 2 | 1 | 1 | `_.note(_).await?` | `src/flows.rs:4` |\n"
        ),
        "the shape that recurs ranks first, named at its lines:\n{out}"
    );
    assert!(
        err.is_empty(),
        "the census writes its report to stdout and nothing to stderr:\n{err}"
    );
    Ok(())
}

#[test]
fn census_stops_at_the_rows_asked_for() -> TestResult {
    let tree = Scratch::new("script-tool-census-top")?;
    plant(tree.path(), "flows.rs", RECURRING)?;
    let all = tool(tree.path(), &["script", "census", "flows.rs"])?;
    let one = tool(tree.path(), &["script", "census", "--top", "1", "flows.rs"])?;
    let (all_out, _) = text(&all);
    let (one_out, err) = text(&one);
    assert_eq!(one.status.code(), Some(0), "{err}");
    assert!(
        all_out.contains("`_.note(_).await?`"),
        "without --top both shapes print:\n{all_out}"
    );
    assert!(
        one_out.contains("`let _ = _.get(0)`") && !one_out.contains("`_.note(_).await?`"),
        "--top 1 prints the most recurrent shape and not the next:\n{one_out}"
    );
    Ok(())
}

#[test]
fn an_empty_tree_is_clean_and_says_so() -> TestResult {
    let tree = Scratch::new("script-tool-empty")?;
    let output = tool(tree.path(), &["script", "check"])?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.contains("OK  script check — 0 script! in 0 files, nothing refused"),
        "{out}"
    );
    Ok(())
}
