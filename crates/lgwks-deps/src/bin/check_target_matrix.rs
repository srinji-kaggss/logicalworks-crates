//! The declared target matrix, executed (#276, ADR 0003): one `cargo check` per
//! target and per crate, with each check's outcome printed and bounded.
//!
//! This was a shell script until 2026-10-22. It is Rust because the repository
//! holds Rust, and because the claim it makes — "the Rust-only surface builds
//! for every target we declare" — is now stated beside the decision that makes
//! the *default* set carry C: `hash`'s backend compiles C per target, so a
//! required check may not name the default set. The default set is checked too,
//! and a failure of it is exempt only when the exact cc-rs toolchain phrases
//! appear, which keeps the exemption from becoming a lid over a real regression.
//!
//! Usage: `check-target-matrix [TARGET ...]`, unset meaning every declared
//! target plus the host triple.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io::{self, Write};
use std::process::{Command, ExitCode};

/// The targets the estate declares support for.
const DEFAULT_TARGETS: [&str; 6] = [
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "wasm32-wasip1",
    "x86_64-unknown-freebsd",
    "aarch64-linux-android",
    "aarch64-apple-ios",
];

/// The phrase a cc-rs refusal carries.
const CC_PREFIX: &str = "error occurred in cc-rs";
/// The half of the exemption that names the reason: no compiler for the target.
const CC_MISSING: [&str; 2] = ["failed to find tool", "did not execute successfully"];

/// Why the matrix could not run.
#[derive(Debug)]
enum MatrixError {
    /// An operating-system call refused.
    Io(std::io::Error),
    /// `rustc -vV` printed no host line.
    NoHostLine,
    /// rustup could not install a declared target.
    Unreachable {
        /// The target that could not be installed.
        target: String,
        /// What rustup said.
        cause: String,
    },
}

impl Display for MatrixError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io(ref cause) => write!(formatter, "{cause}"),
            Self::NoHostLine => formatter.write_str("rustc -vV printed no host line"),
            Self::Unreachable {
                ref target,
                ref cause,
            } => write!(formatter, "rustup could not install {target}: {cause}"),
        }
    }
}

impl Error for MatrixError {}

impl From<std::io::Error> for MatrixError {
    fn from(cause: std::io::Error) -> Self {
        Self::Io(cause)
    }
}

/// One check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The target built the set.
    Pass,
    /// This runner has no C toolchain for the target, which the check names.
    Exempt,
    /// A failure that is not the declared exemption.
    Fail,
}

impl Verdict {
    /// The word the summary prints.
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Exempt => "exempt",
            Self::Fail => "FAIL",
        }
    }
}

/// One check as the matrix runs it: a target, a crate and a feature set.
struct Check {
    /// What the check is, for the printed line.
    label: String,
    /// The cargo arguments, in order.
    arguments: Vec<String>,
    /// Whether a missing C toolchain is the declared exemption for it.
    exemptible: bool,
}

impl Check {
    /// Runs the check and returns its verdict with the compiler's output.
    fn run(&self) -> (Verdict, String) {
        let output = match Command::new("cargo")
            .args(["check", "--locked"])
            .args(&self.arguments)
            .env("RUSTFLAGS", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .output()
        {
            Ok(output) => output,
            Err(cause) => return (Verdict::Fail, format!("cargo did not run: {cause}")),
        };
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if output.status.success() {
            return (Verdict::Pass, text);
        }
        if self.exemptible && is_exempt(&text) {
            return (Verdict::Exempt, text);
        }
        (Verdict::Fail, text)
    }
}

/// True when a failure is the declared C-toolchain exemption: a cc-rs refusal
/// whose cause is a compiler this runner does not have for the target.
fn is_exempt(text: &str) -> bool {
    text.contains(CC_PREFIX) && CC_MISSING.iter().any(|phrase| text.contains(phrase))
}

/// The host triple, so the matrix always checks the machine it runs on.
fn host_triple() -> Result<String, MatrixError> {
    let output = Command::new("rustc").args(["-vV"]).output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::trim))
        .ok_or(MatrixError::NoHostLine)?;
    Ok(host.to_owned())
}

/// Installs `target` when rustup does not have it yet, reporting whether it did.
fn ensure_target(target: &str) -> Result<bool, MatrixError> {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()?;
    let listed = String::from_utf8_lossy(&installed.stdout);
    if listed.lines().any(|line| line.trim() == target) {
        return Ok(false);
    }
    let added = Command::new("rustup")
        .args(["target", "add", target])
        .output()?;
    if added.status.success() {
        return Ok(true);
    }
    Err(MatrixError::Unreachable {
        target: target.to_owned(),
        cause: String::from_utf8_lossy(&added.stderr).trim().to_owned(),
    })
}

/// The checks for one target: required first, then the recorded ones.
fn checks_for(target: &str) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut push = |crate_name: &str, features: &[&str], exemptible: bool| {
        let mut arguments = vec![
            "--target".to_owned(),
            target.to_owned(),
            "-p".to_owned(),
            crate_name.to_owned(),
        ];
        arguments.extend(features.iter().map(|feature| (*feature).to_owned()));
        checks.push(Check {
            label: format!(
                "{target} :: cargo check -p {crate_name} {}",
                features.join(" ")
            ),
            arguments,
            exemptible,
        });
    };
    // Required: the Rust-only surface, on every target. No `hash` backend, no
    // `http`, no grammar, so no C compiler is involved.
    push("lgwks_std", &["--no-default-features"], false);
    push(
        "lgwks_std",
        &["--no-default-features", "--features", "trace"],
        false,
    );
    push(
        "lgwks_std",
        &["--no-default-features", "--features", "random"],
        false,
    );
    // The gate crate's own default selection is empty and pure Rust, so this is
    // required: a storefront that needed C to build would not be a storefront.
    push("lgwks_deps", &[], false);
    // Recorded: the default sets. `lgwks_std`'s carries `hash`, whose backend
    // compiles C per target, and `lgwks_ast`'s carries the grammars.
    push("lgwks_std", &[], true);
    push("lgwks_ast", &[], true);
    checks
}

