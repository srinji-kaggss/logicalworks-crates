//! `lgwks-deps` is the human-facing half of INV-DEP-EDGE-OWNED: the same audit
//! the build script runs, runnable by hand, plus the two commands that make the
//! admission process a path rather than a folk practice.
//!
//! This binary is a doctor, not an authority. `check` diagnoses, `request`
//! prints a block for a human to fill in, and `init` writes a fail-closed
//! starting register. None of them can approve anything — approval is a diff
//! with a name on it, which is the whole point of the contract.
//!
//! `vendor check` is the physical counterpart: the register says which edges
//! are owned, the vendor tree says which bytes the offline build resolves,
//! and the subcommand proves the lockfile is fully covered by the tree.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::{
    CONTRACT_PATH, Refusal, check_dependencies, check_dependencies_against, contract::Contract,
    repository_root,
};

const USAGE: &str = "\
lgwks-deps — dependency admission for the std+ estate

USAGE
  lgwks-deps check [PATH]              audit the repo at PATH (default: cwd)
             [--contract FILE]         read the register from FILE instead of
                                       PATH/contract/APPROVED.toml. Diagnosis
                                       only — a build always reads the register
                                       committed beside the code it builds.
  lgwks-deps request <CRATE> <VERSION> print an approval block to fill in
  lgwks-deps init [PATH]               write a fail-closed starting register
   lgwks-deps tiers                     print the admission ladder
   lgwks-deps freshness [PATH]          check resolved deps against crates.io
              [--json]                  output as JSON instead of a table
   lgwks-deps vendor check [PATH]       prove every locked package resolves
                                        to the shared vendor tree

EXIT
   0  every authored external edge has an admitted semantic owner
   2  a refusal, a missing register, or an unparseable one
";

fn parse_check_args(args: &[String]) -> (Option<PathBuf>, Option<PathBuf>) {
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let override_path = args
        .iter()
        .position(|a| a == "--contract")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let target_path = positional.first().map(|p| PathBuf::from(p.as_str()));
    (target_path, override_path)
}

fn handle_check(args: &[String]) -> ExitCode {
    let (target_path, override_path) = parse_check_args(args);
    run_check(target_path, override_path)
}

fn handle_help() -> ExitCode {
    print!("{USAGE}");
    ExitCode::SUCCESS
}

fn handle_tiers() -> ExitCode {
    print!("{LADDER}");
    ExitCode::SUCCESS
}

fn handle_unknown(other: &str) -> ExitCode {
    eprintln!("lgwks-deps: unknown command {other:?}\n");
    eprint!("{USAGE}");
    ExitCode::from(2)
}

fn dispatch(command: &str, args: &[String]) -> ExitCode {
    match command {
        "check" => handle_check(&args[1..]),
        "request" => run_request(args.get(1), args.get(2)),
        "init" => run_init(args.get(1).map(PathBuf::from)),
        "tiers" => handle_tiers(),
        "freshness" => handle_freshness(&args[1..]),
        "vendor" => handle_vendor(&args[1..]),
        "-h" | "--help" | "help" => handle_help(),
        other => handle_unknown(other),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    dispatch(command, &args)
}

// ── check ───────────────────────────────────────────────────────────────────

fn audit_root(
    root: &Path,
    contract_override: &Option<PathBuf>,
) -> Result<(Contract, Vec<Refusal>), String> {
    let outcome = match contract_override {
        Some(path) => check_dependencies_against(root, path),
        None => check_dependencies(root),
    };
    outcome.map_err(|e| e.to_string())
}

fn report_refusals(root: &Path, register: &Contract, refusals: &[Refusal]) -> ExitCode {
    eprintln!(
        "REFUSED  {} — {} dependency-edge violations\n",
        root.display(),
        refusals.len()
    );
    for refusal in refusals {
        eprintln!("  {refusal}");
    }
    eprintln!(
        "\nEach one is a decision, not a paperwork step. Climb the ladder first \
         (`lgwks-deps tiers`);\nif the answer is still a dependency, \
         `lgwks-deps request <crate> <version>` prints the block."
    );
    if !register.enforce {
        eprintln!("\nNOTE  [policy] enforce = false, so builds still pass. This is adoption-only.");
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

fn report_ok(root: &Path, count: usize) -> ExitCode {
    println!(
        "OK  {} — {} semantic approvals, every authored external edge is owned",
        root.display(),
        count
    );
    ExitCode::SUCCESS
}

fn run_check(path: Option<PathBuf>, contract_override: Option<PathBuf>) -> ExitCode {
    let start = path.unwrap_or_else(|| PathBuf::from("."));
    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(e) => return refuse(&e.to_string()),
    };
    let (register, refusals) = match audit_root(&root, &contract_override) {
        Ok(outcome) => outcome,
        Err(err_msg) => return refuse(&err_msg),
    };

    if refusals.is_empty() {
        report_ok(&root, register.entries.len())
    } else {
        report_refusals(&root, &register, &refusals)
    }
}

fn refuse(message: &str) -> ExitCode {
    eprintln!("REFUSED  {message}");
    ExitCode::from(2)
}

// ── request ─────────────────────────────────────────────────────────────────

fn print_request_template(krate: &str, version: &str) {
    print!("{LADDER}");
    println!(
        "\n\
         If every rung above still leaves a dependency, append this to {CONTRACT_PATH},\n\
         fill in the blanks, and commit it. The commit is the approval.\n\n\
         [[approved]]\n\
         crate = \"{krate}\"\n\
         tier = \"boundary\"          # boundary | vendor — see the ladder above\n\
         version = \"{version}\"\n\
         owner = \"\"                 # workspace crate responsible for the capability\n\
         capability = \"\"            # stable semantic capability name\n\
         source = \"registry\"        # registry | git | path\n\
         allowed_consumers = \"\"     # comma-separated workspace crate names\n\
         allowed_kinds = \"normal\"   # normal, build, and/or dev\n\
         reason = \"\"                # one sentence naming what std cannot do\n\
         approved_by = \"\"           # the human who decided\n\
         approved_on = \"\"           # YYYY-MM-DD\n\
         review = \"\"                # path or URL to the evidence\n"
    );
}

fn run_request(krate: Option<&String>, version: Option<&String>) -> ExitCode {
    let (Some(krate), Some(version)) = (krate, version) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    print_request_template(krate, version);
    ExitCode::SUCCESS
}

// ── init ────────────────────────────────────────────────────────────────────

const STARTER: &str = "\
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

fn check_target_exists(target: &Path) -> Result<(), String> {
    if target.exists() {
        Err(format!(
            "{} already exists; init will not overwrite it",
            target.display()
        ))
    } else {
        Ok(())
    }
}

fn create_parent_dirs(target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))
    } else {
        Ok(())
    }
}

