//! Runs the acceptance tests named in `docs/acceptance/t-rows.toml`, records them,
//! and renders the table.
//!
//! Every acceptance row in `docs/orchestration-acceptance.spec.md` names the tests
//! that address it. That claim is only worth something if someone can check it
//! against a real run, so this is the check: it reads the map, selects exactly the
//! named tests, runs them once (or reads what CI already ran, from JUnit), and
//! records what actually happened.
//!
//! **The receipt lives in a database, not in the repository.** By default it is
//! `$XDG_STATE_HOME/lgwks-acceptance/receipts.sqlite`, or
//! `~/.local/state/lgwks-acceptance/receipts.sqlite` when that is unset: state
//! belongs to the user's machine, and evidence is no more part of the source than a
//! build artifact is. `--db` points it anywhere else, and `--export` writes one
//! run as a single JSON artifact for a CI upload.
//!
//! Usage:
//!
//! ```text
//! acceptance-receipts [--root PATH] [--map docs/acceptance/t-rows.toml] [--db PATH]
//!     [--revision SHA] [--head SHA] [--spec docs/orchestration-acceptance.spec.md]
//!     [--package lgwks_bot] [--features full] [--export PATH]
//!     [--from-junit FILE...] [--platform NAME] [--write-table] [--check] [--dry-run]
//! ```
//!
//! Exit status is non-zero when a named test is missing from the run, when a named
//! test fails, or when a row's state claims more than the observed evidence, so the
//! same command works as a gate.

mod junit;
mod render;
mod store;
#[path = "../shared/toml_subset.rs"]
mod toml_subset;
#[path = "../shared/utc.rs"]
mod utc;

#[cfg(test)]
#[path = "../shared/fixture.rs"]
mod fixture;

#[cfg(test)]
mod journey_tests;

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use junit::{Outcomes, is_asked_for, junit_command, match_outcome, merge_junit, run_nextest};
use render::{
    Header, Row, committed_table, export_json, filter_expression, first_difference, load_rows,
    partial_rows, passed_count, py_float, render_table, replace_table,
};
use store::{NewRun, Run, Store, Tested, connect};

/// A condition that makes the receipt untrustworthy, named rather than guessed.
#[derive(Debug)]
pub(crate) struct Failure(pub(crate) String);

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for Failure {}

/// Logs `message` at the site that produced it, then hands it back as the `Err`.
pub(crate) fn fail<T>(message: String, site: &'static str) -> Result<T, Failure> {
    lgwks_std::trace::debug!(message = %message, site, "returning an error to the caller");
    Err(Failure(message))
}

/// The text of `text` from byte `from`, or empty when that is not a boundary.
pub(crate) fn tail_of(text: &str, from: usize) -> &str {
    match text.get(from..) {
        Some(rest) if !rest.is_empty() => rest,
        Some(_) | None => "",
    }
}

/// The text of `text` up to byte `to`, or empty when that is not a boundary.
pub(crate) fn head_of(text: &str, to: usize) -> &str {
    match text.get(..to) {
        Some(front) if !front.is_empty() => front,
        Some(_) | None => "",
    }
}

/// What the command line asked for.
struct Args {
    /// The repository root.
    root: PathBuf,
    /// The row map.
    map: String,
    /// The receipt database.
    db: Option<String>,
    /// The revision the receipt describes.
    revision: Option<String>,
    /// The revision being rendered as the head.
    head: Option<String>,
    /// The document holding the table.
    spec: String,
    /// The package under test.
    package: String,
    /// The features under test.
    features: String,
    /// Where to write this run's JSON.
    export: Option<String>,
    /// The JUnit reports to record instead of running.
    from_junit: Vec<PathBuf>,
    /// The platform a JUnit run reports for.
    platform: Option<String>,
    /// Rewrite the spec's table.
    write_table: bool,
    /// Compare the spec's table.
    check: bool,
    /// Print the filter and stop.
    dry_run: bool,
}

/// The value after a flag.
fn flag_value<'argument>(
    remaining: &mut impl Iterator<Item = &'argument String>,
    flag: &str,
) -> Result<String, Failure> {
    match remaining.next() {
        Some(found) => Ok(found.clone()),
        None => Err(Failure(format!("{flag} needs a value"))),
    }
}

