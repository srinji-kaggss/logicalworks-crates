//! Issue #701: a consumer of the estate's own crates is admitted, and a
//! third-party edge beside them is still refused.
//!
//! The defect was end to end, so the fixtures here are end to end: a real
//! three-file crate whose manifest declares published `lgwks_*` edges, audited
//! by the built binary the way a consumer runs it — `init` first, then `check`.
//! Before the fix, `init` wrote a fail-closed register that refused the estate's
//! own crates, and the only form on offer for `lgwks_bot` was the third-party
//! `boundary` block.
//!
//! Each test writes its own tree under the OS temp root, so no test can change
//! another's verdict, and `argv` carries no path the caller had to guess.

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use lgwks_std::{hex, random};

/// What every test here returns.
///
/// The workspace forbids `unwrap`/`expect` outright, with no test exemption, so
/// a test propagates its failure with `?` instead of aborting the run.
type TestResult = Result<(), Box<dyn Error>>;

/// One invocation's outcome.
struct Outcome {
    /// Process exit code, or `None` when a signal ended the process.
    code: Option<i32>,
    /// What the command wrote to stdout.
    stdout: String,
    /// What the command wrote to stderr.
    stderr: String,
}

/// A scratch tree under the OS temp root, removed when the test ends.
///
/// RAII rather than a cleanup call at the end of the test body: the assertions
/// that fail are exactly the runs that must not leave the tree behind.
struct Scratch {
    /// The directory this guard owns.
    path: PathBuf,
}

