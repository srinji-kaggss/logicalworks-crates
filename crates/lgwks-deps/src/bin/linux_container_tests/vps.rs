//! The `vps` suite: the `bench/async` matrix under the hyperscale axis's
//! 2-vCPU / 2 GB profile (#269).
//!
//! `vps` is not a test suite. It builds the bench uncapped, then runs the built
//! binary in a second container capped by cgroup, so the cap measures the run
//! and not the compile, and writes each door's record beside the profile. It
//! runs alone, on a quiet host: a capped run beside other jobs measures them.
//!
//! The profile is data, `bench/async/vps/profile.toml`: which manifest and
//! binary, where the records go, the cap, what the container reports about the
//! cap it was given, and the doors in the order they run. Nothing about the
//! matrix is written here, so a door added to the file is a door that runs.
//!
//! Every function here is a pure function of the profile's text, so what a run
//! decides is checked without Docker.

use crate::plan::{CONTAINER_TARGET, Cap, Run};
use crate::toml_subset::{Table, list, raw, read_tables, text};
use crate::{Failure, fail};

/// The name the suite is invoked by.
pub(crate) const SUITE: &str = "vps";

/// Where the profile is, from the repository root.
pub(crate) const PROFILE: &str = "bench/async/vps/profile.toml";

/// The most doors a profile may run: one that repeated a door by mistake is
/// refused rather than run for hours under a cap.
const MAX_DOORS: usize = 64;

/// What a door that may reach the cap runs under: the command's own exit is
/// reported, not raised, because an out-of-memory kill (137) is the cap
/// reached, which is a result. `$0` is the door's name and `"$@"` the command.
const REPORT_EXIT: &str = "\"$@\" || echo \"linux-container-tests: $0 exited $? under the cap\"";

/// One bench door: a command the matrix runs.
struct Door {
    /// The door's name, as the exit report names it.
    name: String,
    /// The bench's arguments.
    arguments: Vec<String>,
    /// The record the door writes, as a bare file name under the records directory.
    record: Option<String>,
    /// Whether the door may exceed the cap, which is reported rather than raised.
    may_exceed_cap: bool,
}

/// The parsed profile.
pub(crate) struct Profile {
    /// The bench's manifest, from the repository root.
    manifest: String,
    /// The bench's binary, as the build names it under `release/`.
    binary: String,
    /// The directory the records are written to.
    records: String,
    /// The limits the matrix runs under.
    cap: Cap,
    /// What the container reports about the cap it was given, each an argument vector.
    reports: Vec<Vec<String>>,
    /// The doors, in the order they run.
    doors: Vec<Door>,
}

/// The one table written as `[path]`.
fn table<'tables>(tables: &'tables [Table], path: &str) -> Result<&'tables Table, Failure> {
    let mut found = tables
        .iter()
        .filter(|candidate| !candidate.array && candidate.path == path);
    match (found.next(), found.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => fail(format!("{PROFILE}: no [{path}] table"), "table"),
        (Some(_), Some(_)) => fail(format!("{PROFILE}: [{path}] is written twice"), "table"),
    }
}

/// The non-empty string under `key`.
fn required<'table>(table: &'table Table, key: &str) -> Result<&'table str, Failure> {
    match text(table, key) {
        Some(found) if !found.is_empty() => Ok(found),
        Some(_) | None => fail(
            format!(
                "{PROFILE}: [{}] needs a non-empty string `{key}`",
                table.path
            ),
            "required",
        ),
    }
}

/// The non-empty list of strings under `key`.
fn words_under(table: &Table, key: &str) -> Result<Vec<String>, Failure> {
    match list(table, key) {
        Some(found) if !found.is_empty() => Ok(found.to_vec()),
        Some(_) | None => fail(
            format!("{PROFILE}: [{}] needs a non-empty list `{key}`", table.path),
            "words_under",
        ),
    }
}

/// The boolean under `key`, false when it is absent; any other spelling is refused.
fn flag(table: &Table, key: &str) -> Result<bool, Failure> {
    match raw(table, key) {
        None => Ok(false),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => fail(
            format!(
                "{PROFILE}: [{}] `{key}` is {other}, not true or false",
                table.path
            ),
            "flag",
        ),
    }
}

