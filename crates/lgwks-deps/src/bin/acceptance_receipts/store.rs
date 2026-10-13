//! The receipt database: SQLite, written and read through the `sqlite3`
//! command-line program every supported host ships.
//!
//! The receipt lives in a database, not in the repository (item 4 of #271). The
//! reason is durability rather than tidiness: a receipt is the record the spec's
//! table is rendered *from*, so it has to be queryable by revision, by row and by
//! platform, and it has to survive the run that wrote it. One JSON file per
//! revision in the tree gives none of those, and grows the repository by a file
//! per run. No SQLite library is linked: the program is driven over a pipe, so the
//! workspace gains no dependency.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lgwks_std::json::{self, Value};

use super::{Failure, fail};

/// The schema, created if absent. `INSERT OR REPLACE` under these keys is what makes
/// a re-run of one revision idempotent.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS run (
    seq           INTEGER NOT NULL,
    revision      TEXT NOT NULL,
    platform      TEXT NOT NULL,
    feature_set   TEXT NOT NULL,
    package       TEXT NOT NULL,
    command       TEXT NOT NULL,
    wall_seconds  REAL NOT NULL,
    run_at        TEXT NOT NULL,
    artifact_path TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (revision, platform, feature_set)
);

CREATE TABLE IF NOT EXISTS test_result (
    revision    TEXT NOT NULL,
    row_id      TEXT NOT NULL,
    test        TEXT NOT NULL,
    platform    TEXT NOT NULL,
    feature_set TEXT NOT NULL,
    command     TEXT NOT NULL,
    status      TEXT NOT NULL,
    duration_ms REAL NOT NULL,
    run_at      TEXT NOT NULL,
    PRIMARY KEY (revision, row_id, test, platform, feature_set),
    FOREIGN KEY (revision, platform, feature_set)
        REFERENCES run (revision, platform, feature_set)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS test_result_by_revision
    ON test_result (revision, row_id, platform, feature_set);
";

/// The first sixteen bytes of every SQLite database file.
const MAGIC: &[u8] = b"SQLite format 3\0";

/// An open receipt database.
pub(super) struct Store {
    /// The database file.
    path: PathBuf,
}

/// One run, as it is recorded.
pub(super) struct NewRun<'run> {
    /// The revision the receipt describes.
    pub(super) revision: &'run str,
    /// The platform the tests ran on.
    pub(super) platform: &'run str,
    /// The feature set under test.
    pub(super) feature_set: &'run str,
    /// The package under test.
    pub(super) package: &'run str,
    /// What ran.
    pub(super) command: &'run str,
    /// How long it took.
    pub(super) wall_seconds: f64,
    /// When it ran.
    pub(super) run_at: &'run str,
    /// Where the exported artifact went.
    pub(super) artifact_path: &'run str,
}

/// One run, as it is read back.
pub(super) struct Run {
    /// The revision the receipt describes.
    pub(super) revision: String,
    /// The platform the tests ran on.
    pub(super) platform: String,
    /// The feature set under test.
    pub(super) feature_set: String,
    /// The package under test.
    pub(super) package: String,
    /// What ran.
    pub(super) command: String,
    /// How long it took.
    pub(super) wall_seconds: f64,
    /// When it ran.
    pub(super) run_at: String,
}

/// One named test's recorded outcome.
#[derive(Clone)]
pub(super) struct Tested {
    /// The row that names the test.
    pub(super) row_id: String,
    /// The test as the map names it.
    pub(super) test: String,
    /// What happened.
    pub(super) status: String,
    /// How long it took.
    pub(super) duration_ms: f64,
}

/// `text` as an SQL string literal.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A real number as an SQL literal.
fn real(value: f64) -> String {
    if value.is_finite() { format!("{value:?}") } else { "0.0".to_owned() }
}

