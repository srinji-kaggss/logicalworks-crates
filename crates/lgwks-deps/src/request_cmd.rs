//! The `request` and `init` commands: the approval template a human fills in,
//! and the fail-closed starting register.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::CONTRACT_PATH;

use crate::{LADDER, USAGE, refuse, resolve_repository_root, subject_path};

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
