//! The lane table: `scripts/gate-lanes.toml`, the one definition of what the
//! gate checks, read through the shared TOML-subset reader.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::toml_subset::{Table, list, raw, read_tables, text};
use super::{GateError, fail, read};

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
        Some(_) | None => return fail(GateError::Manifest(format!("{place}: missing id")), "build_lane"),
    };
    if !seen.insert(id.clone()) {
        return fail(GateError::Manifest(format!("{place}: duplicate id {id:?}")), "build_lane");
    }
    let surfaces = as_list(table, "surfaces");
    if surfaces.is_empty() || surfaces.iter().any(|surface| surface != "local" && surface != "ci") {
        return fail(
            GateError::Manifest(format!("{place} surfaces: expected local/ci, got {surfaces:?}")),
            "build_lane",
        );
    }
    let command = text(table, "command").map(str::to_owned);
    let builtin = text(table, "builtin").map(str::to_owned);
    if command.is_some() == builtin.is_some() {
        return fail(GateError::Manifest(format!("{place}: exactly one of command/builtin")), "build_lane");
    }
    let in_ci = text(table, "ci_step").is_some() || text(table, "ci_job").is_some();
    if surfaces.iter().any(|surface| surface == "ci") && !in_ci {
        return fail(GateError::Manifest(format!("{place}: a ci lane needs ci_step or ci_job")), "build_lane");
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
    let rust = match tables.iter().find(|table| table.path == "toolchain").and_then(|table| text(table, "rust")) {
        Some(pinned) => pinned.to_owned(),
        None => "unspecified".to_owned(),
    };
    let mut lanes = Vec::new();
    let mut seen = BTreeSet::new();
    for table in tables.iter().filter(|table| table.path == "lane" && table.array) {
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

/// Whether `path` is an executable regular file.
pub(super) fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Whether the prerequisite is available under `root`.
fn check_need(need: &str, root: &Path, env_path: &str) -> Result<bool, GateError> {
    let (kind, name) = match need.split_once(':') {
        Some((kind, name)) if !name.is_empty() => (kind, name),
        Some(_) | None => return fail(GateError::Manifest(format!("unknown need {need:?}")), "check_need"),
    };
    match kind {
        "script" => Ok(is_executable_file(&root.join(name))),
        "bin" => Ok(std::env::split_paths(env_path).any(|directory| is_executable_file(&directory.join(name)))),
        "os" => Ok(platform_name() == name),
        _ => fail(GateError::Manifest(format!("unknown need {need:?}")), "check_need"),
    }
}

/// The needs of `lane` that are not available.
pub(super) fn missing_needs(lane: &Lane, root: &Path, env_path: &str) -> Result<Vec<String>, GateError> {
    let mut missing = Vec::new();
    for need in &lane.needs {
        if !check_need(need, root, env_path)? {
            missing.push(need.clone());
        }
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::flag;
    use crate::toml_subset::{list, read_tables, text};

    #[test]
    fn a_lane_table_reads_strings_flags_lists_and_multiline_commands() -> Result<(), String> {
        let source = "[toolchain]\nrust = \"1.99.0\"\n\n# a comment\n[[lane]]\nid = \"alpha\"\nrequired = false\nneeds = [\"bin:cargo\", \"script:a.sh\"]\ncommand = \"\"\"\\\ncargo one && \\\ncargo two\\\n\"\"\"\n";
        let tables = read_tables(source);
        let lane = tables.iter().find(|table| table.path == "lane").ok_or("no lane table")?;
        assert_eq!(text(lane, "id"), Some("alpha"), "a plain string");
        assert_eq!(flag(lane, "required"), Some(false), "a boolean");
        assert_eq!(list(lane, "needs").map(<[String]>::len), Some(2), "a list of strings");
        assert_eq!(text(lane, "command"), Some("cargo one && cargo two"), "line continuations fold");
        Ok(())
    }

    #[test]
    fn a_missing_key_is_absent_not_defaulted() -> Result<(), String> {
        let tables = read_tables("[[lane]]\nid = \"alpha\"\n");
        let lane = tables.iter().find(|table| table.path == "lane").ok_or("no lane table")?;
        assert_eq!(flag(lane, "required"), None, "absent stays absent");
        assert_eq!(text(lane, "command"), None, "absent stays absent");
        Ok(())
    }
}
