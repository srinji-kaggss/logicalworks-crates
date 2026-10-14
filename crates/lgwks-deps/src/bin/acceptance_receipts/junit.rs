//! What the named tests did: read from the JUnit reports CI already wrote, or
//! from a nextest run of exactly the named tests.
//!
//! A map entry names its test as `binary::test`, and nextest names its own as
//! `lgwks_bot::binary$test`, so the two never agree on a prefix. A tail that
//! follows any of the three separators the spellings use (`::`, `$`, a space)
//! joins them without accepting a different test whose name merely starts the
//! same way.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use lgwks_std::json::{self, Value};

use super::{Failure, fail, head_of, tail_of};

/// The separators a reported name may carry before a map entry's tail.
const TAIL_SEPARATORS: [&str; 3] = ["::", "$", " "];

/// One test's outcome: `ok`, `failed`, `ignored`, `timeout` or `aborted`, and its milliseconds.
#[derive(Clone)]
pub(super) struct Observed {
    /// What happened.
    pub(super) status: String,
    /// How long it took.
    pub(super) millis: f64,
}

/// Every reported test and its outcome, in the order reported.
pub(super) type Outcomes = Vec<(String, Observed)>;

/// Records `name` in `outcomes`, replacing an earlier report of the same name in place.
fn record(outcomes: &mut Outcomes, name: String, observed: Observed) {
    match outcomes.iter_mut().find(|entry| entry.0 == name) {
        Some(entry) => entry.1 = observed,
        None => outcomes.push((name, observed)),
    }
}

/// Every spelling of one map entry that a nextest report may carry, longest first.
pub(super) fn names_for(entry: &str) -> Vec<String> {
    let mut candidates = vec![entry.to_owned()];
    if let Some((_, rest)) = entry.split_once("::")
        && !rest.is_empty()
    {
        candidates.push(rest.to_owned());
    }
    candidates.sort_by_key(|name| std::cmp::Reverse(name.len()));
    candidates.dedup();
    candidates
}

/// Whether a reported name is `candidate` or ends in a separator and `candidate`.
fn reported_matches(reported: &str, candidate: &str) -> bool {
    reported == candidate
        || TAIL_SEPARATORS
            .iter()
            .any(|separator| reported.ends_with(&format!("{separator}{candidate}")))
}

/// The observed outcome for a map entry, or `None` when the run never ran it.
pub(super) fn match_outcome(outcomes: &Outcomes, entry: &str) -> Option<Observed> {
    for candidate in names_for(entry) {
        for reported in outcomes {
            if reported_matches(&reported.0, &candidate) {
                return Some(reported.1.clone());
            }
        }
    }
    None
}

/// Whether a reported name answers for any of the map's entries.
pub(super) fn is_asked_for(reported: &str, asked: &[String]) -> bool {
    asked
        .iter()
        .flat_map(|entry| names_for(entry))
        .any(|candidate| reported_matches(reported, &candidate))
}

/// The character an XML entity stands for.
fn entity_char(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        numeric => numeric
            .strip_prefix('#')
            .and_then(|digits| match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => digits.parse::<u32>().ok(),
            })
            .and_then(char::from_u32),
    }
}

