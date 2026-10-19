//! The builtin lanes that run other programs and judge what they print: the
//! docs.rs feature sets, the debugger-doctor journey and the simulation share.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use lgwks_std::json::{self, Value};

use super::builtins::{crate_directories, head, pass, refuse, relative, rust_files_under, tail};
use super::children::Launch;
use super::fixture::Fixture;
use super::lanes::{RUN_BOUND, flag};
use super::toml_subset::{list, read_tables, text};
use super::{Context, GateError, Verdict, read};

/// A command line from string slices.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// `command` run in `root` with `env`, under the gate's bound.
fn launch<'run>(
    command: &'run [String],
    root: &'run Path,
    env: &'run BTreeMap<String, String>,
) -> Launch<'run> {
    Launch {
        argv: command,
        root,
        env,
        stdin: None,
        bound: RUN_BOUND,
    }
}

/// Every `[package.metadata.docs.rs]` names a feature set that builds, on the
/// nightly toolchain docs.rs uses. A runner's first nightly run compiles the
/// whole dependency graph once (the step is cancelled at the job cap when the
/// host is loaded); its target directory keeps the result, so later runs only
/// document the workspace crates.
pub(super) fn docsrs_metadata(context: &Context<'_>) -> Result<Verdict, GateError> {
    let mut commands: Vec<(String, Vec<String>)> = Vec::new();
    for directory in crate_directories(context.root) {
        let manifest = directory.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let tables = read_tables(&read(&manifest)?);
        let Some(docs) = tables
            .iter()
            .find(|table| table.path == "package.metadata.docs.rs")
        else {
            continue;
        };
        let Some(name) = tables
            .iter()
            .find(|table| table.path == "package")
            .and_then(|table| text(table, "name"))
        else {
            continue;
        };
        // docs.rs builds on nightly with the `--cfg docsrs` this lane sets, and the
        // crate roots turn on `doc_cfg` under that cfg, which stable refuses.
        let mut command = argv(&[
            "cargo",
            "+nightly",
            "doc",
            "--no-deps",
            "--locked",
            "-p",
            name,
        ]);
        if flag(docs, "all-features") == Some(true) {
            command.push("--all-features".to_owned());
            commands.push((format!("{name}: all-features"), command));
            continue;
        }
        let features = match list(docs, "features") {
            Some(found) => found.join(","),
            None => String::new(),
        };
        let label = if features.is_empty() {
            "(defaults only)".to_owned()
        } else {
            features.clone()
        };
        if !features.is_empty() {
            command.push("--features".to_owned());
            command.push(features);
        }
        commands.push((format!("{name}: {label}"), command));
    }
    if commands.is_empty() {
        return Ok(pass("no crate declares [package.metadata.docs.rs]"));
    }
    let mut env = context.env.clone();
    env.insert(
        "RUSTDOCFLAGS".to_owned(),
        "-D warnings --cfg docsrs".to_owned(),
    );
    let mut log = vec![format!("cargo doc plans: {}", commands.len())];
    for planned in &commands {
        let shown = planned.1.join(" ");
        log.push(format!("  {}: {shown}", planned.0));
        let code = context
            .children
            .run(&launch(&planned.1, context.root, &env), context.out);
        if code != 0 {
            log.push(format!("failed (exit {code}): {shown}"));
            return Ok(refuse(log.join("\n")));
        }
    }
    Ok(pass(&log.join("\n")))
}

/// A JSON object's boolean under `section` and `key`, if it is one.
fn flag_at(report: &Value, section: &str, key: &str) -> Option<bool> {
    report
        .get(section)
        .and_then(|inner| inner.get(key))
        .and_then(Value::as_bool)
}

/// The first `limit` characters of `text`, for a failure message.
fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// The checks the success journey must report as passed.
const REQUIRED_CHECKS: [&str; 5] = [
    "default_includes_trace",
    "trace_includes_tracing",
    "trace_includes_tracing_subscriber",
    "tracing_declared",
    "tracing_subscriber_declared",
];

