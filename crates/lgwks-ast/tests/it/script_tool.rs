//! `lgwks-ast script map|check`, driven as a user runs it (#384).
//!
//! Every test runs the shipped binary on a scratch tree and asserts what the
//! user sees: the map, the refusal with its `path:line:column`, the summary,
//! the JSON document an agent parses, and the exit status a gate reads. The
//! trees hold what a repository holds: scripts in nested directories, build
//! output that must not be read, a file that is not Rust tokens, and paths
//! that do not exist.
//!
//! The `diff` tests add the shape a CI gate reads (#655): two checked-out
//! source roots, a structural classification with `line` ignored, the approval
//! marker and its absence, and the seeded replay.

#![cfg(feature = "tool")]

use std::error::Error;
use std::path::Path;

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

// ── script diff (#655) ──────────────────────────────────────────────────────

/// A flow that ends by giving back a value.
const GIVE_BACK: &str = "lgwks_bot::script! {\n    flow add() -> u32:\n        give back 1\n}\n";

/// The same flow with one `act` domain added above the value.
const ACT_ADDED: &str = "lgwks_bot::script! {\n    flow add() -> u32:\n        act store::put on site with item\n        give back 1\n}\n";

/// A flow whose retry admits `3` attempts.
const RETRY_THREE: &str = "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        retry up to 3 times:\n            x\n}\n";
/// The same flow raised to `5` attempts.
const RETRY_FIVE: &str = "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        retry up to 5 times:\n            x\n}\n";

/// A retry that waits `10ms` between attempts.
const RETRY_WAIT_TEN: &str = "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        retry up to 3 times, waiting 10ms:\n            x\n}\n";

/// The same retry waiting `50ms`.
const RETRY_WAIT_FIFTY: &str = "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        retry up to 3 times, waiting 50ms:\n            x\n}\n";

/// A flow bounded by a `20ms` deadline.
const WITHIN: &str = "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        within 20ms:\n            retry up to 3 times:\n                x\n}\n";

/// The same flow with the deadline removed, so the retry runs unbounded.
const WITHIN_REMOVED: &str = "lgwks_bot::script! {\n    flow add(x: u32) -> u32:\n        retry up to 3 times:\n            x\n}\n";

/// Run `script diff` on two roots under `tree`.
fn diff(
    tree: &Path,
    base: &str,
    head: &str,
    json: bool,
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut arguments = vec!["script", "diff"];
    if json {
        arguments.push("--json");
    }
    arguments.push(base);
    arguments.push(head);
    tool(tree, &arguments)
}

/// The JSON document a diff wrote, and the status it exited with.
fn diff_json(tree: &Path, base: &str, head: &str) -> Result<(Value, Option<i32>), Box<dyn Error>> {
    let output = diff(tree, base, head, true)?;
    let (out, err) = text(&output);
    let document: Value = lgwks_std::json::from_str(&out)
        .map_err(|error| format!("stdout:\n{out}\nstderr:\n{err}\n{error}"))?;
    Ok((document, output.status.code()))
}

#[test]
fn a_line_shift_is_an_empty_diff() -> TestResult {
    let tree = Scratch::new("script-tool-diff-shift")?;
    plant(tree.path(), "base/src/flows.rs", ACCEPTED)?;
    plant(
        tree.path(),
        "head/src/flows.rs",
        &format!("// the same script, lower down\n\n\n// and here\n{ACCEPTED}"),
    )?;
    let output = diff(tree.path(), "base", "head", false)?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a moved line is not a change:\nstdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.contains("No structural change"),
        "the Markdown says the maps match:\n{out}"
    );
    let (document, code) = diff_json(tree.path(), "base", "head")?;
    assert_eq!(code, Some(0), "the JSON exit status agrees:\n{document}");
    assert_eq!(
        document["changes"],
        Value::Array(Vec::new()),
        "no change is reported:\n{document}"
    );
    Ok(())
}

