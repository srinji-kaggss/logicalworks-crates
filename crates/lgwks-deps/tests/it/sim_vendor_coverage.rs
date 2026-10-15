//! Lockfile-to-tree coverage, as a deterministic simulation over real trees.
//!
//! INV-VENDOR-SINGLE-TREE says every registry package a `Cargo.lock` resolves
//! is in the vendor tree with the exact bytes the lock pins. The binding is the
//! `.cargo-checksum.json` `"package"` hash that `cargo vendor` writes beside
//! each crate (cargo `src/cargo/ops/vendor.rs`; the directory source verifies
//! the same file, `src/cargo/sources/directory.rs`). Cargo serializes that
//! object with sorted keys, so the `"files"` map, which names every file in the
//! crate, comes *before* the crate's own hash.
//!
//! One seed draws a lockfile of workspace, registry and git packages and the
//! tree that vendors it: directory names, checksum spellings (compact, pretty,
//! key order, escaped file names) and file maps all vary. The reader must
//! report exactly the drawn model; each scenario then damages one thing — a
//! removed directory, changed bytes, a git manifest at another version, a
//! hashless directory, a crate that ships a file named `package` — and the
//! report must change by exactly that. Every scenario runs twice per seed, on
//! disk, and the two traces must hash alike.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lgwks_deps::vendor::{self, Report, VendorError};

use crate::replay::{Failure, refuse, replay_sweep};
use crate::sim::{Rng, Trace};

/// Every scenario's result.
type TestResult = Result<(), Failure>;

/// Seeds per scenario: each seed writes and removes a whole tree, so the sweep
/// is narrower than the pure parser families'.
const SEEDS: u64 = 128;

/// The registry source `Cargo.lock` writes for crates.io.
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// Every scenario here, over the same seeds.
fn sweep(name: &str, body: impl FnMut(&mut Rng, &mut Trace) -> TestResult) -> TestResult {
    replay_sweep(name, SEEDS, body)
}

/// Where a drawn package came from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// A workspace member: no source, nothing vendored.
    Local,
    /// A registry crate pinned by its `.crate` sha256.
    Registry(String),
    /// A git crate, pinned by revision and matched by manifest identity.
    Git,
}

/// One drawn package and how the tree vendors it.
#[derive(Debug, Clone)]
struct Package {
    /// Unique package name.
    name: String,
    /// Resolved version.
    version: String,
    /// Source kind and pin.
    kind: Kind,
    /// Directory name in the tree; the check must not depend on it.
    dir: String,
}

/// How one `.cargo-checksum.json` is spelled.
#[derive(Debug, Clone, Copy)]
struct Spelling {
    /// One line, as cargo writes it, or indented across lines.
    pretty: bool,
    /// `"package"` before `"files"` instead of after.
    package_first: bool,
    /// Spaces around the colon.
    spaced: bool,
    /// A file name carrying escaped quotes that spell `"package"`.
    escaped_name: bool,
    /// A crate-root file literally named `package`.
    root_package_file: bool,
}

impl Spelling {
    /// Cargo's own spelling: compact, sorted keys, no tricks.
    const CANONICAL: Self = Self {
        pretty: false,
        package_first: false,
        spaced: false,
        escaped_name: false,
        root_package_file: false,
    };

    /// Any spelling JSON allows (RFC 8259 §2, §4, §7).
    fn draw(rng: &mut Rng) -> Self {
        Self {
            pretty: rng.coin(),
            package_first: rng.coin(),
            spaced: rng.coin(),
            escaped_name: rng.coin(),
            root_package_file: rng.coin(),
        }
    }
}

/// A sha256 drawn as 64 lowercase hex digits.
fn hash(rng: &mut Rng) -> Result<String, Failure> {
    let mut text = String::with_capacity(64);
    for _ in 0..4 {
        write!(text, "{:016x}", rng.next_u64())?;
    }
    Ok(text)
}

