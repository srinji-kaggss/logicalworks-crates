//! The map, and the spec's per-row table rendered from it and one revision's receipt.
//!
//! The table is *rendered from the database for one exact revision*, not written
//! by hand: `--write-table` rewrites the block between the two marker comments, and
//! `--check` compares the committed block against what the map and that revision's
//! receipt render. A hand-edited table that no run produced is a claim with nothing
//! behind it, so `--check` is the gate that makes the table evidence rather than
//! prose.
//!
//! A row reads `accepted` only when the revision it was observed at is the head
//! being rendered. A receipt for an older commit is evidence about *that* code, and
//! promoting a row on the strength of it would be the one thing this exists to
//! prevent.

use std::path::Path;

use lgwks_std::json::{self, Value, json};

use super::store::{Run, Tested};
use super::toml_subset::{Table, list, raw, read_tables, text};
use super::{Failure, fail, head_of, tail_of};

/// The generated block's own delimiters, so `--write-table` and `--check` agree on
/// exactly which lines belong to the run rather than to the prose around them.
const TABLE_START: &str = "<!-- acceptance-table: start -->";

/// See [`TABLE_START`].
const TABLE_END: &str = "<!-- acceptance-table: end -->";

/// The evidence ladder. A receipt may confirm a state at or below the one the map
/// claims; it can never promote one on its own, and the head is what promotes a row
/// to `accepted`.
const LADDER: [&str; 6] = [
    "planned",
    "missing",
    "present",
    "exercised",
    "independently_evidenced",
    "accepted",
];

/// States a row may hold when the receipt observed every named test passing.
const OBSERVED_WHEN_ALL_PASS: [&str; 3] = ["exercised", "independently_evidenced", "accepted"];

/// One row of the map.
pub(super) struct Row {
    /// `TNN`.
    pub(super) id: String,
    /// The contract the row requires.
    pub(super) requires: String,
    /// The state the map claims.
    pub(super) state: String,
    /// What the named tests still do not assert.
    pub(super) gap: String,
    /// The named tests.
    pub(super) tests: Vec<String>,
}

/// What the map says about itself.
pub(super) struct Header {
    /// The specification the rows belong to.
    pub(super) spec: String,
}

/// A float as Python prints it: `1.0`, not `1`.
pub(super) fn py_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Whether `id` is `T` and two digits.
fn is_row_id(id: &str) -> bool {
    id.strip_prefix('T')
        .is_some_and(|digits| digits.len() == 2 && digits.chars().all(|ch| ch.is_ascii_digit()))
}

/// The text under `key`, or empty when the key is absent.
fn text_or_empty(table: &Table, key: &str) -> String {
    match text(table, key) {
        Some(found) => found.to_owned(),
        None => String::new(),
    }
}

/// Reads the map: its header, then every row in file order.
pub(super) fn load_rows(map_path: &Path) -> Result<(Header, Vec<Row>), Failure> {
    if !map_path.is_file() {
        return fail(
            format!("row map not found: {}", map_path.display()),
            "load_rows",
        );
    }
    let source = match std::fs::read_to_string(map_path) {
        Ok(source) => source,
        Err(error) => return fail(format!("{}: {error}", map_path.display()), "load_rows"),
    };
    let tables = read_tables(&source);
    let top = tables.iter().find(|table| table.path.is_empty());
    let schema = top.and_then(|table| raw(table, "schema"));
    if schema != Some("1") {
        return fail(
            format!(
                "{} declares schema {schema:?}, and this reader understands schema 1",
                map_path.display()
            ),
            "load_rows",
        );
    }
    let spec = match top {
        Some(table) => text_or_empty(table, "spec"),
        None => String::new(),
    };
    let mut rows: Vec<Row> = Vec::new();
    for table in tables
        .iter()
        .filter(|table| table.path == "row" && table.array)
    {
        let row = build_row(table, &rows)?;
        rows.push(row);
    }
    if rows.is_empty() {
        return fail(
            format!("row map has no [[row]] entries: {}", map_path.display()),
            "load_rows",
        );
    }
    Ok((Header { spec }, rows))
}