/// Writes `contents` to `path`, naming the file when it cannot be written.
fn write_file(path: &Path, contents: &str) -> Result<(), GateError> {
    fs::write(path, contents).map_err(|cause| GateError::Io {
        path: path.to_path_buf(),
        cause,
    })
}

/// Drive the public debugger doctor through success and fail-closed paths.
pub(super) fn debug_e2e(context: &Context<'_>) -> Result<Verdict, GateError> {
    let mut env: BTreeMap<String, String> = context.env.clone();
    env.insert("LGWKS_LOG".to_owned(), "info".to_owned());
    env.insert("LGWKS_LOG_FORMAT".to_owned(), "json".to_owned());
    let command = argv(&[
        "cargo",
        "run",
        "--quiet",
        "--locked",
        "-p",
        "lgwks_deps",
        "--bin",
        "lgwks-deps",
        "--",
        "debug",
        ".",
        "--json",
    ]);
    let success = context
        .children
        .capture(&launch(&command, context.root, &env));
    if success.code != 0 {
        return Ok(refuse(format!(
            "debug success journey failed: exit={} stdout={:?} stderr={:?}",
            success.code,
            clip(&success.stdout, 400),
            clip(&success.stderr, 400)
        )));
    }
    let report: Value = match json::from_str(&success.stdout) {
        Ok(report) => report,
        Err(error) => {
            return Ok(refuse(format!(
                "debug success journey did not return JSON: {error}"
            )));
        }
    };
    let missing: Vec<&str> = REQUIRED_CHECKS
        .iter()
        .copied()
        .filter(|name| flag_at(&report, "checks", name) != Some(true))
        .collect();
    let admitted = report.get("admitted").and_then(Value::as_bool);
    if admitted != Some(true) || !missing.is_empty() {
        return Ok(refuse(format!(
            "debug success journey missing admitted checks: admitted={admitted:?} missing={missing:?}"
        )));
    }
    if !success.stderr.contains("debugger installed")
        || !success.stderr.contains("debug doctor completed")
    {
        return Ok(refuse(
            "debug success journey did not emit install and completion lifecycle events".to_owned(),
        ));
    }
    let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-debug-e2e")?;
    let manifest_dir = fixture.path().join("crates").join("lgwks-std");
    fs::create_dir_all(&manifest_dir).map_err(|cause| GateError::Io {
        path: manifest_dir.clone(),
        cause,
    })?;
    write_file(
        &fixture.path().join("Cargo.lock"),
        "# debugger e2e fixture\n",
    )?;
    let manifest = read(&context.root.join("crates/lgwks-std/Cargo.toml"))?;
    let refused_manifest = manifest
        .replace(", \"dep:tracing-subscriber\"", "")
        .replace("    \"dep:tracing-subscriber\",\n", "");
    if refused_manifest == manifest {
        return Ok(refuse(
            "debug fail-closed fixture could not remove dep:tracing-subscriber from trace"
                .to_owned(),
        ));
    }
    write_file(&manifest_dir.join("Cargo.toml"), &refused_manifest)?;
    let mut fail_command = command;
    let place = fail_command.len().saturating_sub(2);
    if let Some(slot) = fail_command.get_mut(place) {
        *slot = fixture.path().display().to_string();
    }
    let refused = context
        .children
        .capture(&launch(&fail_command, context.root, &env));
    if refused.code != 2 {
        return Ok(refuse(format!(
            "debug fail-closed journey did not refuse: exit={} stdout={:?} stderr={:?}",
            refused.code,
            clip(&refused.stdout, 400),
            clip(&refused.stderr, 400)
        )));
    }
    let refused_report: Value = match json::from_str(&refused.stdout) {
        Ok(report) => report,
        Err(error) => {
            return Ok(refuse(format!(
                "debug fail-closed journey did not return JSON: {error}"
            )));
        }
    };
    if refused_report.get("admitted").and_then(Value::as_bool) != Some(false) {
        return Ok(refuse(format!(
            "debug fail-closed journey admitted a broken trace surface: {refused_report:?}"
        )));
    }
    if flag_at(
        &refused_report,
        "checks",
        "trace_includes_tracing_subscriber",
    ) != Some(false)
    {
        return Ok(refuse(
            "debug fail-closed journey did not name the missing tracing-subscriber feature edge"
                .to_owned(),
        ));
    }
    Ok(pass(
        "successful end-to-end journey result: debug_json_exit=0 admitted=true lifecycle=installed+completed; \
         fail_closed_exit=2 missing=trace_includes_tracing_subscriber",
    ))
}