/// Runs `sqlite3` with `arguments`, `script` on its input, and returns what it printed.
fn sqlite(arguments: &[&str], script: &str) -> Result<String, Failure> {
    let mut child = match Command::new("sqlite3")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return fail(
                format!("the sqlite3 command-line program is required to hold receipts: {error}"),
                "sqlite",
            );
        }
    };
    if let Some(mut input) = child.stdin.take()
        && let Err(error) = input.write_all(script.as_bytes())
    {
        lgwks_std::trace::debug!(%error, "sqlite3 closed its input early");
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => return fail(format!("sqlite3 could not be waited on: {error}"), "sqlite"),
    };
    if !output.status.success() {
        return fail(
            format!("sqlite3 refused: {}", String::from_utf8_lossy(&output.stderr).trim()),
            "sqlite",
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Opens the receipt database, creating its schema, refusing a foreign file.
///
/// WAL is on before anything is written: a reader rendering the spec's table while
/// a run is still appending to the same file is the normal case here, and the
/// default rollback journal would serialise them.
pub(super) fn connect(db_path: &Path) -> Result<Store, Failure> {
    if let Ok(mut file) = fs::File::open(db_path) {
        let mut header = [0_u8; 16];
        let read = match file.read(&mut header) {
            Ok(count) => count,
            Err(error) => {
                lgwks_std::trace::debug!(%error, "a database header could not be read");
                0
            }
        };
        if read > 0 && !header.starts_with(MAGIC) {
            return fail(
                format!(
                    "{} is not a SQLite database (its first 16 bytes are {:?}); refusing to write a receipt into it",
                    db_path.display(),
                    header.get(..read)
                ),
                "connect",
            );
        }
    }
    if let Some(parent) = db_path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return fail(format!("{}: {error}", parent.display()), "connect");
    }
    let store = Store {
        path: db_path.to_path_buf(),
    };
    store.run_script(&format!("PRAGMA journal_mode=WAL;\n{SCHEMA}"))?;
    Ok(store)
}

impl Store {
    /// Runs a script, stopping at the first error.
    fn run_script(&self, script: &str) -> Result<(), Failure> {
        let path = self.path.display().to_string();
        sqlite(&["-batch", "-bail", &path], script).map(|_| ())
    }

    /// The rows a query returns, as JSON objects.
    fn query(&self, sql: &str) -> Result<Vec<Value>, Failure> {
        let path = self.path.display().to_string();
        let printed = sqlite(&["-batch", "-json", &path], sql)?;
        if printed.trim().is_empty() {
            return Ok(Vec::new());
        }
        match json::from_str::<Value>(&printed) {
            Ok(Value::Array(rows)) => Ok(rows),
            Ok(_) | Err(_) => fail("sqlite3 printed rows that are not JSON".to_owned(), "query"),
        }
    }

    /// Writes one run and every one of its test results, in one transaction.
    ///
    /// `INSERT OR REPLACE` under a primary key of `(revision, row_id, test,
    /// platform, feature_set)` is what makes a re-run of the same revision
    /// idempotent: the second run overwrites the first rather than doubling every
    /// count, and a half-finished run leaves nothing behind because the whole write
    /// is one transaction. `seq` is assigned in the same statement rather than read
    /// from the clock, because "newest run first" has to be true for two runs
    /// recorded inside one second.
    pub(super) fn record_run(&self, run: &NewRun<'_>, results: &[Tested]) -> Result<(), Failure> {
        let mut statements = vec![
            "BEGIN IMMEDIATE;".to_owned(),
            format!(
                "INSERT OR REPLACE INTO run (seq, revision, platform, feature_set, package, command, wall_seconds, run_at, artifact_path) \
                 VALUES ((SELECT COALESCE(MAX(seq), 0) + 1 FROM run), {}, {}, {}, {}, {}, {}, {}, {});",
                quote(run.revision),
                quote(run.platform),
                quote(run.feature_set),
                quote(run.package),
                quote(run.command),
                real(run.wall_seconds),
                quote(run.run_at),
                quote(run.artifact_path)
            ),
        ];
        statements.extend(results.iter().map(|result| {
            format!(
                "INSERT OR REPLACE INTO test_result (revision, row_id, test, platform, feature_set, command, status, duration_ms, run_at) \
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {});",
                quote(run.revision),
                quote(&result.row_id),
                quote(&result.test),
                quote(run.platform),
                quote(run.feature_set),
                quote(run.command),
                quote(&result.status),
                real(result.duration_ms),
                quote(run.run_at)
            )
        }));
        statements.push("COMMIT;".to_owned());
        self.run_script(&statements.join("\n"))
    }

    /// The run recorded for `revision` on `platform` with `feature_set`, or a refusal.
    ///
    /// The feature set and the platform are parameters rather than a lookup because
    /// the database may legitimately hold several for one revision, and resolving
    /// that by picking one would let the wrong run satisfy the claim. A CI job reads
    /// back a Linux receipt from a machine that is not Linux, and a lookup keyed on
    /// the reader would find nothing.
    pub(super) fn read_run(&self, revision: &str, feature_set: &str, platform: &str) -> Result<Run, Failure> {
        let rows = self.query(&format!(
            "SELECT revision, platform, feature_set, package, command, wall_seconds, run_at FROM run \
             WHERE revision = {} AND platform = {} AND feature_set = {};",
            quote(revision),
            quote(platform),
            quote(feature_set)
        ))?;
        let Some(row) = rows.first() else {
            return fail(
                format!(
                    "no receipt for revision {revision} on {platform} with features {feature_set}; run without --check to record one"
                ),
                "read_run",
            );
        };
        let text = |key: &str| row.get(key).and_then(Value::as_str).map(str::to_owned);
        match (
            text("revision"),
            text("platform"),
            text("feature_set"),
            text("package"),
            text("command"),
            row.get("wall_seconds").and_then(Value::as_f64),
            text("run_at"),
        ) {
            (Some(revision), Some(platform), Some(feature_set), Some(package), Some(command), Some(wall_seconds), Some(run_at)) => Ok(Run {
                revision,
                platform,
                feature_set,
                package,
                command,
                wall_seconds,
                run_at,
            }),
            _ => fail("a recorded run is missing a column".to_owned(), "read_run"),
        }
    }

    /// Every `(row_id, test)` outcome the revision recorded.
    pub(super) fn read_results(&self, revision: &str, feature_set: &str, platform: &str) -> Result<Vec<Tested>, Failure> {
        let rows = self.query(&format!(
            "SELECT row_id, test, status, duration_ms FROM test_result \
             WHERE revision = {} AND platform = {} AND feature_set = {};",
            quote(revision),
            quote(platform),
            quote(feature_set)
        ))?;
        if rows.is_empty() {
            return fail(
                format!("the receipt for revision {revision} records no test results; a partial receipt cannot render a table"),
                "read_results",
            );
        }
        let mut found = Vec::new();
        for row in &rows {
            let text = |key: &str| row.get(key).and_then(Value::as_str).map(str::to_owned);
            let (Some(row_id), Some(test), Some(status), Some(duration_ms)) =
                (text("row_id"), text("test"), text("status"), row.get("duration_ms").and_then(Value::as_f64))
            else {
                return fail("a recorded result is missing a column".to_owned(), "read_results");
            };
            found.push(Tested {
                row_id,
                test,
                status,
                duration_ms,
            });
        }
        Ok(found)
    }

    /// How many rows a counting query returns; for the idempotence tests.
    #[cfg(test)]
    pub(super) fn count(&self, table: &str, revision: &str) -> Result<u64, Failure> {
        let rows = self.query(&format!("SELECT COUNT(*) AS n FROM {table} WHERE revision = {};", quote(revision)))?;
        match rows.first().and_then(|row| row.get("n")).and_then(Value::as_u64) {
            Some(count) => Ok(count),
            None => fail("a count was not a number".to_owned(), "count"),
        }
    }

    /// Runs a script directly; for tests that must plant a partial receipt.
    #[cfg(test)]
    pub(super) fn plant(&self, script: &str) -> Result<(), Failure> {
        self.run_script(script)
    }
}

#[cfg(test)]
mod tests {
    use super::{NewRun, Tested, connect, quote, real};
    use crate::fixture::Fixture;
    use std::error::Error;
    use std::fs;

    type TestResult = Result<(), Box<dyn Error>>;

    /// A run recorded at `revision` with the given statuses.
    fn run_at(revision: &str) -> NewRun<'_> {
        NewRun {
            revision,
            platform: "Darwin-arm64",
            feature_set: "full",
            package: "lgwks_bot",
            command: "cargo nextest run",
            wall_seconds: 1.0,
            run_at: "2026-10-05T00:00:00Z",
            artifact_path: "",
        }
    }

    /// Three passing results.
    fn three_results() -> Vec<Tested> {
        ["a", "b", "c"]
            .iter()
            .map(|name| Tested {
                row_id: "T01".to_owned(),
                test: (*name).to_owned(),
                status: "ok".to_owned(),
                duration_ms: 1.5,
            })
            .collect()
    }

    #[test]
    fn quoting_doubles_single_quotes_and_numbers_stay_numbers() {
        assert_eq!(quote("it's"), "'it''s'", "an apostrophe is doubled");
        assert_eq!(real(1.0), "1.0", "a whole number stays real");
        assert_eq!(real(f64::NAN), "0.0", "a non-number is never written");
    }

    #[test]
    fn a_rerun_of_one_revision_is_idempotent() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-receipt-test")?;
        let store = connect(&fixture.path().join("receipts.sqlite"))?;
        store.record_run(&run_at("rev-one"), &three_results())?;
        store.record_run(&run_at("rev-one"), &three_results())?;
        assert_eq!(store.count("run", "rev-one")?, 1, "a re-run overwrites its own run, not adds a second");
        assert_eq!(store.count("test_result", "rev-one")?, 3, "and does not duplicate the test results");
        Ok(())
    }

    #[test]
    fn two_revisions_coexist_and_are_separately_readable() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-receipt-test")?;
        let store = connect(&fixture.path().join("receipts.sqlite"))?;
        store.record_run(&run_at("rev-one"), &three_results())?;
        store.record_run(&run_at("rev-two"), &three_results())?;
        assert_eq!(store.read_run("rev-one", "full", "Darwin-arm64")?.revision, "rev-one", "the first");
        assert_eq!(store.read_run("rev-two", "full", "Darwin-arm64")?.revision, "rev-two", "the second");
        let absent = store.read_run("rev-absent", "full", "Darwin-arm64").err().map(|failure| failure.0);
        assert_eq!(absent.map(|text| text.contains("rev-absent")), Some(true), "a missing revision is named");
        Ok(())
    }

    #[test]
    fn a_corrupt_database_is_refused() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-receipt-test")?;
        let path = fixture.path().join("not-a-database.sqlite");
        fs::write(&path, b"this is not a sqlite file at all")?;
        let refused = connect(&path).err().map(|failure| failure.0);
        assert_eq!(refused.map(|text| text.contains("not a SQLite database")), Some(true), "a foreign file");
        Ok(())
    }

    #[test]
    fn a_partial_receipt_is_refused() -> TestResult {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-receipt-test")?;
        let store = connect(&fixture.path().join("receipts.sqlite"))?;
        store.plant(
            "INSERT INTO run (seq, revision, platform, feature_set, package, command, wall_seconds, run_at) \
             VALUES (99, 'rev-partial', 'Darwin-arm64', 'full', 'lgwks_bot', 'cmd', 1.0, '2026-10-05T00:00:00Z');",
        )?;
        let refused = store.read_results("rev-partial", "full", "Darwin-arm64").err().map(|failure| failure.0);
        assert_eq!(refused.map(|text| text.contains("records no test results")), Some(true), "no results");
        Ok(())
    }
}