#[test]
fn a_new_act_domain_widens_and_needs_the_source_marker() -> TestResult {
    let tree = Scratch::new("script-tool-diff-act")?;
    plant(tree.path(), "base/src/flows.rs", GIVE_BACK)?;
    plant(tree.path(), "head/src/flows.rs", ACT_ADDED)?;
    let output = diff(tree.path(), "base", "head", false)?;
    let (out, err) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an unapproved widening is a refused gate:\nstdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.contains("**UNAPPROVED WIDENING (1)**")
            && out.contains("a new `act` domain changes the world"),
        "the Markdown names the widening and its reason:\n{out}"
    );
    let (document, code) = diff_json(tree.path(), "base", "head")?;
    assert_eq!(code, Some(1), "the JSON exit status agrees:\n{document}");
    assert_eq!(document["unapproved_widening"], 1, "{document}");
    assert_eq!(document["approved"], false, "{document}");
    assert_eq!(document["changes"][0]["class"], "widening", "{document}");
    assert_eq!(document["changes"][0]["kind"], "Act", "{document}");
    assert_eq!(document["changes"][0]["approved"], false, "{document}");

    plant(
        tree.path(),
        "approved/src/flows.rs",
        &format!("// lgwks-architecture-approved: the store write is the point\n{ACT_ADDED}"),
    )?;
    let (approved, code) = diff_json(tree.path(), "base", "approved")?;
    assert_eq!(
        code,
        Some(0),
        "the marker approves the widening:\n{approved}"
    );
    assert_eq!(approved["approved"], true, "{approved}");
    assert_eq!(approved["unapproved_widening"], 0, "{approved}");
    assert_eq!(
        approved["approvals"][0]["file"], "src/flows.rs",
        "{approved}"
    );
    assert!(
        approved["approvals"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("the store write is the point")),
        "the reason is the text after the marker:\n{approved}"
    );
    Ok(())
}

#[test]
fn a_raised_retry_bound_widens_and_a_lowered_one_narrows() -> TestResult {
    let tree = Scratch::new("script-tool-diff-retry")?;
    plant(tree.path(), "three/flows.rs", RETRY_THREE)?;
    plant(tree.path(), "five/flows.rs", RETRY_FIVE)?;

    let (raised, code) = diff_json(tree.path(), "three", "five")?;
    assert_eq!(code, Some(1), "a raised bound is a widening:\n{raised}");
    assert_eq!(raised["changes"][0]["class"], "widening", "{raised}");
    assert!(
        raised["changes"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("raised from 3 to 5")),
        "the reason names both bounds:\n{raised}"
    );

    let (lowered, code) = diff_json(tree.path(), "five", "three")?;
    assert_eq!(
        code,
        Some(0),
        "a lowered bound is a narrowing and needs no approval:\n{lowered}"
    );
    assert_eq!(lowered["changes"][0]["class"], "narrowing", "{lowered}");
    assert!(
        lowered["changes"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("lowered from 5 to 3")),
        "the reverse direction is named:\n{lowered}"
    );

    plant(tree.path(), "wait-ten/flows.rs", RETRY_WAIT_TEN)?;
    plant(tree.path(), "wait-fifty/flows.rs", RETRY_WAIT_FIFTY)?;
    let (waited, code) = diff_json(tree.path(), "wait-ten", "wait-fifty")?;
    assert_eq!(
        code,
        Some(1),
        "a longer wait between attempts is a widening:\n{waited}"
    );
    assert_eq!(waited["changes"][0]["class"], "widening", "{waited}");
    assert!(
        waited["changes"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("raised from `10ms` to `50ms`")),
        "the reason names both waits:\n{waited}"
    );
    Ok(())
}

#[test]
fn a_new_flow_widens_and_an_approval_marker_clears_it() -> TestResult {
    let tree = Scratch::new("script-tool-diff-flow")?;
    plant(tree.path(), "base/flows.rs", GIVE_BACK)?;
    plant(
        tree.path(),
        "head/flows.rs",
        &format!(
            "{GIVE_BACK}lgwks_bot::script! {{\n    flow audit() -> u32:\n        give back 1\n}}\n"
        ),
    )?;
    let (document, code) = diff_json(tree.path(), "base", "head")?;
    assert_eq!(code, Some(1), "a new flow is a widening:\n{document}");
    assert_eq!(document["changes"][0]["class"], "widening", "{document}");
    assert_eq!(document["changes"][0]["kind"], "Flow", "{document}");
    assert_eq!(document["changes"][0]["flow"], "audit", "{document}");

    plant(
        tree.path(),
        "approved/flows.rs",
        &format!(
            "// lgwks-architecture-approved: the audit flow is the new capability\n{}{}",
            GIVE_BACK, "lgwks_bot::script! {\n    flow audit() -> u32:\n        give back 1\n}\n"
        ),
    )?;
    let (approved, code) = diff_json(tree.path(), "base", "approved")?;
    assert_eq!(code, Some(0), "the marker clears the gate:\n{approved}");
    assert_eq!(approved["changes"][0]["approved"], true, "{approved}");
    Ok(())
}

