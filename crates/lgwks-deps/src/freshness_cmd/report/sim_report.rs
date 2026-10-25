//! The table and the JSON array over rows drawn from seeds, including the
//! strings that broke the first JSON writer: backslashes, quotes, newlines.

use std::error::Error;

use lgwks_std::json::{self, Value};
use lgwks_std::seeded::Seeded;

use super::{
    FreshnessResult, Newest, VIA_WIDTH, error_count, print_json, print_table, stale_count,
    summary_line, via_cell,
};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// Strings a repository or an error message might carry.
const AWKWARD: [&str; 6] = [
    "plain",
    "back\\slash",
    "a \"quote\"",
    "line\nbreak",
    "tab\there",
    "snow\u{2603}man",
];

/// A fresh row, a stale row and a failed row.
fn three() -> Vec<FreshnessResult> {
    vec![
        FreshnessResult::found(
            "serde",
            "1.0.0",
            Some(Newest {
                text: "1.0.0".to_owned(),
                stale: false,
            }),
            Some("https://github.com/serde-rs/serde".to_owned()),
            &["serde".to_owned()],
        ),
        FreshnessResult::found(
            "bevy_ecs",
            "0.19.1",
            Some(Newest {
                text: "0.20.0".to_owned(),
                stale: true,
            }),
            None,
            &["lgwks_bot".to_owned()],
        ),
        FreshnessResult::failed("tokio", "1.0.0", &[], "HTTP 503".to_owned()),
    ]
}

#[test]
fn the_totals_count_stale_and_failed_rows_and_name_what_was_skipped() {
    let rows = three();
    assert_eq!(stale_count(&rows), 1, "one stale row");
    assert_eq!(error_count(&rows), 1, "one failed row");
    assert_eq!(
        summary_line(&rows, 0),
        "3 checked, 1 stale, 1 errors",
        "no skipped clause when nothing was skipped"
    );
    assert_eq!(
        summary_line(&rows, 4),
        "3 checked, 1 stale, 1 errors, 4 skipped (not from the queried index)",
        "the skipped count is named"
    );
}

#[test]
fn a_failed_lookup_is_an_error_row_and_never_a_stale_one() {
    let failed = FreshnessResult::failed("tokio", "1.0.0", &[], "HTTP 503".to_owned());
    assert!(failed.is_error(), "it failed");
    assert!(!failed.is_stale(), "a network fault is not staleness");
}

#[test]
fn a_registry_that_offers_nothing_claims_nothing() {
    let bare = FreshnessResult::found("ghost", "1.0.0", None, None, &[]);
    assert!(!bare.is_stale(), "nothing to compare against");
    assert!(!bare.is_error(), "and nothing went wrong");
}

#[test]
fn the_table_shows_version_via_and_failure_in_their_columns() -> TestResult {
    let mut out = Vec::new();
    print_table(&three(), 2, &mut out)?;
    let text = String::from_utf8(out)?;
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.first().is_some_and(|header| header.contains("via")),
        "the header names the via column: {text}"
    );
    let bevy = lines
        .iter()
        .find(|line| line.starts_with("bevy_ecs"))
        .ok_or("no bevy_ecs row")?;
    assert!(
        bevy.contains("0.20.0") && bevy.contains("YES") && bevy.contains("lgwks_bot"),
        "a stale row shows the newest version, the mark and its edge: {bevy}"
    );
    let tokio = lines
        .iter()
        .find(|line| line.starts_with("tokio"))
        .ok_or("no tokio row")?;
    assert!(
        tokio.contains("err") && tokio.contains("HTTP 503") && tokio.contains(" ? "),
        "a failed row shows err and its reason, not a version: {tokio}"
    );
    assert!(
        text.ends_with("3 checked, 1 stale, 1 errors, 2 skipped (not from the queried index)\n"),
        "the totals close the table: {text}"
    );
    Ok(())
}

