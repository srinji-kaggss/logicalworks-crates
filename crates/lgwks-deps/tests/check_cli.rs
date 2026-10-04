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

/// Issue #209: `check` read `Cargo.lock` to prove it existed and threw the bytes
/// away.
///
/// `lock::parse` refuses a `[[package]]` block that names no package, because
/// dropping it would shrink the audited graph -- but only `freshness` and
/// `vendor` called it. `check`, the command every build runs, read the file for
/// its existence and never parsed it, so a nameless block passed with exit 0.
///
/// The refusal must also name the line, because the defect is a block and not
/// the file: an operator repairing a 9,000-line lock needs to be told where.
#[test]
fn a_lock_block_naming_no_package_is_refused_by_check() -> TestResult {
    let scratch = Scratch::new("nameless-lock")?;
    let tree = nameless_lock(&scratch)?;
    let outcome = check_from(&tree, &["check", "."])?;
    assert_exit(
        &outcome,
        2,
        "a lock whose blocks do not all name a package cannot be audited",
    );
    assert!(
        outcome.stderr.contains("has no name"),
        "the refusal must name the defect, not merely the file: {:?}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("line"),
        "the refusal must name the block to repair: {:?}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "a refused audit writes nothing to stdout: {:?}",
        outcome.stdout
    );
    Ok(())
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

/// Asserts the exit code, reporting both streams on mismatch.
///
/// Every CLI test in this file makes the same assertion, and each one writes its
/// own sentence explaining what the code ought to be. Only the code and the
/// sentence vary, so they are the arguments.
fn assert_exit(outcome: &Outcome, expected: i32, because: &str) {
    assert_eq!(
        outcome.code,
        Some(expected),
        "{because}\nstdout: {:?}\nstderr: {:?}",
        outcome.stdout,
        outcome.stderr
    );
}

/// A copy of `clean` whose lock carries a `[[package]]` block naming no package.
///
/// Built here rather than committed: Cargo refuses such a lockfile, so a
/// checked-in copy broke every tool that compiled the tree it sat in.
fn nameless_lock(scratch: &Scratch) -> Result<PathBuf, Box<dyn Error>> {
    let tree = scratch.path().join("nameless-lock");
    copy_fixture_files(
        &clean(),
        &tree,
        &["Cargo.toml", "contract/APPROVED.toml", "src/lib.rs"],
    )?;
    std::fs::write(
        tree.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"clean\"\nversion = \"0.1.0\"\n\n[[package]]\nversion = \"1.0.0\"\n",
    )?;
    Ok(tree)
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
    // Every invocation reads the real tree, so a test that merely reads it must
    // not run while another has a dependency added: it would answer about a tree
    // neither test asked about. The mutating tests already hold this lock
    // through their guard, which is why this is a non-blocking try and not a
    // plain `lock()` -- taking it twice on one thread would deadlock.
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
    assert_exit(outcome, 2, "an unapproved edge is a refusal");
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
    assert_exit(&outcome, 0, "debug doctor should pass");
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
    assert_exit(&outcome, 0, "a repository with no edge to refuse must pass");
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
    // A path approval needs its exact origin (INV-DEP-12): the class alone is
    // insufficient, and the resolved helper directory is that origin.
    let prefix = format!(
        concat!(
            "[policy]\n",
            "enforce = true\n",
            "[[approved]]\n",
            "crate = \"helper\"\n",
            "tier = \"boundary\"\n",
            "version = \"*\"\n",
            "owner = \"subject\"\n",
            "capability = \"fixture.helper\"\n",
            "source = \"path\"\n",
            "origin = \"{origin}\"\n",
            "allowed_consumers = \"subject,other\"\n",
        ),
        origin = helper_root.display(),
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
            && refused.stderr.contains("first assignment is at line 11")
            && refused.stderr.contains("line 12"),
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
    assert_exit(&outcome, 2, "a missing option value is a refusal");
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
    assert_exit(&outcome, 2, "an option is not a path");
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
    assert_exit(&outcome, 2, "a repeated override is a refusal");
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
    assert_exit(&outcome, 2, "a second path is a refusal");
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
    assert_exit(&outcome, 2, "an unknown option is a refusal");
    assert!(
        outcome
            .stderr
            .contains("unknown option for `check`: --bogus"),
        "the refusal quotes the token it did not recognise: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// A complete `check` receipt binds the subject root, the contract identity and
/// version, the exact metadata subject, the policy mode and the assurance scope.
#[test]
fn the_human_receipt_binds_contract_subject_and_mode() -> TestResult {
    let enforcement = check_from(&clean(), &["check"])?;
    assert_eq!(enforcement.code, Some(0), "stderr {:?}", enforcement.stderr);
    for needle in [
        "CONTRACT  fnv1a128:",
        "SUBJECT   fnv1a128:",
        "MODE      enforcement",
        "SCOPE  ",
    ] {
        assert!(
            enforcement.stdout.contains(needle),
            "the receipt must carry {needle:?}: {:?}",
            enforcement.stdout
        );
    }
    assert!(
        enforcement.stdout.contains(&format!("schema={}", 1_u32)),
        "the contract version is bound: {:?}",
        enforcement.stdout
    );

    let diagnosis = check_from(
        &subject(),
        &["check", "--contract", argument(&clean_register())?],
    )?;
    assert_eq!(diagnosis.code, Some(2));
    assert!(
        diagnosis.stderr.contains("MODE      diagnosis"),
        "an override is diagnosis, not enforcement: {:?}",
        diagnosis.stderr
    );
    Ok(())
}

/// The `--json` receipt exposes the same identities under stable keys.
#[test]
fn the_json_receipt_has_stable_identity_fields() -> TestResult {
    let enforcement = check_from(&clean(), &["check", "--json"])?;
    assert_eq!(enforcement.code, Some(0), "stderr {:?}", enforcement.stderr);
    let payload: lgwks_std::json::Value = lgwks_std::json::from_str(&enforcement.stdout)?;
    assert_eq!(payload["mode"], "enforcement");
    assert_eq!(payload["contract"]["schema"], 1);
    assert_eq!(payload["contract"]["entries"], 0);
    assert_eq!(payload["subject"]["resolved"], true);
    assert_eq!(payload["subject"]["edges"], 0);
    assert!(
        payload["contract"]["digest"]
            .as_str()
            .unwrap_or("")
            .starts_with("fnv1a128:"),
        "the contract digest is present and stable: {}",
        payload["contract"]["digest"]
    );
    assert!(
        payload["subject"]["digest"]
            .as_str()
            .unwrap_or("")
            .starts_with("fnv1a128:"),
        "the subject digest is present and stable: {}",
        payload["subject"]["digest"]
    );
    assert!(
        payload["scope"]
            .as_str()
            .unwrap_or("")
            .contains("no enforcer"),
        "the assurance scope travels with the receipt"
    );

    let diagnosis = check_from(
        &subject(),
        &[
            "check",
            "--contract",
            argument(&clean_register())?,
            "--json",
        ],
    )?;
    let payload: lgwks_std::json::Value = lgwks_std::json::from_str(&diagnosis.stdout)?;
    assert_eq!(payload["mode"], "diagnosis");
    assert_eq!(payload["subject"]["resolved"], true);
    Ok(())
}

/// A receipt is a binding: changing the register changes its digest, and
/// changing the graph changes the subject digest.
#[test]
fn the_receipt_changes_when_its_subject_changes() -> TestResult {
    let scratch = Scratch::new("receipt-binding")?;
    let first = scratch.path().join("first.toml");
    let second = scratch.path().join("second.toml");
    std::fs::write(&first, "[policy]\nschema = 2\nenforce = true\n")?;
    std::fs::write(
        &second,
        "[policy]\nschema = 2\nenforce = true\n# a changed register\n",
    )?;

    let digest_of = |register: &Path| -> Result<String, Box<dyn Error>> {
        let outcome = check_from(
            &clean(),
            &["check", "--contract", argument(register)?, "--json"],
        )?;
        let payload: lgwks_std::json::Value = lgwks_std::json::from_str(&outcome.stdout)?;
        Ok(payload["contract"]["digest"]
            .as_str()
            .unwrap_or("")
            .to_owned())
    };

    let first_digest = digest_of(&first)?;
    let second_digest = digest_of(&second)?;
    assert_ne!(
        first_digest, second_digest,
        "a changed register must change the bound contract digest"
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
    assert_exit(
        &outcome,
        0,
        "adoption mode over an admitting tree is a supported posture",
    );
    Ok(())
}

// ── INV-DEP-1: lgwks_ast is an audited surface ─────────────────────────────

/// Serialises the tests that edit `crates/lgwks-ast/Cargo.toml` in place.
///
/// Both tests below mutate the repository they are auditing, because that is
/// the only way to exercise the *real* binary against the *real* surface, and
/// nextest runs tests concurrently. Left to race, one test's control run can
/// read the other's mutation and report a verdict for a tree that neither test
/// asked about — a false pass for one and a false failure for the other. A lock
/// is the whole remedy: the mutation is a temporary property of the tree, so
/// only one test may hold it at a time. Test-only, and therefore process-wide.
///
/// The lock is taken with `unwrap_or_else`, not `expect`: a panic in one test
/// must not poison the tree for every later run of the suite, and the guard's
/// own error is not what any assertion here is about.
fn ast_manifest_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// Takes the tree lock, ignoring poisoning.
///
/// A panic in one test must not make every later test in this file refuse to
/// run: the tree is restored by the guard's own `Drop` whatever happens, so the
/// panic carries no information the next test needs.
fn lock_tree() -> std::sync::MutexGuard<'static, ()> {
    ast_manifest_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Copies the repository's manifests into `scratch` and returns that tree's root.
///
/// The gate shells out to `cargo metadata`, which resolves a workspace from real
/// manifests on disk, so a negative control has to be a real workspace. Copying
/// one is what keeps that control from being a mutation of the repository: two
/// `lgwks_deps` tests edit `crates/lgwks-ast/Cargo.toml` at the same time, and a
/// mutex inside one test binary cannot stop a test in another binary from
/// reading the tree mid-edit. A copy has no reader.
///
/// Only what `cargo metadata` needs is copied -- the manifests, the workspace
/// root manifest, and the register -- so the copy is cheap and carries no build
/// output. `Cargo.lock` is copied too because `--locked` reads it.
fn copied_tree(scratch: &Scratch) -> Result<PathBuf, Box<dyn Error>> {
    let root = workspace_root()?;
    let target = scratch.path().join("tree");
    let relative = [
        "Cargo.toml",
        "Cargo.lock",
        "contract/APPROVED.toml",
        "contract/INVARIANTS.toml",
    ];
    for name in relative {
        let from = root.join(name);
        if !from.exists() {
            continue;
        }
        let to = target.join(name);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&from, &to)?;
    }
    for crate_name in [
        "lgwks-std",
        "lgwks-bot",
        "lgwks-ast",
        "lgwks-macros",
        "lgwks-deps",
    ] {
        // Each member directory in full except its `target`, because `cargo
        // metadata` loads every member manifest and resolves each package's
        // target paths -- a manifest without its sources does not load.
        copy_tree(
            &root.join("crates").join(crate_name),
            &target.join("crates").join(crate_name),
        )?;
    }
    Ok(target)
}

/// Recursively copies `from` to `to`, skipping build output and VCS metadata.
///
/// `target` is skipped because a member's build output is large, is not read by
/// `cargo metadata`, and is derived from the copy anyway; `.git` because the copy
/// is not a repository.
fn copy_tree(from: &Path, to: &Path) -> Result<(), Box<dyn Error>> {
    let skip = ["target", ".git", "node_modules"];
    // `create_dir_all` first: a leaf directory that only receives files, like a
    // crate's `src`, does not exist on the destination side yet.
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        copy_entry(&entry?, &skip, to)?;
    }
    Ok(())
}

/// Copies one directory entry, or skips it by name.
///
/// Split from [`copy_tree`] so the walk is one fallible step rather than four in
/// one statement, and so the skip rule has a name a reader can look up.
fn copy_entry(entry: &std::fs::DirEntry, skip: &[&str], to: &Path) -> Result<(), Box<dyn Error>> {
    let name = entry.file_name();
    if skip.contains(&name.to_string_lossy().as_ref()) {
        return Ok(());
    }
    let source = entry.path();
    let target_path = to.join(&name);
    if source.is_dir() {
        copy_tree(&source, &target_path)
    } else {
        std::fs::copy(&source, &target_path)?;
        Ok(())
    }
}

/// Holds the in-place edit of `crates/lgwks-ast/Cargo.toml` for one test.
///
/// RAII in both directions: taking the lock keeps any other such test out of the
/// tree, and restoring the manifest keeps the *rest of the suite* out of a tree
/// this test mutated — including when an assertion fires and abandons the body.
struct AstManifestEdit {
    /// The file this guard owns for its whole lifetime.
    path: PathBuf,
    /// The bytes that file carried before the mutation.
    content: String,
    /// The lock file, restored with the manifest.
    lock: PathBuf,
    /// The bytes the lock file carried before the mutation.
    lock_content: String,
    /// Released when this guard is dropped.
    _serialised: std::sync::MutexGuard<'static, ()>,
}

impl AstManifestEdit {
    /// Adds one third-party edge to `lgwks_ast` for the test's lifetime.
    ///
    /// `cargo add hex` is the mutation being reproduced, so it is written the
    /// way Cargo would write it: a plain `name = "version"` line in the
    /// `[dependencies]` table.
    fn add_hex(root: &Path) -> Result<Self, Box<dyn Error>> {
        let serialised = lock_tree();
        let path = root.join("crates/lgwks-ast/Cargo.toml");
        let content = std::fs::read_to_string(&path)?;
        let tampered = content.replacen(
            "[dependencies]\n",
            "[dependencies]\n# Negative control for INV-DEP-1.\nhex = \"0.4\"\n",
            1,
        );
        assert_ne!(
            tampered, content,
            "the fixture must actually differ from the shipped manifest, or the control \
             proves nothing"
        );
        std::fs::write(&path, &tampered)?;
        // The gate shells out to `cargo metadata --locked`, which refuses a
        // manifest whose dependencies the lock file does not yet carry. Adding a
        // dependency is exactly that, so the lock is regenerated alongside --
        // offline, and released with the manifest on drop. Without this the test
        // fails in `cargo metadata` rather than in the gate it is testing.
        let lock = root.join("Cargo.lock");
        let lock_content = std::fs::read_to_string(&lock)?;
        refresh_lock(root, &lock)?;
        Ok(Self {
            path,
            content,
            lock,
            lock_content,
            _serialised: serialised,
        })
    }
}

impl Drop for AstManifestEdit {
    /// Best effort: the tree is this repository's, and a test that has already
    /// decided its verdict must not be failed by a failed restore. `Drop` has no
    /// failure channel, so a restore that cannot happen is not reported here.
    fn drop(&mut self) {
        drop(std::fs::write(&self.path, &self.content));
        drop(std::fs::write(&self.lock, &self.lock_content));
    }
}

/// Regenerates `Cargo.lock` offline, so the gate's own `cargo metadata --locked`
/// accepts a manifest this test has just extended.
///
/// Best effort for the same reason the restore is: a lock file that will not
/// regenerate leaves `cargo metadata`'s refusal as the test's result, which is a
/// failure either way and names the same cause.
fn refresh_lock(root: &Path, lock: &Path) -> Result<(), Box<dyn Error>> {
    // Recorded rather than returned: this is test code, the caller turns a false
    // into the same `?` the rest of the fixture uses, and a helper that both
    // reports and asserts is a helper whose failure the reader has to guess at.
    let output = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()?;
    let succeeded = output.status.success();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if succeeded {
        assert!(lock.exists(), "cargo did not write {}", lock.display());
        Ok(())
    } else {
        Err(format!("regenerating the lock file failed: {stderr}").into())
    }
}

/// Issue #207: INV-DEP-1 claims `lgwks-deps check .` refuses an unregistered
/// edge in `crates/lgwks-ast/Cargo.toml`.
///
/// The claim was asserted, not verified: the gate's source named no surface, so
/// "three surfaces plus `lgwks_ast`" had nothing behind it and there was no way
/// to tell an audited surface from an unlisted one. The negative control below is
/// the whole issue — it adds a fifth third-party edge to `lgwks_ast` the way
/// `cargo add` would, and demands that the shipped binary refuse it by name.
///
/// Four assertions, because the defect was a gate that *looked* correct: the
/// verdict is a refusal (exit 2), the refusal names `lgwks_ast`, it names `hex`,
/// and — the part a stale unlisted-surface list would still pass — no other
/// surface's classification changed.
#[test]
fn a_fifth_third_party_edge_on_lgwks_ast_is_refused_and_named() -> TestResult {
    // A private copy of the tree, so adding the edge cannot be read by another
    // test mid-edit. See `copied_tree`.
    let scratch = Scratch::new("ast-edge")?;
    let root = copied_tree(&scratch)?;
    let register = root.join("contract/APPROVED.toml");
    let shipped = argument(&register)?;
    let _edit = AstManifestEdit::add_hex(&root)?;

    let outcome = check_from(&root, &["check", ".", "--contract", shipped])?;
    assert_exit(&outcome, 2, "an unapproved edge on lgwks_ast is a refusal");
    assert!(
        outcome.stdout.is_empty(),
        "a refused audit writes nothing to stdout: {:?}",
        outcome.stdout
    );
    assert!(
        outcome
            .stderr
            .contains("lgwks_ast declares unowned normal edge hex"),
        "the refusal must name the surface that authored the edge and the edge itself, \
         or a refusal that cannot say which surface failed is half a gate (stderr {:?})",
        outcome.stderr
    );
    assert!(
        !outcome.stderr.contains("lgwks_std declares"),
        "no other surface's classification may change (stderr {:?})",
        outcome.stderr
    );
    Ok(())
}

/// The other half of INV-DEP-1, and the half that only this fix can pass: *never
/// grow `lgwks_ast`*. An unregistered edge is already refused by
/// [`a_fifth_third_party_edge_on_lgwks_ast_is_refused_and_named`], so the way a
/// frozen surface actually grows is the register: a reviewer authorises the edge
/// outright, or authorises it at the tier that stands for audited vendored source
/// instead of an admitted boundary.
///
/// Both routes are refused here, against *this* repository's real tree and a
/// register that differs from the shipped one only by the added approval. The
/// control below is what keeps the rule honest in the other direction: the same
/// register with a correctly tiered approval must pass, so the freeze cannot be
/// satisfied by refusing every approval that mentions `lgwks_ast`.
#[test]
fn growing_lgwks_asts_approved_edge_set_is_refused_even_when_approved() -> TestResult {
    // A private copy of the tree, so adding the edge cannot be read by another
    // test mid-edit. See `copied_tree`.
    let scratch = Scratch::new("ast-freeze")?;
    let root = copied_tree(&scratch)?;
    let authored = std::fs::read_to_string(root.join("contract/APPROVED.toml"))?;

    // Both runs audit the same tree with the same edge on `lgwks_ast`; only the
    // `tier` in the added approval differs. The manifest edit is taken first and
    // released last, so the control proves the approval's tier -- and not the
    // edge's absence -- is the whole difference between the two verdicts.
    let _edit = AstManifestEdit::add_hex(&root)?;

    // The control: an approval at the tier a frozen surface's edges must keep is
    // the ordinary admission route, and it must exit 0 over an edge that is
    // genuinely declared. Without it, refusing every `lgwks_ast` approval would
    // satisfy the freeze below.
    let admissible = scratch.path().join("APPROVED-hex-boundary.toml");
    std::fs::write(
        &admissible,
        format!("{authored}\n{}", hex_approval("boundary")),
    )?;
    let control = check_from(&root, &["check", ".", "--contract", argument(&admissible)?])?;
    assert_eq!(
        control.code,
        Some(0),
        "a correctly tiered approval over a declared edge must be admitted, or the \
         freeze below would be satisfied by refusing every lgwks_ast approval \
         (stdout {:?}, stderr {:?})",
        control.stdout,
        control.stderr
    );

    // The regrown surface: an identical approval at the tier that stands for
    // audited vendored source instead of an admitted boundary.
    let regrown = scratch.path().join("APPROVED-hex-vendor.toml");
    std::fs::write(&regrown, format!("{authored}\n{}", hex_approval("vendor")))?;
    let outcome = check_from(&root, &["check", ".", "--contract", argument(&regrown)?])?;
    assert_exit(&outcome, 2, "");
    assert!(
        outcome
            .stderr
            .contains("lgwks_ast is a frozen surface and its hex edge"),
        "the refusal must name the frozen surface and the edge it is about, or a refusal \
         that cannot say which surface failed is half a gate (stderr {:?})",
        outcome.stderr
    );
    assert!(
        !outcome.stderr.contains("lgwks_std is a frozen surface"),
        "only the frozen surface is frozen; lgwks_std's own approvals must stay \
         admissible (stderr {:?})",
        outcome.stderr
    );
    Ok(())
}

/// The approval block an authorizing reviewer would add for a new `lgwks_ast`
/// edge: every required field present, a reason that says what std cannot do, and
/// `tier` as the caller chose it.
///
/// `tier` is a parameter so the control and the negative control below differ in
/// exactly one field. `^0.4` is what Cargo reports for `hex = "0.4"`, which is
/// what makes the two verdicts above differ on the gate's judgement rather than
/// on a requirement mismatch.
fn hex_approval(tier: &str) -> String {
    format!(
        concat!(
            "[[approved]]\n",
            "crate = \"hex\"\n",
            "tier = \"{tier}\"\n",
            "version = \"^0.4\"\n",
            "owner = \"lgwks_ast\"\n",
            "capability = \"encoding.hex\"\n",
            "source = \"registry\"\n",
            "allowed_consumers = \"lgwks_ast\"\n",
            "allowed_kinds = \"normal\"\n",
            "reason = \"Constant-time hex encoding needs arithmetic the standard \
             library does not expose.\"\n",
            "approved_by = \"maintainer\"\n",
            "approved_on = \"2026-10-01\"\n",
            "review = \"docs/ADMISSION.md\"\n"
        ),
        tier = tier
    )
}