/// A decoded XML attribute value.
fn decode_entities(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find('&') {
        out.push_str(head_of(rest, start));
        let after = tail_of(rest, start.saturating_add(1));
        let Some((entity, remainder)) = after.split_once(';') else {
            out.push('&');
            rest = after;
            continue;
        };
        match entity_char(entity) {
            Some(ch) => out.push(ch),
            None => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = remainder;
    }
    out.push_str(rest);
    out
}

/// One element's tag, as the scanner reads it.
struct Tag {
    /// The element name.
    name: String,
    /// The attributes, decoded.
    attributes: Vec<(String, String)>,
    /// Whether the tag closes itself.
    empty: bool,
}

/// Reads the inside of a start tag (between `<` and `>`).
fn read_tag(inside: &str) -> Option<Tag> {
    let trimmed = inside.trim();
    let (body, empty) = match trimmed.strip_suffix('/') {
        Some(front) => (front, true),
        None => (trimmed, false),
    };
    let (name, after_name) = match body.split_once(char::is_whitespace) {
        Some(parts) => parts,
        None => (body, ""),
    };
    if name.is_empty() {
        return None;
    }
    let mut attributes = Vec::new();
    let mut rest = after_name.trim_start();
    while !rest.is_empty() {
        let (key, after_key) = rest.split_once('=')?;
        let after_key = after_key.trim_start();
        let quote = after_key
            .chars()
            .next()
            .filter(|ch| *ch == '"' || *ch == '\'')?;
        let (value, remainder) = tail_of(after_key, 1).split_once(quote)?;
        attributes.push((key.trim().to_owned(), decode_entities(value)));
        rest = remainder.trim_start();
    }
    Some(Tag {
        name: name.to_owned(),
        attributes,
        empty,
    })
}

/// A `<testcase>` as the report states it.
struct Case {
    /// Its `name`, when it has one.
    name: Option<String>,
    /// Its `time` in seconds, as written.
    time: Option<String>,
    /// Whether a `<failure>` or `<error>` child is present.
    failed: bool,
    /// Whether a `<skipped>` child is present.
    skipped: bool,
}

/// The attribute `key` of a tag.
fn attribute(tag: &Tag, key: &str) -> Option<String> {
    tag.attributes
        .iter()
        .find(|pair| pair.0 == key)
        .map(|pair| pair.1.clone())
}

/// Logs why a document is not well-formed, then hands the reason back as the `Err`.
fn malformed<T>(reason: String) -> Result<T, String> {
    lgwks_std::trace::debug!(reason = %reason, "a JUnit document is malformed");
    Err(reason)
}

/// Every `<testcase>` of a well-formed XML document, or why it is not one.
fn read_cases(text: &str) -> Result<Vec<Case>, String> {
    let mut cases: Vec<Case> = Vec::new();
    let mut stack: Vec<(String, Option<usize>)> = Vec::new();
    let mut roots = 0_usize;
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        rest = tail_of(rest, open.saturating_add(1));
        if let Some(after) = rest.strip_prefix("!--") {
            let end = after.find("-->").ok_or("a comment is not closed")?;
            rest = tail_of(after, end.saturating_add(3));
            continue;
        }
        if let Some(after) = rest.strip_prefix("![CDATA[") {
            let end = after.find("]]>").ok_or("a CDATA section is not closed")?;
            rest = tail_of(after, end.saturating_add(3));
            continue;
        }
        let end = rest.find('>').ok_or("a tag is not closed")?;
        let inside = head_of(rest, end);
        rest = tail_of(rest, end.saturating_add(1));
        if inside.starts_with('?') || inside.starts_with('!') {
            continue;
        }
        if let Some(closing) = inside.strip_prefix('/') {
            let (name, _) = stack.pop().ok_or("an end tag has no start tag")?;
            if name != closing.trim() {
                return malformed(format!("</{}> closes <{name}>", closing.trim()));
            }
            continue;
        }
        let tag = read_tag(inside).ok_or("a tag is malformed")?;
        if stack.is_empty() {
            roots = roots.saturating_add(1);
            if roots > 1 {
                return malformed("junk after the document element".to_owned());
            }
        }
        let parent_case = stack.last().and_then(|entry| entry.1);
        if let Some(case) = parent_case.and_then(|index| cases.get_mut(index)) {
            match tag.name.as_str() {
                "failure" | "error" => case.failed = true,
                "skipped" => case.skipped = true,
                _ => {}
            }
        }
        let own_case = if tag.name == "testcase" {
            cases.push(Case {
                name: attribute(&tag, "name"),
                time: attribute(&tag, "time"),
                failed: false,
                skipped: false,
            });
            Some(cases.len().saturating_sub(1))
        } else {
            None
        };
        if !tag.empty {
            stack.push((tag.name, own_case));
        }
    }
    if !stack.is_empty() {
        return malformed("an element is not closed".to_owned());
    }
    if roots == 0 {
        return malformed("no document element".to_owned());
    }
    Ok(cases)
}