/// A record name, which must stay a bare file name so a door cannot write
/// outside the records directory.
fn record_name(door: &Table) -> Result<Option<String>, Failure> {
    let Some(record) = text(door, "record") else {
        return Ok(None);
    };
    if record.is_empty() || record == "." || record == ".." || record.contains(['/', '\\']) {
        return fail(
            format!("{PROFILE}: door record {record:?} is not a bare file name"),
            "record_name",
        );
    }
    Ok(Some(record.to_owned()))
}

/// One door, from its `[[door]]` table.
fn read_door(entry: &Table) -> Result<Door, Failure> {
    let name = required(entry, "name")?.to_owned();
    let arguments = words_under(entry, "arguments")?;
    let record = record_name(entry)?;
    let may_exceed_cap = flag(entry, "may_exceed_cap")?;
    Ok(Door {
        name,
        arguments,
        record,
        may_exceed_cap,
    })
}

/// The limits, from the `[cap]` table.
fn read_cap(entry: &Table) -> Result<Cap, Failure> {
    let cpus = required(entry, "cpus")?.to_owned();
    let memory = required(entry, "memory")?.to_owned();
    let tmpfs = required(entry, "tmpfs")?.to_owned();
    Ok(Cap {
        cpus,
        memory,
        tmpfs,
    })
}

/// The profile in `source`.
pub(crate) fn parse(source: &str) -> Result<Profile, Failure> {
    let tables = read_tables(source);
    let build = table(&tables, "build")?;
    let cap = table(&tables, "cap")?;
    let mut reports = Vec::new();
    let mut doors = Vec::new();
    for entry in tables.iter().filter(|candidate| candidate.array) {
        match entry.path.as_str() {
            "report" => reports.push(words_under(entry, "argv")?),
            "door" => doors.push(read_door(entry)?),
            other => {
                return fail(format!("{PROFILE}: unknown table [[{other}]]"), "parse");
            }
        }
    }
    if doors.is_empty() || doors.len() > MAX_DOORS {
        return fail(
            format!(
                "{PROFILE}: {} doors; a profile runs between 1 and {MAX_DOORS}",
                doors.len()
            ),
            "parse",
        );
    }
    let manifest = required(build, "manifest")?.to_owned();
    let binary = required(build, "binary")?.to_owned();
    let records = required(build, "records")?.to_owned();
    let cap = read_cap(cap)?;
    Ok(Profile {
        manifest,
        binary,
        records,
        cap,
        reports,
        doors,
    })
}

/// The command that runs one door: the binary, its arguments and its record.
fn door_command(binary: &str, records: &str, door: &Door) -> Vec<String> {
    let mut command = vec![binary.to_owned()];
    command.extend(door.arguments.iter().cloned());
    if let Some(ref record) = door.record {
        command.push(format!("--json={records}/{record}"));
    }
    if !door.may_exceed_cap {
        return command;
    }
    let mut guarded = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        REPORT_EXIT.to_owned(),
        door.name.clone(),
    ];
    guarded.extend(command);
    guarded
}

/// The two runs the suite makes: the uncapped build, then the capped matrix.
pub(crate) fn runs(profile: &Profile) -> Vec<Run> {
    let build = ["cargo", "build", "--locked", "--release", "--manifest-path"]
        .iter()
        .map(|word| (*word).to_owned())
        .chain([profile.manifest.clone()])
        .collect();
    let binary = format!("{CONTAINER_TARGET}/release/{}", profile.binary);
    let mut matrix = vec![vec![
        "mkdir".to_owned(),
        "-p".to_owned(),
        profile.records.clone(),
    ]];
    matrix.extend(profile.reports.iter().cloned());
    matrix.extend(
        profile
            .doors
            .iter()
            .map(|door| door_command(&binary, &profile.records, door)),
    );
    vec![
        Run {
            commands: vec![build],
            cap: None,
        },
        Run {
            commands: matrix,
            cap: Some(profile.cap.clone()),
        },
    ]
}
