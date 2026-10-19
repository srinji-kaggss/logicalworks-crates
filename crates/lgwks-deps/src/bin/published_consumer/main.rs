//! Consumes every crate this repository publishes the way a stranger would: from
//! crates.io, at the exact version its newest release tag names, in a package
//! with no path into this tree. A workspace caller search is not downstream
//! evidence (#155); this is.
//!
//! One receipt line per consumer:
//!
//! ```text
//! <crate> tag=<tag> published=<crates.io newest> main=<version on main>
//!   commits-since-tag=<n> features=<set> program=<readme|minimal> consumer=<pass|FAIL>
//! ```
//!
//! The program is the crate's own README quickstart at that tag, the surface the
//! release promised, so a README that does not compile against its own release
//! fails here instead of sitting unrun. It is made from the page the way rustdoc
//! makes a doctest (see `program`). A crate whose page has no runnable block gets
//! a minimal program over its public modules, and the receipt says so.
//!
//! Usage, from the repository: `cargo run -p lgwks_deps --bin published-consumer`.
//! It needs the network (crates.io). Exit status: 0 every consumer passed; 1 a
//! consumer failed or a tag names a version crates.io does not serve as its
//! newest; 2 the check could not run.

#[path = "../shared/children.rs"]
mod children;
#[path = "../shared/console.rs"]
mod console;
mod consumer;
mod program;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use children::{Children, Launch};
use console::Console;
use consumer::{ROWS, Row, consume};
use program::{first_rust_block, main_rs};

/// How long one git or cargo query may take.
const QUERY_BOUND: Duration = Duration::from_secs(120);

/// Why the check could not run.
#[derive(Debug)]
enum Failure {
    /// A file or directory could not be made, read or written.
    Io {
        /// What was being touched.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A command, tag, version or entropy the check depends on gave nothing.
    Missing(String),
    /// Arguments were given; the check takes none.
    Usage,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "{}: {cause}", path.display()),
            Self::Missing(ref what) => write!(formatter, "{what}"),
            Self::Usage => write!(formatter, "takes no arguments; run it from the repository"),
        }
    }
}

impl std::error::Error for Failure {}

impl Failure {
    /// An I/O failure on `path`, logged where it happened.
    fn io<T>(path: &Path, cause: io::Error) -> Result<T, Self> {
        lgwks_std::trace::debug!(%cause, path = ?path, "a file operation failed");
        Err(Self::Io {
            path: path.to_path_buf(),
            cause,
        })
    }

    /// A call with arguments, logged where it was refused.
    fn usage<T>() -> Result<T, Self> {
        lgwks_std::trace::debug!("the check takes no arguments");
        Err(Self::Usage)
    }

    /// A missing input, logged where it was found missing.
    fn missing<T>(what: String) -> Result<T, Self> {
        lgwks_std::trace::debug!(what = %what, "an input the check needs is missing");
        Err(Self::Missing(what))
    }
}

/// The per-run directory every consumer is built in, removed when the run ends.
/// Its name is 64 random bits, so two runs on one machine never share one.
struct Work {
    /// The directory.
    path: PathBuf,
}

impl Work {
    /// Makes a directory no other run holds.
    fn make() -> Result<Self, Failure> {
        let bytes = match lgwks_std::random::bytes::<8>() {
            Ok(found) => found,
            Err(error) => return Failure::missing(format!("no entropy for a work name: {error}")),
        };
        let name = format!("published-consumer-{:016x}", u64::from_le_bytes(bytes));
        let path = std::env::temp_dir().join(name);
        match fs::create_dir(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error) => Failure::io(&path, error),
        }
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            lgwks_std::trace::debug!(%error, path = ?self.path, "the work directory could not be removed");
        }
    }
}

/// What every query and consumer of one run shares.
#[derive(Clone, Copy)]
struct Context<'run> {
    /// The supervisor every child runs under.
    children: &'run Children,
    /// Where lines go.
    console: &'run Console,
    /// The environment every child sees.
    env: &'run BTreeMap<String, String>,
    /// The per-run directory.
    work: &'run Path,
    /// The repository root.
    root: &'run Path,
}

