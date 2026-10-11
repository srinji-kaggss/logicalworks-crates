//! `request`, `init`, `vendor check` and `scan`, and the admission ladder text.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::CONTRACT_PATH;

use crate::{USAGE, read_lock_text, refuse, resolve_repository_root, subject_path};

// ── request ─────────────────────────────────────────────────────────────────

/// Prints the ladder followed by an approval block pre-filled with the crate
/// and version.
///
/// The blank fields are the point: this command writes a template a human must
/// complete and commit, and it deliberately cannot write the register itself.
pub(crate) fn print_request_template(
    krate: &str,
    version: &str,
    out: &mut impl io::Write,
) -> io::Result<()> {
    write!(out, "{LADDER}")?;
    writeln!(
        out,
        "\n\
         If every rung above still leaves a dependency, append this to {CONTRACT_PATH},\n\
         fill in the blanks, and commit it. The commit is the approval.\n\n\
         [[approved]]\n\
         crate = \"{krate}\"\n\
         tier = \"boundary\"          # boundary | vendor — see the ladder above\n\
         version = \"{version}\"\n\
         owner = \"\"                 # workspace crate responsible for the capability\n\
         capability = \"\"            # stable semantic capability name\n\
         license = \"\"              # SPDX expression the package declares\n\
         source = \"registry\"        # registry | git | path\n\
         allowed_consumers = \"\"     # comma-separated workspace crate names\n\
         allowed_kinds = \"normal\"   # normal, build, and/or dev\n\
         reason = \"\"                # one sentence naming what std cannot do\n\
         approved_by = \"\"           # the human who decided\n\
         approved_on = \"\"           # YYYY-MM-DD\n\
         review = \"\"                # path or URL to the evidence\n"
    )
}

/// Prints an approval template, or the usage block when either argument is
/// missing.
///
/// Both arguments are required (a template with no crate name is not useful),
/// so a partial invocation is a usage error on stderr with exit code 2.
pub(crate) fn run_request(
    krate: Option<&String>,
    version: Option<&String>,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let (Some(krate), Some(version)) = (krate, version) else {
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    };
    print_request_template(krate, version, out)?;
    Ok(ExitCode::SUCCESS)
}

// ── init ────────────────────────────────────────────────────────────────────

/// The fail-closed register written by `init`.
///
/// `enforce = true` is the default on purpose: a repository brought onto the
/// gate starts refusing unregistered edges immediately, and standing enforcement
/// down is an explicit, reviewable edit rather than a default someone forgot to
/// change.
pub(crate) const STARTER: &str = "\
# Approved dependency edges — the semantic contract for INV-DEP-EDGE-OWNED.
#
# A crate reaches this file only after the ladder in `lgwks-deps tiers` has been
# climbed and every rung above a dependency was rejected for a stated reason.
# Adding a block here is an approval; the commit that adds it is the signature.
#
# ELIMINATE and CONSOLIDATE crates never appear here. They become a module in
# `lgwks_std` instead, which is why `tier` admits only `boundary` and `vendor`.

[policy]
# Fail-closed. Set to false only while a repo is being brought onto the gate,
# and only in a diff a human signed off — refusals then report as warnings.
enforce = true
# Canonical repository URL. A workspace member declaring a different repository
# is a copied foreign crate and must be consumed as a released dependency.
repository = \"\"
";

/// Refuses when `init` would overwrite an existing register.
///
/// An existing register may carry a human's approvals; `init` never clobbers
/// one. The caller reports the returned message as a refusal.
pub(crate) fn check_target_exists(target: &Path) -> Result<(), String> {
    if target.exists() {
        Err(format!(
            "{} already exists; init will not overwrite it",
            target.display()
        ))
    } else {
        Ok(())
    }
}

/// Creates the register's parent directory if it does not exist.
///
/// A path with no parent (a bare filename) needs no directory and is not an
/// error; `create_dir_all` is idempotent, so an existing directory is fine.
pub(crate) fn create_parent_dirs(target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))
    } else {
        Ok(())
    }
}

/// Writes the starter register, refusing if one is already present.
pub(crate) fn prepare_init_file(target: &Path) -> Result<(), String> {
    check_target_exists(target)?;
    create_parent_dirs(target)?;
    std::fs::write(target, STARTER)
        .map_err(|error| format!("cannot write {}: {error}", target.display()))
}

/// Writes a fail-closed register at the repository root.
pub(crate) fn run_init(
    path: Option<PathBuf>,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let start = subject_path(path.as_deref());
    let root = unwrap_or_refuse!(resolve_repository_root(&start), err);
    let target = root.join(CONTRACT_PATH);
    if let Err(msg) = prepare_init_file(&target) {
        return refuse(&msg, err);
    }
    writeln!(out, "OK  initialized {}", target.display())?;
    Ok(ExitCode::SUCCESS)
}