/// Milliseconds from a `time` attribute in seconds.
fn millis_of(path: &Path, time: Option<String>) -> Result<f64, Failure> {
    match time.filter(|seconds| !seconds.is_empty()) {
        Some(seconds) => match seconds.trim().parse::<f64>() {
            Ok(parsed) => Ok(parsed * 1000.0),
            Err(_) => fail(
                format!(
                    "{} has a testcase time {seconds:?} that is not a number",
                    path.display()
                ),
                "millis_of",
            ),
        },
        None => Ok(0.0),
    }
}

/// Reads one JUnit file into outcomes.
///
/// nextest writes one `<testcase>` per test, named `binary::module::test` for
/// this crate's shape, with a `<failure>`, `<skipped>` or `<error>` child carrying
/// the outcome. A malformed file is a refusal rather than a partial parse: a JUnit
/// file this reader could not finish is evidence of nothing, and treating it as an
/// empty one would turn every row's named test into "not run" while looking like a
/// clean report.
pub(super) fn parse_junit(path: &Path) -> Result<Outcomes, Failure> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return fail(
                format!("{} is not readable JUnit XML: {error}", path.display()),
                "parse_junit",
            );
        }
    };
    let cases = match read_cases(&text) {
        Ok(cases) => cases,
        Err(problem) => {
            return fail(
                format!("{} is not readable JUnit XML: {problem}", path.display()),
                "parse_junit",
            );
        }
    };
    if cases.is_empty() {
        return fail(
            format!(
                "{} holds no <testcase> elements; it is not a test report",
                path.display()
            ),
            "parse_junit",
        );
    }
    let mut outcomes = Outcomes::new();
    for case in cases {
        let Some(name) = case.name.filter(|name| !name.is_empty()) else {
            continue;
        };
        let status = if case.failed {
            "failed"
        } else if case.skipped {
            "ignored"
        } else {
            "ok"
        };
        let millis = millis_of(path, case.time)?;
        record(
            &mut outcomes,
            name,
            Observed {
                status: status.to_owned(),
                millis,
            },
        );
    }
    if outcomes.is_empty() {
        return fail(
            format!(
                "{} names no testcase, so it cannot answer for any row",
                path.display()
            ),
            "parse_junit",
        );
    }
    Ok(outcomes)
}

/// Every shard's results in one map, with a duplicate named rather than merged.
///
/// The shards partition one suite, so a name appearing twice means the
/// partitioning is wrong, and silently keeping the first would make the receipt
/// describe a run that could not have happened.
pub(super) fn merge_junit(paths: &[PathBuf]) -> Result<Outcomes, Failure> {
    let mut merged = Outcomes::new();
    let mut origin: Vec<(String, PathBuf)> = Vec::new();
    for path in paths {
        for reported in parse_junit(path)? {
            if let Some(first) = origin.iter().find(|entry| entry.0 == reported.0) {
                return fail(
                    format!(
                        "{} appears in both {} and {}; the shards partition one suite, so a name may not be reported twice",
                        reported.0,
                        first.1.display(),
                        path.display()
                    ),
                    "merge_junit",
                );
            }
            origin.push((reported.0.clone(), path.clone()));
            merged.push(reported);
        }
    }
    Ok(merged)
}

/// What the CI job it is reading from actually ran.
pub(super) fn junit_command(paths: &[PathBuf], package: &str, features: &str) -> String {
    let names: Vec<String> = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!(
        "cargo nextest run --locked -p {package} --features {features} --profile ci --partition count:4 (JUnit reports: {})",
        names.join(", ")
    )
}