/// Why one target could not be run at all.
#[derive(Debug)]
struct TargetError {
    /// The target.
    target: String,
    /// What went wrong.
    cause: String,
}

impl Display for TargetError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.target, self.cause)
    }
}

impl Error for TargetError {}

/// What a completed matrix found.
struct Summary {
    /// Checks that failed and were not the declared exemption.
    failures: usize,
    /// Checks this runner had no C toolchain for.
    exempt: usize,
}

/// Runs the matrix for `targets`, writing every line to `out`.
fn run(out: &mut impl Write, targets: &[String]) -> Result<Summary, MatrixError> {
    let host = host_triple()?;
    writeln!(
        out,
        "cross-target matrix: host {host}, {} target(s)",
        targets.len()
    )?;
    let mut all = targets.to_vec();
    all.push(host);
    let mut summary = Summary {
        failures: 0,
        exempt: 0,
    };
    for target in &all {
        writeln!(out, "\n=== {target}")?;
        match ensure_target(target) {
            Ok(true) => writeln!(out, "  added {target}")?,
            Ok(false) => {}
            Err(cause) => {
                let error = TargetError {
                    target: target.clone(),
                    cause: cause.to_string(),
                };
                lgwks_std::trace::debug!(error = %error, "matrix: the target is unreachable here");
                writeln!(out, "  FAIL  {error}")?;
                summary.failures = summary.failures.saturating_add(1);
                continue;
            }
        }
        for check in checks_for(target) {
            let started = std::time::Instant::now();
            let (verdict, output) = check.run();
            let seconds = started.elapsed().as_secs();
            writeln!(
                out,
                "  {:<6} {seconds:>4}s  {}",
                verdict.label(),
                check.label
            )?;
            match verdict {
                Verdict::Pass => {}
                Verdict::Exempt => {
                    summary.exempt = summary.exempt.saturating_add(1);
                    for line in output.lines().filter(|line| line.starts_with("error")) {
                        writeln!(out, "          {line}")?;
                    }
                    writeln!(
                        out,
                        "          not exercised here: no C toolchain for {target}; a new failure on this line is a regression"
                    )?;
                }
                Verdict::Fail => {
                    summary.failures = summary.failures.saturating_add(1);
                    for line in output.lines().filter(|line| line.starts_with("error")) {
                        writeln!(out, "          {line}")?;
                    }
                }
            }
        }
    }
    writeln!(
        out,
        "\n{} check(s) exempt for a missing C toolchain",
        summary.exempt
    )?;
    if summary.failures == 0 {
        writeln!(out, "target matrix passed")?;
    } else {
        writeln!(
            out,
            "target matrix failed: {} required or unexpected failure(s)",
            summary.failures
        )?;
    }
    Ok(summary)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let targets = if arguments.is_empty() {
        DEFAULT_TARGETS
            .iter()
            .map(|target| (*target).to_owned())
            .collect()
    } else {
        arguments
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let summary = match run(&mut out, &targets) {
        Ok(summary) => summary,
        Err(cause) => {
            lgwks_std::trace::debug!(error = %cause, "matrix: the run could not start");
            let stderr = io::stderr();
            let mut err = stderr.lock();
            let _reported = writeln!(err, "check-target-matrix: {cause}");
            return ExitCode::from(2);
        }
    };
    if summary.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{Verdict, checks_for, is_exempt};

    /// The exemption needs both halves: a cc-rs refusal *and* a missing tool.
    #[test]
    fn only_a_cc_toolchain_refusal_is_exempt() {
        let missing = "error occurred in cc-rs: failed to find tool \"x86_64-linux-musl-gcc\": No such file or directory";
        assert!(is_exempt(missing), "a missing compiler is the exemption");
        let real = "error[E0432]: unresolved import `lgwks_std::hash`";
        assert!(!is_exempt(real), "a Rust error is never exempt");
        let partial = "error occurred in cc-rs: command failed";
        assert!(
            !is_exempt(partial),
            "a cc-rs failure with no missing tool is not the exemption"
        );
    }

    /// The required checks name no C-carrying set; the recorded ones do.
    #[test]
    fn the_required_checks_are_the_rust_only_surface() {
        let checks = checks_for("wasm32-wasip1");
        let required: Vec<&str> = checks
            .iter()
            .filter(|check| !check.exemptible)
            .map(|check| check.label.as_str())
            .collect();
        assert_eq!(required.len(), 4, "four required checks per target");
        for label in required {
            assert!(
                label.contains("--no-default-features") || label.contains("lgwks_deps"),
                "a required check may not name a set that compiles C: {label}"
            );
        }
        assert_eq!(
            checks.iter().filter(|check| check.exemptible).count(),
            2,
            "the two default sets are the recorded checks"
        );
    }

    /// Every check carries the target it was built for.
    #[test]
    fn every_check_names_its_target() {
        for check in checks_for("aarch64-apple-ios") {
            assert!(
                check
                    .arguments
                    .iter()
                    .any(|argument| argument == "aarch64-apple-ios"),
                "{} does not name its target",
                check.label
            );
        }
        assert_eq!(Verdict::Pass.label(), "pass");
        assert_eq!(Verdict::Fail.label(), "FAIL");
    }
}