// ── vendor ──────────────────────────────────────────────────────────────────

/// Handles `vendor check`, rejecting any other `vendor` subcommand.
pub(crate) fn handle_vendor(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if args.first().map(String::as_str) != Some("check") {
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    }
    let positional: Vec<&String> = args[1..]
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .collect();
    let start = subject_path(
        positional
            .first()
            .map(|candidate| Path::new(candidate.as_str())),
    );
    run_vendor_check(&start, out, err)
}

/// Writes the refusal for a lock file the vendor tree does not cover: how many
/// locked packages are missing, each one by name and version, and the repair.
pub(crate) fn write_missing_packages(
    root: &Path,
    tree: &Path,
    report: &lgwks_deps::vendor::Report,
    err: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        err,
        "REFUSED  {} — {} of {} locked packages missing from {}\n",
        root.display(),
        report.missing().len(),
        // Both operands are counts of entries in one lock file, so the
        // sum is bounded by its package count and cannot overflow.
        report.covered().saturating_add(report.missing().len()),
        tree.display()
    )?;
    for missing in report.missing() {
        writeln!(err, "  {} {}", missing.name(), missing.version())?;
    }
    writeln!(
        err,
        "\nRe-run the vendor sync for this repo, then re-check."
    )
}

/// Proves every locked package resolves to the shared vendor tree.
///
/// Exit 0 when the tree covers the lock file and 2 when any locked package is
/// missing from it, since an uncovered package means the offline build would
/// reach the network.
pub(crate) fn run_vendor_check(
    start: &Path,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let root = unwrap_or_refuse!(resolve_repository_root(start), err);
    let tree = match lgwks_deps::vendor::tree_for(&root) {
        Ok(tree) => tree,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let lock_text = unwrap_or_refuse!(read_lock_text(&root), err);
    match lgwks_deps::vendor::check_coverage(&lock_text, &tree) {
        Ok(report) if report.is_complete() => {
            writeln!(
                out,
                "OK  {} — {} locked packages covered by {}, {} local skipped",
                root.display(),
                report.covered(),
                tree.display(),
                report.skipped_local()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(report) => {
            write_missing_packages(&root, &tree, &report, err)?;
            Ok(ExitCode::from(2))
        }
        Err(error) => refuse(&error.to_string(), err),
    }
}

// ── scan ────────────────────────────────────────────────────────────────────

/// Refuses `scan`, naming where it went.
///
/// The zero-gate detectors moved to `lgwks-ast scan` (#408): they read Rust
/// through `lgwks_ast`'s grammar now, and this crate cannot depend on it
/// without a cycle. A caller still running `lgwks-deps scan` gets exit 2 and the
/// command to run instead, never a clean verdict for files nobody scanned.
pub(crate) fn handle_scan(
    _args: &[String],
    _out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    writeln!(
        err,
        "lgwks-deps: `scan` moved to `lgwks-ast scan [PATH]...` (#408); \
         install it with `cargo install lgwks_ast --features tool`"
    )?;
    Ok(ExitCode::from(2))
}

// ── Ladder text ─────────────────────────────────────────────────────────────

/// The admission ladder printed by `tiers` and prefixed to an approval
/// template.
///
/// Read lowest rung first: each step up is an escalation that needs a stated
/// reason, and only the last two rungs (`VENDOR` and `BOUNDARY`) produce a
/// register entry at all.
pub(crate) const LADDER: &str = "\
The core admission ladder (INV-DEP-EDGE-OWNED)

Every direct dependency authored in a workspace manifest must be accounted for.
Transitive closure remains lockfile provenance, not package-level authority.
Lower rungs are preferred; each step up is an escalation that requires a reason.

  1. Rust standard library (std / core / alloc)
     Preferred unconditionally. Zero dependencies, zero supply-chain risk.

  2. Workspace stdlib+ (`lgwks_std`)
     The common substrate: id (uuid v4), hex, time (RFC 3339), glob, fs, leb128, task.
     The workspace facade owns its audited external implementations behind one API.

  3. ELIMINATE
     Crates whose functionality belongs in `lgwks_std` or std.
     Target for removal: write the minimal zero-dependency implementation.

  4. CONSOLIDATE
     Multiple crates solving the same problem.
     Target for convergence: pick one, retire the rest.

  5. VENDOR
     A mature, audit-clean dependency whose code is reviewed and checked into the
     workspace rather than resolved through crates.io at build time.

  6. BOUNDARY
     A third-party dependency approved for use across an external boundary
     (e.g., protocol parsers, hardware drivers, cryptographic primitives).
     Must be declared in `contract/APPROVED.toml` with a human sign-off.
";
