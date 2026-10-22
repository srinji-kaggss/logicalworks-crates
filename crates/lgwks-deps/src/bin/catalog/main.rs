//! `lgwks-deps catalog` prints the estate's capability catalog (#732): every
//! capability this repository owns, the path that owns it, and whether it is on
//! by default.
//!
//! This is the discovery half of the std-first gate. The gate refuses a
//! hand-rolled copy of a capability and names the owner; this command is where
//! that owner, and every other one, can be looked up before writing anything.
//!
//! Usage:
//!
//! ```text
//! lgwks-deps catalog [ROOT]            the catalog as a table
//!            catalog --json [ROOT]     the same as one JSON object
//!            catalog --write [ROOT]    re-render docs/catalog.md from the register
//!            catalog --check [ROOT]    refuse a catalog that disagrees with the tree
//! ```
//!
//! `--check` is what the `catalog` lane runs: every row's file must exist, every
//! row's feature must be one its crate declares, every `lgwks_*` owner the gate
//! can name in a refusal must have a row, and `docs/catalog.md` must be a fresh
//! render. Exit 2 on any finding, like every other refusal in this binary.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::catalog;

/// What the caller asked for.
enum Request {
    /// Print the catalog.
    Table,
    /// Print the catalog as one JSON object.
    Json,
    /// Re-render `docs/catalog.md`.
    Write,
    /// Check the catalog against the tree.
    Check,
}

/// Why the command line was refused.
#[derive(Debug)]
enum ParseError {
    /// Two run modes were named.
    TwoModes,
    /// An option this command does not take.
    UnknownOption(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TwoModes => {
                formatter.write_str("catalog takes one mode: --json, --write or --check")
            }
            Self::UnknownOption(ref option) => {
                write!(formatter, "unknown option for `catalog`: {option}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// One pass over the arguments: one mode, one optional root.
fn parse(arguments: &[String]) -> Result<(Request, PathBuf), ParseError> {
    let mut request = Request::Table;
    let mut mode_seen = false;
    let mut root = PathBuf::from(".");
    for argument in arguments {
        match argument.as_str() {
            "--json" | "--write" | "--check" => {
                if mode_seen {
                    let refusal = ParseError::TwoModes;
                    lgwks_std::trace::debug!(error = %refusal, "catalog: refusing two modes");
                    return Err(refusal);
                }
                mode_seen = true;
                request = match argument.as_str() {
                    "--json" => Request::Json,
                    "--write" => Request::Write,
                    _ => Request::Check,
                };
            }
            other if other.starts_with("--") => {
                let refusal = ParseError::UnknownOption(other.to_owned());
                lgwks_std::trace::debug!(error = %refusal, "catalog: refusing an unknown option");
                return Err(refusal);
            }
            other => root = PathBuf::from(other),
        }
    }
    Ok((request, root))
}

/// Runs the requested mode over `root`.
fn run(
    request: &Request,
    root: &Path,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    let held = match catalog::read(root) {
        Ok(held) => held,
        Err(error) => {
            writeln!(err, "REFUSED  {error}")?;
            return Ok(ExitCode::from(2));
        }
    };
    match *request {
        Request::Table => {
            let rendered = match catalog::render_table(&held) {
                Ok(rendered) => rendered,
                Err(error) => {
                    writeln!(err, "REFUSED  {error}")?;
                    return Ok(ExitCode::from(2));
                }
            };
            write!(out, "{rendered}")?;
            Ok(ExitCode::SUCCESS)
        }
        Request::Json => {
            write!(out, "{}", catalog::render_json(&held))?;
            Ok(ExitCode::SUCCESS)
        }
        Request::Write => {
            let path = match catalog::write_document(root, &held) {
                Ok(path) => path,
                Err(error) => {
                    writeln!(err, "REFUSED  {error}")?;
                    return Ok(ExitCode::from(2));
                }
            };
            writeln!(
                out,
                "wrote {} — {} capabilities",
                path.display(),
                held.capabilities()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Request::Check => {
            let findings = match catalog::check(root, &held) {
                Ok(findings) => findings,
                Err(error) => {
                    writeln!(err, "REFUSED  {error}")?;
                    return Ok(ExitCode::from(2));
                }
            };
            let current = match catalog::document_is_current(root, &held) {
                Ok(current) => current,
                Err(error) => {
                    writeln!(err, "REFUSED  {error}")?;
                    return Ok(ExitCode::from(2));
                }
            };
            if findings.is_empty() && current {
                writeln!(
                    out,
                    "OK  {} — {} capabilities, every owner resolves, {} is current",
                    root.display(),
                    held.capabilities(),
                    catalog::DOC_PATH
                )?;
                return Ok(ExitCode::SUCCESS);
            }
            writeln!(
                err,
                "REFUSED  {} — {} catalog finding(s)",
                root.display(),
                findings.len()
            )?;
            for finding in &findings {
                writeln!(err, "  {finding}")?;
            }
            if !current {
                writeln!(
                    err,
                    "  {} is not a fresh render of {}: run `lgwks-deps catalog --write`",
                    catalog::DOC_PATH,
                    catalog::CATALOG_PATH
                )?;
            }
            Ok(ExitCode::from(2))
        }
    }
}

/// Parses the arguments, runs the command, and reports its exit code.
fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    let (request, root) = match parse(&arguments) {
        Ok(parsed) => parsed,
        Err(message) => {
            if writeln!(err, "lgwks-deps catalog: {message}").is_err() {
                return ExitCode::from(2);
            }
            return ExitCode::from(2);
        }
    };
    match run(&request, &root, &mut out, &mut err) {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            if writeln!(err, "lgwks-deps catalog: {error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(2)
        }
    }
}

/// The command's own tests.
#[cfg(test)]
mod tests {
    use super::{Request, parse};
    use std::error::Error;
    use std::path::PathBuf;

    /// A test's result type.
    type TestResult = Result<(), Box<dyn Error>>;

    /// The bare command prints the table at the working directory.
    #[test]
    fn the_default_is_the_table_at_the_working_directory() -> TestResult {
        let (request, root) = parse(&[])?;
        assert!(matches!(request, Request::Table));
        assert_eq!(root, PathBuf::from("."));
        Ok(())
    }

    /// A mode and a root are read in either order, and two modes are refused.
    #[test]
    fn one_mode_and_one_root_are_accepted_and_two_modes_are_refused() -> TestResult {
        let arguments = vec!["--json".to_owned(), "some/repo".to_owned()];
        let (request, root) = parse(&arguments)?;
        assert!(matches!(request, Request::Json));
        assert_eq!(root, PathBuf::from("some/repo"));
        let twice = vec!["--json".to_owned(), "--check".to_owned()];
        assert!(parse(&twice).is_err(), "two modes name two different runs");
        let unknown = vec!["--jsno".to_owned()];
        assert!(parse(&unknown).is_err(), "an unknown option is refused");
        Ok(())
    }
}
