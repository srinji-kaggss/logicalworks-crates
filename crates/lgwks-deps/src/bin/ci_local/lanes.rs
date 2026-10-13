//! The lane table, and the running of one lane.
//!
//! `scripts/gate-lanes.toml` is the one definition of what the gate checks, read
//! through the shared TOML-subset reader. A lane runs here once: its platform and
//! needs are judged, then its builtin check or shell command, and the outcome is
//! stamped and timed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use super::process::Children;
use super::toml_subset::{Table, list, raw, read_tables, text};
use super::utc::stamp;
use super::{
    Context, GateError, LaneResult, Status, Verdict, builtins, fail, invariants, read, runners,
};

/// The lane table, relative to the repository root.
pub(super) const MANIFEST_REL: &str = "scripts/gate-lanes.toml";

/// The boolean under `key` of a table.
pub(super) fn flag(table: &Table, key: &str) -> Option<bool> {
    match raw(table, key) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) | None => None,
    }
}

/// The boolean under `key`, or `fallback` when the key is absent.
fn flag_or(table: &Table, key: &str, fallback: bool) -> bool {
    match flag(table, key) {
        Some(true) => true,
        Some(false) => false,
        None => fallback,
    }
}

/// One lane of the table.
pub(super) struct Lane {
    /// The stable lane name.
    pub(super) id: String,
    /// `local`, `ci`, or both.
    pub(super) surfaces: Vec<String>,
    /// Whether an unavailable lane refuses the gate.
    pub(super) required: bool,
    /// The shell command.
    pub(super) command: Option<String>,
    /// The builtin check, instead of a command.
    pub(super) builtin: Option<String>,
    /// Prerequisites checked before the lane runs.
    pub(super) needs: Vec<String>,
    /// Platforms the lane applies to; empty means every host.
    pub(super) platforms: Vec<String>,
    /// Whether the lane belongs to the `--fast` set.
    pub(super) fast: bool,
    /// Why a lane is optional or platform-bound.
    pub(super) reason: String,
    /// Lanes that build the same feature set share a group.
    pub(super) group: Option<String>,
}

/// The lane table.
pub(super) struct Manifest {
    /// `[toolchain] rust`, or `unspecified`.
    pub(super) rust: String,
    /// Every lane, in order.
    pub(super) lanes: Vec<Lane>,
}

/// A whitespace-separated string or a list of strings.
fn as_list(table: &Table, key: &str) -> Vec<String> {
    if let Some(found) = text(table, key) {
        return found.split_whitespace().map(str::to_owned).collect();
    }
    match list(table, key) {
        Some(found) => found.to_vec(),
        None => Vec::new(),
    }
}

/// Builds and validates one lane from its table.
fn build_lane(table: &Table, place: &str, seen: &mut BTreeSet<String>) -> Result<Lane, GateError> {
    let id = match text(table, "id") {
        Some(id) if !id.is_empty() => id.to_owned(),
        Some(_) | None => {
            return fail(
                GateError::Manifest(format!("{place}: missing id")),
                "build_lane",
            );
        }
    };
    if !seen.insert(id.clone()) {
        return fail(
            GateError::Manifest(format!("{place}: duplicate id {id:?}")),
            "build_lane",
        );
    }
    let surfaces = as_list(table, "surfaces");
    if surfaces.is_empty()
        || surfaces
            .iter()
            .any(|surface| surface != "local" && surface != "ci")
    {
        return fail(
            GateError::Manifest(format!(
                "{place} surfaces: expected local/ci, got {surfaces:?}"
            )),
            "build_lane",
        );
    }
    let command = text(table, "command").map(str::to_owned);
    let builtin = text(table, "builtin").map(str::to_owned);
    if command.is_some() == builtin.is_some() {
        return fail(
            GateError::Manifest(format!("{place}: exactly one of command/builtin")),
            "build_lane",
        );
    }
    let in_ci = text(table, "ci_step").is_some() || text(table, "ci_job").is_some();
    if surfaces.iter().any(|surface| surface == "ci") && !in_ci {
        return fail(
            GateError::Manifest(format!("{place}: a ci lane needs ci_step or ci_job")),
            "build_lane",
        );
    }
    Ok(Lane {
        id,
        surfaces,
        required: flag_or(table, "required", true),
        command,
        builtin,
        needs: as_list(table, "needs"),
        platforms: as_list(table, "platforms"),
        fast: flag_or(table, "fast", false),
        reason: match text(table, "reason") {
            Some(found) if !found.is_empty() => found.to_owned(),
            Some(_) | None => String::new(),
        },
        group: text(table, "group").map(str::to_owned),
    })
}