/// Reads one `libtest-json-plus` event line into a test's name and outcome.
fn read_event(line: &str) -> Option<(String, Observed)> {
    let event: Value = json::from_str(line).ok()?;
    if event.get("type").and_then(Value::as_str) != Some("test") {
        return None;
    }
    let kind = event.get("event").and_then(Value::as_str)?;
    let name = event.get("name").and_then(Value::as_str)?;
    if !["ok", "failed", "ignored", "timeout", "aborted"].contains(&kind) {
        return None;
    }
    let millis = match event.get("exec_time").and_then(Value::as_f64) {
        Some(seconds) => seconds * 1000.0,
        None => 0.0,
    };
    Some((
        name.to_owned(),
        Observed {
            status: kind.to_owned(),
            millis,
        },
    ))
}

/// Runs the selected tests once and returns each test's outcome and the seconds taken.
///
/// `libtest-json-plus` is the only nextest format that reports each test by name as
/// it finishes, so a crash or a kill cannot silently become a pass.
pub(super) fn run_nextest(
    root: &Path,
    expression: &str,
    package: &str,
    features: &str,
) -> Result<(Outcomes, f64), Failure> {
    let started = Instant::now();
    let done = Command::new("cargo")
        .args([
            "nextest",
            "run",
            "--locked",
            "-p",
            package,
            "--features",
            features,
        ])
        .args([
            "--message-format",
            "libtest-json-plus",
            "--no-fail-fast",
            "-E",
            expression,
        ])
        .env("NEXTEST_EXPERIMENTAL_LIBTEST_JSON", "1")
        .current_dir(root)
        .stdin(Stdio::null())
        .output();
    let output = match done {
        Ok(output) => output,
        Err(error) => {
            return fail(
                format!("cargo nextest could not run: {error}"),
                "run_nextest",
            );
        }
    };
    let elapsed = started.elapsed().as_secs_f64();
    let mut outcomes = Outcomes::new();
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    for line in printed
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
    {
        if let Some((name, observed)) = read_event(line) {
            record(&mut outcomes, name, observed);
        }
    }
    if outcomes.is_empty() {
        let code = output.status.code();
        let stderr: String = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(600)
            .collect();
        return fail(
            format!("nextest reported no per-test events; exit={code:?} stderr={stderr}"),
            "run_nextest",
        );
    }
    Ok((outcomes, elapsed))
}

#[cfg(test)]
mod tests {
    use super::{Observed, match_outcome, merge_junit, names_for, parse_junit, read_cases};
    use crate::fixture::Fixture;
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;

    type TestResult = Result<(), Box<dyn Error>>;