/// A lockfile model of 1-12 packages with unique names.
fn draw_packages(rng: &mut Rng, opaque_dirs: bool) -> Result<Vec<Package>, Failure> {
    let count = rng.between(1, 12);
    let mut packages = Vec::new();
    for index in 0..count {
        let name = format!("crate-{index}-{:04x}", rng.below(0x1_0000));
        let version = format!("{}.{}.{}", rng.below(4), rng.below(20), rng.below(50));
        let kind = match rng.below(3) {
            0 => Kind::Local,
            1 => Kind::Registry(hash(rng)?),
            _ => Kind::Git,
        };
        let dir = if opaque_dirs || rng.coin() {
            format!("d{index}")
        } else {
            format!("{name}-{version}")
        };
        packages.push(Package {
            name,
            version,
            kind,
            dir,
        });
    }
    Ok(packages)
}

/// The lockfile Cargo would write for `packages`, in order.
fn lock_text(packages: &[Package]) -> Result<String, Failure> {
    let mut text = String::from("version = 4\n");
    for package in packages {
        write!(
            text,
            "\n[[package]]\nname = \"{}\"\nversion = \"{}\"\n",
            package.name, package.version
        )?;
        match package.kind {
            Kind::Local => {}
            Kind::Registry(ref sum) => {
                write!(text, "source = \"{REGISTRY}\"\nchecksum = \"{sum}\"\n")?;
            }
            Kind::Git => writeln!(
                text,
                "source = \"git+https://example.com/org/{}#{:x}\"",
                package.name,
                package.version.len()
            )?,
        }
    }
    Ok(text)
}

/// A `.cargo-checksum.json` carrying `package_hash`, spelled as `spelling`.
fn checksum_json(rng: &mut Rng, package_hash: &str, spelling: Spelling) -> Result<String, Failure> {
    let colon = if spelling.spaced { " : " } else { ":" };
    let gap = if spelling.pretty { "\n  " } else { "" };
    let mut names = vec!["Cargo.toml".to_owned(), "src/lib.rs".to_owned()];
    if spelling.escaped_name {
        names.push("docs/\\\"package\\\"".to_owned());
    }
    if spelling.root_package_file {
        names.push("package".to_owned());
    }
    let mut files = String::new();
    for (position, name) in names.iter().enumerate() {
        if position > 0 {
            files.push(',');
        }
        write!(files, "{gap}  \"{name}\"{colon}\"{}\"", hash(rng)?)?;
    }
    let files = format!("\"files\"{colon}{{{files}{gap}}}");
    let package = format!("\"package\"{colon}\"{package_hash}\"");
    let (first, second) = if spelling.package_first {
        (package, files)
    } else {
        (files, package)
    };
    let close = if spelling.pretty { "\n" } else { "" };
    Ok(format!("{{{gap}{first},{gap}{second}{close}}}\n"))
}

/// A vendor tree on disk, removed when dropped.
struct Tree {
    /// The tree's root directory.
    root: PathBuf,
}

impl Tree {
    /// Writes the tree that vendors every non-local package in `packages`.
    ///
    /// The root is named from the clock's nanoseconds, a process-wide counter
    /// and a seed draw, never from a process id (the OS reuses them), so no two
    /// trees share one: not two test processes, and not the two runs of one
    /// seed, which each remove their own tree when dropped.
    fn write(
        rng: &mut Rng,
        label: &str,
        packages: &[Package],
        spell: impl Fn(&mut Rng) -> Spelling,
    ) -> Result<Self, Failure> {
        static TREES: AtomicU64 = AtomicU64::new(0);
        let unique = TREES.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lgwks-deps-sim-vendor-{label}-{nanos}-{unique}-{:016x}",
            rng.next_u64()
        ));
        std::fs::create_dir_all(&root)?;
        let tree = Self { root };
        for package in packages {
            let package_hash = match package.kind {
                Kind::Local => continue,
                Kind::Registry(ref sum) => sum.clone(),
                Kind::Git => hash(rng)?,
            };
            let spelling = spell(rng);
            let dir = tree.root.join(&package.dir);
            std::fs::create_dir_all(dir.join("src"))?;
            std::fs::write(
                dir.join(".cargo-checksum.json"),
                checksum_json(rng, &package_hash, spelling)?,
            )?;
            std::fs::write(
                dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{}\"\nversion = \"{}\"\n",
                    package.name, package.version
                ),
            )?;
        }
        Ok(tree)
    }

    /// The directory `package` is vendored in.
    fn dir(&self, package: &Package) -> PathBuf {
        self.root.join(&package.dir)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            lgwks_std::trace::debug!(%error, root = %self.root.display(), "sim_vendor_coverage: tree left behind");
        }
    }
}