/// Reads and validates the lane table under `root`.
pub(super) fn load_lanes(root: &Path) -> Result<Manifest, GateError> {
    let tables = read_tables(&read(&root.join(MANIFEST_REL))?);
    let rust = match tables
        .iter()
        .find(|table| table.path == "toolchain")
        .and_then(|table| text(table, "rust"))
    {
        Some(pinned) => pinned.to_owned(),
        None => "unspecified".to_owned(),
    };
    let mut lanes = Vec::new();
    let mut seen = BTreeSet::new();
    for table in tables
        .iter()
        .filter(|table| table.path == "lane" && table.array)
    {
        let place = format!("{MANIFEST_REL} lane[{}]", lanes.len());
        lanes.push(build_lane(table, &place, &mut seen)?);
    }
    Ok(Manifest { rust, lanes })
}

/// The name this host goes by in a lane's `platforms`.
pub(super) fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Whether a lane applies to this host.
pub(super) fn platform_applies(lane: &Lane) -> bool {
    lane.platforms.is_empty() || lane.platforms.iter().any(|name| name == platform_name())
}

/// Whether the file's permissions let anyone execute it.
#[cfg(unix)]
fn has_execute_bit(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// Whether the file's permissions let anyone execute it; a platform without an
/// execute bit judges by the file alone.
#[cfg(not(unix))]
fn has_execute_bit(_meta: &fs::Metadata) -> bool {
    true
}

/// Whether `path` is an executable regular file.
pub(super) fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && has_execute_bit(&meta),
        Err(_) => false,
    }
}

/// Whether the prerequisite is available under `root`.
fn check_need(need: &str, root: &Path, env_path: &str) -> Result<bool, GateError> {
    let (kind, name) = match need.split_once(':') {
        Some((kind, name)) if !name.is_empty() => (kind, name),
        Some(_) | None => {
            return fail(
                GateError::Manifest(format!("unknown need {need:?}")),
                "check_need",
            );
        }
    };
    match kind {
        "script" => Ok(is_executable_file(&root.join(name))),
        "bin" => Ok(std::env::split_paths(env_path)
            .any(|directory| is_executable_file(&directory.join(name)))),
        "os" => Ok(platform_name() == name),
        _ => fail(
            GateError::Manifest(format!("unknown need {need:?}")),
            "check_need",
        ),
    }
}

/// The needs of `lane` that are not available.
pub(super) fn missing_needs(
    lane: &Lane,
    root: &Path,
    env_path: &str,
) -> Result<Vec<String>, GateError> {
    let mut missing = Vec::new();
    for need in &lane.needs {
        if !check_need(need, root, env_path)? {
            missing.push(need.clone());
        }
    }
    Ok(missing)
}

/// The `PATH` a lane's needs are checked against.
fn path_of(env: &BTreeMap<String, String>) -> String {
    match env.get("PATH") {
        Some(path) if !path.is_empty() => path.clone(),
        Some(_) | None => String::new(),
    }
}

/// The environment lanes run in.
pub(super) fn child_env(scratch: Option<&Path>, target: Option<&Path>) -> BTreeMap<String, String> {
    let vars = std::env::vars_os()
        .map(|entry| {
            (
                entry.0.to_string_lossy().into_owned(),
                entry.1.to_string_lossy().into_owned(),
            )
        })
        .collect();
    child_env_from(vars, scratch, target)
}