fn prepare_init_file(target: &Path) -> Result<(), String> {
    check_target_exists(target)?;
    create_parent_dirs(target)?;
    std::fs::write(target, STARTER).map_err(|e| format!("cannot write {}: {e}", target.display()))
}

fn run_init(path: Option<PathBuf>) -> ExitCode {
    let start = path.unwrap_or_else(|| PathBuf::from("."));
    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(e) => return refuse(&e.to_string()),
    };
    let target = root.join(CONTRACT_PATH);
    if let Err(msg) = prepare_init_file(&target) {
        return refuse(&msg);
    }
    println!("OK  initialized {}", target.display());
    ExitCode::SUCCESS
}

// ── freshness ──────────────────────────────────────────────────────────────

fn handle_freshness(args: &[String]) -> ExitCode {
    let json_output = args.iter().any(|a| a == "--json");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let start = positional
        .first()
        .map(|p| PathBuf::from(p.as_str()))
        .unwrap_or_else(|| PathBuf::from("."));

    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(e) => return refuse(&e.to_string()),
    };

    let lock_path = root.join("Cargo.lock");
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(t) => t,
        Err(e) => return refuse(&format!("cannot read {}: {e}", lock_path.display())),
    };

    let resolved = match lgwks_deps::lock::parse(&lock_text) {
        Ok(r) => r,
        Err(e) => return refuse(&format!("Cargo.lock: {e}")),
    };

    let registry: Vec<&lgwks_deps::lock::Resolved> = resolved.iter().filter(|p| !p.local).collect();

    if registry.is_empty() {
        println!("no registry dependencies in Cargo.lock");
        return ExitCode::SUCCESS;
    }

    let results = query_crates_io(&registry);

    if json_output {
        print_freshness_json(&results);
    } else {
        print_freshness_table(&results);
    }

    let stale_count = results.iter().filter(|r| r.stale).count();
    if stale_count > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

struct FreshnessResult {
    name: String,
    resolved: String,
    latest: String,
    repository: String,
    stale: bool,
    error: Option<String>,
}

fn query_crates_io(packages: &[&lgwks_deps::lock::Resolved]) -> Vec<FreshnessResult> {
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();

    for pkg in packages {
        if !seen.insert(&pkg.name) {
            continue;
        }

        let output = std::process::Command::new("curl")
            .args([
                "-sf",
                "--max-time",
                "10",
                "-H",
                "User-Agent: lgwks-deps/0.1 (srinji@logicalworks.ca)",
                &format!("https://crates.io/api/v1/crates/{}", pkg.name),
            ])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let body = String::from_utf8_lossy(&o.stdout);
                let (latest, repo) = parse_crate_response(&body);
                let stale = !latest.is_empty() && latest != pkg.version;
                results.push(FreshnessResult {
                    name: pkg.name.clone(),
                    resolved: pkg.version.clone(),
                    latest,
                    repository: repo,
                    stale,
                    error: None,
                });
            }
            Ok(o) => {
                results.push(FreshnessResult {
                    name: pkg.name.clone(),
                    resolved: pkg.version.clone(),
                    latest: String::new(),
                    repository: String::new(),
                    stale: false,
                    error: Some(format!("HTTP {}", o.status)),
                });
            }
            Err(e) => {
                results.push(FreshnessResult {
                    name: pkg.name.clone(),
                    resolved: pkg.version.clone(),
                    latest: String::new(),
                    repository: String::new(),
                    stale: false,
                    error: Some(e.to_string()),
                });
            }
        }
    }
    results
}