/// Builds and validates one row; `earlier` are the rows read before it.
fn build_row(table: &Table, earlier: &[Row]) -> Result<Row, Failure> {
    let id = text_or_empty(table, "id");
    if !is_row_id(&id) {
        return fail(format!("row id is not TNN: {id:?}"), "build_row");
    }
    if earlier.iter().any(|row| row.id == id) {
        return fail(format!("row id appears twice: {id}"), "build_row");
    }
    let Some(tests) = list(table, "tests") else {
        return fail(format!("{id} has no tests list"), "build_row");
    };
    let state = text_or_empty(table, "state");
    if !LADDER.contains(&state.as_str()) {
        return fail(
            format!("{id} claims state {state:?}, which is not on the ladder"),
            "build_row",
        );
    }
    Ok(Row {
        id,
        requires: text_or_empty(table, "requires"),
        state,
        gap: text_or_empty(table, "gap"),
        tests: tests.to_vec(),
    })
}

/// Reduces a map entry to the test name nextest's filter matches.
///
/// The map writes every test as `<binary>::<test>`, and nextest matches its
/// `test(/re/)` filter against the test alone.
pub(super) fn test_name(entry: &str) -> &str {
    match entry.split_once("::") {
        Some((_, rest)) => rest,
        None => entry,
    }
}

/// `name` with every regex-special character escaped.
fn regex_escape(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\u{b}\u{c}".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// One nextest filter expression naming every test the map carries.
///
/// Anchoring each name is what keeps a row's receipt from being satisfied by a
/// longer name that happens to contain it.
pub(super) fn filter_expression(rows: &[Row]) -> Result<String, Failure> {
    let mut names: Vec<&str> = rows
        .iter()
        .flat_map(|row| row.tests.iter())
        .map(|entry| test_name(entry))
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return fail("no test names to run".to_owned(), "filter_expression");
    }
    let alternation: Vec<String> = names
        .iter()
        .map(|name| format!("^{}$", regex_escape(name)))
        .collect();
    Ok(format!("test(/{}/)", alternation.join("|")))
}

/// What one row reads as, given the map's claim and what the run observed.
///
/// The run can lower a claim and never raise one: a row the map calls `present`
/// stays `present` even when every named test passes, because the gap sentence, not
/// the test count, is what holds it there. The single promotion is `accepted`, and
/// it requires the receipt's revision to be the head being rendered.
pub(super) fn observed_state(
    claimed: &str,
    passed: usize,
    total: usize,
    revision: &str,
    head: &str,
) -> String {
    if total == 0 {
        return "planned".to_owned();
    }
    if passed != total {
        return if passed > 0 { "present" } else { "missing" }.to_owned();
    }
    if claimed == "accepted" && revision != head {
        return "exercised".to_owned();
    }
    claimed.to_owned()
}

/// The recorded outcome of a row's test.
fn find_result<'found>(
    results: &'found [Tested],
    row_id: &str,
    test: &str,
) -> Option<&'found Tested> {
    results
        .iter()
        .find(|found| found.row_id == row_id && found.test == test)
}

/// How many of a row's named tests the receipt records as passing.
pub(super) fn passed_count(row: &Row, results: &[Tested]) -> usize {
    row.tests
        .iter()
        .filter(|test| {
            find_result(results, &row.id, test).is_some_and(|found| found.status == "ok")
        })
        .count()
}

/// The state a row reads as in this receipt.
pub(super) fn row_state(row: &Row, results: &[Tested], revision: &str, head: &str) -> String {
    observed_state(
        &row.state,
        passed_count(row, results),
        row.tests.len(),
        revision,
        head,
    )
}

/// The rows that read as still partial in this receipt.
pub(super) fn partial_rows<'rows>(
    rows: &'rows [Row],
    results: &[Tested],
    revision: &str,
    head: &str,
) -> Vec<&'rows str> {
    rows.iter()
        .filter(|row| {
            !OBSERVED_WHEN_ALL_PASS.contains(&row_state(row, results, revision, head).as_str())
        })
        .map(|row| row.id.as_str())
        .collect()
}

