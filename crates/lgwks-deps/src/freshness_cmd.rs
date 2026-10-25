//! The `freshness` command: every registry dependency in the lock file against
//! the registry's sparse index.
//!
//! The lookups run concurrently, a bounded number at a time, because the time
//! they take is network latency and not work: this repository's own 943 locked
//! packages took 77 s at `--jobs 1` and 12 s at the default 32, with the same
//! table both times (measured 2026-10-25 against crates.io). Each package is
//! judged by SemVer precedence against the newest version it could move to, and
//! each is reported with the direct dependency it comes through, so the reader
//! knows whose release to wait for.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::process::ExitCode;

use crate::{
    USAGE, handle_help, locked_graph, read_lock_text, refuse, resolve_repository_root, served_by,
    subject_path,
};

mod args;
mod fetch;
mod index;
mod pool;
mod report;
mod repository;
mod version;
mod via;

pub(crate) use via::{Graph, Node};

use fetch::FetchError;
use index::{Entry, IndexError};
use report::FreshnessResult;
use version::Version;

/// Exit status when at least one package is behind the newest version on offer.
const EXIT_STALE: u8 = 1;

/// Exit status when the check could not complete: a refusal, an unreadable
/// lock, or lookups that failed with nothing found stale. A caller that reads
/// `0` or `1` as an answer must never be handed one by a check that did not
/// finish.
const EXIT_INCOMPLETE: u8 = 2;

/// Reports every registry dependency in the lock file against the registry.
///
/// Exit 0 when every package is current, 1 when at least one is behind, and 2
/// when the check could not complete: a refused invocation, an unreadable lock,
/// or a lookup that failed when nothing else was found stale. A package from
/// another source (a git or path dependency, or another registry than the one
/// asked) is counted as skipped rather than asked about the wrong index.
pub(crate) fn handle_freshness(
    arguments: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let request = match args::parse(arguments) {
        Ok(request) => request,
        Err(error) => {
            writeln!(err, "lgwks-deps: {error}\n")?;
            write!(err, "{USAGE}")?;
            return Ok(ExitCode::from(EXIT_INCOMPLETE));
        }
    };
    let run = match request {
        args::Request::Help => return handle_help(out),
        args::Request::Run(run) => run,
    };

    let start = subject_path(run.target.as_deref());
    let root = unwrap_or_refuse!(resolve_repository_root(&start), err);
    let lock_text = unwrap_or_refuse!(read_lock_text(&root), err);
    let resolved = match lgwks_deps::lock::parse(&lock_text) {
        Ok(parsed) => parsed,
        Err(error) => return refuse(&format!("Cargo.lock: {error}"), err),
    };

    let served: Vec<(usize, &lgwks_deps::lock::Resolved)> = resolved
        .iter()
        .enumerate()
        .filter(|pair| served_by(&run.index, pair.1))
        .collect();
    let skipped = resolved
        .iter()
        .filter(|package| !package.is_local() && !served_by(&run.index, package))
        .count();
    if served.is_empty() {
        if skipped == 0 {
            writeln!(out, "no registry dependencies in Cargo.lock")?;
        } else {
            writeln!(
                out,
                "no locked package comes from {} ({skipped} from other sources)",
                run.index
            )?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let graph = locked_graph(&resolved);
    let outcomes = look_up(&served, &run);
    let sources = repository::registry_sources();
    let results: Vec<FreshnessResult> = served
        .iter()
        .map(|pair| {
            row(
                pair.1,
                graph.via(pair.0),
                outcomes.get(pair.1.name()),
                sources.as_deref(),
            )
        })
        .collect();

    if run.json {
        report::print_json(&results, out)?;
    } else {
        report::print_table(&results, skipped, out)?;
    }
    let status = exit_status(report::stale_count(&results), report::error_count(&results));
    if run.json && status != 0 {
        writeln!(
            err,
            "freshness: {}",
            report::summary_line(&results, skipped)
        )?;
    }
    Ok(ExitCode::from(status))
}

/// The exit status for `stale` packages behind and `errors` failed lookups.
///
/// A stale package is an answer, whatever else failed: the caller has something
/// to act on. Failures alone are not an answer, so they are never `0`.
fn exit_status(stale: usize, errors: usize) -> u8 {
    match (stale, errors) {
        (0, 0) => 0,
        (0, _) => EXIT_INCOMPLETE,
        _ => EXIT_STALE,
    }
}

/// What asking the index about one name came to.
enum Lookup {
    /// The index file, read.
    Found(Vec<Entry>),
    /// Why it could not be read.
    Failed(String),
}

/// Why one name's lookup failed, before it is worded for a row.
enum LookupError {
    /// The name or the file was refused.
    Index(IndexError),
    /// The request did not return a body.
    Fetch(FetchError),
}

impl From<IndexError> for LookupError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

impl From<FetchError> for LookupError {
    fn from(error: FetchError) -> Self {
        Self::Fetch(error)
    }
}

impl fmt::Display for LookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Index(ref error) => fmt::Display::fmt(error, formatter),
            Self::Fetch(ref error) => fmt::Display::fmt(error, formatter),
        }
    }
}

/// Asks the index about every distinct name among `served`, a bounded number at
/// a time.
///
/// Names are de-duplicated first: a lock commonly resolves several versions of
/// one package, and the index answers once per name.
fn look_up(
    served: &[(usize, &lgwks_deps::lock::Resolved)],
    run: &args::Args,
) -> BTreeMap<String, Lookup> {
    let names: Vec<String> = served
        .iter()
        .map(|pair| pair.1.name())
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let index_url = run.index.clone();
    let done = pool::run(
        names.clone(),
        run.jobs,
        move |name: String| match fetch_entries(&index_url, &name) {
            Ok(entries) => Lookup::Found(entries),
            Err(error) => Lookup::Failed(error.to_string()),
        },
    );
    lgwks_std::trace::debug!(
        workers = done.workers,
        names = names.len(),
        "freshness lookups finished"
    );
    names
        .into_iter()
        .zip(done.results)
        .map(|(name, result)| {
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => Lookup::Failed(error.to_string()),
            };
            (name, outcome)
        })
        .collect()
}

