//! Audits every crate against `std`-first, `lgwks_std`-first and `lgwks_deps`.
//!
//! `AGENTS.md` says the workspace compiles against `std` and `lgwks_std` first
//! and that every other edge is a registered decision. `lgwks-deps check`
//! enforces the manifest half; this reads the source, where two failures live:
//!
//!   * a `use` of a crate that is not a declared dependency of the crate writing
//!     it, a build that works only because something else re-exports it;
//!   * a capability reimplemented by hand that `std` or `lgwks_std` supplies.
//!
//! Findings: `UNDECLARED`, `UNAUTHORIZED`, `STD-FIRST`, `POLICY-ONLY`, plus
//! `UNATTRIBUTED`, `EMPTY` and `STALE EXEMPTION` for an audit that read nothing,
//! and `SURFACE`: a library file declares more `pub` items than
//! `contract/PUBLIC_SURFACE.toml` allows, or the contract allows more than it
//! declares (#526, `surface.rs`), and `EXAMPLE`: a documentation file holds more
//! direct-use examples than the contract's `[examples]` table allows (#526,
//! `examples.rs`; both ratchets are judged by `ratchet.rs`).
//!
//! The whole `use` tree is read, not only its root: `use lgwks_std::json::serde;`
//! binds `serde`, so a later `use serde::de::X;` resolves through `lgwks_std` and
//! reaches no edge at all, which is the rule being followed.
//!
//! Usage: `check-std-first [--justify] [ROOT]`. Exit 0 when nothing is found.
//! `check-std-first --surface-baseline [ROOT]` prints the surface and example
//! contract tables the tree implies, to record a legitimate change.

mod examples;
mod files;
mod lexer;
mod manifest;
mod ratchet;
mod scans;
mod surface;
#[path = "../shared/toml_subset.rs"]
mod toml_subset;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lexer::{Kind, code_only, line_of, module_names, path_reaches, statements, use_tree};
use manifest::Record;
use scans::{
    POLICY_ONLY, policy_only_findings, std_first_hits, storefront_consumers, storefront_only,
};
use toml_subset::{Table, read_tables};

/// The deepest directory nesting the source walk accepts.
pub(crate) const MAX_DEPTH: usize = 64;

/// The standard library, plus the relative-path prefixes every module may write.
const CLEAN_ROOTS: [&str; 7] = [
    "std",
    "core",
    "alloc",
    "proc_macro",
    "crate",
    "self",
    "super",
];

/// The surface that implements the capabilities the `STD-FIRST` patterns are
/// about, so the one place its own reference implementation may live.
const SURFACE_OWNER: &str = "lgwks_std";

/// A line a pattern matches that is not the pattern's subject, pinned by text so
/// a moved line is a `STALE EXEMPTION` rather than a silent yes.
struct Exemption {
    /// The file, relative to the repository root.
    file: &'static str,
    /// The 1-based line.
    line: usize,
    /// The line's stripped text.
    expected: &'static str,
}

/// The audit's record of what a reader looked at and decided. A script line
/// planted to prove the `script!` parser refuses an unowned spawn; it is parsed
/// as data and never compiled, so it spawns nothing.
///
/// The expected text is spelled in two pieces so this file does not contain the
/// pattern it exempts.
const EXEMPT: [Exemption; 1] = [Exemption {
    file: "crates/lgwks-ast/src/script/sim_script.rs",
    line: 703,
    expected: concat!("line: \"let planted = std::thr", "ead::spawn(work)\","),
}];

/// Why the audit could not run.
#[derive(Debug)]
pub(crate) enum AuditError {
    /// A file or directory could not be read.
    Io {
        /// What was being read.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// The source tree nests deeper than the walk accepts.
    TooDeep {
        /// Where the walk stopped.
        path: PathBuf,
    },
    /// A manifest lacks something the audit needs.
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// What is missing.
        missing: &'static str,
    },
}