/// A stable percentage with four fractional digits, from integers.
pub(super) fn percent(numerator: usize, denominator: usize) -> String {
    let scaled = match numerator
        .saturating_mul(2_000_000)
        .saturating_add(denominator)
        .checked_div(denominator.saturating_mul(2))
    {
        Some(value) if value > 0 => value,
        Some(_) | None => 0,
    };
    format!(
        "{}.{:04}%",
        scaled.div_euclid(10_000),
        scaled.rem_euclid(10_000)
    )
}

/// Whether the text does not continue a word.
fn ends_word(text: &str) -> bool {
    !text
        .chars()
        .next()
        .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

/// Whether `rest` (after `#[`) opens a `test` attribute, bare or path-qualified.
fn is_test_attribute(rest: &str) -> bool {
    if let Some(after) = rest.strip_prefix("test")
        && ends_word(after)
    {
        return true;
    }
    for (index, _) in rest.match_indices("::") {
        let prefix_end = index.saturating_add(2);
        let in_path = head(rest, prefix_end)
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':');
        if in_path
            && let Some(after) = tail(rest, prefix_end).strip_prefix("test")
            && ends_word(after)
        {
            return true;
        }
    }
    false
}

/// Counts source-level test attributes: the total, and those in simulation source.
fn source_visible_test_counts(root: &Path) -> Result<(usize, usize), GateError> {
    let mut total = 0_usize;
    let mut simulated = 0_usize;
    for path in rust_files_under(&root.join("crates"), &["target"])? {
        let shown = format!("/{}", relative(root, &path));
        let in_sim_source = shown.contains("/tests/sim/")
            || path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("sim_"));
        for line in read(&path)?.lines() {
            if let Some(rest) = line.trim().strip_prefix("#[")
                && is_test_attribute(rest)
            {
                total = total.saturating_add(1);
                if in_sim_source {
                    simulated = simulated.saturating_add(1);
                }
            }
        }
    }
    Ok((total, simulated))
}

/// The binary name of a nextest suite, or empty.
fn binary_name_of(suite: &Value) -> &str {
    match suite.get("binary-name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name,
        Some(_) | None => "",
    }
}

/// How many of a nextest listing's cases there are, and how many are simulations.
fn listed_counts(payload: &Value) -> (usize, usize) {
    let total = match payload
        .get("test-count")
        .and_then(Value::as_u64)
        .map(usize::try_from)
    {
        Some(Ok(count)) if count > 0 => count,
        Some(Ok(_) | Err(_)) | None => 0,
    };
    let mut simulated = 0_usize;
    let Some(suites) = payload.get("rust-suites").and_then(Value::as_object) else {
        return (total, simulated);
    };
    for (suite_id, suite) in suites {
        let whole_binary = binary_name_of(suite).starts_with("sim_") || suite_id.contains("::sim_");
        let Some(cases) = suite.get("testcases").and_then(Value::as_object) else {
            continue;
        };
        for case in cases.keys() {
            // A consolidated binary (`tests/it/`, #272) keeps each former `sim_*`
            // file as a module, so a case counts when any module on its path is a
            // simulation; the test function's own name does not.
            let mut parts: Vec<&str> = case.split("::").collect();
            parts.pop();
            if whole_binary || parts.iter().any(|module| module.starts_with("sim_")) {
                simulated = simulated.saturating_add(1);
            }
        }
    }
    (total, simulated)
}