#[test]
fn a_removed_within_widens_and_does_not_invent_a_new_retry() -> TestResult {
    let tree = Scratch::new("script-tool-diff-within")?;
    plant(tree.path(), "base/flows.rs", WITHIN)?;
    plant(tree.path(), "head/flows.rs", WITHIN_REMOVED)?;
    let (document, code) = diff_json(tree.path(), "base", "head")?;
    assert_eq!(
        code,
        Some(1),
        "a removed deadline is a widening:\n{document}"
    );
    assert_eq!(
        document["widening"], 1,
        "only the deadline is reported; the retry it held is the same retry:\n{document}"
    );
    assert_eq!(document["changes"][0]["kind"], "Within", "{document}");
    assert_eq!(document["changes"][0]["class"], "widening", "{document}");
    assert_eq!(document["changes"][0]["from"], "20ms", "{document}");
    assert!(
        document["changes"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("without a deadline")),
        "the reason says what removing it does:\n{document}"
    );
    Ok(())
}

/// A new deadline whose body is a new `act` domain.
const NESTED_ACT: &str = "lgwks_bot::script! {\n    flow add() -> u32:\n        within 20ms:\n            act store::put on site with item\n            1\n}\n";

/// A `step` whose body is the one deadline.
const STEPPED_WITHIN: &str = "lgwks_bot::script! {\n    flow add() -> u32:\n        step work:\n            within 20ms:\n                1\n}\n";

#[test]
fn a_widening_nested_in_a_new_or_removed_block_still_gates() -> TestResult {
    let tree = Scratch::new("script-tool-diff-nested")?;
    plant(tree.path(), "plain/flows.rs", GIVE_BACK)?;
    plant(tree.path(), "act/flows.rs", NESTED_ACT)?;
    plant(tree.path(), "stepped/flows.rs", STEPPED_WITHIN)?;

    let (added, code) = diff_json(tree.path(), "plain", "act")?;
    assert_eq!(
        code,
        Some(1),
        "an `act` inside a new `within` still widens:\n{added}"
    );
    let act = added["changes"]
        .as_array()
        .and_then(|changes| {
            changes
                .iter()
                .find(|change| change["kind"] == "Act" && change["class"] == "widening")
        })
        .ok_or("the nested act is not reported as a widening")?;
    assert_eq!(act["path"], "within 20ms > act", "{added}");

    let (removed, code) = diff_json(tree.path(), "stepped", "plain")?;
    assert_eq!(
        code,
        Some(1),
        "a `within` lost with its `step` still widens:\n{removed}"
    );
    assert!(
        removed["changes"].as_array().is_some_and(|changes| changes
            .iter()
            .any(|change| change["kind"] == "Within" && change["class"] == "widening")),
        "the removed deadline is reported even though its step went too:\n{removed}"
    );
    Ok(())
}

#[test]
fn the_diff_json_has_stable_keys() -> TestResult {
    let tree = Scratch::new("script-tool-diff-keys")?;
    plant(tree.path(), "base/flows.rs", RETRY_THREE)?;
    plant(tree.path(), "head/flows.rs", RETRY_FIVE)?;
    let (document, code) = diff_json(tree.path(), "base", "head")?;
    assert_eq!(code, Some(1), "{document}");
    for key in [
        "verb",
        "base",
        "head",
        "widening",
        "narrowing",
        "neutral",
        "unapproved_widening",
        "approved",
        "approvals",
        "changes",
    ] {
        assert!(
            document.get(key).is_some(),
            "the document has the stable key `{key}`:\n{document}"
        );
    }
    assert_eq!(document["verb"], "diff", "{document}");
    let change = &document["changes"][0];
    for key in [
        "class", "file", "flow", "path", "kind", "from", "to", "reason", "approved",
    ] {
        assert!(
            change.get(key).is_some(),
            "every change has the stable key `{key}`:\n{document}"
        );
    }
    assert_eq!(change["file"], "flows.rs", "{document}");
    assert_eq!(change["flow"], "add", "{document}");
    assert_eq!(change["path"], "retry 5", "{document}");
    Ok(())
}