/// Reads the command line.
fn parse_args(arguments: &[String]) -> Result<Args, Failure> {
    let mut args = Args {
        root: PathBuf::from("."),
        map: "docs/acceptance/t-rows.toml".to_owned(),
        db: None,
        revision: None,
        head: None,
        spec: "docs/orchestration-acceptance.spec.md".to_owned(),
        package: "lgwks_bot".to_owned(),
        features: "full".to_owned(),
        export: None,
        from_junit: Vec::new(),
        platform: None,
        write_table: false,
        check: false,
        dry_run: false,
    };
    let mut remaining = arguments.iter().peekable();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--write-table" => args.write_table = true,
            "--check" => args.check = true,
            "--dry-run" => args.dry_run = true,
            "--root" => args.root = PathBuf::from(flag_value(&mut remaining, "--root")?),
            "--map" => args.map = flag_value(&mut remaining, "--map")?,
            "--db" => args.db = Some(flag_value(&mut remaining, "--db")?),
            "--revision" => args.revision = Some(flag_value(&mut remaining, "--revision")?),
            "--head" => args.head = Some(flag_value(&mut remaining, "--head")?),
            "--spec" => args.spec = flag_value(&mut remaining, "--spec")?,
            "--package" => args.package = flag_value(&mut remaining, "--package")?,
            "--features" => args.features = flag_value(&mut remaining, "--features")?,
            "--export" => args.export = Some(flag_value(&mut remaining, "--export")?),
            "--platform" => args.platform = Some(flag_value(&mut remaining, "--platform")?),
            "--from-junit" => {
                args.from_junit
                    .push(PathBuf::from(flag_value(&mut remaining, "--from-junit")?));
                while let Some(next) = remaining.next_if(|candidate| !candidate.starts_with("--")) {
                    args.from_junit.push(PathBuf::from(next));
                }
            }
            other => return Err(Failure(format!("unknown argument {other:?}"))),
        }
    }
    Ok(args)
}

/// What `program` prints for `arguments`, trimmed, if it succeeds.
fn printed(program: &str, arguments: &[&str], root: &Path) -> Option<String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        None
    }
}

/// What a receipt is keyed on beside the revision and the feature set.
fn current_platform(root: &Path) -> String {
    let system = printed("uname", &["-s"], root);
    let machine = printed("uname", &["-m"], root);
    match (system, machine) {
        (Some(system), Some(machine)) => format!("{system}-{machine}"),
        _ => std::env::consts::OS.to_owned(),
    }
}

/// The revision this working tree builds, from git, named not guessed.
fn head_revision(root: &Path) -> Result<String, Failure> {
    match printed("git", &["rev-parse", "HEAD"], root) {
        Some(found) if !found.is_empty() => Ok(found),
        Some(_) | None => fail("git rev-parse HEAD failed".to_owned(), "head_revision"),
    }
}

/// Where a receipt lives when nobody says otherwise.
fn default_db_path() -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(found) if !found.is_empty() => PathBuf::from(found),
        Some(_) | None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local").join("state"),
            None => PathBuf::from(".local").join("state"),
        },
    };
    base.join("lgwks-acceptance").join("receipts.sqlite")
}

/// Writes a line, logging a failure to write.
fn say(stream: &mut dyn Write, text: &str) {
    if let Err(error) = writeln!(stream, "{text}") {
        lgwks_std::trace::debug!(%error, "a line could not be written");
    }
}

/// The two output streams of a run.
struct Streams<'sink> {
    /// Standard output.
    out: &'sink mut dyn Write,
    /// Standard error.
    err: &'sink mut dyn Write,
}

impl Streams<'_> {
    /// A line on standard output.
    fn tell(&mut self, text: &str) {
        say(self.out, text);
    }

    /// A line on standard error.
    fn warn(&mut self, text: &str) {
        say(self.err, text);
    }
}

/// The name after the last `$` of a nextest name.
fn after_dollar(name: &str) -> &str {
    match name.rsplit_once('$') {
        Some((_, tail)) => tail,
        None => name,
    }
}

/// Everything a run needs once the map and the command line are read.
struct Plan<'run> {
    /// The command line.
    args: &'run Args,
    /// The map's header.
    header: &'run Header,
    /// The map's rows.
    rows: &'run [Row],
    /// The revision the receipt describes.
    revision: &'run str,
    /// The revision being rendered as the head.
    head: &'run str,
    /// The receipt database's path.
    db_path: &'run Path,
    /// The nextest filter naming every test.
    expression: &'run str,
}

/// The platform of a check or a JUnit run: the one named, else this host's.
fn platform_of(args: &Args) -> String {
    match args.platform.clone() {
        Some(found) => found,
        None => current_platform(&args.root),
    }
}

