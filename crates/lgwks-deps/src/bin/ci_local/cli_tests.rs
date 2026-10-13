//! The coordinator driven through its public entry point against fixture trees
//! and controlled commands (issue #127 acceptance).
//!
//! These are not production CI execution and never claim to be: a real full
//! Rust/CI run on the final integrated revision is separate evidence. They cover
//! the cases #127 named: an absent and a non-executable smoke helper, a required
//! checker with a missing prerequisite, a forbidden file already committed, and
//! changed or untracked build inputs, plus the two controls: all prerequisites
//! present, and an explicitly optional platform lane.

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::fixture::Fixture;
use crate::{Console, run};

type TestResult = Result<(), Box<dyn Error>>;

/// A writer whose contents the test reads back.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.0.lock() {
            Ok(mut held) => held.extend_from_slice(bytes),
            Err(poisoned) => poisoned.into_inner().extend_from_slice(bytes),
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    /// Everything written so far.
    fn text(&self) -> String {
        match self.0.lock() {
            Ok(held) => String::from_utf8_lossy(&held).into_owned(),
            Err(poisoned) => String::from_utf8_lossy(&poisoned.into_inner()).into_owned(),
        }
    }
}

/// What a run printed and exited with.
struct Ran {
    /// The exit code.
    code: u8,
    /// Standard output.
    out: String,
    /// Standard error.
    err: String,
}

impl Ran {
    /// Both streams, for a failure message and for assertions that do not care which.
    fn both(&self) -> String {
        format!("{}{}", self.out, self.err)
    }
}

/// Runs the coordinator against `root` with `arguments`.
fn run_cli(arguments: &[&str], root: &Path) -> Ran {
    let out = Buffer::default();
    let err = Buffer::default();
    let console = Console::new(Box::new(out.clone()), Box::new(err.clone()));
    let mut full: Vec<String> = vec!["--root".to_owned(), root.display().to_string()];
    full.extend(arguments.iter().map(|argument| (*argument).to_owned()));
    let code = run(&full, &console);
    Ran {
        code,
        out: out.text(),
        err: err.text(),
    }
}

/// Runs `git` in `root` with a fixed identity and no user configuration.
fn git(root: &Path, arguments: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "gate test")
        .env("GIT_AUTHOR_EMAIL", "gate@example.invalid")
        .env("GIT_COMMITTER_NAME", "gate test")
        .env("GIT_COMMITTER_EMAIL", "gate@example.invalid")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Writes `text` at `relative` under `root`, making directories.