/// INV-GATE-ZERO-DEPS: no JSON parser — extract fields by line scan.
fn parse_crate_response(body: &str) -> (String, String) {
    let newest = extract_json_string(body, "newest_version");
    let repo = extract_json_string(body, "repository");
    (newest, repo)
}

fn extract_json_string(body: &str, key: &str) -> String {
    let needle = format!("\"{}\":\"", key);
    let alt_needle = format!("\"{}\": \"", key);
    let start = body
        .find(&needle)
        .map(|i| i + needle.len())
        .or_else(|| body.find(&alt_needle).map(|i| i + alt_needle.len()));
    match start {
        Some(s) => body[s..]
            .find('"')
            .map(|e| body[s..s + e].to_string())
            .unwrap_or_default(),
        None => String::new(),
    }
}

// Each label is a column value; the format string owns the alignment.
#[allow(clippy::print_literal)]
fn print_freshness_table(results: &[FreshnessResult]) {
    println!(
        "{:<30} {:<12} {:<12} {:<5} {}",
        "crate", "resolved", "latest", "stale", "repository"
    );
    println!("{}", "-".repeat(90));
    for r in results {
        if let Some(err) = &r.error {
            println!(
                "{:<30} {:<12} {:<12} {:<5} {}",
                r.name, r.resolved, "?", "err", err
            );
        } else {
            let stale_mark = if r.stale { "YES" } else { "" };
            println!(
                "{:<30} {:<12} {:<12} {:<5} {}",
                r.name, r.resolved, r.latest, stale_mark, r.repository
            );
        }
    }
    let stale_count = results.iter().filter(|r| r.stale).count();
    let err_count = results.iter().filter(|r| r.error.is_some()).count();
    println!(
        "\n{} checked, {} stale, {} errors",
        results.len(),
        stale_count,
        err_count
    );
}

fn print_freshness_json(results: &[FreshnessResult]) {
    println!("[");
    for (i, r) in results.iter().enumerate() {
        let comma = if i + 1 < results.len() { "," } else { "" };
        let error_field = match &r.error {
            Some(e) => format!(", \"error\": \"{}\"", e.replace('"', "\\\"")),
            None => String::new(),
        };
        println!(
            "  {{\"name\": \"{}\", \"resolved\": \"{}\", \"latest\": \"{}\", \
             \"stale\": {}, \"repository\": \"{}\"{}}}{}",
            r.name, r.resolved, r.latest, r.stale, r.repository, error_field, comma
        );
    }
    println!("]");
}

// ── vendor ──────────────────────────────────────────────────────────────────

fn handle_vendor(args: &[String]) -> ExitCode {
    if args.first().map(String::as_str) != Some("check") {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    let positional: Vec<&String> = args[1..].iter().filter(|a| !a.starts_with("--")).collect();
    let start = positional
        .first()
        .map(|p| PathBuf::from(p.as_str()))
        .unwrap_or_else(|| PathBuf::from("."));
    run_vendor_check(&start)
}

fn run_vendor_check(start: &Path) -> ExitCode {
    let root = match lgwks_deps::repository_root(start) {
        Ok(root) => root,
        Err(e) => return refuse(&e.to_string()),
    };
    let tree = match lgwks_deps::vendor::tree_for(&root) {
        Ok(tree) => tree,
        Err(e) => return refuse(&e.to_string()),
    };
    let lock_path = root.join("Cargo.lock");
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(e) => return refuse(&format!("cannot read {}: {e}", lock_path.display())),
    };
    match lgwks_deps::vendor::check_coverage(&lock_text, &tree) {
        Ok(report) if report.missing.is_empty() => {
            println!(
                "OK  {} — {} locked packages covered by {}, {} local skipped",
                root.display(),
                report.covered,
                tree.display(),
                report.skipped_local
            );
            ExitCode::SUCCESS
        }
        Ok(report) => {
            eprintln!(
                "REFUSED  {} — {} of {} locked packages missing from {}\n",
                root.display(),
                report.missing.len(),
                report.covered + report.missing.len(),
                tree.display()
            );
            for missing in &report.missing {
                eprintln!("  {} {}", missing.name, missing.version);
            }
            eprintln!("\nRe-run the vendor sync for this repo, then re-check.");
            ExitCode::from(2)
        }
        Err(e) => refuse(&e.to_string()),
    }
}

// ── Ladder text ─────────────────────────────────────────────────────────────
const LADDER: &str = "\
The std+ admission ladder (INV-DEP-EDGE-OWNED)

Every direct dependency authored in a workspace manifest must be accounted for.
Transitive closure remains lockfile provenance, not package-level authority.
Lower rungs are preferred; each step up is an escalation that requires a reason.

  1. Rust standard library (std / core / alloc)
     Preferred unconditionally. Zero dependencies, zero supply-chain risk.

  2. Workspace stdlib+ (`lgwks_std`)
     The common substrate: id (uuid v4), hex, time (RFC 3339), glob, fs, leb128, task.
     The estate facade owns its audited external implementations behind one API.

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