/// `--check`: the committed table must be what the map and the receipt render.
fn check_table(plan: &Plan<'_>, store: &Store, streams: &mut Streams<'_>) -> Result<u8, Failure> {
    let args = plan.args;
    let platform = platform_of(args);
    let spec_path = args.root.join(&args.spec);
    let spec_text = match std::fs::read_to_string(&spec_path) {
        Ok(text) => text,
        Err(error) => return fail(format!("{}: {error}", spec_path.display()), "check_table"),
    };
    let run = store.read_run(plan.revision, &args.features, &platform)?;
    let results = store.read_results(plan.revision, &args.features, &platform)?;
    let wanted = render_table(plan.rows, &run, &results, plan.head);
    let found = committed_table(&spec_text)?;
    if found != wanted {
        streams.warn(&format!(
            "acceptance-receipts: the committed per-row table in {} is not what the map and the receipt render. Re-record with --write-table and commit the result.",
            args.spec
        ));
        for line in first_difference(&found, &wanted) {
            streams.warn(&format!("  {line}"));
        }
        return Ok(1);
    }
    streams.tell(&format!(
        "acceptance-receipts: the committed table in {} matches the map and the receipt for {} in {}",
        args.spec,
        head_of(plan.revision, 8),
        plan.db_path.display()
    ));
    Ok(0)
}

/// What a run observed, and how it was observed.
struct Observation {
    /// Each test's outcome.
    outcomes: Outcomes,
    /// Seconds taken.
    elapsed: f64,
    /// The platform it ran on.
    platform: String,
    /// The command that produced it.
    command: String,
}

/// Reads or runs the tests the map names.
fn observe(plan: &Plan<'_>) -> Result<Observation, Failure> {
    let args = plan.args;
    if args.from_junit.is_empty() {
        let (outcomes, elapsed) =
            run_nextest(&args.root, plan.expression, &args.package, &args.features)?;
        return Ok(Observation {
            outcomes,
            elapsed,
            platform: current_platform(&args.root),
            command: format!(
                "cargo nextest run --locked -p {} --features {} --message-format libtest-json-plus --no-fail-fast -E '{}'",
                args.package, args.features, plan.expression
            ),
        });
    }
    // The CI path: the tests already ran, once, across the shards. This records
    // what they reported rather than running them again, so the receipt costs
    // seconds and the suite is not executed twice.
    let outcomes = merge_junit(&args.from_junit)?;
    let millis: f64 = outcomes.iter().map(|entry| entry.1.millis).sum();
    Ok(Observation {
        outcomes,
        elapsed: millis / 1000.0,
        platform: platform_of(args),
        command: junit_command(&args.from_junit, &args.package, &args.features),
    })
}

/// Every named test with its outcome, and the failures among them.
fn judge(rows: &[Row], outcomes: &Outcomes) -> (Vec<Tested>, Vec<String>) {
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for row in rows {
        for name in &row.tests {
            let (status, duration_ms) = match match_outcome(outcomes, name) {
                Some(observed) => (observed.status, observed.millis),
                None => ("missing".to_owned(), 0.0),
            };
            if status == "missing" {
                failures.push(format!("{}::{name} was not run", row.id));
            } else if status != "ok" {
                failures.push(format!("{}::{name}={status}", row.id));
            }
            results.push(Tested {
                row_id: row.id.clone(),
                test: name.clone(),
                status,
                duration_ms,
            });
        }
    }
    (results, failures)
}

/// Seconds rounded to the millisecond.
fn rounded(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

/// Records a run, then writes and prints what was asked for.
fn record(plan: &Plan<'_>, store: &Store, streams: &mut Streams<'_>) -> Result<u8, Failure> {
    let args = plan.args;
    let observation = observe(plan)?;
    let (results, failures) = judge(plan.rows, &observation.outcomes);
    let run_at = utc::stamp();
    let artifact = match args.export.as_deref() {
        Some(path) => path.to_owned(),
        None => String::new(),
    };
    store.record_run(
        &NewRun {
            revision: plan.revision,
            platform: &observation.platform,
            feature_set: &args.features,
            package: &args.package,
            command: &observation.command,
            wall_seconds: rounded(observation.elapsed),
            run_at: &run_at,
            artifact_path: &artifact,
        },
        &results,
    )?;
    let run = store.read_run(plan.revision, &args.features, &observation.platform)?;
    let stored = store.read_results(plan.revision, &args.features, &observation.platform)?;
    report(plan, &observation, &run, &stored, streams)?;
    if failures.is_empty() {
        return Ok(0);
    }
    streams.warn(&format!("named tests not passing: {}", failures.len()));
    for failure in failures.iter().take(20) {
        streams.warn(&format!("  {failure}"));
    }
    Ok(1)
}

/// Rewrites the spec's generated table from the recorded receipt.
fn write_table(plan: &Plan<'_>, run: &Run, stored: &[Tested]) -> Result<(), Failure> {
    let spec_path = plan.args.root.join(&plan.args.spec);
    let spec_text = match std::fs::read_to_string(&spec_path) {
        Ok(text) => text,
        Err(error) => return fail(format!("{}: {error}", spec_path.display()), "write_table"),
    };
    let replaced = replace_table(&spec_text, &render_table(plan.rows, run, stored, plan.head))?;
    match std::fs::write(&spec_path, replaced) {
        Ok(()) => Ok(()),
        Err(error) => fail(format!("{}: {error}", spec_path.display()), "write_table"),
    }
}

/// Writes one run as a portable JSON artifact.
fn write_export(plan: &Plan<'_>, path: &str, run: &Run, stored: &[Tested]) -> Result<(), Failure> {
    let exported = export_json(plan.rows, run, stored, plan.head)?;
    let target = Path::new(path);
    if let Some(parent) = target.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return fail(format!("{}: {error}", parent.display()), "write_export");
    }
    match std::fs::write(target, exported) {
        Ok(()) => Ok(()),
        Err(error) => fail(format!("{path}: {error}"), "write_export"),
    }
}

