//! `check` against copied trees: the adoption posture and the lgwks_ast edge
//! ceiling, each driven on a scratch copy of this repository.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    Outcome, Scratch, TestResult, argument, assert_exit, check_from, register_with_violations,
    workspace_root,
};

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

/// Takes the tree lock, recovering from poisoning.
///
/// A panic in one test must not make every later test in this file refuse to
/// run. The guarded state is a `()` that serialises the tree copy, and the
/// copy is restored by its own guard's `Drop` however the holder died, so the
/// poison flag carries no information the next holder needs: what it may not do
/// is inherit the flag and refuse, because the state behind it is intact by
/// construction.
fn lock_tree() -> std::sync::MutexGuard<'static, ()> {
    match ast_manifest_lock().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
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
            "license = \"MIT OR Apache-2.0\"\n",
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
