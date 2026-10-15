//! Unit tests for the invariant register: registration validity, reference
//! resolution's negative and positive controls, and evidence binding against
//! a throwaway Git history.

use std::error::Error;

use super::resolve::{declares_active_test, declares_module};
use super::*;

type TestResult = Result<(), Box<dyn Error>>;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn members() -> Vec<crate::metadata::Member> {
    vec![
        crate::metadata::Member {
            name: "lgwks_bot".to_owned(),
            manifest_dir: workspace_root().join("crates/lgwks-bot"),
        },
        crate::metadata::Member {
            name: "lgwks_deps".to_owned(),
            manifest_dir: workspace_root().join("crates/lgwks-deps"),
        },
    ]
}

fn entry(id: &str, scope: &str, enforced_by: &str) -> String {
    format!(
        "[[invariant]]\n\
         id = \"{id}\"\n\
         statement = \"The implementation preserves this reviewed contract.\"\n\
         scope = \"{scope}\"\n\
         owner = \"{scope}\"\n\
         enforcement = \"static-check\"\n\
         enforced_by = \"{enforced_by}\"\n\
         approved_by = \"maintainer\"\n\
         approved_on = \"2026-09-20\"\n\
         review = \"crates/lgwks-deps/src/invariants.rs\"\n"
    )
}

fn audit_text(text: &str) -> Result<Audit, ErrorKind> {
    let register = Register::parse(text)?;
    Ok(audit(&register, &workspace_root(), &members()))
}

fn refusals(text: &str) -> Result<Vec<Refusal>, ErrorKind> {
    Ok(audit_text(text)?.refusals)
}

// ── Registration validity ───────────────────────────────────────────────

#[test]
fn an_enforcement_kind_outside_the_grammar_is_refused_at_load() {
    for kind in ["review", "intent", "static_check", "Monitor", ""] {
        let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used").replace(
            "enforcement = \"static-check\"",
            &format!("enforcement = \"{kind}\""),
        );
        assert!(
            matches!(
                Register::parse(&text),
                Err(ErrorKind::UnsupportedEnforcement { .. })
            ),
            "enforcement {kind:?} is outside the closed grammar"
        );
    }
}

#[test]
fn an_absolute_or_parent_relative_enforced_by_is_refused_at_load() {
    for spelling in ["/etc/passwd", "../outside/check.rs", "a/../../b.rs"] {
        let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", spelling);
        assert!(
            matches!(
                Register::parse(&text),
                Err(ErrorKind::EnforcerNotConfined { .. })
            ),
            "enforced_by {spelling:?} must not be readable as a reference"
        );
    }
}

#[test]
fn an_empty_evidence_triple_is_admitted_but_a_broken_one_is_refused() -> TestResult {
    let complete = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used");
    assert!(
        Register::parse(&complete)?.entries[0].evidence.is_none(),
        "a static check without a recorded run is a valid entry"
    );
    let partial = format!("{complete}evidence_revision = \"deadbeef\"\n");
    assert!(matches!(
        Register::parse(&partial),
        Err(ErrorKind::EvidenceIncomplete {
            missing: "evidence_invocation",
            ..
        })
    ));
    Ok(())
}

#[test]
fn evidence_must_name_a_revision_the_enforcer_and_a_pass() -> TestResult {
    let base = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used");
    let with = |revision: &str, invocation: &str, result: &str| {
        format!(
            "{base}evidence_revision = \"{revision}\"\n\
             evidence_invocation = \"{invocation}\"\n\
             evidence_result = \"{result}\"\n"
        )
    };
    assert!(matches!(
        Register::parse(&with("abc", "cargo clippy -- clippy::unwrap_used", "pass")),
        Err(ErrorKind::EvidenceRevisionShape { .. })
    ));
    assert!(matches!(
        Register::parse(&with("deadbeef", "cargo clippy", "pass")),
        Err(ErrorKind::EvidenceDoesNotNameEnforcer { .. })
    ));
    assert!(matches!(
        Register::parse(&with(
            "deadbeef",
            "cargo clippy -- clippy::unwrap_used",
            "warn"
        )),
        Err(ErrorKind::EvidenceNotPassing { .. })
    ));
    let register = Register::parse(&with(
        "deadbeef",
        "cargo clippy -- clippy::unwrap_used",
        "pass",
    ))?;
    let evidence = register.entries[0]
        .evidence
        .as_ref()
        .ok_or("the complete triple must be recorded")?;
    assert_eq!(evidence.revision(), "deadbeef");
    assert_eq!(evidence.invocation(), "cargo clippy -- clippy::unwrap_used");
    Ok(())
}