#[test]
fn a_long_via_list_stays_in_its_column_and_the_json_keeps_every_name() -> TestResult {
    // The shape of `aho-corasick` in this repository's own lock: eight edges.
    let edges: Vec<String> = [
        "ast-grep-core",
        "ast-grep-language",
        "candle-core",
        "candle-nn",
        "candle-transformers",
        "gpui",
        "regex",
        "tokenizers 0.23.2",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    let first = |count: usize| edges.iter().take(count).cloned().collect::<Vec<_>>();
    assert_eq!(via_cell(&[]), "-", "no edge");
    assert_eq!(via_cell(&first(1)), "ast-grep-core", "one edge, whole");
    assert_eq!(
        via_cell(&first(2)),
        "ast-grep-core +1",
        "a second name that overruns the column is counted, never cut in half"
    );
    let pair = vec!["serde".to_owned(), "syn".to_owned()];
    assert_eq!(via_cell(&pair), "serde, syn", "a pair that fits is named");
    let wide = via_cell(&edges);
    assert!(
        wide.starts_with("ast-grep-core") && wide.ends_with(" +7"),
        "the first name, then the rest counted: {wide}"
    );
    let narrow = vec!["serde".to_owned(), "syn".to_owned(), "quote".to_owned()];
    assert_eq!(via_cell(&narrow), "serde, syn, quote", "all of them fit");
    let long_first = vec!["a-crate-name-longer-than-the-column".to_owned()];
    assert_eq!(
        via_cell(&long_first),
        "a-crate-name-longer-than-the-column",
        "a lone long name is shown whole"
    );
    assert!(
        VIA_WIDTH >= via_cell(&narrow).len(),
        "the narrow case is inside the column"
    );
    let row = FreshnessResult::found("aho-corasick", "1.1.5", None, None, &edges);
    let mut out = Vec::new();
    print_json(&[row], &mut out)?;
    let parsed: Value = json::from_str(&String::from_utf8(out)?)?;
    assert_eq!(
        parsed[0]["via"].as_array().map(Vec::len),
        Some(8),
        "the JSON keeps all eight"
    );
    Ok(())
}

#[test]
fn the_json_has_a_fixed_set_of_keys_and_an_array_of_edges() -> TestResult {
    let mut out = Vec::new();
    print_json(&three(), &mut out)?;
    let parsed: Value = json::from_str(&String::from_utf8(out)?)?;
    let rows = parsed.as_array().ok_or("the report is an array")?;
    assert_eq!(rows.len(), 3, "one object per package");
    for row in rows {
        let object = row.as_object().ok_or("a row is an object")?;
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "error",
                "latest",
                "name",
                "repository",
                "resolved",
                "stale",
                "via"
            ],
            "every key is always present"
        );
    }
    let bevy = rows.get(1).ok_or("no second row")?;
    assert_eq!(bevy["stale"], true, "the stale flag");
    assert_eq!(bevy["latest"], "0.20.0", "the newest version");
    assert_eq!(bevy["via"][0], "lgwks_bot", "the edge");
    assert!(
        bevy["repository"].is_null(),
        "an unknown repository is null"
    );
    assert!(bevy["error"].is_null(), "no error is null");
    let tokio = rows.get(2).ok_or("no third row")?;
    assert_eq!(tokio["error"], "HTTP 503", "the failure");
    assert!(
        tokio["latest"].is_null(),
        "a failed lookup knows no version"
    );
    assert_eq!(
        tokio["via"].as_array().map(Vec::len),
        Some(0),
        "no edge is an empty array"
    );
    Ok(())
}

#[test]
fn awkward_strings_round_trip_through_the_json_over_seeds() -> TestResult {
    for seed in 0..120_u64 {
        let mut stream = Seeded::from_seed(seed);
        let mut rows = Vec::new();
        let mut expected = Vec::new();
        for ordinal in 0..stream.below(6)?.saturating_add(1) {
            let repository = *AWKWARD
                .get(stream.index(AWKWARD.len())?)
                .ok_or("repository index")?;
            let message = *AWKWARD
                .get(stream.index(AWKWARD.len())?)
                .ok_or("message index")?;
            let name = format!("pkg{ordinal}");
            if stream.below(3)? == 0 {
                rows.push(FreshnessResult::failed(
                    &name,
                    "1.0.0",
                    &[],
                    message.to_owned(),
                ));
                expected.push((name, None, Some(message)));
            } else {
                rows.push(FreshnessResult::found(
                    &name,
                    "1.0.0",
                    None,
                    Some(repository.to_owned()),
                    &[],
                ));
                expected.push((name, Some(repository), None));
            }
        }
        let mut out = Vec::new();
        print_json(&rows, &mut out)?;
        let parsed: Value = json::from_str(&String::from_utf8(out)?)?;
        let parsed_rows = parsed.as_array().ok_or("an array")?;
        assert_eq!(parsed_rows.len(), expected.len(), "seed {seed}: row count");
        for (row, entry) in parsed_rows.iter().zip(&expected) {
            assert_eq!(row["name"], entry.0.as_str(), "seed {seed}: name");
            assert_eq!(
                row["repository"].as_str(),
                entry.1,
                "seed {seed}: repository"
            );
            assert_eq!(row["error"].as_str(), entry.2, "seed {seed}: error");
        }
    }
    Ok(())
}

#[test]
fn a_table_for_awkward_rows_is_one_line_per_package_plus_the_frame() -> TestResult {
    let rows = vec![FreshnessResult::found(
        "plain",
        "1.0.0",
        None,
        Some("https://x.test".to_owned()),
        &[],
    )];
    let mut out = Vec::new();
    print_table(&rows, 0, &mut out)?;
    let text = String::from_utf8(out)?;
    assert_eq!(
        text.lines().count(),
        5,
        "header, rule, one row, a blank line, the totals: {text}"
    );
    Ok(())
}