/// The environment lanes run in, given the environment `ci-local` was started in.
fn child_env_from(
    mut env: BTreeMap<String, String>,
    scratch: Option<&Path>,
    target: Option<&Path>,
) -> BTreeMap<String, String> {
    let existing = path_of(&env);
    if let Some(home) = env.get("HOME") {
        let cargo = Path::new(home).join(".cargo").join("bin");
        env.insert("PATH".to_owned(), format!("{}:{existing}", cargo.display()));
    }
    // A caller that already chose incremental compilation keeps its choice: cargo
    // fingerprints the setting, so a lane that forced `0` over the workflow's `1`
    // rebuilt every workspace test target the earlier steps had just built. The
    // `simulation-evidence` lane's `nextest list` spent 2 m 43 s on that rebuild
    // and the Build job hit its 5-minute limit (run 38407172423).
    env.entry("CARGO_INCREMENTAL".to_owned())
        .or_insert_with(|| "0".to_owned());
    env.insert("CARGO_TERM_COLOR".to_owned(), "never".to_owned());
    env.entry("RUST_BACKTRACE".to_owned())
        .or_insert_with(|| "1".to_owned());
    if let Some(directory) = scratch {
        env.insert("TMPDIR".to_owned(), format!("{}/", directory.display()));
    }
    if let Some(directory) = target {
        env.insert(
            "CARGO_TARGET_DIR".to_owned(),
            directory.display().to_string(),
        );
    }
    env
}

/// Runs a builtin lane by name.
fn run_builtin(name: &str, context: &Context<'_>) -> Result<Verdict, GateError> {
    match name {
        "unwrap-scan" => builtins::unwrap_scan(context),
        "suppressions" => builtins::suppressions(context),
        "artifacts" => builtins::artifacts(context),
        "invariants" => invariants::invariants(context),
        "contract-drift" => builtins::contract_drift(context),
        "docsrs-metadata" => runners::docsrs_metadata(context),
        "readme-quickstart" => builtins::readme_quickstart(context),
        "debug-e2e" => runners::debug_e2e(context),
        "simulation-evidence" => runners::simulation_evidence(context),
        _ => Ok(Verdict {
            code: 2,
            detail: format!("unknown builtin {name:?}"),
        }),
    }
}

/// Opens a lane's log, making its directory.
fn open_log(path: &Path) -> Result<File, GateError> {
    if let Some(parent) = path.parent()
        && let Err(cause) = fs::create_dir_all(parent)
    {
        return fail(
            GateError::Io {
                path: parent.to_path_buf(),
                cause,
            },
            "open_log",
        );
    }
    match File::create(path) {
        Ok(file) => Ok(file),
        Err(cause) => fail(
            GateError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "open_log",
        ),
    }
}

/// What a lane did, before it is timed and stamped.
struct Outcome {
    /// How it ended.
    status: Status,
    /// What there is to say about it.
    detail: String,
}

/// Runs a lane that applies and has its needs, writing to `log`.
fn execute_lane(
    lane: &Lane,
    root: &Path,
    env: &BTreeMap<String, String>,
    log: Option<&Path>,
    children: &Children,
) -> Result<Outcome, GateError> {
    let file = match log {
        Some(path) => Some(open_log(path)?),
        None => None,
    };
    if let Some(name) = lane.builtin.as_deref() {
        let context = Context {
            root,
            env,
            children,
            out: file.as_ref(),
        };
        let verdict = run_builtin(name, &context)?;
        if let Some(mut sink) = file.as_ref()
            && let Err(error) = writeln!(sink, "{}", verdict.detail)
        {
            lgwks_std::trace::debug!(%error, "a lane's log could not be written");
        }
        let status = if verdict.code == 0 {
            Status::Pass
        } else {
            Status::Fail
        };
        return Ok(Outcome {
            status,
            detail: verdict.detail,
        });
    }
    let command = match lane.command.as_deref() {
        Some(command) => command.to_owned(),
        None => String::new(),
    };
    let code = children.run(
        &["bash".to_owned(), "-c".to_owned(), command],
        root,
        env,
        file.as_ref(),
    );
    let (status, detail) = if code == 0 {
        (Status::Pass, String::new())
    } else {
        (Status::Fail, format!("exit {code}"))
    };
    Ok(Outcome { status, detail })
}