impl Context<'_> {
    /// Runs `argv` in `dir` and returns what it prints, when it succeeds.
    fn capture(&self, dir: &Path, argv: &[&str]) -> Option<String> {
        let words: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        let launch = Launch {
            argv: &words,
            root: dir,
            env: self.env,
            stdin: None,
            bound: QUERY_BOUND,
        };
        let done = self.children.capture(&launch);
        if done.code == 0 {
            return Some(done.stdout);
        }
        lgwks_std::trace::debug!(argv = ?argv, code = done.code, stderr = %done.stderr, "a query did not succeed");
        None
    }

    /// Like [`Self::capture`], trimmed, with a failure that names the command.
    fn printed(&self, dir: &Path, argv: &[&str]) -> Result<String, Failure> {
        match self.capture(dir, argv) {
            Some(text) => Ok(text.trim().to_owned()),
            None => Failure::missing(format!("`{}` did not succeed", argv.join(" "))),
        }
    }

    /// The newest release tag of `name`, e.g. `lgwks_std-v0.10.0`.
    fn latest_tag(&self, name: &str) -> Result<String, Failure> {
        let pattern = format!("{name}-v*");
        let listed = self.printed(
            self.root,
            &["git", "tag", "--list", &pattern, "--sort=-v:refname"],
        )?;
        match listed.lines().next() {
            Some(tag) if !tag.is_empty() => Ok(tag.to_owned()),
            Some(_) | None => Failure::missing(format!("{name} has no release tag")),
        }
    }

    /// The newest version crates.io serves for `name`.
    fn published_version(&self, name: &str) -> Result<String, Failure> {
        let info = self.printed(self.work, &["cargo", "info", name])?;
        match info_version(&info) {
            Some(version) => Ok(version),
            None => Failure::missing(format!("`cargo info {name}` names no version")),
        }
    }

    /// The text of `page` of the crate in `dir`, at `tag`; `None` when the tag has
    /// no such page.
    fn page_at(&self, tag: &str, dir: &str, page: &str) -> Option<String> {
        let object = format!("{tag}:{dir}/{page}");
        self.capture(self.root, &["git", "show", &object])
    }

    /// The version `row`'s crate has on main.
    fn main_version(&self, row: &Row) -> Result<String, Failure> {
        let path = self.root.join(row.dir).join("Cargo.toml");
        let text = match fs::read_to_string(&path) {
            Ok(found) => found,
            Err(error) => return Failure::io(&path, error),
        };
        match manifest_version(&text) {
            Some(version) => Ok(version),
            None => Failure::missing(format!("{} names no package version", path.display())),
        }
    }

    /// Builds one consumer and prints its receipt; how many failures it found.
    fn receipt(&self, row: &Row) -> Result<u32, Failure> {
        let tag = self.latest_tag(row.name)?;
        let published = self.published_version(row.name)?;
        let main_version = self.main_version(row)?;
        let range = format!("{tag}..HEAD");
        let since = self.printed(self.root, &["git", "rev-list", "--count", &range])?;
        let mut failures: u32 = 0;
        if tag_version(&tag, row.name) != Some(published.as_str()) {
            self.console.warn(&format!(
                "{}: newest tag {tag} but crates.io serves {published}",
                row.name
            ));
            failures = failures.saturating_add(1);
        }
        let block = self
            .page_at(&tag, row.dir, row.page)
            .and_then(|page| first_rust_block(&page));
        let (program, source) = match block {
            Some(found) => (main_rs(&found), "readme"),
            None => (row.minimal.to_owned(), "minimal"),
        };
        let passed = consume(
            self.children,
            self.console,
            self.env,
            self.work,
            row,
            &published,
            &program,
        )?;
        if !passed {
            failures = failures.saturating_add(1);
        }
        self.console.say(&format!(
            "{} tag={tag} published={published} main={main_version} commits-since-tag={since} features={} program={source} consumer={}",
            row.name,
            row.label,
            if passed { "pass" } else { "FAIL" },
        ));
        Ok(failures)
    }
}

/// The version `cargo info` prints, from its `version: 4.0.0 (latest ..)` line.
fn info_version(info: &str) -> Option<String> {
    info.lines()
        .find_map(|line| line.strip_prefix("version: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
}

/// The version a release tag `<name>-v<version>` names.
fn tag_version<'tag>(tag: &'tag str, name: &str) -> Option<&'tag str> {
    tag.strip_prefix(name)?.strip_prefix("-v")
}

/// The `[package]` version of a manifest: its first `version = "…"` line.
fn manifest_version(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split_once('"'))
        .map(|(version, _)| version.to_owned())
}

/// Runs every consumer; how many failed.
fn execute(console: &Console, children: &Children) -> Result<u32, Failure> {
    if std::env::args().nth(1).is_some() {
        return Failure::usage();
    }
    let work = Work::make()?;
    let mut env = children::inherited_env();
    children::forget_launched_crate(&mut env);
    // One target directory for every consumer, so the shared dependency graph is
    // compiled once rather than once per feature set.
    env.insert(
        "CARGO_TARGET_DIR".to_owned(),
        work.path.join("target").display().to_string(),
    );
    let here = Context {
        children,
        console,
        env: &env,
        work: &work.path,
        root: Path::new("."),
    };
    let top = here.printed(Path::new("."), &["git", "rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(top);
    let context = Context {
        root: &root,
        ..here
    };
    let mut failures: u32 = 0;
    for row in &ROWS {
        failures = failures.saturating_add(context.receipt(row)?);
    }
    Ok(failures)
}

fn main() -> ExitCode {
    let console = Console::new(Box::new(std::io::stdout()), Box::new(std::io::stderr()));
    let children = Children::new(true);
    let outcome = execute(&console, &children);
    children.stop_all();
    children.finish();
    match outcome {
        Ok(0) => {
            console.say("published consumers: all passed");
            ExitCode::SUCCESS
        }
        Ok(count) => {
            console.warn(&format!("published consumers: {count} failure(s)"));
            ExitCode::from(1)
        }
        Err(failure) => {
            console.warn(&format!("published-consumer: {failure}"));
            ExitCode::from(2)
        }
    }
}