impl fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "{}: {cause}", path.display()),
            Self::TooDeep { ref path } => {
                write!(
                    formatter,
                    "{}: nested deeper than {MAX_DEPTH} directories",
                    path.display()
                )
            }
            Self::Manifest { ref path, missing } => {
                write!(formatter, "{}: {missing}", path.display())
            }
        }
    }
}

impl std::error::Error for AuditError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
pub(crate) fn fail<T>(error: AuditError, site: &'static str) -> Result<T, AuditError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// `path` relative to `repo`, with `/` separators.
pub(crate) fn posix_relative(repo: &Path, path: &Path) -> String {
    match path.strip_prefix(repo) {
        Ok(relative) => relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

/// Reads a file whole, naming it when it cannot be read.
fn read(path: &Path) -> Result<String, AuditError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            AuditError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// A workspace member: the directory holding it and the crates it declares.
struct Member {
    /// Its directory's final name.
    directory_name: String,
    /// Every crate its manifest declares.
    declared: BTreeSet<String>,
}

/// Workspace member crate name -> the member.
fn workspace(repo: &Path) -> Result<BTreeMap<String, Member>, AuditError> {
    let root_path = repo.join("Cargo.toml");
    let root = read_tables(&read(&root_path)?);
    let mut members = BTreeMap::new();
    for entry in manifest::workspace_members(&root) {
        let directory = repo.join(&entry);
        let member_path = directory.join("Cargo.toml");
        let tables = read_tables(&read(&member_path)?);
        let Some(name) = manifest::crate_name(&tables) else {
            return fail(
                AuditError::Manifest {
                    path: member_path,
                    missing: "no [package] name",
                },
                "workspace",
            );
        };
        let directory_name = match directory.file_name() {
            Some(found) => found.to_string_lossy().into_owned(),
            None => entry.clone(),
        };
        members.insert(
            name,
            Member {
                directory_name,
                declared: manifest::declared_dependencies(&tables),
            },
        );
    }
    Ok(members)
}

/// The register, merged per crate.
fn register(repo: &Path) -> Result<BTreeMap<String, Record>, AuditError> {
    let tables: Vec<Table> = read_tables(&read(&repo.join("contract/APPROVED.toml"))?);
    Ok(manifest::approvals(&tables))
}

/// The text of an optional field, as Python printed it.
fn shown(field: Option<&String>) -> &str {
    match field {
        Some(text) => text,
        None => "None",
    }
}

/// The one-line record behind an edge: who owns it and why it was approved.
fn justification(record: Option<&Record>) -> String {
    match record {
        None => "no record in contract/APPROVED.toml".to_owned(),
        Some(found) => format!(
            "owner={} capability={} tier={} — {}",
            shown(found.owner.as_ref()),
            shown(found.capability.as_ref()),
            shown(found.tier.as_ref()),
            shown(found.reason.as_ref())
        ),
    }
}

/// The exemptions that still describe the line they were granted for. Stale ones
/// become findings, checked up front so a moved line is seen even when no
/// pattern matches the recorded number. Only the checkout the binary was built
/// from carries them: a deliberately failing control elsewhere would otherwise
/// drown in noise.
fn valid_exemptions(
    repo: &Path,
    findings: &mut Vec<String>,
) -> Result<BTreeSet<String>, AuditError> {
    let mut valid = BTreeSet::new();
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let same = match (fs::canonicalize(repo), fs::canonicalize(&home)) {
        (Ok(left), Ok(right)) => left == right,
        (Err(_), _) | (_, Err(_)) => false,
    };
    if !same {
        return Ok(valid);
    }
    for exemption in &EXEMPT {
        let key = format!("{}:{}", exemption.file, exemption.line);
        let target = repo.join(exemption.file);
        if !target.exists() {
            findings.push(format!("{key} — STALE EXEMPTION: no such file"));
            continue;
        }
        let text = read(&target)?;
        let lines: Vec<&str> = text.lines().collect();
        let Some(actual) = exemption
            .line
            .checked_sub(1)
            .and_then(|index| lines.get(index))
        else {
            findings.push(format!(
                "{key} — STALE EXEMPTION: the file has {} line(s)",
                lines.len()
            ));
            continue;
        };
        let actual = actual.trim();
        if actual != exemption.expected {
            let now: String = actual.chars().take(70).collect();
            let was: String = exemption.expected.chars().take(70).collect();
            findings.push(format!(
                "{key} — STALE EXEMPTION: line {} is now `{now}`, not the `{was}` it was granted for",
                exemption.line
            ));
            continue;
        }
        valid.insert(key);
    }
    Ok(valid)
}

/// What the scan of the whole tree learned.
struct Audit {
    /// Every finding.
    findings: Vec<String>,
    /// Crate reached past the clean roots -> the crates that reach it.
    reached: BTreeMap<String, BTreeSet<String>>,
    /// How many source files were read.
    files: usize,
    /// The storefront-only edges.
    owned: BTreeSet<String>,
    /// Each storefront-only edge -> its consuming files.
    consumers: BTreeMap<String, BTreeSet<String>>,
}

/// Scans one crate's files, recording findings and reached edges.
fn scan_crate(
    repo: &Path,
    crate_name: &str,
    paths: &[PathBuf],
    members: &BTreeMap<String, Member>,
    foreign: &BTreeSet<String>,
    exempt: &BTreeSet<String>,
    audit: &mut Audit,
) -> Result<(), AuditError> {
    let declared = match members.get(crate_name) {
        Some(member) => &member.declared,
        None => return Ok(()),
    };
    let mut modules = BTreeSet::new();
    let mut sources = Vec::new();
    for path in paths {
        let text = files::cleaned_text(path)?;
        modules.extend(module_names(&code_only(&text)));
        sources.push((posix_relative(repo, path), text));
    }
    let mut clean: BTreeSet<String> = CLEAN_ROOTS.iter().map(|root| (*root).to_owned()).collect();
    clean.extend(members.keys().cloned());
    clean.extend(modules);
    let mut seen = BTreeSet::new();
    for source in &sources {
        let relative = &source.0;
        let text = &source.1;
        let code = code_only(text);
        let found = statements(&code);
        let mut bindings = BTreeSet::new();
        for statement in &found {
            if let Kind::Use { ref body } = statement.kind {
                bindings.extend(use_tree(body, "").1);
            }
        }
        let mut resolvable: BTreeSet<String> = clean.union(&bindings).cloned().collect();
        resolvable.extend(declared.iter().cloned());
        for name in foreign {
            for offset in path_reaches(&code, name) {
                let number = line_of(&code, offset);
                if declared.contains(name) {
                    audit
                        .reached
                        .entry(name.clone())
                        .or_default()
                        .insert(crate_name.to_owned());
                    continue;
                }
                if resolvable.contains(name) {
                    continue;
                }
                if !seen.insert(format!("{relative}:{number}:{name}")) {
                    continue;
                }
                audit.findings.push(format!(
                    "{relative}:{number} — UNDECLARED: `{name}::` is reached with no `use` and \
                     `{crate_name}` does not declare it"
                ));
            }
        }
        for statement in &found {
            let roots: BTreeSet<String> = match statement.kind {
                Kind::Extern { ref root } => BTreeSet::from([root.clone()]),
                Kind::Use { ref body } => use_tree(body, "").0,
            };
            for root in roots {
                if root.is_empty() || clean.contains(&root) {
                    continue;
                }
                audit
                    .reached
                    .entry(root.clone())
                    .or_default()
                    .insert(crate_name.to_owned());
                if resolvable.contains(&root) {
                    continue;
                }
                audit.findings.push(format!(
                    "{relative}:{} — UNDECLARED: `{root}` is neither a clean root, nor a module \
                     `{crate_name}` declares, nor a name this file binds",
                    line_of(&code, statement.start)
                ));
            }
        }
        if crate_name == SURFACE_OWNER {
            continue;
        }
        let mut hits = std_first_hits(text);
        hits.sort_by_key(|hit| hit.0);
        for hit in hits {
            let key = format!("{relative}:{}", line_of(text, hit.0));
            if exempt.contains(&key) {
                continue;
            }
            audit.findings.push(format!("{key} — STD-FIRST: {}", hit.1));
        }
    }
    Ok(())
}

/// Judges every reached edge against the register, once per edge.
fn register_findings(
    audit: &mut Audit,
    members: &BTreeMap<String, Member>,
    approved: &BTreeMap<String, Record>,
) {
    for (dep, crates) in &audit.reached {
        let consumers: Vec<&String> = crates
            .iter()
            .filter(|name| {
                members
                    .get(*name)
                    .is_some_and(|member| member.declared.contains(dep))
            })
            .collect();
        if consumers.is_empty() {
            continue;
        }
        let Some(record) = approved.get(dep) else {
            audit.findings.push(format!(
                "{consumers:?} declare `{dep}`, which has no record in contract/APPROVED.toml"
            ));
            continue;
        };
        for name in consumers {
            if !record.consumers.contains(name) {
                audit.findings.push(format!(
                    "crates/{name}/Cargo.toml — UNAUTHORIZED: `{dep}` is owned by `{}` and admits \
                     {:?}, not `{name}`",
                    shown(record.owner.as_ref()),
                    record.consumers
                ));
            }
        }
    }
}

/// Runs the whole audit over `repo`.
fn audit_repository(repo: &Path) -> Result<Audit, AuditError> {
    let members = workspace(repo)?;
    let approved = register(repo)?;
    let source = files::source_files(repo)?;
    let by_directory: BTreeMap<String, String> = members
        .iter()
        .map(|(name, member)| (member.directory_name.clone(), name.clone()))
        .collect();
    let mut by_crate: BTreeMap<String, Vec<PathBuf>> = members
        .keys()
        .map(|name| (name.clone(), Vec::new()))
        .collect();
    let mut findings = Vec::new();
    for path in &source {
        match files::owning_crate(repo, path, &by_directory) {
            Some(name) => by_crate.entry(name.clone()).or_default().push(path.clone()),
            None => findings.push(format!(
                "{} — UNATTRIBUTED: no workspace member owns this path",
                posix_relative(repo, path)
            )),
        }
    }
    for (name, member) in &members {
        if by_crate.get(name).is_some_and(Vec::is_empty) {
            findings.push(format!(
                "crates/{} — EMPTY: `{name}` contributed no source file to the audit",
                member.directory_name
            ));
        }
    }
    let exempt = valid_exemptions(repo, &mut findings)?;
    let declared_anywhere: BTreeSet<String> = members
        .values()
        .flat_map(|member| member.declared.iter().cloned())
        .collect();
    let foreign: BTreeSet<String> = approved
        .keys()
        .cloned()
        .chain(declared_anywhere)
        .filter(|name| !members.contains_key(name) && !CLEAN_ROOTS.contains(&name.as_str()))
        .collect();
    let owned = storefront_only(&approved);
    let mut audit = Audit {
        findings,
        reached: BTreeMap::new(),
        files: source.len(),
        owned: owned.clone(),
        consumers: BTreeMap::new(),
    };
    for (name, paths) in &by_crate {
        scan_crate(repo, name, paths, &members, &foreign, &exempt, &mut audit)?;
    }
    register_findings(&mut audit, &members, &approved);
    audit.consumers = storefront_consumers(repo, &source, &owned)?;
    let marker = policy_only_findings(&approved, &audit.consumers);
    audit.findings.extend(marker);
    let surface = surface::findings(repo, &source)?;
    audit.findings.extend(surface);
    let crates: BTreeSet<String> = members.keys().cloned().collect();
    let examples = examples::findings(repo, &source, &crates)?;
    audit.findings.extend(examples);
    Ok(audit)
}

/// The unconsumed storefront-only edges.
fn unconsumed(audit: &Audit) -> Vec<&String> {
    audit
        .owned
        .iter()
        .filter(|name| audit.consumers.get(*name).is_some_and(BTreeSet::is_empty))
        .collect()
}

/// Prints the approval record behind every reached edge.
fn justify(
    out: &mut impl Write,
    audit: &Audit,
    members: &BTreeMap<String, Member>,
    approved: &BTreeMap<String, Record>,
) -> io::Result<()> {
    if audit.reached.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "Crates reached past std / lgwks_std / lgwks_bot / lgwks_ast / lgwks_deps:"
    )?;
    for (dep, crates) in &audit.reached {
        let declaring: Vec<&String> = crates
            .iter()
            .filter(|name| {
                members
                    .get(*name)
                    .is_some_and(|member| member.declared.contains(dep))
            })
            .collect();
        let through: Vec<&String> = crates
            .iter()
            .filter(|name| !declaring.contains(name))
            .collect();
        writeln!(out, "  {dep} — declared by {declaring:?}")?;
        if !through.is_empty() {
            writeln!(out, "    resolved through a surface by {through:?}")?;
        }
        writeln!(out, "    {}", justification(approved.get(dep)))?;
    }
    writeln!(out)?;
    let policy_only = unconsumed(audit);
    let names = if policy_only.is_empty() {
        "none".to_owned()
    } else {
        policy_only
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    writeln!(
        out,
        "Admitted, not implemented ({POLICY_ONLY} {} of {} storefront-only edges):",
        policy_only.len(),
        audit.owned.len()
    )?;
    writeln!(out, "  {names}")?;
    writeln!(out)
}

/// Runs the audit and reports; the exit code is the verdict.
fn execute(
    root: &Path,
    with_justify: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<u8, Box<dyn std::error::Error>> {
    let audit = audit_repository(root)?;
    if with_justify {
        justify(out, &audit, &workspace(root)?, &register(root)?)?;
    }
    if !audit.findings.is_empty() {
        writeln!(err, "{} finding(s):", audit.findings.len())?;
        for finding in &audit.findings {
            writeln!(err, "  {finding}")?;
        }
        return Ok(1);
    }
    let members = workspace(root)?.len();
    let reached: Vec<&str> = audit.reached.keys().map(String::as_str).collect();
    let list = if reached.is_empty() {
        "none".to_owned()
    } else {
        reached.join(", ")
    };
    writeln!(
        out,
        "std-first holds: {} source file(s) across {members} crate(s); {} crate(s) reached past \
         the clean roots ({list}); {} storefront edge(s) marked {POLICY_ONLY}",
        audit.files,
        audit.reached.len(),
        unconsumed(&audit).len()
    )?;
    Ok(0)
}

/// Prints the public-surface contract the tree implies (`--surface-baseline`): the
/// `pub` ceilings, then the doc-example ceilings.
fn record_surface(root: &Path, out: &mut impl Write) -> Result<u8, Box<dyn std::error::Error>> {
    let source = files::source_files(root)?;
    let crates: BTreeSet<String> = workspace(root)?.into_keys().collect();
    surface::baseline(root, &source, out)?;
    examples::baseline(root, &source, &crates, out)?;
    Ok(0)
}

fn main() -> ExitCode {
    let mut root = PathBuf::from(".");
    let mut with_justify = false;
    let mut baseline = false;
    for argument in std::env::args_os().skip(1) {
        if argument == "--justify" {
            with_justify = true;
        } else if argument == "--surface-baseline" {
            baseline = true;
        } else {
            root = PathBuf::from(argument);
        }
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    let verdict = if baseline {
        record_surface(&root, &mut out)
    } else {
        execute(&root, with_justify, &mut out, &mut err)
    };
    match verdict {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if writeln!(err, "{error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::execute;
    use std::error::Error;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn the_repository_holds_std_first() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute(&root, false, &mut out, &mut err)?;
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(
            String::from_utf8_lossy(&out).starts_with("std-first holds:"),
            "the verdict line"
        );
        Ok(())
    }
}