    /// A nextest-shaped JUnit report for `cases`, each `(name, status)`.
    fn junit_document(cases: &[(&str, &str)]) -> String {
        let body: Vec<String> = cases
            .iter()
            .map(|entry| {
                let child = match entry.1 {
                    "failed" => "<failure message=\"failed\">assertion failed</failure>",
                    "ignored" => "<skipped message=\"ignored\"/>",
                    _ => "",
                };
                format!("    <testcase classname=\"lgwks_bot::it\" name=\"{}\" time=\"0.010\">{child}</testcase>", entry.0)
            })
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites><testsuite name=\"lgwks_bot\" tests=\"{}\">\n{}\n  </testsuite></testsuites>\n",
            cases.len(),
            body.join("\n")
        )
    }

    /// Writes a report into the fixture and returns its path.
    fn write_report(
        fixture: &Fixture,
        name: &str,
        cases: &[(&str, &str)],
    ) -> Result<PathBuf, Box<dyn Error>> {
        let path = fixture.path().join(name);
        fs::write(&path, junit_document(cases))?;
        Ok(path)
    }

    #[test]
    fn a_malformed_junit_file_is_refused() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-junit-test")?;
        let path = fixture.path().join("broken.xml");
        fs::write(&path, "<testsuites><testsuite><testcase name=\"a\"")?;
        let refused = parse_junit(&path).err().map(|failure| failure.0);
        assert_eq!(
            refused.map(|text| text.contains("not readable JUnit")),
            Some(true),
            "a truncated tag is a refusal"
        );
        Ok(())
    }

    #[test]
    fn junit_without_a_testcase_is_refused() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-junit-test")?;
        let path = fixture.path().join("empty.xml");
        fs::write(&path, "<?xml version=\"1.0\"?><testsuites/>")?;
        let refused = parse_junit(&path).err().map(|failure| failure.0);
        assert_eq!(
            refused.map(|text| text.contains("no <testcase>")),
            Some(true),
            "an empty report answers for nothing"
        );
        Ok(())
    }

    #[test]
    fn statuses_and_the_wellformedness_rules_are_read_as_a_parser_reads_them() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-junit-test")?;
        let path = write_report(
            &fixture,
            "one.xml",
            &[
                ("a::ok_case", "ok"),
                ("a::bad_case", "failed"),
                ("a::skipped_case", "ignored"),
            ],
        )?;
        let found = parse_junit(&path)?;
        let status = |name: &str| {
            found
                .iter()
                .find(|entry| entry.0 == name)
                .map(|entry| entry.1.status.clone())
        };
        assert_eq!(
            status("a::ok_case").as_deref(),
            Some("ok"),
            "a bare testcase passes"
        );
        assert_eq!(
            status("a::bad_case").as_deref(),
            Some("failed"),
            "a failure child fails it"
        );
        assert_eq!(
            status("a::skipped_case").as_deref(),
            Some("ignored"),
            "a skipped child ignores it"
        );
        assert!(read_cases("<a><b></a></b>").is_err(), "crossed tags");
        assert!(read_cases("<a></a><b></b>").is_err(), "two roots");
        let hidden = read_cases("<a><!-- <testcase name=\"x\"> --></a>")?;
        assert!(hidden.is_empty(), "a comment hides its tags");
        Ok(())
    }

    #[test]
    fn four_shards_merge_and_a_duplicate_is_refused() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-junit-test")?;
        let shards = vec![
            write_report(&fixture, "shard-1.xml", &[("a::one_t01", "ok")])?,
            write_report(&fixture, "shard-2.xml", &[("a::two_t01", "ok")])?,
            write_report(&fixture, "shard-3.xml", &[("b::one_t02", "ok")])?,
            write_report(&fixture, "shard-4.xml", &[("unrelated::elsewhere", "ok")])?,
        ];
        assert_eq!(merge_junit(&shards)?.len(), 4, "four shards, four tests");
        let twice = vec![
            write_report(&fixture, "dup-1.xml", &[("a::one_t01", "ok")])?,
            write_report(&fixture, "dup-2.xml", &[("a::one_t01", "ok")])?,
        ];
        let refused = merge_junit(&twice).err().map(|failure| failure.0);
        assert_eq!(
            refused.map(|text| text.contains("may not be reported twice")),
            Some(true),
            "a repeated name"
        );
        Ok(())
    }

    #[test]
    fn a_map_entry_matches_its_nextest_spelling_and_nothing_longer() {
        assert_eq!(
            names_for("it::a::one"),
            vec!["it::a::one".to_owned(), "a::one".to_owned()],
            "longest first"
        );
        let outcomes = vec![
            (
                "lgwks_bot::it$a::one".to_owned(),
                Observed {
                    status: "ok".to_owned(),
                    millis: 1.0,
                },
            ),
            (
                "lgwks_bot::it$a::one_more".to_owned(),
                Observed {
                    status: "failed".to_owned(),
                    millis: 2.0,
                },
            ),
        ];
        assert_eq!(
            match_outcome(&outcomes, "it::a::one").map(|found| found.status),
            Some("ok".to_owned()),
            "its own spelling"
        );
        assert_eq!(
            match_outcome(&outcomes, "it::a::on").map(|found| found.status),
            None,
            "a prefix is not a match"
        );
    }
}