#[test]
fn a_monitor_without_a_recorded_run_is_refused_at_load() {
    let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "clippy::unwrap_used").replace(
        "enforcement = \"static-check\"",
        "enforcement = \"monitor\"",
    );
    assert!(matches!(
        Register::parse(&text),
        Err(ErrorKind::MonitorNeedsEvidence { .. })
    ));
}

// ── Reference resolution: negative controls ─────────────────────────────

/// Control 1: a lint name that cannot exist. The previous audit accepted
/// any `rustc::name` by shape alone, which is the issue's counterexample.
#[test]
fn a_lint_the_repository_does_not_declare_is_refused() -> TestResult {
    let refusals = refusals(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot",
        "rustc::this_lint_does_not_exist",
    ))?;
    assert!(
        matches!(refusals.as_slice(), [Refusal::LintNotDeclared { .. }]),
        "an undeclared lint cannot fire, so it is not enforcement; got {refusals:?}"
    );
    Ok(())
}

/// Control 2: a lint that exists but is declared `allow`.
#[test]
fn a_lint_declared_at_allow_is_refused() -> TestResult {
    // Under `target/`, which is build output rather than source, and
    // rewritten on every run so a leftover fixture cannot change the
    // verdict the way a stale one would.
    let root = workspace_root().join("target/lgwks-deps-lint-fixture");
    std::fs::create_dir_all(&root)?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = []\n\n[workspace.lints.clippy]\nunwrap_used = \"allow\"\n",
    )?;
    let register = Register::parse(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot",
        "clippy::unwrap_used",
    ))?;
    let audit = audit(&register, &root, &members());
    assert!(
        matches!(
            audit.refusals(),
            [Refusal::LintDisabled { level, .. }] if level == "allow"
        ),
        "an allowed lint cannot fail a build; got {:?}",
        audit.refusals()
    );
    Ok(())
}

/// Control 3: a scope naming a module no `mod` item reaches.
#[test]
fn a_scope_naming_a_module_that_does_not_exist_is_refused() -> TestResult {
    let refusals = refusals(&entry(
        "INV-BOT-NEVER-FAILS",
        "lgwks_bot::this_module_does_not_exist",
        "clippy::unwrap_used",
    ))?;
    assert!(
        matches!(refusals.as_slice(), [Refusal::ScopeModuleNotFound { .. }]),
        "the package prefix existing is not the module existing; got {refusals:?}"
    );
    Ok(())
}

/// Control 4: a real file that declares no test — the issue's `README.md`
/// case, with the file chosen from this repository rather than invented.
#[test]
fn a_file_declaring_no_test_is_refused() -> TestResult {
    for spelling in ["README.md", "Cargo.toml", "clippy.toml"] {
        let refusals = refusals(&entry("INV-BOT-FOUR-VERBS", "lgwks_bot", spelling))?;
        assert!(
            matches!(refusals.as_slice(), [Refusal::EnforcerIsNotATest { .. }]),
            "{spelling} exists but nothing in it can fail; got {refusals:?}"
        );
    }
    Ok(())
}

/// Control 5: a file that declares tests, but only ones a run would skip.
#[test]
fn an_ignored_or_unselected_test_is_not_enforcement() -> TestResult {
    assert!(!declares_active_test(
        "#[cfg(test)]\nmod tests {\n    #[test]\n    #[ignore]\n    fn skipped() {}\n}\n"
    ));
    assert!(!declares_active_test(
        "#[test]\n#[cfg(feature = \"never\")]\nfn unselected() {}\n"
    ));
    // The predicate that only *mentions* `test`, and the one that negates
    // it: neither is the test profile, and the second is its opposite.
    assert!(!declares_active_test(
        "#[test]\n#[cfg(not(test))]\nfn inverted() {}\n"
    ));
    assert!(!declares_active_test(
        "#[cfg(feature = \"test\")]\n#[test]\nfn feature_named_test() {}\n"
    ));
    assert!(!declares_active_test(
        "#[test]\n#[cfg(all(test, unix))]\nfn conjunctive() {}\n"
    ));
    assert!(declares_active_test(
        "#[cfg(test)]\n#[test]\nfn explicit_profile() {}\n"
    ));
    assert!(!declares_active_test("pub fn helper() {}\n"));
    assert!(!declares_active_test("#[cfg(test)]\nmod tests {\n}\n"));
    assert!(declares_active_test("#[test]\nfn runs() {}\n"));
    assert!(declares_active_test(
        "#[cfg(test)]\nmod tests {\n    #[test]\n    #[should_panic]\n    fn runs() {}\n}\n"
    ));
    assert!(declares_active_test("#[tokio::test]\nasync fn runs() {}\n"));
    Ok(())
}

