//! `check`'s argument grammar, exercised against two real repositories.
//!
//! A parser tuple is not the unit under test here. The defect these tests pin
//! was only observable end to end: `--contract`'s value was read as the
//! positional target, root discovery then climbed to the directory holding the
//! *register*, and the gate returned a success verdict for a repository nobody
//! asked about. Every test below therefore asserts three things together — the
//! exit code, the repository named in the verdict, and the edge that was
//! refused — because any one of them alone is satisfied by the defect.
//!
//! The fixture pair is `tests/fixtures/contract-override`:
//!
//! - `clean/` authors no dependency edge and approves nothing, so auditing it
//!   succeeds. It is the tree that used to be audited by mistake.
//! - `subject/` authors one path edge to the sibling `helper/` package outside
//!   its workspace, with no approval anywhere, so auditing it must refuse and
//!   name `helper`. It is the tree the operator asked about.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What every test here returns.
///
/// The workspace forbids `unwrap`/`expect` outright, with no test exemption, so
/// a test propagates its failure with `?` instead of aborting the run.
type TestResult = Result<(), Box<dyn Error>>;

/// The fixture pair's root.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract-override")
}

/// The repository with no edge to refuse.
fn clean() -> PathBuf {
    fixtures().join("clean")
}

/// The repository whose edge must be refused.
fn subject() -> PathBuf {
    fixtures().join("subject")
}

/// The unapproved dependency `subject` authors.
fn helper() -> PathBuf {
    fixtures().join("helper")
}

/// `clean`'s register, which the override tests point at.
fn clean_register() -> PathBuf {
    clean().join("contract/APPROVED.toml")
}

/// One CLI invocation's outcome.
struct Outcome {
    /// Process exit code, or `None` when a signal ended the process.
    code: Option<i32>,
    /// What the command wrote to stdout.
    stdout: String,
    /// What the command wrote to stderr.
    stderr: String,
}

/// A fixture path as the CLI must receive it.
///
/// A non-UTF-8 fixture path would make the argument list wrong rather than the
/// code under test, so it is reported instead of lossily converted.
fn argument(path: &Path) -> Result<&str, Box<dyn Error>> {
    path.to_str()
        .ok_or_else(|| format!("fixture path is not UTF-8: {}", path.display()).into())
}