/// Prove deterministic simulation coverage by executable and source views.
pub(super) fn simulation_evidence(context: &Context<'_>) -> Result<Verdict, GateError> {
    let command = argv(&[
        "cargo",
        "nextest",
        "list",
        "--workspace",
        "--locked",
        "--message-format",
        "json",
        "--cargo-quiet",
    ]);
    let listed = context
        .children
        .capture(&launch(&command, context.root, context.env));
    if listed.code != 0 {
        return Ok(refuse(format!(
            "nextest listing failed: exit={} stdout={:?} stderr={:?}",
            listed.code,
            clip(&listed.stdout, 400),
            clip(&listed.stderr, 400)
        )));
    }
    let payload: Value = match json::from_str(&listed.stdout) {
        Ok(payload) => payload,
        Err(error) => return Ok(refuse(format!("nextest listing was not JSON: {error}"))),
    };
    let (total, simulated) = listed_counts(&payload);
    if total == 0 {
        return Ok(refuse("nextest listing reported no tests".to_owned()));
    }
    if simulated.saturating_mul(2) < total {
        return Ok(refuse(format!(
            "simulation tests below half in nextest listing: sim={simulated} total={total}"
        )));
    }
    let (source_total, source_sim) = source_visible_test_counts(context.root)?;
    if source_total == 0 {
        return Ok(refuse(
            "source-visible test counter found no tests".to_owned(),
        ));
    }
    if source_sim.saturating_mul(2) < source_total {
        return Ok(refuse(format!(
            "source-visible simulation tests below half: sim={source_sim} total={source_total}"
        )));
    }
    Ok(pass(&format!(
        "successful simulation result: nextest_sim={simulated} nextest_total={total} nextest_percent={}; \
         source_visible_sim={source_sim} source_visible_total={source_total} source_visible_percent={}",
        percent(simulated, total),
        percent(source_sim, source_total)
    )))
}

#[cfg(test)]
mod tests {
    use super::{is_test_attribute, listed_counts, percent};
    use lgwks_std::json::{self, Value};

    #[test]
    fn a_percentage_has_four_digits_and_rounds_half_up() {
        assert_eq!(percent(2044, 4087), "50.0122%", "the measured share");
        assert_eq!(percent(1, 3), "33.3333%", "a repeating fraction");
        assert_eq!(percent(1, 1), "100.0000%", "everything");
        assert_eq!(percent(0, 7), "0.0000%", "nothing");
    }

    #[test]
    fn a_test_attribute_is_bare_or_path_qualified_and_whole_word() {
        assert!(is_test_attribute("test]"), "bare");
        assert!(is_test_attribute("tokio::test]"), "qualified");
        assert!(
            is_test_attribute("tokio::test(flavor = \"x\")]"),
            "with arguments"
        );
        assert!(!is_test_attribute("test_case(1)]"), "a longer word");
        assert!(!is_test_attribute("cfg(test)]"), "a cfg");
    }

    #[test]
    fn a_nextest_listing_counts_simulations_by_binary_or_module() -> Result<(), json::Error> {
        let payload: Value = json::from_str(
            "{\"test-count\": 4, \"rust-suites\": {\
              \"p::sim_a\": {\"binary-name\": \"sim_a\", \"testcases\": {\"one\": {}, \"two\": {}}},\
              \"p::it\": {\"binary-name\": \"it\", \"testcases\": {\"sim_x::case\": {}, \"plain::case\": {}}}}}",
        )?;
        assert_eq!(
            listed_counts(&payload),
            (4, 3),
            "a whole sim binary and a sim module count"
        );
        Ok(())
    }
}