fn put(root: &Path, relative: &str, text: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

/// The lane table of a fixture: a toolchain pin and the given lane blocks.
fn manifest(blocks: &[&str]) -> String {
    format!("[toolchain]\nrust = \"0.0.0\"\n\n{}", blocks.join("\n"))
}

/// Commits everything in `root` with plumbing, so no repository hook runs.
fn commit(root: &Path) -> TestResult {
    git(root, &["add", "-A"])?;
    let tree = git(root, &["write-tree"])?;
    let id = git(root, &["commit-tree", &tree, "-m", "seed"])?;
    git(root, &["update-ref", "refs/heads/main", &id])?;
    Ok(())
}

/// A fixture repository holding `lane_table` and `keep.txt`, committed without hooks.
fn seeded(lane_table: &str) -> Result<Fixture, Box<dyn Error>> {
    let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-ci-local-test")?;
    git(fixture.path(), &["init", "-q", "-b", "main"])?;
    put(fixture.path(), "scripts/gate-lanes.toml", lane_table)?;
    put(fixture.path(), "keep.txt", "ok\n")?;
    commit(fixture.path())?;
    Ok(fixture)
}

/// The receipt a run wrote.
fn receipt_text(root: &Path) -> Result<String, Box<dyn Error>> {
    let entry = fs::read_dir(root.join("evidence"))?
        .flatten()
        .find(|found| found.file_name().to_string_lossy().starts_with("ci-local-"))
        .ok_or("no receipt was written")?;
    Ok(fs::read_to_string(entry.path())?)
}

/// A local lane block.
fn lane(id: &str, extra: &str) -> String {
    format!(
        "[[lane]]\nid = \"{id}\"\nsurfaces = \"local\"\nrequired = true\nfast = true\n{extra}\n"
    )
}

#[test]
fn all_prerequisites_present_is_green() -> TestResult {
    let alpha = "[[lane]]\nid = \"alpha\"\nsurfaces = \"local ci\"\nrequired = true\nfast = true\ncommand = \"true\"\nci_step = \"Alpha\"\n";
    let fixture = seeded(&manifest(&[
        alpha,
        &lane("beta", "builtin = \"artifacts\""),
    ]))?;
    let done = run_cli(&["--fast", "--receipt"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    for wanted in ["passed: 2", "failed: 0", "blocked: 0"] {
        assert!(done.out.contains(wanted), "{wanted} in {}", done.out);
    }
    Ok(())
}

#[test]
fn a_required_lane_with_a_missing_need_is_blocked_not_skipped() -> TestResult {
    let fixture = seeded(&manifest(&[&lane(
        "needs-missing-tool",
        "command = \"true\"\nneeds = [\"bin:definitely-not-a-real-binary\"]",
    )]))?;
    let done = run_cli(&["--fast"], fixture.path());
    assert_ne!(
        done.code, 0,
        "a missing required dependency must not green the gate"
    );
    assert!(done.both().contains("blocked"), "{}", done.both());
    assert!(
        done.both().contains("bin:definitely-not-a-real-binary"),
        "{}",
        done.both()
    );
    Ok(())
}

#[test]
fn an_absent_smoke_helper_blocks_package_smoke() -> TestResult {
    let fixture = seeded(&manifest(&[&lane(
        "package-smoke",
        "command = \"./scripts/lgwks-std-package-smoke.sh\"\nneeds = [\"script:scripts/lgwks-std-package-smoke.sh\"]",
    )]))?;
    let done = run_cli(&["--fast"], fixture.path());
    assert_ne!(done.code, 0);
    assert!(done.both().contains("blocked"), "{}", done.both());
    assert!(
        done.both().contains("lgwks-std-package-smoke.sh"),
        "{}",
        done.both()
    );
    Ok(())
}

#[test]
fn a_non_executable_smoke_helper_blocks_package_smoke() -> TestResult {
    let fixture = seeded(&manifest(&[&lane(
        "package-smoke",
        "command = \"./scripts/lgwks-std-package-smoke.sh\"\nneeds = [\"script:scripts/lgwks-std-package-smoke.sh\"]",
    )]))?;
    let helper = fixture.path().join("scripts/lgwks-std-package-smoke.sh");
    fs::write(&helper, "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o644))?;
    let done = run_cli(&["--fast"], fixture.path());
    assert_ne!(done.code, 0);
    assert!(done.both().contains("blocked"), "{}", done.both());
    Ok(())
}

#[test]
fn an_optional_platform_lane_is_skipped_never_passed() -> TestResult {
    let optional = "[[lane]]\nid = \"gpui-macos\"\nsurfaces = \"local ci\"\nrequired = false\nfast = true\ncommand = \"true\"\nci_step = \"Check normal GPUI renderer feature\"\nplatforms = [\"nonexistent-os\"]\nneeds = [\"bin:xcodebuild\"]\nreason = \"needs the Xcode Metal toolchain\"\n";
    let fixture = seeded(&manifest(&[
        &lane("always", "command = \"true\""),
        optional,
    ]))?;
    let done = run_cli(&["--fast", "--receipt"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    assert!(done.out.contains("gpui-macos"), "{}", done.out);
    assert!(done.out.contains("skipped: 1"), "{}", done.out);
    let text = receipt_text(fixture.path())?;
    assert!(text.contains("gpui-macos"), "{text}");
    assert!(!text.contains("| gpui-macos | **pass**"), "{text}");
    assert!(text.contains("Not covered by this surface"), "{text}");
    Ok(())
}

#[test]
fn a_ci_only_lane_is_named_as_uncovered() -> TestResult {
    let ci_only = "[[lane]]\nid = \"grammar-matrix\"\nsurfaces = \"ci\"\nrequired = true\ncommand = \"true\"\nci_step = \"Execute the lgwks-ast grammar matrix\"\nreason = \"CI runs it as a bash loop\"\n";
    let fixture = seeded(&manifest(&[
        &lane("local-one", "command = \"true\""),
        ci_only,
    ]))?;
    let done = run_cli(&["--fast", "--receipt"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    let text = receipt_text(fixture.path())?;
    assert!(text.contains("Not covered by this surface"), "{text}");
    assert!(text.contains("grammar-matrix"), "{text}");
    assert!(!text.contains("| grammar-matrix | **pass**"), "{text}");
    Ok(())
}

#[test]
fn a_forbidden_file_already_committed_fails() -> TestResult {
    let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-ci-local-test")?;
    git(fixture.path(), &["init", "-q", "-b", "main"])?;
    put(
        fixture.path(),
        "scripts/gate-lanes.toml",
        &manifest(&[&lane("artifacts", "builtin = \"artifacts\"")]),
    )?;
    put(fixture.path(), "target/leaked.rlib", "derived\n")?;
    put(fixture.path(), "keep.txt", "ok\n")?;
    git(fixture.path(), &["add", "-f", "."])?;
    let tree = git(fixture.path(), &["write-tree"])?;
    let id = git(
        fixture.path(),
        &[
            "commit-tree",
            &tree,
            "-m",
            "commit a derived artifact on purpose",
        ],
    )?;
    git(fixture.path(), &["update-ref", "refs/heads/main", &id])?;
    let done = run_cli(&["--fast"], fixture.path());
    assert_ne!(
        done.code, 0,
        "a committed derived artifact must refuse the gate"
    );
    assert!(done.both().contains("fail"), "{}", done.both());
    assert!(
        done.both().contains("target/leaked.rlib"),
        "{}",
        done.both()
    );
    Ok(())
}

#[test]
fn a_staged_but_uncommitted_forbidden_file_fails() -> TestResult {
    let fixture = seeded(&manifest(&[&lane("artifacts", "builtin = \"artifacts\"")]))?;
    put(fixture.path(), "graphify-out/dump.json", "{}\n")?;
    git(fixture.path(), &["add", "-f", "graphify-out/dump.json"])?;
    let done = run_cli(&["--fast"], fixture.path());
    assert_ne!(done.code, 0);
    assert!(
        done.both().contains("graphify-out/dump.json"),
        "{}",
        done.both()
    );
    Ok(())
}

#[test]
fn untracked_build_output_alone_is_not_a_failure_but_is_recorded() -> TestResult {
    let fixture = seeded(&manifest(&[&lane("artifacts", "builtin = \"artifacts\"")]))?;
    put(fixture.path(), "target/scratch", "local build\n")?;
    let done = run_cli(&["--fast", "--receipt"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    let text = receipt_text(fixture.path())?;
    assert!(text.contains("target/scratch"), "{text}");
    assert!(text.contains("UNVERIFIED"), "{text}");
    Ok(())
}

#[test]
fn the_receipt_records_the_full_commit_tree_and_untracked_inputs() -> TestResult {
    let fixture = seeded(&manifest(&[&lane("alpha", "command = \"true\"")]))?;
    put(fixture.path(), "scratch.txt", "untracked\n")?;
    put(fixture.path(), "keep.txt", "changed\n")?;
    let done = run_cli(&["--fast", "--receipt"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    let text = receipt_text(fixture.path())?;
    let head = git(fixture.path(), &["rev-parse", "HEAD"])?;
    let tree = git(fixture.path(), &["rev-parse", "HEAD^{tree}"])?;
    assert!(text.contains(&format!("- commit: `{head}`")), "{text}");
    assert!(text.contains(&format!("- tree: `{tree}`")), "{text}");
    for wanted in [
        "UNVERIFIED",
        "scratch.txt",
        "keep.txt",
        "untracked: `scratch.txt`",
    ] {
        assert!(text.contains(wanted), "{wanted} in {text}");
    }
    assert!(!text.contains("identity: **verified**"), "{text}");
    Ok(())
}

#[test]
fn the_receipt_is_verified_only_from_a_clean_tree() -> TestResult {
    let fixture = seeded(&manifest(&[&lane("alpha", "command = \"true\"")]))?;
    let done = run_cli(&["--fast", "--receipt"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    let text = receipt_text(fixture.path())?;
    assert!(text.contains("identity: **verified**"), "{text}");
    assert!(text.contains("clean index, no untracked files"), "{text}");
    Ok(())
}

/// The repository this binary was built from.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn list_prints_every_lane_and_surface() {
    let done = run_cli(&["--list"], &repo());
    assert_eq!(done.code, 0, "{}", done.both());
    for wanted in ["gate-parity", "grammar-matrix", "appcui-native"] {
        assert!(done.out.contains(wanted), "{wanted} in {}", done.out);
    }
}

#[test]
fn an_unknown_lane_is_a_usage_error() {
    let done = run_cli(&["--lane", "not-a-lane"], &repo());
    assert_eq!(done.code, 2, "{}", done.both());
    assert!(done.err.contains("unknown lane id"), "{}", done.err);
}

#[test]
fn lane_mode_runs_exactly_the_named_lane() -> TestResult {
    let alpha = "[[lane]]\nid = \"alpha\"\nsurfaces = \"local ci\"\nrequired = true\ncommand = \"true\"\nci_step = \"Alpha\"\n";
    let beta =
        "[[lane]]\nid = \"beta\"\nsurfaces = \"local\"\nrequired = true\ncommand = \"false\"\n";
    let fixture = seeded(&manifest(&[alpha, beta]))?;
    let done = run_cli(&["--lane", "alpha"], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    assert!(done.out.contains("alpha: pass"), "{}", done.out);
    let (_, table) = done
        .out
        .rsplit_once('─')
        .ok_or("the run printed no table")?;
    assert!(!table.contains("beta"), "beta must not run: {table}");
    Ok(())
}

#[test]
fn lanes_in_different_groups_are_in_flight_at_the_same_time() -> TestResult {
    let pipes = Fixture::claim(&std::env::temp_dir(), "lgwks-ci-local-pipes")?;
    let (first, second) = (pipes.path().join("first"), pipes.path().join("second"));
    for pipe in [&first, &second] {
        if !Command::new("mkfifo").arg(pipe).status()?.success() {
            return Err("mkfifo failed".into());
        }
    }
    // Each lane leaves a token for the other and waits for the other's: run in
    // series, the first waits out its bound and fails; run together, both end at once.
    let meet = |mine: &Path, theirs: &Path| {
        format!(
            "exec 3<>{mine}; exec 4<>{theirs}; echo go >&3; read -t 20 -u 4 line || exit 7",
            mine = mine.display(),
            theirs = theirs.display()
        )
    };
    let one = lane(
        "rendezvous-one",
        &format!("command = '''{}'''", meet(&first, &second)),
    );
    let two = lane(
        "rendezvous-two",
        &format!("command = '''{}'''", meet(&second, &first)),
    );
    let fixture = seeded(&manifest(&[&one, &two]))?;
    let done = run_cli(&[], fixture.path());
    assert_eq!(done.code, 0, "{}", done.both());
    assert!(done.out.contains("rendezvous-one: pass"), "{}", done.out);
    assert!(done.out.contains("rendezvous-two: pass"), "{}", done.out);
    Ok(())
}

#[test]
fn a_failing_lane_prints_its_own_output() -> TestResult {
    let failing = lane(
        "reason-one",
        "command = \"echo the-reason-it-failed; exit 3\"",
    );
    let passing = lane("reason-two", "command = \"true\"");
    let fixture = seeded(&manifest(&[&failing, &passing]))?;
    let done = run_cli(&[], fixture.path());
    assert_eq!(done.code, 1, "{}", done.both());
    assert!(done.err.contains("reason-one: fail"), "{}", done.err);
    assert!(done.err.contains("the-reason-it-failed"), "{}", done.err);
    assert!(done.out.contains("reason-two: pass"), "{}", done.out);
    Ok(())
}

#[test]
fn a_usage_mistake_names_itself_and_exits_two() {
    let done = run_cli(&["--jobs", "many"], &repo());
    assert_eq!(done.code, 2, "{}", done.both());
    assert!(done.err.contains("--jobs wants a number"), "{}", done.err);
}