/// Runs one lane, or says why it did not run.
pub(super) fn run_lane(
    lane: &Lane,
    root: &Path,
    env: &BTreeMap<String, String>,
    log: Option<&Path>,
    children: &Children,
) -> LaneResult {
    let started = stamp();
    let clock = Instant::now();
    let finish = |status: Status, detail: String| LaneResult {
        lane_id: lane.id.clone(),
        status,
        started: started.clone(),
        ended: stamp(),
        detail,
        seconds: clock.elapsed().as_secs_f64(),
    };
    if !platform_applies(lane) {
        return finish(
            Status::Skip,
            format!(
                "out of scope for {}; declared for {:?}",
                platform_name(),
                lane.platforms
            ),
        );
    }
    let missing = match missing_needs(lane, root, &path_of(env)) {
        Ok(missing) => missing,
        Err(error) => return finish(Status::Fail, error.to_string()),
    };
    if !missing.is_empty() {
        let status = if lane.required {
            Status::Blocked
        } else {
            Status::Skip
        };
        let need_list = missing.join(", ");
        let detail = if lane.reason.is_empty() {
            format!("missing prerequisite(s): {need_list}")
        } else {
            format!("missing prerequisite(s): {need_list} ({})", lane.reason)
        };
        return finish(status, detail);
    }
    match execute_lane(lane, root, env, log, children) {
        Ok(outcome) => finish(outcome.status, outcome.detail),
        Err(error) => finish(Status::Fail, format!("the lane could not run: {error}")),
    }
}

#[cfg(test)]
mod sim_env;

#[cfg(test)]
mod tests {
    use super::{child_env_from, flag};
    use crate::toml_subset::{list, read_tables, text};
    use std::collections::BTreeMap;

    #[test]
    fn a_lane_keeps_the_incremental_setting_its_caller_chose() {
        let chosen = BTreeMap::from([("CARGO_INCREMENTAL".to_owned(), "1".to_owned())]);
        assert_eq!(
            child_env_from(chosen, None, None).get("CARGO_INCREMENTAL"),
            Some(&"1".to_owned()),
            "an explicit setting is not overwritten"
        );
        assert_eq!(
            child_env_from(BTreeMap::new(), None, None).get("CARGO_INCREMENTAL"),
            Some(&"0".to_owned()),
            "unset, a lane builds without incremental state"
        );
    }

    #[test]
    fn a_lane_table_reads_strings_flags_lists_and_multiline_commands() -> Result<(), String> {
        let source = "[toolchain]\nrust = \"1.99.0\"\n\n# a comment\n[[lane]]\nid = \"alpha\"\nrequired = false\nneeds = [\"bin:cargo\", \"script:a.sh\"]\ncommand = \"\"\"\\\ncargo one && \\\ncargo two\\\n\"\"\"\n";
        let tables = read_tables(source);
        let lane = tables
            .iter()
            .find(|table| table.path == "lane")
            .ok_or("no lane table")?;
        assert_eq!(text(lane, "id"), Some("alpha"), "a plain string");
        assert_eq!(flag(lane, "required"), Some(false), "a boolean");
        assert_eq!(
            list(lane, "needs").map(<[String]>::len),
            Some(2),
            "a list of strings"
        );
        assert_eq!(
            text(lane, "command"),
            Some("cargo one && cargo two"),
            "line continuations fold"
        );
        Ok(())
    }

    #[test]
    fn a_missing_key_is_absent_not_defaulted() -> Result<(), String> {
        let tables = read_tables("[[lane]]\nid = \"alpha\"\n");
        let lane = tables
            .iter()
            .find(|table| table.path == "lane")
            .ok_or("no lane table")?;
        assert_eq!(flag(lane, "required"), None, "absent stays absent");
        assert_eq!(text(lane, "command"), None, "absent stays absent");
        Ok(())
    }
}
