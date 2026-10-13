//! Asserts `scripts/gate-lanes.toml` and `.github/workflows/ci.yml` describe one gate.
//!
//! Issue #127 refused the claim "the script is the single definition of the
//! gate" while the workflow was free to invent its own commands. A second
//! hand-copied list drifts, and each side still looks green on its own terms.
//! This check makes the drift loud.
//!
//! Rules:
//!   1. Lane-id bijection. Every lane that names a `ci_step` claims a step of
//!      that name in ci.yml, and every gate step in ci.yml (everything but the
//!      toolchain pin, the checkout and the setup steps) is claimed by exactly
//!      one lane. A lane may instead name a `ci_job`, for a job whose `run:`
//!      steps are unnamed; that job must exist.
//!   2. Command agreement. For a shared lane (`surfaces = "local ci"`) that
//!      carries a `command`, every `&&`-separated part of that command is a
//!      segment of the step's run body. A shortened command is a different
//!      command and fails.
//!   3. Builtin agreement. A `builtin` lane that runs in CI must be invoked as
//!      `python3 scripts/ci_local.py --lane <id>`, so the text of the check
//!      exists once.
//!   4. Toolchain agreement. The manifest's `toolchain.rust` is the version
//!      every `toolchain:` pin in the workflow installs.
//!
//! Usage: `check-gate-parity [ROOT]`. Exit 0 when all rules hold.

mod reader;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use reader::{Step, read_manifest, read_workflow};

/// The lane table, relative to the repository root.
const MANIFEST_REL: &str = "scripts/gate-lanes.toml";

/// The workflow, relative to the repository root.
const WORKFLOW_REL: &str = ".github/workflows/ci.yml";

/// Step-name prefixes that are setup, not gate.
const SKIP_STEP_PREFIXES: [&str; 7] = [
    "Pin Rust", "Checkout", "Install", "Set up", "Upload", "Download", "Cache",
];

/// Why the check could not run.
#[derive(Debug)]
enum ParityError {
    /// A file could not be read.
    Io {
        /// What was being read.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
}

impl fmt::Display for ParityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io { ref path, ref cause } => write!(formatter, "{}: {cause}", path.display()),
        }
    }
}