/// Control 6: an escape to an existing file outside the repository. The
/// path is rejected at load, so it can never reach the filesystem at all.
#[test]
fn an_external_path_is_refused_before_the_filesystem_is_consulted() {
    let text = entry("INV-BOT-FOUR-VERBS", "lgwks_bot", "/etc/hosts");
    assert!(matches!(
        Register::parse(&text),
        Err(ErrorKind::EnforcerNotConfined {
            reason: PathRefusal::Absolute,
            ..
        })
    ));
}

/// Control 7: a real, passing, unrelated test is not evidence about this
/// statement — the recorded invocation must name the entry's own enforcer.
#[test]
fn a_passing_run_of_something_else_is_not_evidence() {
    let text = format!(
        "{}evidence_revision = \"deadbeef\"\n\
         evidence_invocation = \"cargo test -p lgwks_deps --lib tests::a_valid_register_passes\"\n\
         evidence_result = \"pass\"\n",
        entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-bot/src/verb.rs"
        )
    );
    assert!(matches!(
        Register::parse(&text),
        Err(ErrorKind::EvidenceDoesNotNameEnforcer { .. })
    ));
}

// ── Reference resolution: positive controls ─────────────────────────────

#[test]
fn a_declared_active_lint_resolves() -> TestResult {
    let refusals = refusals(&entry(
        "INV-DEP-EDGE-OWNED",
        "lgwks_deps",
        "clippy::unwrap_used",
    ))?;
    assert!(
        refusals.is_empty(),
        "clippy::unwrap_used is declared at forbid in this workspace; got {refusals:?}"
    );
    Ok(())
}

#[test]
fn a_declared_lint_namespace_is_read_from_the_matching_table() -> TestResult {
    let register = Register::parse(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot",
        "rustc::missing_docs",
    ))?;
    let audit = audit(&register, &workspace_root(), &members());
    assert!(
        audit.refusals().is_empty(),
        "rustc::missing_docs lives in [workspace.lints.rust] at deny; got {:?}",
        audit.refusals()
    );
    Ok(())
}

#[test]
fn a_resolved_entry_without_a_run_is_resolved_not_attested() -> TestResult {
    let audit = audit_text(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot",
        "crates/lgwks-deps/src/invariants/tests.rs",
    ))?;
    assert_eq!(audit.resolved(), 1, "a resolved reference is not a run");
    assert_eq!(audit.attested(), 0);
    assert_eq!(audit.refused(), 0);
    assert_eq!(audit.outcomes()[0].status, Status::Resolved);
    Ok(())
}

/// A throwaway Git repository holding one enforcer file, so evidence can be
/// bound against a history the test controls rather than this checkout's.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    const ENFORCER: &'static str = "tests/guard.rs";

    fn git(&self, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            // The throwaway repository must not inherit the machine's global
            // git config: a global core.hooksPath would run an estate hook here and
            // make a binding test fail for a reason outside this code.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()?;
        if !output.status.success() {
            let refusal: Result<String, Box<dyn std::error::Error>> =
                Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "git: returning an error to the caller");
            return refusal;
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn commit(&self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
        self.git(&["add", "-A"])?;
        self.git(&["commit", "--allow-empty", "-m", message])?;
        self.git(&["rev-parse", "HEAD"])
    }

    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Nanos plus a counter, never a process id: the OS reuses pids, and a
        // reused id would make two runs share one repository.
        static SCRATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lgwks-deps-binding-{name}-{nanos}-{unique}"));
        std::fs::create_dir_all(root.join("tests"))?;
        let scratch = Self { root };
        scratch.git(&["init", "--quiet", "--initial-branch=main"])?;
        std::fs::write(
            scratch.root.join(Self::ENFORCER),
            "#[test]\nfn holds() {}\n",
        )?;
        Ok(scratch)
    }

    fn audit(&self, revision: &str) -> Result<Audit, Box<dyn std::error::Error>> {
        let text = format!(
            "{}evidence_revision = \"{revision}\"\n\
             evidence_invocation = \"cargo test -- {}\"\n\
             evidence_result = \"pass\"\n",
            entry("INV-BOT-FOUR-VERBS", "lgwks_bot", Self::ENFORCER),
            Self::ENFORCER
        );
        let members = [crate::metadata::Member {
            name: "lgwks_bot".to_owned(),
            manifest_dir: self.root.clone(),
        }];
        Ok(audit(&Register::parse(&text)?, &self.root, &members))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            lgwks_std::trace::debug!(%error, "the scratch repository could not be removed");
        }
    }
}