/// Writes the table and the export, and prints the one-line summary.
fn report(
    plan: &Plan<'_>,
    observation: &Observation,
    run: &Run,
    stored: &[Tested],
    streams: &mut Streams<'_>,
) -> Result<(), Failure> {
    let args = plan.args;
    let requested: usize = plan.rows.iter().map(|row| row.tests.len()).sum();
    let passed: usize = plan.rows.iter().map(|row| passed_count(row, stored)).sum();
    let asked: Vec<String> = plan
        .rows
        .iter()
        .flat_map(|row| row.tests.iter().cloned())
        .collect();
    let mut unrequested: Vec<&str> = observation
        .outcomes
        .iter()
        .filter(|entry| !is_asked_for(&entry.0, &asked))
        .map(|entry| after_dollar(&entry.0))
        .collect();
    unrequested.sort_unstable();
    unrequested.dedup();
    if !unrequested.is_empty() && args.from_junit.is_empty() {
        streams.warn(&format!(
            "acceptance-receipts: {} test(s) the filter ran that no row names: {}",
            unrequested.len(),
            unrequested.join(", ")
        ));
    }
    if args.write_table {
        write_table(plan, run, stored)?;
        streams.tell(&format!("rewrote the generated table in {}", args.spec));
    }
    if let Some(path) = args.export.as_deref() {
        write_export(plan, path, run, stored)?;
        streams.tell(&format!(
            "exported the receipt for {} to {path}",
            head_of(plan.revision, 8)
        ));
    }
    let partial = partial_rows(plan.rows, stored, plan.revision, plan.head);
    let partial_names = if partial.is_empty() {
        "-".to_owned()
    } else {
        partial.join(",")
    };
    streams.tell(&format!(
        "receipt {} revision={} rows={} tests_passed={passed}/{requested} rows_still_partial={} partial={partial_names} wall={}s spec={}",
        plan.db_path.display(),
        plan.revision,
        plan.rows.len(),
        partial.len(),
        py_float(rounded(observation.elapsed)),
        plan.header.spec
    ));
    Ok(())
}

/// Runs with `arguments` and returns the exit status.
fn execute(arguments: &[String], streams: &mut Streams<'_>) -> Result<u8, Failure> {
    let args = parse_args(arguments)?;
    let (header, rows) = load_rows(&args.root.join(&args.map))?;
    let expression = filter_expression(&rows)?;
    let revision = match args.revision.clone() {
        Some(found) => found,
        None => head_revision(&args.root)?,
    };
    let head = match args.head.clone() {
        Some(found) => found,
        None => head_revision(&args.root)?,
    };
    let db_path = match args.db.as_deref() {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(path) => args.root.join(path),
        None => default_db_path(),
    };
    if args.dry_run {
        streams.tell(&format!(
            "cargo nextest run --locked -p {} --features {} --message-format libtest-json-plus --no-fail-fast -E '{expression}'",
            args.package, args.features
        ));
        return Ok(0);
    }
    let store = connect(&db_path)?;
    let plan = Plan {
        args: &args,
        header: &header,
        rows: &rows,
        revision: &revision,
        head: &head,
        db_path: &db_path,
        expression: &expression,
    };
    if args.check {
        return check_table(&plan, &store, streams);
    }
    record(&plan, &store, streams)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
    };
    match execute(&arguments, &mut streams) {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            streams.warn(&format!("acceptance-receipts: {failure}"));
            ExitCode::from(2)
        }
    }
}