/// The spec's per-row block, rendered from the map and one revision's receipt.
pub(super) fn render_table(rows: &[Row], run: &Run, results: &[Tested], head: &str) -> String {
    let short = head_of(&run.revision, 8);
    let wall = py_float(run.wall_seconds);
    let mut lines = vec![
        TABLE_START.to_owned(),
        String::new(),
        format!(
            "Generated by the `acceptance-receipts` binary from [`docs/acceptance/t-rows.toml`](acceptance/t-rows.toml) and the receipt recorded for `{short}` on `{}` with features `{}` at {}, {wall}s wall, over {} with features `{}`. The full command is in the receipt's `run` table; it is not repeated here because its `-E` alternation names every test in the map and is several kilobytes of prose nobody reads. A row is `exercised` only when a named test drove its falsifier; `present` means named tests cover part of the text and the gap column says what is not asserted. A row reads `accepted` only when the revision it was observed at is the head being rendered, and none is here: the externally bounded subprocess and per-backend campaign has not run. Re-record with `--write-table`; `--check` fails when this block disagrees with the map and the receipt.",
            run.platform, run.feature_set, run.run_at, run.package, run.feature_set
        ),
        String::new(),
        "| ID | State | Tests | Gap |".to_owned(),
        "|---|---|---|---|".to_owned(),
    ];
    let mut named_total = 0_usize;
    let mut passed_total = 0_usize;
    for row in rows {
        let passed = passed_count(row, results);
        named_total = named_total.saturating_add(row.tests.len());
        passed_total = passed_total.saturating_add(passed);
        let gap = match row.gap.trim() {
            "" => "—",
            found => found,
        };
        lines.push(format!(
            "| {} | {} | {passed}/{} | {gap} |",
            row.id,
            row_state(row, results, &run.revision, head),
            row.tests.len()
        ));
    }
    let partial = partial_rows(rows, results, &run.revision, head);
    lines.extend([
        String::new(),
        format!(
            "At `{short}` on `{}`: {} rows, {passed_total}/{named_total} named tests passing, {} rows exercised, {} still partial, {wall}s wall. Still partial: {}.",
            run.platform,
            rows.len(),
            rows.len().saturating_sub(partial.len()),
            partial.len(),
            partial.join(", ")
        ),
        String::new(),
        TABLE_END.to_owned(),
    ]);
    lines.join("\n")
}

/// The byte range of the generated block, markers included.
fn block_range(spec_text: &str) -> Result<(usize, usize), Failure> {
    match (spec_text.find(TABLE_START), spec_text.find(TABLE_END)) {
        (Some(start), Some(end)) if end >= start => {
            Ok((start, end.saturating_add(TABLE_END.len())))
        }
        _ => fail(
            format!(
                "the generated table's markers are missing or out of order; expected {TABLE_START:?} before {TABLE_END:?}"
            ),
            "block_range",
        ),
    }
}

/// Puts `table` between the markers, or refuses if the markers are not there.
///
/// A missing marker is a refusal rather than an append: a script that cannot find
/// its own delimiters would otherwise add a second table beside the first and leave
/// a reader to guess which one a run produced.
pub(super) fn replace_table(spec_text: &str, table: &str) -> Result<String, Failure> {
    let (start, end) = block_range(spec_text)?;
    Ok(format!(
        "{}{table}{}",
        head_of(spec_text, start),
        tail_of(spec_text, end)
    ))
}

/// The block currently between the markers, or a refusal.
pub(super) fn committed_table(spec_text: &str) -> Result<String, Failure> {
    let (start, end) = block_range(spec_text)?;
    match spec_text.get(start..end) {
        Some(block) => Ok(block.to_owned()),
        None => fail(
            "the generated table's markers split a character".to_owned(),
            "committed_table",
        ),
    }
}

/// A line clipped and quoted for a failure message.
fn quoted(line: Option<&str>) -> String {
    match line {
        Some(text) => format!("{:?}", text.chars().take(160).collect::<String>()),
        None => "'<end of block>'".to_owned(),
    }
}

