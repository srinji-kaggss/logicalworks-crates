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

use lgwks_std::{hex, random};

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
    command_from(current_dir, args, &[])
}

/// Runs `lgwks-deps` with `args`, from `current_dir`.
fn command_from(
    current_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<Outcome, Box<dyn Error>> {
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let mut command = Command::new(binary);
    command.args(args).current_dir(current_dir);
    for &(key, value) in envs {
        command.env(key, value);
    }
    let output = command.output()?;
    Ok(Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

/// The repository root containing the command under test.
fn workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "crates/lgwks-deps has no workspace root".into())
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

/// Copies selected fixture files into a disposable workspace tree.
fn copy_fixture_files(source: &Path, target: &Path, files: &[&str]) -> TestResult {
    for file in files {
        let destination = target.join(file);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source.join(file), destination)?;
    }
    Ok(())
}

/// Rewrites one authored consumer field in a copy of the real register so the
/// tree genuinely carries edge violations.
///
/// The register is copied rather than edited in place: the negative control has
/// to inject *real* violations that `audit_direct` finds on its own, and it must
/// not leave this repository's own register in a violating state if the test
/// fails midway. Returns the copied register's path.
fn register_with_violations(
    scratch: &Scratch,
    tag: &str,
    enforce: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let root = workspace_root()?;
    let register = scratch.path().join(format!("APPROVED-{tag}.toml"));
    let authored = std::fs::read_to_string(root.join("contract/APPROVED.toml"))?;
    // Point one entry's consumers at a package that does not exist, which
    // breaks that edge's approval and produces real `ConsumerNotAllowed`
    // refusals plus the `UnusedApproval` they leave behind.
    let violated = authored
        .replace(
            "allowed_consumers = \"lgwks_std\"",
            "allowed_consumers = \"lgwks_bogus_consumer\"",
        )
        .replace("enforce = true", &format!("enforce = {enforce}"));
    assert_ne!(
        violated, authored,
        "the fixture must actually differ from the shipped register, or the control proves nothing"
    );
    std::fs::write(&register, violated)?;
    Ok(register)
}

#[test]
fn debug_reports_default_sdk_bootstrap_in_json() -> TestResult {
    let root = workspace_root()?;
    let outcome = command_from(
        &root,
        &["debug", "--json"],
        &[("LGWKS_LOG", "info"), ("LGWKS_LOG_FORMAT", "json")],
    )?;
    assert_eq!(
        outcome.code,
        Some(0),
        "debug doctor should pass (stdout {:?}, stderr {:?})",
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("debugger installed"),
        "install event should be emitted to stderr: {:?}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("debug doctor completed"),
        "doctor lifecycle event should be emitted to stderr: {:?}",
        outcome.stderr
    );
    let report: lgwks_std::json::Value = lgwks_std::json::from_str(&outcome.stdout)?;
    assert_eq!(report["command"], "debug");
    assert_eq!(report["admitted"], true);
    assert_eq!(report["format"], "json");
    assert_eq!(report["checks"]["default_includes_trace"], true);
    assert_eq!(report["checks"]["trace_includes_tracing_subscriber"], true);
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

/// The original defect's user-visible counterexample and its single-key control.
#[test]
fn conflicting_approval_keys_cannot_admit_a_real_path_edge() -> TestResult {
    let scratch = Scratch::new("duplicate-approval")?;
    let subject_root = scratch.path().join("subject");
    let helper_root = scratch.path().join("helper");
    copy_fixture_files(
        &subject(),
        &subject_root,
        &["Cargo.toml", "Cargo.lock", "src/lib.rs"],
    )?;
    copy_fixture_files(&helper(), &helper_root, &["Cargo.toml", "src/lib.rs"])?;
    let register = scratch.path().join("APPROVED.toml");
    let prefix = concat!(
        "[policy]\n",
        "enforce = true\n",
        "[[approved]]\n",
        "crate = \"helper\"\n",
        "tier = \"boundary\"\n",
        "version = \"*\"\n",
        "owner = \"subject\"\n",
        "capability = \"fixture.helper\"\n",
        "source = \"path\"\n",
        "allowed_consumers = \"subject,other\"\n",
    );
    let suffix = concat!(
        "allowed_kinds = \"normal\"\n",
        "reason = \"This path fixture needs an explicit external package boundary.\"\n",
        "approved_by = \"reviewer\"\n",
        "approved_on = \"2026-09-20\"\n",
        "review = \"tests/check_cli.rs\"\n",
    );
    std::fs::write(&register, format!("{prefix}{suffix}"))?;
    let valid = check_from(
        &subject_root,
        &[
            "check",
            argument(&subject_root)?,
            "--contract",
            argument(&register)?,
        ],
    )?;
    assert_eq!(
        valid.code,
        Some(0),
        "one valid approval assignment must admit the actual edge (stderr {:?})",
        valid.stderr
    );
    assert!(
        valid.stderr.is_empty(),
        "valid control must be quiet: {:?}",
        valid.stderr
    );

    let conflicting = format!("{prefix}allowed_consumers = \"subject\"\n{suffix}");
    std::fs::write(&register, conflicting)?;
    let refused = check_from(
        &subject_root,
        &[
            "check",
            argument(&subject_root)?,
            "--contract",
            argument(&register)?,
        ],
    )?;
    assert_eq!(
        refused.code,
        Some(2),
        "the real check command must refuse a conflicting approval (stdout {:?}, stderr {:?})",
        refused.stdout,
        refused.stderr
    );
    assert!(
        refused.stdout.is_empty(),
        "a refused contract emits no success output: {:?}",
        refused.stdout
    );
    assert!(
        refused
            .stderr
            .contains("duplicate key \"allowed_consumers\"")
            && refused.stderr.contains("first assignment is at line 10")
            && refused.stderr.contains("line 11"),
        "the CLI refusal must name the field and both positions: {:?}",
        refused.stderr
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
    /// Creates `<temp>/lgwks-deps-check-cli-<nonce>-<tag>`, empty.
    ///
    /// The tag is per-test, so two tests never share a directory and a stale
    /// one from an earlier run cannot change a verdict.
    fn new(tag: &str) -> Result<Self, Box<dyn Error>> {
        let nonce = hex::encode(random::bytes::<8>()?);
        let path = std::env::temp_dir().join(format!("lgwks-deps-check-cli-{nonce}-{tag}"));
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

/// One audited tree under one enforcement posture.
struct Posture {
    /// The `enforce` token the copied register carried.
    enforce: &'static str,
    /// What the binary did with it.
    outcome: Outcome,
}

/// Issue #204: `[policy] enforce = false` is a posture, not an off switch.
///
/// The baseline defect was that the gate's entire verdict reduced to that one
/// boolean: a reviewable one-token diff turned 26 real refusals into exit 0.
/// This test drives the shipped binary against *this* repository with real
/// injected violations and asserts the exit code in **both** directions, because
/// either half alone is satisfied by the defect — under the old code
/// `enforce = true` exited 2 and only the `enforce = false` half could fail.
///
/// The negative control demonstrates wrong behaviour rather than a compile
/// failure: the refusals are produced by `audit_direct` finding the injected
/// violation on its own, and the stand-down is refused by name.
#[test]
fn adoption_mode_cannot_stand_down_a_tree_that_carries_violations() -> TestResult {
    let root = workspace_root()?;
    let scratch = Scratch::new("adoption-mode")?;

    // Both runs audit the same real tree through the same real binary; only the
    // `enforce` token in a copied register differs between them.
    let mut runs = Vec::new();
    for (tag, enforce) in [("enforced", "true"), ("adoption", "false")] {
        let register = register_with_violations(&scratch, tag, enforce)?;
        let outcome = check_from(&root, &["check", ".", "--contract", argument(&register)?])?;
        runs.push(Posture { enforce, outcome });
    }

    for run in &runs {
        assert_eq!(
            run.outcome.code,
            Some(2),
            "enforce = {} with real injected violations must exit 2, not 0 (stderr {:?})",
            run.enforce,
            run.outcome.stderr
        );
    }

    // The refusal that distinguishes the two runs has to be named, so a rename
    // cannot satisfy the test, and it must name the count it stood down.
    let adoption = runs
        .iter()
        .find(|run| run.enforce == "false")
        .ok_or("the adoption-mode run was not collected")?;
    assert!(
        adoption.outcome.stderr.contains("adoption mode"),
        "an adoption-mode stand-down must be refused in its own words, distinct from \
         the edge refusals it counted (stderr {:?})",
        adoption.outcome.stderr
    );

    // The control only means something if the violations are real. Assert the
    // edge refusals themselves were found, so a register that failed to mutate
    // cannot quietly produce the same exit code for the wrong reason.
    assert!(
        adoption
            .outcome
            .stderr
            .contains("dependency-edge violations"),
        "the run must actually have found edge violations (stderr {:?})",
        adoption.outcome.stderr
    );
    Ok(())
}

/// The other half of #204, and the direction that keeps adoption mode
/// meaningful: on a tree with no refusals, `enforce = false` is still a
/// legitimate posture and must exit 0.
///
/// Without this, the fix above would be satisfied by refusing every register
/// that says `enforce = false`, which would delete adoption mode rather than
/// removing its power to launder a violating tree.
#[test]
fn adoption_mode_over_a_clean_tree_still_passes() -> TestResult {
    let root = workspace_root()?;
    let scratch = Scratch::new("adoption-clean")?;
    // The real register, copied and only re-posted under the adoption token.
    let register = scratch.path().join("APPROVED-adoption.toml");
    let authored = std::fs::read_to_string(root.join("contract/APPROVED.toml"))?;
    std::fs::write(
        &register,
        authored.replace("enforce = true", "enforce = false"),
    )?;
    let outcome = check_from(&root, &["check", ".", "--contract", argument(&register)?])?;
    assert_eq!(
        outcome.code,
        Some(0),
        "adoption mode over an admitting tree is a supported posture (stdout {:?}, stderr {:?})",
        outcome.stdout,
        outcome.stderr
    );
    Ok(())
}