/// One name's index file, fetched and read.
fn fetch_entries(index_url: &str, name: &str) -> Result<Vec<Entry>, LookupError> {
    let path = index::path(name)?;
    let body = fetch::get(&format!("{index_url}/{path}"))?;
    Ok(index::parse(&body)?)
}

/// The row for one locked package, given what the index said about its name.
fn row(
    package: &lgwks_deps::lock::Resolved,
    via: &[String],
    lookup: Option<&Lookup>,
    sources: Option<&std::path::Path>,
) -> FreshnessResult {
    let name = package.name();
    let locked = package.version();
    let Some(lookup) = lookup else {
        return FreshnessResult::failed(name, locked, via, "no lookup was made".to_owned());
    };
    match *lookup {
        Lookup::Found(ref entries) => match Version::parse(locked) {
            Ok(resolved) => FreshnessResult::found(
                name,
                locked,
                index::newest(entries, &resolved),
                sources.and_then(|directory| repository::cached(directory, name, locked)),
                via,
            ),
            Err(error) => FreshnessResult::failed(
                name,
                locked,
                via,
                format!("the locked version is unreadable: {error}"),
            ),
        },
        Lookup::Failed(ref reason) => FreshnessResult::failed(name, locked, via, reason.clone()),
    }
}

/// What the setup of a simulation anywhere under this module can fail with.
///
/// A helper returns this rather than a boxed trait object, so its caller can
/// match the cause; a `#[test]` converts it with `?`.
#[cfg(test)]
#[derive(Debug)]
enum SimError {
    /// The seeded stream refused a draw.
    Draw(lgwks_std::seeded::DrawError),
    /// The operating system had no entropy for a scratch name.
    Entropy(lgwks_std::random::EntropyError),
    /// A scratch directory could not be made.
    Io(io::Error),
    /// A version the simulation spelled or drew did not parse.
    Version(version::VersionError),
    /// An index body the simulation built did not parse.
    Index(IndexError),
    /// The arguments a simulation spelled were refused.
    Args(args::ArgsError),
    /// A number in a drawn version did not parse.
    Number(std::num::ParseIntError),
    /// Something the simulation relies on was not there.
    Missing(&'static str),
}

#[cfg(test)]
impl fmt::Display for SimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Draw(ref error) => write!(formatter, "the seeded stream: {error}"),
            Self::Entropy(ref error) => write!(formatter, "no entropy: {error}"),
            Self::Io(ref error) => write!(formatter, "scratch directory: {error}"),
            Self::Version(ref error) => write!(formatter, "version: {error}"),
            Self::Index(ref error) => write!(formatter, "index: {error}"),
            Self::Args(ref error) => write!(formatter, "arguments: {error}"),
            Self::Number(ref error) => write!(formatter, "number: {error}"),
            Self::Missing(what) => write!(formatter, "missing: {what}"),
        }
    }
}

#[cfg(test)]
impl std::error::Error for SimError {}

#[cfg(test)]
impl From<lgwks_std::seeded::DrawError> for SimError {
    fn from(error: lgwks_std::seeded::DrawError) -> Self {
        Self::Draw(error)
    }
}

#[cfg(test)]
impl From<lgwks_std::random::EntropyError> for SimError {
    fn from(error: lgwks_std::random::EntropyError) -> Self {
        Self::Entropy(error)
    }
}

#[cfg(test)]
impl From<io::Error> for SimError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
impl From<version::VersionError> for SimError {
    fn from(error: version::VersionError) -> Self {
        Self::Version(error)
    }
}

#[cfg(test)]
impl From<IndexError> for SimError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

#[cfg(test)]
impl From<args::ArgsError> for SimError {
    fn from(error: args::ArgsError) -> Self {
        Self::Args(error)
    }
}

#[cfg(test)]
impl From<std::num::ParseIntError> for SimError {
    fn from(error: std::num::ParseIntError) -> Self {
        Self::Number(error)
    }
}

#[cfg(test)]
impl From<&'static str> for SimError {
    fn from(what: &'static str) -> Self {
        Self::Missing(what)
    }
}

#[cfg(test)]
mod sim_exit {
    //! The exit status over every pair of counts: the part of the contract a
    //! caller scripts against.

    use super::{EXIT_INCOMPLETE, EXIT_STALE, exit_status};

    #[test]
    fn the_status_is_zero_only_when_nothing_is_behind_and_nothing_failed() {
        assert_eq!(exit_status(0, 0), 0, "every package current");
        for stale in 1..40_usize {
            for errors in 0..40_usize {
                assert_eq!(
                    exit_status(stale, errors),
                    EXIT_STALE,
                    "{stale} stale with {errors} failed is an answer"
                );
            }
        }
        for errors in 1..40_usize {
            assert_eq!(
                exit_status(0, errors),
                EXIT_INCOMPLETE,
                "{errors} failed with nothing stale did not complete"
            );
        }
    }

    #[test]
    fn the_three_statuses_are_distinct() {
        assert_ne!(EXIT_STALE, 0, "stale is not success");
        assert_ne!(EXIT_INCOMPLETE, 0, "incomplete is not success");
        assert_ne!(EXIT_STALE, EXIT_INCOMPLETE, "stale is not incomplete");
    }
}