impl Scratch {
    /// Creates `<temp>/lgwks-deps-estate-<nonce>-<tag>`, empty.
    ///
    /// The tag is per-test, so two tests never share a directory and a stale one
    /// from an earlier run cannot change a verdict.
    fn new(tag: &str) -> Result<Self, Box<dyn Error>> {
        let nonce = hex::encode(random::bytes::<8>()?);
        let path = std::env::temp_dir().join(format!("lgwks-deps-estate-{nonce}-{tag}"));
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

/// Runs the built `lgwks-deps` binary in `current_dir`.
///
/// `CARGO_NET_OFFLINE` is set for every call: the fixture declares registry
/// edges that this test never builds, so the gate's second `cargo metadata` read
/// (the one that carries licences) must answer "offline" instead of reaching
/// crates.io. The verdict under test comes from the first read, which resolves
/// no dependency at all.
fn run(current_dir: &Path, args: &[&str]) -> Result<Outcome, Box<dyn Error>> {
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let output = Command::new(binary)
        .args(args)
        .current_dir(current_dir)
        .env("CARGO_NET_OFFLINE", "true")
        .output()?;
    Ok(Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

/// Writes a consumer crate whose manifest declares exactly `edges`.
///
/// Three files: the manifest, a lock file with no resolved package (the declared
/// read resolves none), and a library source. Nothing else is read, so the tree
/// says precisely what the test means and no more.
fn consumer(
    scratch: &Scratch,
    name: &str,
    edges: &[(&str, &str)],
) -> Result<PathBuf, Box<dyn Error>> {
    let root = scratch.path().join(name);
    std::fs::create_dir_all(root.join("src"))?;
    let mut manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
    );
    for &(krate, requirement) in edges {
        writeln!(manifest, "{krate} = \"{requirement}\"")?;
    }
    std::fs::write(root.join("Cargo.toml"), manifest)?;
    std::fs::write(root.join("Cargo.lock"), "version = 4\n")?;
    std::fs::write(
        root.join("src/lib.rs"),
        "//! A consumer of the estate's published crates.\n",
    )?;
    Ok(root)
}

/// Reads `check --json`'s `admitted` flag, or names the defect that hid it.
///
/// The receipt shape is contractual, and the whole issue is decided by this one
/// field, so a document that omits it is a failure rather than an assertion
/// about a missing value read as false.
fn admitted(outcome: &Outcome) -> Result<bool, Box<dyn Error>> {
    let receipt: lgwks_std::json::Value = lgwks_std::json::from_str(&outcome.stdout)?;
    receipt["admitted"].as_bool().ok_or_else(|| {
        format!(
            "the receipt carries no `admitted` flag: {:?}",
            outcome.stdout
        )
        .into()
    })
}

/// The crate names the receipt refused.
fn refused_crates(outcome: &Outcome) -> Result<Vec<String>, Box<dyn Error>> {
    let receipt: lgwks_std::json::Value = lgwks_std::json::from_str(&outcome.stdout)?;
    let rows = receipt["refusals"].as_array().ok_or_else(|| {
        format!(
            "the receipt carries no refusals array: {:?}",
            outcome.stdout
        )
    })?;
    let mut names = Vec::with_capacity(rows.len());
    for row in rows {
        let krate = row["crate"]
            .as_str()
            .ok_or_else(|| format!("a refusal names no crate: {row:?}"))?;
        names.push(krate.to_owned());
    }
    names.sort();
    Ok(names)
}

/// The issue's own reproduction, now green: two estate edges only.
///
/// `init` writes a register the consumer has nothing to fill in — no
/// `[[approved]]` block, no `approved_by`, no `reason` — and `check` on that
/// register admits the tree. Both halves are asserted, because either one alone
/// is satisfied by a gate that stopped refusing everything.
#[test]
fn a_consumer_of_the_estate_crates_is_admitted_by_init_then_check() -> TestResult {
    let scratch = Scratch::new("estate-only")?;
    let root = consumer(
        &scratch,
        "estate_consumer",
        &[("lgwks_std", "3"), ("lgwks_bot", "3")],
    )?;

    let init = run(&root, &["init", "."])?;
    assert_eq!(
        init.code,
        Some(0),
        "init writes a starting register (stdout {:?}, stderr {:?})",
        init.stdout,
        init.stderr
    );
    let register = std::fs::read_to_string(root.join("contract/APPROVED.toml"))?;
    for blank in ["[[approved]]", "approved_by", "reason ="] {
        assert!(
            !register.contains(blank),
            "init must leave nothing for a consumer to hand-fill for an estate crate, \
             but the register carries {blank:?}: {register:?}"
        );
    }

    let check = run(&root, &["check", ".", "--json"])?;
    assert_eq!(
        check.code,
        Some(0),
        "a consumer whose only edges are estate crates is admitted (stdout {:?}, stderr {:?})",
        check.stdout,
        check.stderr
    );
    assert!(
        admitted(&check)?,
        "the receipt must say the tree is admitted: {:?}",
        check.stdout
    );
    assert_eq!(
        refused_crates(&check)?,
        Vec::<String>::new(),
        "the gate refuses nothing on a two-estate-edge tree: {:?}",
        check.stdout
    );

    // The same tree audited again reaches the same verdict, so `check` stays a
    // pure read of tree plus register rather than a state machine.
    let again = run(&root, &["check", ".", "--json"])?;
    assert_eq!(
        again.code,
        Some(0),
        "a second audit of an unchanged tree is admitted again (stderr {:?})",
        again.stderr
    );
    Ok(())
}

/// The other half: an estate consumer that also reaches a third-party crate is
/// still refused, and only for that crate.
///
/// `rusqlite` is the genuine boundary edge in the issue's reproduction
/// (`logical_ci`), so it is the negative control: the fix must not turn the gate
/// off for anything that is not the estate's own.
#[test]
fn a_third_party_edge_beside_the_estate_ones_is_still_refused() -> TestResult {
    let scratch = Scratch::new("estate-plus-third-party")?;
    let root = consumer(
        &scratch,
        "mixed_consumer",
        &[("lgwks_std", "3"), ("lgwks_bot", "3"), ("rusqlite", "0.32")],
    )?;

    let init = run(&root, &["init", "."])?;
    assert_eq!(
        init.code,
        Some(0),
        "init writes a starting register (stderr {:?})",
        init.stderr
    );

    let check = run(&root, &["check", ".", "--json"])?;
    assert_eq!(
        check.code,
        Some(2),
        "an unapproved third-party edge is a refusal (stdout {:?}, stderr {:?})",
        check.stdout,
        check.stderr
    );
    assert!(
        !admitted(&check)?,
        "the receipt must not admit a tree with an unowned third-party edge: {:?}",
        check.stdout
    );
    assert_eq!(
        refused_crates(&check)?,
        vec!["rusqlite".to_owned()],
        "check refuses only the third-party edge, never the estate ones: {:?}",
        check.stdout
    );
    Ok(())
}

/// `request` takes the tier from the ladder, so it refuses to print a
/// third-party approval form for the estate's own crate.
#[test]
fn request_refuses_to_print_a_boundary_form_for_an_estate_crate() -> TestResult {
    let scratch = Scratch::new("request-estate")?;
    let outcome = run(scratch.path(), &["request", "lgwks_bot", "3.1.0"])?;

    assert_eq!(
        outcome.code,
        Some(2),
        "requesting an estate crate is refused, not answered with a form (stdout {:?})",
        outcome.stdout
    );
    assert!(
        outcome.stdout.is_empty(),
        "no boundary form may be printed for an estate crate: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stderr.contains("lgwks_bot") && outcome.stderr.contains("rung 2"),
        "the refusal names the crate and the rung that admits it: {:?}",
        outcome.stderr
    );
    assert!(
        !outcome.stderr.contains("tier = \"boundary\""),
        "a refusal is not a template: {:?}",
        outcome.stderr
    );
    Ok(())
}

/// The control for the test above: a third-party crate still gets its form.
#[test]
fn request_still_prints_the_boundary_form_for_a_third_party_crate() -> TestResult {
    let scratch = Scratch::new("request-third-party")?;
    let outcome = run(scratch.path(), &["request", "rusqlite", "0.32.0"])?;

    assert_eq!(
        outcome.code,
        Some(0),
        "a third-party crate still gets the form to fill in (stderr {:?})",
        outcome.stderr
    );
    assert!(
        outcome.stdout.contains("crate = \"rusqlite\"")
            && outcome.stdout.contains("tier = \"boundary\""),
        "the printed block names the crate at the boundary tier: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("approved_by = \"\"") && outcome.stdout.contains("reason = \"\""),
        "the block still carries the blank fields a human must fill and commit: {:?}",
        outcome.stdout
    );
    Ok(())
}
