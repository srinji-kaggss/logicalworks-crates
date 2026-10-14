//! The CI journey end to end: JUnit shards in, a receipt in the database, the
//! spec's table rewritten from it, and the check that holds the table honest.

use std::error::Error;
use std::fs;
use std::path::Path;

use crate::fixture::Fixture;
use crate::{Streams, execute};

type TestResult = Result<(), Box<dyn Error>>;

/// The map: two rows, one with a gap.
const MAP: &str = "schema = 1\nspec = \"docs/spec.md\"\n\n[[row]]\nid = \"T01\"\nrequires = \"DX-01\"\nstate = \"exercised\"\ngap = \"\"\ntests = [\"it::a::one_t01\", \"it::a::two_t01\"]\n\n[[row]]\nid = \"T02\"\nrequires = \"DX-02\"\nstate = \"present\"\ngap = \"the second half is not asserted\"\ntests = [\"it::b::one_t02\"]\n";

/// The spec, with an empty generated block.
const SPEC: &str =
    "# Spec\n\n<!-- acceptance-table: start -->\nold\n<!-- acceptance-table: end -->\n";

/// A JUnit report holding the given `(name, passes)` cases.
fn report(cases: &[(&str, bool)]) -> String {
    let body: Vec<String> = cases
        .iter()
        .map(|entry| {
            let child = if entry.1 { "" } else { "<failure message=\"failed\"/>" };
            format!("<testcase classname=\"lgwks_bot::it\" name=\"{}\" time=\"0.010\">{child}</testcase>", entry.0)
        })
        .collect();
    format!(
        "<?xml version=\"1.0\"?><testsuites><testsuite name=\"lgwks_bot\">{}</testsuite></testsuites>",
        body.join("")
    )
}

/// Runs the command line and returns the code with both streams.
fn run(arguments: &[String]) -> Result<(u8, String, String), Box<dyn Error>> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(
        arguments,
        &mut Streams {
            out: &mut out,
            err: &mut err,
        },
    )?;
    Ok((
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    ))
}

/// The arguments every step of the journey shares.
fn common(root: &Path, revision: &str) -> Vec<String> {
    [
        "--root",
        &root.display().to_string(),
        "--db",
        &root.join("receipts.sqlite").display().to_string(),
        "--revision",
        revision,
        "--head",
        revision,
        "--platform",
        "linux-x86_64",
    ]
    .iter()
    .map(|argument| (*argument).to_owned())
    .collect()
}

/// A fixture holding the map, the spec and the two shard reports.
fn journey(passing: bool) -> Result<(Fixture, Vec<String>), Box<dyn Error>> {
    let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-acceptance-test")?;
    let root = fixture.path();
    fs::create_dir_all(root.join("docs/acceptance"))?;
    fs::write(root.join("docs/acceptance/t-rows.toml"), MAP)?;
    fs::write(root.join("docs/spec.md"), SPEC)?;
    fs::write(
        root.join("shard-1.xml"),
        report(&[("a::one_t01", true), ("a::two_t01", passing)]),
    )?;
    fs::write(
        root.join("shard-2.xml"),
        report(&[("b::one_t02", true), ("unrelated::elsewhere", true)]),
    )?;
    let mut arguments = common(
        root,
        if passing {
            "rev-journey"
        } else {
            "rev-failing"
        },
    );
    arguments.push("--from-junit".to_owned());
    arguments.push(root.join("shard-1.xml").display().to_string());
    arguments.push(root.join("shard-2.xml").display().to_string());
    Ok((fixture, arguments))
}

#[test]
fn a_green_journey_records_renders_exports_and_checks() -> TestResult {
    let (fixture, mut arguments) = journey(true)?;
    arguments.extend([
        "--spec".to_owned(),
        "docs/spec.md".to_owned(),
        "--write-table".to_owned(),
        "--export".to_owned(),
        fixture
            .path()
            .join("out/receipt.json")
            .display()
            .to_string(),
    ]);
    let (code, out, err) = run(&arguments)?;
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("rewrote the generated table in docs/spec.md"),
        "{out}"
    );
    assert!(
        out.contains("tests_passed=3/3 rows_still_partial=1 partial=T02"),
        "{out}"
    );
    let table = fs::read_to_string(fixture.path().join("docs/spec.md"))?;
    assert!(table.contains("| T01 | exercised | 2/2 | — |"), "{table}");
    assert!(
        table.contains("| T02 | present | 1/1 | the second half is not asserted |"),
        "{table}"
    );
    let exported = fs::read_to_string(fixture.path().join("out/receipt.json"))?;
    assert!(
        exported.contains("\"observed_state\": \"exercised\""),
        "{exported}"
    );
    assert!(exported.ends_with("}\n"), "the export ends with a newline");
    Ok(())
}

#[test]
fn the_check_passes_on_a_rendered_table_and_fails_on_an_edited_one() -> TestResult {
    let (fixture, arguments) = journey(true)?;
    let mut write = arguments;
    write.extend([
        "--spec".to_owned(),
        "docs/spec.md".to_owned(),
        "--write-table".to_owned(),
    ]);
    let (code, out, err) = run(&write)?;
    assert_eq!(code, 0, "{out}{err}");
    let mut check = common(fixture.path(), "rev-journey");
    check.extend([
        "--spec".to_owned(),
        "docs/spec.md".to_owned(),
        "--check".to_owned(),
    ]);
    let (code, out, err) = run(&check)?;
    assert_eq!(code, 0, "a rendered table checks: {out}{err}");
    let spec = fixture.path().join("docs/spec.md");
    let edited = fs::read_to_string(&spec)?.replace("| T01 | exercised |", "| T01 | accepted |");
    fs::write(&spec, edited)?;
    let (code, _, err) = run(&check)?;
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("is not what the map and the receipt render"),
        "{err}"
    );
    Ok(())
}

#[test]
fn a_failing_named_test_fails_the_journey_and_is_named() -> TestResult {
    let (_fixture, arguments) = journey(false)?;
    let (code, _, err) = run(&arguments)?;
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("named tests not passing: 1"), "{err}");
    assert!(err.contains("T01::it::a::two_t01=failed"), "{err}");
    Ok(())
}

#[test]
fn a_named_test_no_shard_reported_is_not_run() -> TestResult {
    let (fixture, arguments) = journey(true)?;
    fs::write(
        fixture.path().join("shard-2.xml"),
        report(&[("unrelated::elsewhere", true)]),
    )?;
    let (code, _, err) = run(&arguments)?;
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("T02::it::b::one_t02 was not run"), "{err}");
    Ok(())
}

#[test]
fn an_unknown_argument_and_a_missing_map_are_refusals() -> TestResult {
    let (fixture, _) = journey(true)?;
    let unknown = run(&["--bogus".to_owned()])
        .err()
        .map(|error| error.to_string());
    assert_eq!(
        unknown.map(|text| text.contains("unknown argument")),
        Some(true),
        "an unknown flag"
    );
    let mut missing = common(fixture.path(), "rev");
    missing.extend(["--map".to_owned(), "docs/absent.toml".to_owned()]);
    let refused = run(&missing).err().map(|error| error.to_string());
    assert_eq!(
        refused.map(|text| text.contains("row map not found")),
        Some(true),
        "a missing map"
    );
    Ok(())
}