fn only_gap(audit: &Audit) -> Option<&EvidenceGap> {
    match *audit.refusals() {
        [Refusal::EvidenceNotBound { ref gap, .. }] => Some(gap),
        _ => None,
    }
}

/// #258: a run recorded at a real ancestor, with the enforcer untouched since,
/// is the one case that attests.
#[test]
fn a_run_at_an_ancestor_with_an_unchanged_enforcer_is_attested() -> TestResult {
    let scratch = Scratch::new("attested")?;
    let observed = scratch.commit("the run is observed here")?;
    scratch.commit("the register is committed after it")?;
    let audit = scratch.audit(&observed)?;
    assert!(audit.refusals().is_empty(), "{:?}", audit.refusals());
    assert_eq!(audit.attested(), 1);
    assert_eq!(audit.outcomes()[0].status, Status::Attested);
    Ok(())
}

/// #258: the falsifier as filed. A well-shaped revision that names nothing in
/// this repository used to certify any commit; it is now refused.
#[test]
fn a_fabricated_revision_does_not_attest() -> TestResult {
    let scratch = Scratch::new("fabricated")?;
    scratch.commit("work")?;
    let audit = scratch.audit("deadbee")?;
    assert_eq!(only_gap(&audit), Some(&EvidenceGap::UnknownRevision));
    assert_eq!(audit.attested(), 0);
    assert_eq!(audit.outcomes()[0].status, Status::Refused);
    Ok(())
}

/// #258: a real commit that `HEAD` does not descend from is a run of other work.
#[test]
fn a_run_on_a_foreign_branch_does_not_attest() -> TestResult {
    let scratch = Scratch::new("foreign")?;
    scratch.commit("base")?;
    scratch.git(&["checkout", "--quiet", "-b", "elsewhere"])?;
    let foreign = scratch.commit("work nobody merged")?;
    scratch.git(&["checkout", "--quiet", "main"])?;
    scratch.commit("main moves on")?;
    let audit = scratch.audit(&foreign)?;
    assert_eq!(only_gap(&audit), Some(&EvidenceGap::NotInHistory));
    Ok(())
}

/// #258: a true run of an enforcer that has since been edited, committed or not,
/// says nothing about the enforcer as it now stands.
#[test]
fn a_run_before_the_enforcer_changed_does_not_attest() -> TestResult {
    let scratch = Scratch::new("changed")?;
    let observed = scratch.commit("run observed")?;
    std::fs::write(
        scratch.root.join(Scratch::ENFORCER),
        "#[test]\nfn holds() { assert!(true); }\n",
    )?;
    let uncommitted = scratch.audit(&observed)?;
    assert!(
        matches!(
            only_gap(&uncommitted),
            Some(EvidenceGap::EnforcerChanged { .. })
        ),
        "an uncommitted edit counts: {:?}",
        uncommitted.refusals()
    );
    scratch.commit("edit committed")?;
    let committed = scratch.audit(&observed)?;
    assert!(matches!(
        only_gap(&committed),
        Some(EvidenceGap::EnforcerChanged { .. })
    ));
    Ok(())
}

/// #258: where Git cannot answer, the answer is a refusal, never an attestation.
#[test]
fn evidence_that_cannot_be_checked_does_not_attest() -> TestResult {
    let scratch = Scratch::new("no-repo")?;
    scratch.commit("work")?;
    std::fs::remove_dir_all(scratch.root.join(".git"))?;
    let audit = scratch.audit("deadbee")?;
    assert!(
        matches!(only_gap(&audit), Some(EvidenceGap::Unverifiable { .. })),
        "{:?}",
        audit.refusals()
    );
    assert_eq!(audit.attested(), 0);
    Ok(())
}