#[test]
fn diff_refuses_a_wrong_shape_of_argument_and_a_refused_script() -> TestResult {
    let tree = Scratch::new("script-tool-diff-usage")?;
    plant(tree.path(), "base/flows.rs", GIVE_BACK)?;
    plant(tree.path(), "head/flows.rs", REFUSED)?;
    let one = tool(tree.path(), &["script", "diff", "base"])?;
    let (_, err) = text(&one);
    assert_eq!(one.status.code(), Some(2), "{err}");
    assert!(
        err.contains("takes two source roots") && err.contains("usage: lgwks-ast script map|check"),
        "one root is a usage error:\n{err}"
    );
    let mixed = tool(tree.path(), &["script", "diff", "base/flows.rs", "base"])?;
    let (_, err) = text(&mixed);
    assert_eq!(mixed.status.code(), Some(2), "{err}");
    assert!(
        err.contains("two directories or two files"),
        "a file beside a directory is refused:\n{err}"
    );
    let missing = tool(tree.path(), &["script", "diff", "base", "no/such/dir"])?;
    let (_, err) = text(&missing);
    assert_eq!(missing.status.code(), Some(2), "{err}");
    assert!(err.contains("cannot read no/such/dir"), "{err}");
    let refused = tool(tree.path(), &["script", "diff", "base", "head"])?;
    let (_, err) = text(&refused);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "a refused script is a parse error, not a diff:\n{err}"
    );
    assert!(
        err.contains("head/flows.rs:3:23: refused:")
            && err.contains("ending the process from a flow"),
        "the refusal is located as `script check` locates it:\n{err}"
    );
    Ok(())
}

/// A script whose one flow retries `attempts` times.
#[cfg(feature = "lang-rust")]
fn generated(attempts: u32) -> String {
    format!(
        "lgwks_bot::script! {{\n    flow seeded(x: u32) -> u32:\n        retry up to {attempts} times:\n            x\n}}\n"
    )
}

#[cfg(feature = "lang-rust")]
#[test]
fn a_seeded_edit_replays_the_same_classification() -> TestResult {
    use crate::seed::Rng;
    for seed in 0..16_u64 {
        let mut stream = Rng::new(seed);
        let attempts = stream.between(1, 19);
        let base = generated(attempts);
        let shifted = format!("// the same script, lower down\n\n{base}");
        let raised = generated(attempts.saturating_add(1));
        let tree = Scratch::new("script-tool-diff-sim")?;
        plant(tree.path(), "base/flows.rs", &base)?;
        plant(tree.path(), "shift/flows.rs", &shifted)?;
        plant(tree.path(), "raised/flows.rs", &raised)?;

        let (empty, code) = diff_json(tree.path(), "base", "shift")?;
        assert_eq!(
            code,
            Some(0),
            "seed {seed}: a line shift is an empty diff:\n{empty}"
        );
        assert_eq!(empty["changes"], Value::Array(Vec::new()), "seed {seed}");

        let first = diff(tree.path(), "base", "raised", true)?;
        let second = diff(tree.path(), "base", "raised", true)?;
        let (first_out, first_err) = text(&first);
        let (second_out, second_err) = text(&second);
        assert_eq!(
            first.status.code(),
            Some(1),
            "seed {seed}: the raised bound widens:\n{first_out}\n{first_err}"
        );
        assert_eq!(
            first_out, second_out,
            "seed {seed}: the same seed replays the same document"
        );
        assert_eq!(
            second.status.code(),
            first.status.code(),
            "seed {seed}: the same seed replays the same status:\n{second_err}"
        );
        let document: Value = lgwks_std::json::from_str(&first_out)?;
        let change = &document["changes"][0];
        assert_eq!(change["class"], "widening", "seed {seed}: {document}");
        assert_eq!(change["kind"], "Retry", "seed {seed}: {document}");
        assert!(
            change["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("raised from")),
            "seed {seed}: the direction is named:\n{document}"
        );
    }
    Ok(())
}