/// What the report must say for `packages` when `missing` are absent.
struct Expected {
    /// Non-local packages present with their bytes.
    covered: usize,
    /// Workspace members.
    local: usize,
    /// `(name, version)` of every absent package, sorted.
    missing: Vec<(String, String)>,
}

impl Expected {
    /// The verdict for `packages` with the names in `absent` uncovered.
    fn of(packages: &[Package], absent: &[&str]) -> Self {
        let mut expected = Self {
            covered: 0,
            local: 0,
            missing: Vec::new(),
        };
        for package in packages {
            if package.kind == Kind::Local {
                expected.local = expected.local.saturating_add(1);
            } else if absent.contains(&package.name.as_str()) {
                expected
                    .missing
                    .push((package.name.clone(), package.version.clone()));
            } else {
                expected.covered = expected.covered.saturating_add(1);
            }
        }
        expected.missing.sort();
        expected
    }
}

/// Checks `report` against `expected` and records it in `trace`.
fn verify(report: &Report, expected: &Expected, trace: &mut Trace) -> TestResult {
    let missing: Vec<(String, String)> = report
        .missing()
        .iter()
        .map(|entry| (entry.name().to_owned(), entry.version().to_owned()))
        .collect();
    trace.record_number("covered", report.covered());
    trace.record_number("local", report.skipped_local());
    trace.record_number("missing", missing.len());
    if report.covered() != expected.covered
        || report.skipped_local() != expected.local
        || missing != expected.missing
        || report.is_complete() != expected.missing.is_empty()
    {
        return refuse(format!(
            "report covered {} local {} missing {missing:?}; expected covered {} local {} missing {:?}",
            report.covered(),
            report.skipped_local(),
            expected.covered,
            expected.local,
            expected.missing
        ));
    }
    Ok(())
}

/// A drawn non-local package of the wanted kind, or `None` when the seed drew
/// none.
fn pick_vendored<'a>(
    rng: &mut Rng,
    packages: &'a [Package],
    want: impl Fn(&Kind) -> bool,
) -> Option<&'a Package> {
    let candidates: Vec<&Package> = packages
        .iter()
        .filter(|package| want(&package.kind))
        .collect();
    rng.pick(&candidates).copied()
}

/// Whether a kind is vendored at all.
fn vendored(kind: &Kind) -> bool {
    *kind != Kind::Local
}

#[test]
fn a_complete_tree_covers_every_package_in_any_spelling() -> TestResult {
    sweep("complete_tree", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "complete", &packages, Spelling::draw)?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[]), trace)
    })
}

#[test]
fn a_removed_directory_is_exactly_the_missing_package() -> TestResult {
    sweep("removed_directory", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "removed", &packages, |_| Spelling::CANONICAL)?;
        let Some(victim) = pick_vendored(rng, &packages, vendored) else {
            trace.record("no vendored package drawn");
            return Ok(());
        };
        std::fs::remove_dir_all(tree.dir(victim))?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[&victim.name]), trace)
    })
}