/// The first lines where two rendered blocks disagree, for a readable failure.
pub(super) fn first_difference(found: &str, wanted: &str) -> Vec<String> {
    let found_lines: Vec<&str> = found.lines().collect();
    let wanted_lines: Vec<&str> = wanted.lines().collect();
    for index in 0..found_lines.len().max(wanted_lines.len()) {
        let left = found_lines.get(index).copied();
        let right = wanted_lines.get(index).copied();
        if left != right {
            let number = index.saturating_add(1);
            return vec![
                format!("line {number}: committed {}", quoted(left)),
                format!("line {number}: expected  {}", quoted(right)),
            ];
        }
    }
    Vec::new()
}

/// One test of a row, as the export states it.
fn exported_test(row: &Row, test: &str, results: &[Tested]) -> Value {
    let found = find_result(results, &row.id, test);
    let status = match found {
        Some(result) => result.status.as_str(),
        None => "missing",
    };
    json!({
        "name": test,
        "status": status,
        "duration_ms": found.map(|result| result.duration_ms),
    })
}

/// The receipt as a portable JSON artifact, for a CI upload.
///
/// The database is the store; this is the copy a build can attach to a run, so it
/// carries the same facts rather than a summary of them.
pub(super) fn export_json(
    rows: &[Row],
    run: &Run,
    results: &[Tested],
    head: &str,
) -> Result<String, Failure> {
    let rendered: Vec<Value> = rows
        .iter()
        .map(|row| {
            let tests: Vec<Value> = row
                .tests
                .iter()
                .map(|test| exported_test(row, test, results))
                .collect();
            json!({
                "id": row.id,
                "requires": row.requires,
                "claimed_state": row.state,
                "observed_state": row_state(row, results, &run.revision, head),
                "gap": row.gap,
                "tests": tests,
            })
        })
        .collect();
    let payload = json!({
        "revision": run.revision,
        "platform": run.platform,
        "feature_set": run.feature_set,
        "package": run.package,
        "command": run.command,
        "wall_seconds": run.wall_seconds,
        "run_at": run.run_at,
        "head": head,
        "rows": rendered,
    });
    match json::to_string_pretty(&payload) {
        Ok(rendered_text) => Ok(format!("{rendered_text}\n")),
        Err(error) => fail(
            format!("the receipt could not be exported: {error}"),
            "export_json",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Row, committed_table, filter_expression, first_difference, observed_state, py_float,
        render_table, replace_table, test_name,
    };
    use crate::store::{Run, Tested};

    /// A two-row map, for the suite to render without reading the repository.
    fn fake_rows(first: &str, second: &str) -> Vec<Row> {
        vec![
            Row {
                id: "T01".to_owned(),
                requires: "DX-01".to_owned(),
                state: first.to_owned(),
                gap: String::new(),
                tests: vec!["it::a::one_t01".to_owned(), "it::a::two_t01".to_owned()],
            },
            Row {
                id: "T02".to_owned(),
                requires: "DX-02".to_owned(),
                state: second.to_owned(),
                gap: "the second half is not asserted".to_owned(),
                tests: vec!["it::b::one_t02".to_owned()],
            },
        ]
    }

    /// A run recorded at `revision`.
    fn fake_run(revision: &str) -> Run {
        Run {
            revision: revision.to_owned(),
            platform: "Darwin-arm64".to_owned(),
            feature_set: "full".to_owned(),
            package: "lgwks_bot".to_owned(),
            command: "cargo nextest run".to_owned(),
            wall_seconds: 1.0,
            run_at: "2026-10-05T00:00:00Z".to_owned(),
        }
    }

    /// The three fake tests with the given statuses.
    fn fake_results(statuses: [&str; 3]) -> Vec<Tested> {
        [
            ("T01", "it::a::one_t01"),
            ("T01", "it::a::two_t01"),
            ("T02", "it::b::one_t02"),
        ]
        .iter()
        .zip(statuses)
        .map(|(named, status)| Tested {
            row_id: named.0.to_owned(),
            test: named.1.to_owned(),
            status: status.to_owned(),
            duration_ms: 1.5,
        })
        .collect()
    }

    #[test]
    fn a_stale_revision_never_renders_accepted() {
        let rows = fake_rows("accepted", "accepted");
        let results = fake_results(["ok", "ok", "ok"]);
        let at_head = render_table(&rows, &fake_run("old"), &results, "old");
        assert!(
            at_head.contains("| T01 | accepted |"),
            "a receipt at the head may promote a row to accepted"
        );
        let stale = render_table(&rows, &fake_run("old"), &results, "newer");
        assert!(
            stale.contains("| T01 | exercised |"),
            "a receipt for an older revision must not render accepted"
        );
        assert!(stale.contains("| T02 | exercised |"), "{stale}");
    }

    #[test]
    fn a_green_run_never_promotes_a_partial_row() {
        let rows = fake_rows("exercised", "present");
        let table = render_table(
            &rows,
            &fake_run("rev"),
            &fake_results(["ok", "ok", "ok"]),
            "rev",
        );
        assert!(table.contains("| T02 | present | 1/1 |"), "{table}");
        assert!(!table.contains("| T02 | exercised |"), "{table}");
        assert!(table.contains("Still partial: T02."), "{table}");
    }

    #[test]
    fn a_failing_test_lowers_the_row_to_present() {
        let rows = fake_rows("exercised", "present");
        let table = render_table(
            &rows,
            &fake_run("rev"),
            &fake_results(["ok", "failed", "ok"]),
            "rev",
        );
        assert!(table.contains("| T01 | present | 1/2 |"), "{table}");
    }

    #[test]
    fn a_missing_named_test_reads_as_not_passed() {
        let rows = fake_rows("exercised", "present");
        let table = render_table(
            &rows,
            &fake_run("rev"),
            &fake_results(["ok", "missing", "missing"]),
            "rev",
        );
        assert!(table.contains("| T01 | present | 1/2 |"), "{table}");
        assert!(table.contains("| T02 | missing | 0/1 |"), "{table}");
    }

    #[test]
    fn the_state_ladder_only_lowers() {
        assert_eq!(
            observed_state("exercised", 0, 0, "a", "a"),
            "planned",
            "no tests named"
        );
        assert_eq!(
            observed_state("exercised", 1, 2, "a", "a"),
            "present",
            "some passed"
        );
        assert_eq!(
            observed_state("exercised", 0, 2, "a", "a"),
            "missing",
            "none passed"
        );
        assert_eq!(
            observed_state("accepted", 2, 2, "a", "b"),
            "exercised",
            "a stale accepted"
        );
        assert_eq!(
            observed_state("present", 2, 2, "a", "a"),
            "present",
            "a claim is never raised"
        );
    }

    #[test]
    fn the_block_is_replaced_between_its_markers_or_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = "before\n<!-- acceptance-table: start -->\nold\n<!-- acceptance-table: end -->\nafter\n";
        let replaced = replace_table(
            spec,
            "<!-- acceptance-table: start -->\nnew\n<!-- acceptance-table: end -->",
        )?;
        assert_eq!(
            replaced,
            "before\n<!-- acceptance-table: start -->\nnew\n<!-- acceptance-table: end -->\nafter\n",
            "swapped"
        );
        assert!(committed_table(spec)?.contains("old"), "read back");
        assert!(
            replace_table("no markers here", "x").is_err(),
            "no markers is a refusal, not an append"
        );
        assert_eq!(
            first_difference("a\nb", "a\nc").len(),
            2,
            "the first differing line, both sides"
        );
        Ok(())
    }

    #[test]
    fn the_filter_anchors_every_name_and_escapes_it() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(test_name("it::a::one"), "a::one", "the binary prefix goes");
        let expression = filter_expression(&fake_rows("exercised", "present"))?;
        assert_eq!(
            expression, "test(/^a::one_t01$|^a::two_t01$|^b::one_t02$/)",
            "anchored and sorted"
        );
        assert_eq!(py_float(1.0), "1.0", "a whole number keeps its point");
        assert_eq!(py_float(19.304), "19.304", "a fraction is itself");
        Ok(())
    }
}