/// A module scope with a real `mod` behind it resolves.
#[test]
fn a_scope_naming_a_real_module_resolves() -> TestResult {
    let refusals = refusals(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot::verb",
        "crates/lgwks-deps/src/invariants/tests.rs",
    ))?;
    assert!(
        refusals.is_empty(),
        "lib.rs declares `pub mod verb;`; got {refusals:?}"
    );
    Ok(())
}

#[test]
fn a_stray_file_no_mod_reaches_is_not_a_module() -> TestResult {
    assert!(!declares_module("// mod ghost;\n", "ghost"));
    assert!(!declares_module("pub fn ghost() {}\n", "ghost"));
    assert!(declares_module("pub mod ghost;\n", "ghost"));
    assert!(declares_module("pub(crate) mod ghost {}\n", "ghost"));
    assert!(declares_module("#[path = \"x.rs\"] mod ghost;\n", "ghost"));
    Ok(())
}

// ── The pre-existing rules ──────────────────────────────────────────────

#[test]
fn an_invariant_without_enforcement_is_refused() -> TestResult {
    let text = concat!(
        "[[invariant]]\n",
        "id = \"INV-BOT-FOUR-VERBS\"\n",
        "statement = \"The implementation preserves this reviewed contract.\"\n",
        "scope = \"lgwks_bot\"\n",
        "owner = \"lgwks_bot\"\n",
        "enforcement = \"static-check\"\n",
        "approved_by = \"maintainer\"\n",
        "approved_on = \"2026-09-20\"\n",
        "review = \"crates/lgwks-deps/src/invariants.rs\"\n",
    );
    assert!(matches!(
        audit_text(text)?.refusals(),
        [Refusal::MissingEnforcedBy { .. }]
    ));
    Ok(())
}

#[test]
fn an_enforcement_path_that_does_not_exist_is_refused() -> TestResult {
    let refusals = refusals(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot",
        "crates/lgwks-bot/src/does-not-exist.rs",
    ))?;
    assert!(matches!(
        refusals.as_slice(),
        [Refusal::EnforcedByNotFound { .. }]
    ));
    Ok(())
}

#[test]
fn an_invariant_scoped_to_an_unknown_crate_is_refused() -> TestResult {
    let refusals = refusals(&entry(
        "INV-BOGUS-NOT-IN-WORKSPACE",
        "not_a_workspace_crate",
        "clippy::missing_docs",
    ))?;
    assert!(
        refusals
            .iter()
            .any(|refusal| matches!(refusal, Refusal::ScopeNotInWorkspace { .. })),
        "an unknown package must be refused; got {refusals:?}"
    );
    Ok(())
}

#[test]
fn duplicate_ids_are_refused_at_the_second_entry() -> TestResult {
    let text = format!(
        "{}\n{}",
        entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants/tests.rs"
        ),
        entry(
            "INV-BOT-FOUR-VERBS",
            "lgwks_bot",
            "crates/lgwks-deps/src/invariants/tests.rs"
        )
    );
    let refusals = refusals(&text)?;
    assert!(matches!(refusals.as_slice(), [Refusal::DuplicateId { .. }]));
    Ok(())
}

#[test]
fn malformed_ids_are_refused() -> TestResult {
    let refusals = refusals(&entry(
        "bad-invariant-id",
        "lgwks_bot",
        "crates/lgwks-deps/src/invariants/tests.rs",
    ))?;
    assert!(matches!(refusals.as_slice(), [Refusal::MalformedId { .. }]));
    Ok(())
}

/// The rule the issue's own `a_valid_register_passes` test demonstrated was
/// missing: the four-verbs statement was bound to this file, which has no
/// relationship to the bot's verb traits. A valid *reference* is no longer
/// a pass; it is a distinct `Resolved` status, and only a recorded run
/// makes it `Attested`.
#[test]
fn a_valid_register_resolves_and_is_never_called_enforced() -> TestResult {
    let register = Register::parse(&entry(
        "INV-BOT-FOUR-VERBS",
        "lgwks_bot",
        "crates/lgwks-deps/src/invariants/tests.rs",
    ))?;
    let audit = audit(&register, &workspace_root(), &members());
    assert!(audit.refusals().is_empty(), "the entry is well formed");
    assert_eq!(audit.attested(), 0, "nothing was executed for this entry");
    assert_eq!(audit.outcomes()[0].status.as_str(), "resolved");
    Ok(())
}

#[test]
fn missing_register_is_not_a_failure() -> TestResult {
    let root = workspace_root().join("target/lgwks-deps-no-invariant-register");
    assert!(check(&root)?.is_none());
    Ok(())
}