impl std::error::Error for ParityError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
fn fail<T>(error: ParityError, site: &'static str) -> Result<T, ParityError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// Reads a file whole, naming it when it cannot be read.
fn read(path: &Path) -> Result<String, ParityError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            ParityError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// Collapses runs of whitespace to single spaces.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `&&`-separated parts of a lane command, normalised.
fn command_parts(command: &str) -> Vec<String> {
    command
        .split("&&")
        .map(normalise)
        .filter(|part| !part.is_empty())
        .collect()
}

/// Splits a step body into comparable command segments.
///
/// A `run: |` block keeps newlines and each line is its own command, except a
/// wrapped option line belongs to the command before it; `&&` and `||` are the
/// separators within a line.
fn step_segments(run: &str) -> Vec<String> {
    let mut logical: Vec<String> = Vec::new();
    let mut current = String::new();
    for raw in run.lines() {
        let line = normalise(raw);
        if line.is_empty() {
            continue;
        }
        if !current.is_empty() && (line.starts_with('-') || current.ends_with('\\')) {
            current = normalise(&format!("{} {line}", current.trim_end_matches('\\')));
            continue;
        }
        if !current.is_empty() {
            logical.push(current);
        }
        current = line;
    }
    if !current.is_empty() {
        logical.push(current);
    }
    logical
        .iter()
        .flat_map(|line| line.replace("||", "&&").split("&&").map(normalise).collect::<Vec<_>>())
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Whether a step is part of the gate rather than setup.
fn is_gate_step(step: &Step) -> bool {
    !SKIP_STEP_PREFIXES
        .iter()
        .any(|prefix| step.name.starts_with(prefix))
}

/// Every `toolchain: "<version>"` pin in the workflow text.
fn toolchain_pins(workflow: &str) -> Vec<String> {
    let mut pins = Vec::new();
    for (start, _) in workflow.match_indices("toolchain:") {
        let Some(rest) = workflow.get(start.saturating_add("toolchain:".len())..) else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with(['"', '\'']) {
            continue;
        }
        let Some(body) = rest.get(1..) else {
            continue;
        };
        if let Some(end) = body.find(['"', '\''])
            && let Some(version) = body.get(..end)
            && !version.is_empty()
        {
            pins.push(version.to_owned());
        }
    }
    pins
}

/// The rule violations between the lane table and the workflow. Empty means they agree.
fn check_parity(manifest_text: &str, workflow_text: &str) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    let manifest = read_manifest(manifest_text);
    let jobs = read_workflow(workflow_text);

    let mut steps_by_name: BTreeMap<&str, &Step> = BTreeMap::new();
    let mut gate_steps: BTreeMap<&str, &Step> = BTreeMap::new();
    for job in &jobs {
        for step in &job.steps {
            steps_by_name.insert(step.name.as_str(), step);
            if is_gate_step(step) {
                gate_steps.insert(step.name.as_str(), step);
            }
        }
    }
    let job_names: BTreeSet<&str> = jobs.iter().map(|job| job.name.as_str()).collect();

    let mut claimed_steps: BTreeMap<String, String> = BTreeMap::new();
    let mut claimed_jobs: BTreeMap<String, String> = BTreeMap::new();
    for lane in &manifest.lanes {
        if !lane.surfaces.iter().any(|surface| surface == "ci") {
            continue;
        }
        if lane.ci_step.is_none() && lane.ci_job.is_none() {
            problems.push(format!("{}: ci lane names neither ci_step nor ci_job", lane.id));
            continue;
        }
        if let Some(ci_step) = lane.ci_step.as_deref() {
            let Some(step) = steps_by_name.get(ci_step) else {
                problems.push(format!("{}: ci_step {ci_step:?} is not a step in ci.yml", lane.id));
                continue;
            };
            if let Some(owner) = claimed_steps.get(ci_step) {
                problems.push(format!(
                    "{}: ci_step {ci_step:?} is already claimed by {owner}",
                    lane.id
                ));
            }
            claimed_steps.insert(ci_step.to_owned(), lane.id.clone());
            if lane.builtin.is_some() {
                let expected = format!("python3 scripts/ci_local.py --lane {}", lane.id);
                if !normalise(&step.run).contains(&expected) {
                    let shown: String = normalise(&step.run).chars().take(80).collect();
                    problems.push(format!(
                        "{}: builtin must run as {expected:?}, step runs {shown:?}",
                        lane.id
                    ));
                }
            } else if let Some(command) = lane.command.as_deref()
                && lane.surfaces.iter().any(|surface| surface == "local")
            {
                let segments = step_segments(&step.run);
                for part in command_parts(command) {
                    if !segments.contains(&part) {
                        let shown: Vec<&String> = segments.iter().take(4).collect();
                        problems.push(format!(
                            "{}: command part {part:?} is not a command of ci.yml step {ci_step:?} \
                             (which runs {shown:?})",
                            lane.id
                        ));
                    }
                }
            }
        }
        if let Some(ci_job) = lane.ci_job.as_deref() {
            if job_names.contains(ci_job) {
                claimed_jobs.insert(ci_job.to_owned(), lane.id.clone());
            } else {
                problems.push(format!("{}: ci_job {ci_job:?} is not a job in ci.yml", lane.id));
            }
        }
    }

    for (name, step) in &gate_steps {
        if claimed_steps.contains_key(*name) || claimed_jobs.contains_key(&step.job) {
            continue;
        }
        problems.push(format!(
            "ci.yml step {name:?} is not claimed by any lane in {MANIFEST_REL}"
        ));
    }

    // Every pin, not any pin: one job left on an older toolchain builds a
    // different compiler's output while the manifest names another.
    if !manifest.rust.is_empty() {
        let pins = toolchain_pins(workflow_text);
        if pins.is_empty() {
            problems.push("ci.yml installs no pinned toolchain".to_owned());
        }
        let drifted: BTreeSet<&String> = pins.iter().filter(|pin| **pin != manifest.rust).collect();
        if !drifted.is_empty() {
            problems.push(format!(
                "toolchain.rust is {:?} but ci.yml also pins {drifted:?}",
                manifest.rust
            ));
        }
    }
    problems
}

/// Runs the check over `root`, writing its verdict to the two handles.
fn execute(
    root: &Path,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<u8, Box<dyn std::error::Error>> {
    let manifest_text = read(&root.join(MANIFEST_REL))?;
    let workflow_text = read(&root.join(WORKFLOW_REL))?;
    let problems = check_parity(&manifest_text, &workflow_text);
    if !problems.is_empty() {
        writeln!(err, "gate parity refused: {} problem(s)", problems.len())?;
        for problem in &problems {
            writeln!(err, "  - {problem}")?;
        }
        return Ok(1);
    }
    let manifest = read_manifest(&manifest_text);
    let shared = manifest
        .lanes
        .iter()
        .filter(|lane| {
            lane.surfaces.iter().any(|surface| surface == "local")
                && lane.surfaces.iter().any(|surface| surface == "ci")
        })
        .count();
    writeln!(
        out,
        "gate parity: {} lane(s) declared, {shared} shared between local and ci; lane ids, \
         commands and toolchain agree",
        manifest.lanes.len()
    )?;
    Ok(0)
}

fn main() -> ExitCode {
    let root = match std::env::args_os().nth(1) {
        Some(argument) => PathBuf::from(argument),
        None => PathBuf::from("."),
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match execute(&root, &mut out, &mut err) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if writeln!(err, "{error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_REL, WORKFLOW_REL, check_parity, step_segments};
    use std::error::Error;
    use std::path::{Path, PathBuf};

    type TestResult = Result<(), Box<dyn Error>>;

    /// The repository this crate sits in.
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The committed lane table and workflow.
    fn files() -> Result<(String, String), Box<dyn Error>> {
        Ok((
            std::fs::read_to_string(root().join(MANIFEST_REL))?,
            std::fs::read_to_string(root().join(WORKFLOW_REL))?,
        ))
    }

    /// The `rust = "..."` pin of a lane table.
    fn pinned(manifest: &str) -> Result<String, Box<dyn Error>> {
        let start = manifest.find("rust = \"").ok_or("the manifest pins no rust")?;
        let rest = manifest
            .get(start.saturating_add("rust = \"".len())..)
            .ok_or("the pin is cut off")?;
        let end = rest.find('"').ok_or("the pin is unterminated")?;
        Ok(rest.get(..end).ok_or("the pin is cut off")?.to_owned())
    }

    #[test]
    fn the_repository_currently_agrees() -> TestResult {
        let (manifest, workflow) = files()?;
        assert_eq!(check_parity(&manifest, &workflow), Vec::<String>::new(), "the gate agrees");
        Ok(())
    }

    #[test]
    fn a_required_checker_removed_from_ci_fails() -> TestResult {
        let (manifest, workflow) = files()?;
        let mutated = workflow.replace("      - name: Check formatting", "      - name: Skipped formatting");
        let problems = check_parity(&manifest, &mutated);
        assert!(!problems.is_empty(), "renaming a gate step must break parity");
        assert!(problems.iter().any(|problem| problem.contains("Check formatting")), "{problems:?}");
        Ok(())
    }

    #[test]
    fn a_required_checker_removed_from_the_manifest_fails() -> TestResult {
        let (manifest, workflow) = files()?;
        let mutated = manifest.replacen("ci_step = \"Check formatting\"\n", "", 1);
        let problems = check_parity(&mutated, &workflow);
        assert!(!problems.is_empty(), "dropping a lane's ci_step must break parity");
        assert!(problems.iter().any(|problem| problem.contains("Check formatting")), "{problems:?}");
        Ok(())
    }

    #[test]
    fn a_substituted_command_fails() -> TestResult {
        let (manifest, workflow) = files()?;
        let shortened = manifest.replace(
            "command = \"cargo fmt --all -- --check\"",
            "command = \"cargo fmt --all\"",
        );
        let problems = check_parity(&shortened, &workflow);
        assert!(!problems.is_empty(), "a substituted command must break parity");
        assert!(problems.iter().any(|problem| problem.contains("cargo fmt --all")), "{problems:?}");
        let wholesale = manifest.replace(
            "command = \"cargo fmt --all -- --check\"",
            "command = \"cargo clippy --workspace --all-targets --locked -- -D warnings\"",
        );
        assert!(
            !check_parity(&wholesale, &workflow).is_empty(),
            "a wholesale substitution must break parity"
        );
        Ok(())
    }

    #[test]
    fn folded_commands_keep_option_continuations_together() {
        let step = "cargo clippy --locked --manifest-path crates/lgwks-std/Cargo.toml\n\
                    --all-targets --all-features -- -D warnings &&\n\
                    cargo fmt --all -- --check";
        assert_eq!(
            step_segments(step),
            vec![
                "cargo clippy --locked --manifest-path crates/lgwks-std/Cargo.toml --all-targets --all-features -- -D warnings",
                "cargo fmt --all -- --check",
            ],
            "a wrapped option line belongs to the command before it"
        );
    }

    #[test]
    fn a_builtin_must_route_through_the_coordinator() -> TestResult {
        let (manifest, workflow) = files()?;
        let mutated = workflow.replace(
            "python3 scripts/ci_local.py --lane unwrap-scan",
            "grep -rn '\\.unwrap()' crates",
        );
        let problems = check_parity(&manifest, &mutated);
        assert!(!problems.is_empty(), "a pasted builtin body must break parity");
        assert!(problems.iter().any(|problem| problem.contains("unwrap-scan")), "{problems:?}");
        Ok(())
    }

    #[test]
    fn a_toolchain_mismatch_fails() -> TestResult {
        let (manifest, workflow) = files()?;
        let pin = pinned(&manifest)?;
        let mutated = manifest.replacen(&format!("rust = \"{pin}\""), "rust = \"0.0.0-mismatch\"", 1);
        assert_ne!(mutated, manifest, "the mutation must actually change the manifest");
        let problems = check_parity(&mutated, &workflow);
        assert!(!problems.is_empty(), "a toolchain drift must break parity");
        assert!(problems.iter().any(|problem| problem.contains("toolchain")), "{problems:?}");
        Ok(())
    }

    #[test]
    fn one_drifted_job_pin_fails() -> TestResult {
        let (manifest, workflow) = files()?;
        let pin = pinned(&manifest)?;
        let mutated = workflow.replacen(
            &format!("toolchain: \"{pin}\""),
            "toolchain: \"0.0.0-drifted\"",
            1,
        );
        assert_ne!(mutated, workflow, "the mutation must change the workflow");
        assert!(
            mutated.contains(&format!("toolchain: \"{pin}\"")),
            "the other jobs still agree"
        );
        let problems = check_parity(&manifest, &mutated);
        assert!(problems.iter().any(|problem| problem.contains("0.0.0-drifted")), "{problems:?}");
        Ok(())
    }

    #[test]
    fn an_unclaimed_gate_step_fails() -> TestResult {
        let (manifest, workflow) = files()?;
        let mutated = workflow.replace(
            "      - name: Check formatting",
            "      - name: Check formatting\n      - name: Check spelling\n        run: codespell",
        );
        let problems = check_parity(&manifest, &mutated);
        assert!(!problems.is_empty(), "an unclaimed gate step must break parity");
        assert!(problems.iter().any(|problem| problem.contains("Check spelling")), "{problems:?}");
        Ok(())
    }
}