#[test]
fn the_right_name_with_other_bytes_is_missing() -> TestResult {
    sweep("other_bytes", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "bytes", &packages, |_| Spelling::CANONICAL)?;
        let Some(victim) = pick_vendored(rng, &packages, |kind| matches!(*kind, Kind::Registry(_)))
        else {
            trace.record("no registry package drawn");
            return Ok(());
        };
        let other = hash(rng)?;
        std::fs::write(
            tree.dir(victim).join(".cargo-checksum.json"),
            checksum_json(rng, &other, Spelling::CANONICAL)?,
        )?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[&victim.name]), trace)
    })
}

#[test]
fn directory_names_carry_no_weight() -> TestResult {
    sweep("opaque_names", |rng, trace| {
        let packages = draw_packages(rng, true)?;
        let tree = Tree::write(rng, "opaque", &packages, Spelling::draw)?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[]), trace)
    })
}

#[test]
fn a_git_package_needs_its_manifest_identity() -> TestResult {
    sweep("git_identity", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "git", &packages, |_| Spelling::CANONICAL)?;
        let Some(victim) = pick_vendored(rng, &packages, |kind| *kind == Kind::Git) else {
            trace.record("no git package drawn");
            return Ok(());
        };
        std::fs::write(
            tree.dir(victim).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"{}-other\"\n",
                victim.name, victim.version
            ),
        )?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[&victim.name]), trace)
    })
}

#[test]
fn a_file_named_package_does_not_shadow_the_crate_hash() -> TestResult {
    sweep("root_package_file", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "shadow", &packages, |rng| Spelling {
            root_package_file: true,
            escaped_name: rng.coin(),
            ..Spelling::draw(rng)
        })?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[]), trace)
    })
}

#[test]
fn an_escaped_file_name_spelling_package_does_not_shadow_the_hash() -> TestResult {
    sweep("escaped_name", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "escaped", &packages, |rng| Spelling {
            escaped_name: true,
            root_package_file: false,
            ..Spelling::draw(rng)
        })?;
        let report = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&report, &Expected::of(&packages, &[]), trace)
    })
}

#[test]
fn a_directory_without_a_package_hash_is_refused_by_name() -> TestResult {
    sweep("hashless_directory", |rng, trace| {
        let packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "hashless", &packages, Spelling::draw)?;
        let broken = tree.root.join(format!("zz-broken-{}", rng.below(100)));
        std::fs::create_dir_all(&broken)?;
        let body = if rng.coin() {
            "{\"files\":{\"package\":\"00\"}}\n"
        } else {
            "{\"files\":{},\"package\":\"\"}\n"
        };
        std::fs::write(broken.join(".cargo-checksum.json"), body)?;
        match vendor::check_coverage(&lock_text(&packages)?, &tree.root) {
            Err(VendorError::ChecksumUnreadable { ref dir, .. }) if *dir == broken => {
                trace.record("refused the hashless directory");
                Ok(())
            }
            other => refuse(format!(
                "a hashless directory {} must be refused by name, got {other:?}",
                broken.display()
            )),
        }
    })
}

#[test]
fn lock_order_does_not_change_the_report() -> TestResult {
    sweep("lock_order", |rng, trace| {
        let mut packages = draw_packages(rng, false)?;
        let tree = Tree::write(rng, "order", &packages, Spelling::draw)?;
        let mut absent = Vec::new();
        for package in &packages {
            if vendored(&package.kind) && rng.coin() {
                std::fs::remove_dir_all(tree.dir(package))?;
                absent.push(package.name.clone());
            }
        }
        let names: Vec<&str> = absent.iter().map(String::as_str).collect();
        let expected = Expected::of(&packages, &names);
        let first = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&first, &expected, trace)?;
        for index in (1..packages.len()).rev() {
            let other = usize::try_from(rng.below(u32::try_from(index)?.saturating_add(1)))?;
            packages.swap(index, other);
        }
        let shuffled = vendor::check_coverage(&lock_text(&packages)?, &tree.root)?;
        verify(&shuffled, &expected, trace)?;
        if first != shuffled {
            return refuse("the same lock in another order produced another report");
        }
        Ok(())
    })
}