/// Runs `lgwks-deps check` with `args`, from `current_dir`.
fn check_from(current_dir: &Path, args: &[&str]) -> Result<Outcome, Box<dyn Error>> {
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let output = Command::new(binary)
        .args(args)
        .current_dir(current_dir)
        .output()?;
    Ok(Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

/// Asserts the run refused `subject` for its unapproved edge to `helper`.
///
/// The three assertions are deliberately inseparable. Exit 2 alone is also
/// what a refusal about the *wrong* repository produces, and a refusal that
/// names no edge is satisfied by any refusal at all.
fn assert_refused_subject(outcome: &Outcome) -> TestResult {
    let refused_edge = format!(
        "subject declares unowned normal edge helper * from path:{}",
        helper().display()
    );
    assert_eq!(
        outcome.code,
        Some(2),
        "an unapproved edge is a refusal (stdout {:?}, stderr {:?})",
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "a refused audit writes nothing to stdout: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.contains(&refused_edge),
        "the verdict must name subject's edge to helper, not some other tree: {:?}",
        outcome.stderr
    );
    assert!(
        !outcome.stderr.contains(clean().to_string_lossy().as_ref()),
        "the verdict must not be about the clean repository: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// The control for every test below.
///
/// Without it, "everything is refused" would satisfy the refusals, and the
/// fixture pair would prove nothing about *which* repository was audited.
#[test]
fn auditing_the_clean_repository_succeeds() -> TestResult {
    let outcome = check_from(&fixtures(), &["check", argument(&clean())?])?;
    assert_eq!(
        outcome.code,
        Some(0),
        "a repository with no edge to refuse must pass (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome.stderr.is_empty(),
        "a passing audit writes nothing to stderr: {:?}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.contains("semantic approvals"),
        "the success line names the approvals it checked: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains(clean().to_string_lossy().as_ref()),
        "the success line names the audited root: {:?}",
        outcome.stdout
    );
    Ok(())
}

/// The defect's primary counterexample: the option's value must not become the
/// target, and an omitted `PATH` must keep meaning the working directory.
#[test]
fn an_omitted_target_audits_the_working_directory_not_the_register() -> TestResult {
    let outcome = check_from(
        &subject(),
        &["check", "--contract", argument(&clean_register())?],
    )?;
    assert_refused_subject(&outcome)
}

/// The same three invocations the issue lists, all of which must inspect
/// `subject`: the override before the target, the override after it, and the
/// target omitted in favour of the working directory.
#[test]
fn the_override_and_the_target_are_the_same_audit_in_any_order() -> TestResult {
    // Bound rather than temporary: the argument list borrows the path strings
    // for as long as the table below lives.
    let root = fixtures();
    let target_path = subject();
    let target = argument(&target_path)?;
    let register_path = clean_register();
    let register = argument(&register_path)?;
    let orderings: [(&[&str], &Path); 3] = [
        (&["check", "--contract", register, target], &root),
        (&["check", target, "--contract", register], &root),
        (&["check", "--contract", register], &target_path),
    ];
    for (args, current_dir) in orderings {
        let outcome = check_from(current_dir, args)?;
        assert_refused_subject(&outcome)?;
    }
    Ok(())
}

/// JSON mode carries the same verdict as the human rendering: same exit code,
/// same refused edge, and a root that identifies `subject`.
#[test]
fn json_mode_names_the_audited_root_and_the_refused_edge() -> TestResult {
    let outcome = check_from(
        &subject(),
        &[
            "check",
            "--contract",
            argument(&clean_register())?,
            "--json",
        ],
    )?;
    assert_eq!(
        outcome.code,
        Some(2),
        "JSON mode carries the same verdict as the table: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.is_empty(),
        "JSON mode writes nothing to stderr, including for a refusal: {:?}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.contains("\"admitted\": false"),
        "the refusal is admitted: false, not merely absent: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"crate\": \"helper\""),
        "the refused edge is named by crate: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"root\": \".\""),
        "an omitted target resolves to the working directory, not to the register: {:?}",
        outcome.stdout
    );
    Ok(())
}

/// A relative target and an absolute one are the same repository: the verdict
/// is about the tree the operator named in both spellings, and the two runs
/// agree on the edge they refuse.
#[test]
fn a_relative_target_resolves_to_the_same_repository() -> TestResult {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let relative = check_from(
        manifest_dir,
        &["check", "tests/fixtures/contract-override/subject"],
    )?;
    assert_refused_subject(&relative)?;
    let absolute = check_from(manifest_dir, &["check", argument(&subject())?])?;
    assert_refused_subject(&absolute)?;
    // The edge is the identity of the audited repository: both spellings must
    // report the same unapproved dependency, resolved to the same path.
    let refused_edge = format!("from path:{}", helper().display());
    for (spelling, outcome) in [("relative", &relative), ("absolute", &absolute)] {
        assert!(
            outcome.stderr.contains(&refused_edge),
            "the {spelling} target must resolve to the same edge: {:?}",
            outcome.stderr
        );
        assert!(
            outcome
                .stderr
                .contains("subject — 1 dependency-edge violations"),
            "the {spelling} target must be judged as subject: {:?}",
            outcome.stderr
        );
    }
    Ok(())
}

/// A register held outside any Cargo repository is still just a register: it
/// is read, and it never becomes the target.
#[test]
fn a_register_outside_any_cargo_repository_is_read_but_never_audited() -> TestResult {
    let scratch = Scratch::new("outside")?;
    let register = scratch.path().join("APPROVED.toml");
    std::fs::write(&register, "[policy]\nenforce = true\n")?;
    let outcome = check_from(
        scratch.path(),
        &[
            "check",
            argument(&subject())?,
            "--contract",
            argument(&register)?,
        ],
    )?;
    assert_refused_subject(&outcome)?;
    // With the target omitted, the working directory is the target, and a
    // scratch directory has no lock file above it: the gate refuses rather
    // than falling back to whatever repository the register happens to sit in
    // (it sits in none).
    let unmapped = check_from(
        scratch.path(),
        &["check", "--contract", argument(&register)?],
    )?;
    assert_eq!(
        unmapped.code,
        Some(2),
        "a target with no repository must refuse (stderr {:?})",
        unmapped.stderr
    );
    assert!(
        unmapped.stderr.contains("no Cargo.lock"),
        "the refusal names the missing lock file: {:?}",
        unmapped.stderr
    );
    Ok(())
}

/// A `--contract` with no value is refused before anything is audited, rather
/// than being read as a target or silently ignored.
#[test]
fn a_contract_without_a_value_is_refused_before_any_audit() -> TestResult {
    let outcome = check_from(&clean(), &["check", "--contract"])?;
    assert_eq!(
        outcome.code,
        Some(2),
        "a missing option value is a refusal (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "no repository may be audited by an invocation that was refused: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.contains("--contract needs a value"),
        "the refusal names the option and what it needs: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// `--contract --json` is a missing value, not a register named `--json`.
#[test]
fn an_option_as_a_contract_value_is_a_missing_value() -> TestResult {
    let outcome = check_from(&clean(), &["check", "--contract", "--json"])?;
    assert_eq!(
        outcome.code,
        Some(2),
        "an option is not a path (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("which is another option"),
        "the refusal says why the token is not a value: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// Two registers are two policies; picking one by position would make the
/// verdict depend on argument order.
#[test]
fn a_repeated_contract_override_is_refused() -> TestResult {
    let root = fixtures();
    let register_path = clean_register();
    let register = argument(&register_path)?;
    let outcome = check_from(
        &root,
        &[
            "check",
            argument(&subject())?,
            "--contract",
            register,
            "--contract",
            register,
        ],
    )?;
    assert_eq!(
        outcome.code,
        Some(2),
        "a repeated override is a refusal (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("more than once"),
        "the refusal names the repetition: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// `check` audits one repository. A second path is refused rather than
/// silently dropped, which is how a mistyped target used to be ignored.
#[test]
fn a_surplus_positional_is_refused() -> TestResult {
    let outcome = check_from(
        &fixtures(),
        &["check", argument(&clean())?, argument(&subject())?],
    )?;
    assert_eq!(
        outcome.code,
        Some(2),
        "a second path is a refusal (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("is a second path"),
        "the refusal names the surplus argument: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// An unrecognised flag is refused rather than ignored.
#[test]
fn an_unknown_flag_is_refused() -> TestResult {
    let outcome = check_from(&fixtures(), &["check", "--bogus", argument(&clean())?])?;
    assert_eq!(
        outcome.code,
        Some(2),
        "an unknown option is a refusal (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("unknown option for `check`: --bogus"),
        "the refusal quotes the token it did not recognise: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// A scratch directory under the OS temp root, removed when the test ends.
///
/// RAII rather than a cleanup call at the end of the test body: the assertions
/// that fail are exactly the runs that must not leave the directory behind.
struct Scratch {
    /// The directory this guard owns.
    path: PathBuf,
}

impl Scratch {
    /// Creates `<temp>/lgwks-deps-check-cli-<pid>-<tag>`, empty.
    ///
    /// The tag is per-test, so two tests never share a directory and a stale
    /// one from an earlier run cannot change a verdict.
    fn new(tag: &str) -> Result<Self, Box<dyn Error>> {
        let path =
            std::env::temp_dir().join(format!("lgwks-deps-check-cli-{}-{tag}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The directory this guard owns.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: the tree is this process's own, below the OS temp root,
        // and a leftover directory is not a reason to fail a test that has
        // already decided its verdict.
        if std::fs::remove_dir_all(&self.path).is_err() {
            // Nothing to report here; `Drop` has no failure channel.
        }
    }
}
