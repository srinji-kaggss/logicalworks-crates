//! `freshness` end to end: the shipped binary, a `Cargo.lock` on disk, and a
//! sparse-index registry on a loopback port.
//!
//! Issue #702 asked for four things a user can see: the lookups run at once, a
//! pre-release or a yanked version is never offered as the newest, each row names
//! the dependency it comes through, and the exit code says whether the check
//! finished. Every one of them is a property of the process, so every one is read
//! off the process: its exit code, its streams, and what the registry saw.
//!
//! Two scenarios draw their whole world from a seed and compare the binary with
//! an independent model written here in plain integers (SemVer 2.0 section 11:
//! numeric before text, a release above its own pre-releases). The rest pin the
//! shapes a seed would take many runs to reach: the three exit codes, a registry
//! that is gone or says nothing, one that never stops sending (cut at the read
//! cap), every refusal, and a lock with packages from other sources.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use lgwks_std::json::{self, Value};

use crate::lock_fixture::{Package, Scratch};
use crate::replay::{Failure, refuse, replay_sweep};
use crate::sim::Rng;
use crate::stub_index::{Reply, Stub};

type TestResult = Result<(), Failure>;

/// Seeds per scenario. Each seed starts a process or two and a registry, so the
/// sweep is narrower than the pure parsers' and still draws every outcome.
const SEEDS: u64 = 16;

/// One run of `lgwks-deps freshness`.
struct Outcome {
    /// The exit code, or `None` when a signal ended the process.
    code: Option<i32>,
    /// What it wrote to stdout.
    stdout: String,
    /// What it wrote to stderr.
    stderr: String,
}

/// Runs `lgwks-deps freshness` with `args` from `dir`.
///
/// `CARGO_HOME` points at `dir`, which holds no registry cache, so the run reads
/// nothing of the machine it happens on: every repository column is unknown.
fn freshness(dir: &Path, args: &[&str]) -> Result<Outcome, Box<dyn Error>> {
    let binary = std::env::var_os("CARGO_BIN_EXE_lgwks-deps")
        .ok_or("Cargo did not provide the lgwks-deps test binary")?;
    let output = Command::new(binary)
        .arg("freshness")
        .args(args)
        .current_dir(dir)
        .env("CARGO_HOME", dir)
        .output()?;
    Ok(Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

/// Both streams, for a failure message.
fn streams(outcome: &Outcome) -> String {
    format!("stdout {:?} stderr {:?}", outcome.stdout, outcome.stderr)
}

/// A plain assertion that names the seed through the sweep's error.
fn check(holds: bool, message: impl Into<String>) -> TestResult {
    if holds { Ok(()) } else { refuse(message) }
}

/// A version as the model holds it: three numbers and an optional `-rc.N`.
#[derive(Debug, Clone, Copy)]
struct Release {
    major: u32,
    minor: u32,
    patch: u32,
    /// The `N` of `-rc.N`, or `None` for a release.
    pre: Option<u32>,
}

impl Release {
    /// A release `major.minor.patch`.
    const fn plain(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: None,
        }
    }

    /// The version as the lock and the index spell it.
    fn spelled(&self) -> String {
        match self.pre {
            Some(number) => format!("{}.{}.{}-rc.{number}", self.major, self.minor, self.patch),
            None => format!("{}.{}.{}", self.major, self.minor, self.patch),
        }
    }

    /// The model's order: numbers numerically, and a release above any
    /// pre-release of the same numbers.
    fn rank(&self) -> (u32, u32, u32, u8, u32) {
        match self.pre {
            Some(number) => (self.major, self.minor, self.patch, 0, number),
            None => (self.major, self.minor, self.patch, 1, 0),
        }
    }
}

/// A version the registry lists.
#[derive(Debug, Clone, Copy)]
struct Published {
    release: Release,
    yanked: bool,
}

/// One locked registry package and what the registry holds for it.
#[derive(Debug, Clone)]
struct Subject {
    name: String,
    locked: Release,
    published: Vec<Published>,
    /// Whether the registry has a file for this name at all.
    listed: bool,
}

/// What the model says the binary must report for a subject.
struct Expected {
    latest: Option<String>,
    stale: bool,
    error: bool,
}

impl Subject {
    /// A package locked at `1.0.0` whose registry lists exactly that.
    fn current(position: usize) -> Self {
        Self {
            name: format!("crate{position}"),
            locked: Release::plain(1, 0, 0),
            published: vec![Published {
                release: Release::plain(1, 0, 0),
                yanked: false,
            }],
            listed: true,
        }
    }

    /// The model's verdict: the newest unyanked version, a pre-release only for a
    /// lock already on one, and stale when it outranks the lock.
    fn expected(&self) -> Expected {
        // A file that lists no version at all is a hollow answer: whatever was
        // locked was published, so nothing was learned and the row is an error.
        if !self.listed || self.published.is_empty() {
            return Expected {
                latest: None,
                stale: false,
                error: true,
            };
        }
        let admit_prerelease = self.locked.pre.is_some();
        let best = self
            .published
            .iter()
            .filter(|offer| !offer.yanked && (admit_prerelease || offer.release.pre.is_none()))
            .map(|offer| offer.release)
            .max_by_key(Release::rank);
        Expected {
            latest: best.map(|release| release.spelled()),
            stale: best.is_some_and(|release| release.rank() > self.locked.rank()),
            error: false,
        }
    }

    /// The registry's file for this name: one JSON line per published version.
    fn index_body(&self) -> String {
        self.published
            .iter()
            .map(|offer| index_line(&self.name, &offer.release.spelled(), offer.yanked))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Where the index keeps this name. Every name here is `crateN`, six
    /// characters, so the layout is `cr/at/<name>`; written out rather than
    /// computed so the model shares no code with the command.
    fn path(&self) -> String {
        format!("cr/at/{}", self.name)
    }
}

/// One line of an index file, with the keys the registry always writes.
fn index_line(name: &str, version: &str, yanked: bool) -> String {
    format!(
        "{{\"name\":\"{name}\",\"vers\":\"{version}\",\"deps\":[],\"cksum\":\"00\",\"features\":{{}},\"yanked\":{yanked}}}"
    )
}

/// A seeded version with small numbers, so equal prefixes are common and the
/// pre-release rule is exercised, and a fifth of them pre-releases.
fn draw_release(rng: &mut Rng) -> Release {
    Release {
        major: rng.below(3),
        minor: rng.below(11),
        patch: rng.below(4),
        pre: (rng.below(5) == 0).then(|| rng.below(3)),
    }
}

/// A seeded world of one to six registry packages.
fn draw_world(rng: &mut Rng) -> Result<Vec<Subject>, Failure> {
    let count = usize::try_from(rng.between(1, 6))?;
    let mut subjects = Vec::with_capacity(count);
    for position in 0..count {
        let locked = draw_release(rng);
        let mut published: Vec<Published> = Vec::new();
        for _ in 0..rng.between(0, 6) {
            let release = draw_release(rng);
            if published
                .iter()
                .all(|seen| seen.release.rank() != release.rank())
            {
                published.push(Published {
                    release,
                    yanked: rng.below(4) == 0,
                });
            }
        }
        subjects.push(Subject {
            name: format!("crate{position}"),
            locked,
            published,
            listed: rng.below(6) != 0,
        });
    }
    Ok(subjects)
}

/// A registry serving `routes`, each path with its body.
fn registry(routes: &[(String, String)], gate: usize) -> Result<Stub, Box<dyn Error>> {
    let borrowed: Vec<(&str, Reply)> = routes
        .iter()
        .map(|route| (route.0.as_str(), Reply::Body(route.1.clone())))
        .collect();
    Stub::start(&borrowed, gate)
}

/// The registry for `subjects`: a file for each that is listed.
fn registry_for(subjects: &[Subject], gate: usize) -> Result<Stub, Box<dyn Error>> {
    let routes: Vec<(String, String)> = subjects
        .iter()
        .filter(|subject| subject.listed)
        .map(|subject| (subject.path(), subject.index_body()))
        .collect();
    registry(&routes, gate)
}

/// A repository whose workspace member `app` declares every subject directly.
fn repository_of(scratch: &Scratch, subjects: &[Subject]) -> Result<PathBuf, Failure> {
    let names: Vec<&str> = subjects
        .iter()
        .map(|subject| subject.name.as_str())
        .collect();
    let mut packages = vec![Package::local("app", &names)];
    for subject in subjects {
        packages.push(Package::registry(
            &subject.name,
            &subject.locked.spelled(),
            &[],
        ));
    }
    scratch.write_lock(&packages)
}

/// One report row, read back from the JSON array.
#[derive(Debug)]
struct Row {
    name: String,
    latest: Option<String>,
    stale: bool,
    via: Vec<String>,
    error: Option<String>,
}

/// A JSON string as an option: `null` is `None`.
fn text_or_null(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// The rows of a `--json` report, in the order the binary printed them.
fn rows(outcome: &Outcome) -> Result<Vec<Row>, Failure> {
    let report: Value = json::from_str(&outcome.stdout)
        .map_err(|error| format!("stdout is not JSON ({error}): {:?}", outcome.stdout))?;
    let array = report
        .as_array()
        .ok_or_else(|| format!("stdout is not an array: {:?}", outcome.stdout))?;
    array
        .iter()
        .map(|row| {
            Ok(Row {
                name: text_or_null(&row["name"]).ok_or("a row carries no name")?,
                latest: text_or_null(&row["latest"]),
                stale: row["stale"]
                    .as_bool()
                    .ok_or("a row carries no stale flag")?,
                via: row["via"]
                    .as_array()
                    .ok_or("a row carries no via array")?
                    .iter()
                    .filter_map(text_or_null)
                    .collect(),
                error: text_or_null(&row["error"]),
            })
        })
        .collect()
}

/// The exit code the contract gives these counts.
fn expected_exit(stale: usize, errors: usize) -> i32 {
    match (stale, errors) {
        (0, 0) => 0,
        (0, _) => 2,
        _ => 1,
    }
}

#[test]
fn the_binary_agrees_with_an_independent_model_of_the_verdict() -> TestResult {
    replay_sweep("freshness-verdicts", SEEDS, |rng, trace| {
        let world = draw_world(rng)?;
        let stub = registry_for(&world, 0)?;
        let scratch = Scratch::new("freshness-verdicts")?;
        let root = repository_of(&scratch, &world)?;
        let outcome = freshness(&root, &["--json", "--index", &stub.url()])?;
        let found = rows(&outcome)?;
        check(
            found.len() == world.len(),
            format!("{} rows for {} packages", found.len(), world.len()),
        )?;
        let mut stale = 0_usize;
        let mut errors = 0_usize;
        for (subject, row) in world.iter().zip(&found) {
            let model = subject.expected();
            check(row.name == subject.name, format!("order: {row:?}"))?;
            check(
                row.stale == model.stale && row.latest == model.latest,
                format!("{subject:?} reported as {row:?}"),
            )?;
            check(
                row.error.is_some() == model.error,
                format!("{subject:?} error state: {row:?}"),
            )?;
            check(
                row.via == [subject.name.clone()],
                format!("a direct dependency comes through itself: {row:?}"),
            )?;
            stale = stale.saturating_add(usize::from(model.stale));
            errors = errors.saturating_add(usize::from(model.error));
            trace.record_number("stale", usize::from(model.stale));
            match model.latest.as_deref() {
                Some(text) => trace.record(text),
                None => trace.record("none"),
            }
        }
        let code = expected_exit(stale, errors);
        check(
            outcome.code == Some(code),
            format!(
                "exit {:?}, expected {code}: {}",
                outcome.code,
                streams(&outcome)
            ),
        )?;
        trace.record_number("exit", code);
        Ok(())
    })
}

#[test]
fn the_jobs_bound_is_the_most_requests_the_registry_ever_holds() -> TestResult {
    replay_sweep("freshness-bound", SEEDS, |rng, trace| {
        let names = usize::try_from(rng.between(1, 9))?;
        let jobs = usize::try_from(rng.between(1, 9))?;
        let expected = names.min(jobs);
        let world: Vec<Subject> = (0..names).map(Subject::current).collect();
        // The registry answers nothing until `expected` requests are in flight
        // together: a client that looked up fewer at a time never gets an answer.
        let stub = registry_for(&world, expected)?;
        let scratch = Scratch::new("freshness-bound")?;
        let root = repository_of(&scratch, &world)?;
        let outcome = freshness(
            &root,
            &["--jobs", &jobs.to_string(), "--index", &stub.url()],
        )?;
        check(outcome.code == Some(0), streams(&outcome))?;
        check(
            stub.served().len() == names,
            format!("{} of {names} names asked", stub.served().len()),
        )?;
        check(
            stub.peak_in_flight() == expected,
            format!(
                "--jobs {jobs} over {names} names held {} requests at once, expected {expected}",
                stub.peak_in_flight()
            ),
        )?;
        trace.record_number("peak", stub.peak_in_flight());
        Ok(())
    })
}

/// A one-package repository, `lib`, locked at `locked`, and a registry listing
/// `published`.
fn one_package(
    scratch: &Scratch,
    locked: &str,
    published: &[(&str, bool)],
) -> Result<(Stub, PathBuf), Failure> {
    let body = published
        .iter()
        .map(|&(version, yanked)| index_line("lib", version, yanked))
        .collect::<Vec<_>>()
        .join("\n");
    let stub = registry(&[("3/l/lib".to_owned(), body)], 0)?;
    let root = scratch.write_lock(&[
        Package::local("app", &["lib"]),
        Package::registry("lib", locked, &[]),
    ])?;
    Ok((stub, root))
}

#[test]
fn the_three_exit_codes_mean_current_behind_and_unfinished() -> TestResult {
    // 0: the lock is on the newest version.
    let scratch = Scratch::new("exit-current")?;
    let (stub, root) = one_package(&scratch, "1.2.0", &[("1.1.0", false), ("1.2.0", false)])?;
    let current = freshness(&root, &["--index", &stub.url()])?;
    check(current.code == Some(0), streams(&current))?;
    check(
        current.stdout.contains("1 checked, 0 stale, 0 errors"),
        streams(&current),
    )?;

    // 1: a newer version exists.
    let scratch = Scratch::new("exit-behind")?;
    let (stub, root) = one_package(&scratch, "1.1.0", &[("1.1.0", false), ("1.2.0", false)])?;
    let behind = freshness(&root, &["--index", &stub.url()])?;
    check(behind.code == Some(1), streams(&behind))?;
    check(behind.stdout.contains("YES"), streams(&behind))?;
    check(
        behind.stdout.contains("1 checked, 1 stale, 0 errors"),
        streams(&behind),
    )?;

    // 2: the registry has no such crate, so nothing was learned.
    let scratch = Scratch::new("exit-unfinished")?;
    let (_listing, root) = one_package(&scratch, "1.0.0", &[])?;
    let empty = registry(&[], 0)?;
    let unfinished = freshness(&root, &["--index", &empty.url()])?;
    check(unfinished.code == Some(2), streams(&unfinished))?;
    check(
        unfinished.stdout.contains("not in the index"),
        streams(&unfinished),
    )
}

#[test]
fn a_stale_package_is_an_answer_even_when_another_lookup_failed() -> TestResult {
    // The other lookup fails two ways: a server error, and a registry that never
    // finishes and is cut at the read cap. Either costs its own row only.
    let failures = [
        (Reply::Status(500), "HTTP 500"),
        (Reply::Endless, "response larger than"),
    ];
    for (reply, reason) in failures {
        let scratch = Scratch::new("stale-and-failed")?;
        let stub = Stub::start(
            &[
                ("3/o/old", Reply::Body(index_line("old", "2.0.0", false))),
                ("3/b/bad", reply),
            ],
            0,
        )?;
        let root = scratch.write_lock(&[
            Package::local("app", &["old", "bad"]),
            Package::registry("old", "1.0.0", &[]),
            Package::registry("bad", "1.0.0", &[]),
        ])?;
        let outcome = freshness(&root, &["--json", "--index", &stub.url()])?;
        check(outcome.code == Some(1), streams(&outcome))?;
        let found = rows(&outcome)?;
        check(
            found.iter().filter(|row| row.stale).count() == 1,
            format!("{reason}: {found:?}"),
        )?;
        check(
            found.iter().any(|row| {
                row.name == "bad"
                    && row
                        .error
                        .as_deref()
                        .is_some_and(|text| text.contains(reason))
            }),
            format!("the failed lookup keeps its reason, {reason}: {found:?}"),
        )?;
        check(
            outcome.stderr.contains("2 checked, 1 stale, 1 errors"),
            format!(
                "--json says why the code is not zero: {}",
                streams(&outcome)
            ),
        )?;
    }
    Ok(())
}

#[test]
fn a_pre_release_and_a_yanked_version_are_never_the_newest() -> TestResult {
    let scratch = Scratch::new("never-offered")?;
    let (stub, root) = one_package(
        &scratch,
        "1.0.0",
        &[("1.0.0", false), ("1.1.0", true), ("2.0.0-rc.1", false)],
    )?;
    let outcome = freshness(&root, &["--json", "--index", &stub.url()])?;
    let found = rows(&outcome)?;
    check(outcome.code == Some(0), streams(&outcome))?;
    check(
        found
            .iter()
            .all(|row| !row.stale && row.latest.as_deref() == Some("1.0.0")),
        format!("a yanked 1.1.0 and a 2.0.0-rc.1 are not on offer: {found:?}"),
    )?;

    // A lock already on a pre-release has asked for them, and is compared with
    // the pre-releases that follow it.
    let scratch = Scratch::new("on-a-prerelease")?;
    let (stub, root) = one_package(
        &scratch,
        "1.0.0-rc.1",
        &[("1.0.0-rc.1", false), ("1.0.0-rc.2", false)],
    )?;
    let outcome = freshness(&root, &["--json", "--index", &stub.url()])?;
    let found = rows(&outcome)?;
    check(outcome.code == Some(1), streams(&outcome))?;
    check(
        found
            .iter()
            .all(|row| row.stale && row.latest.as_deref() == Some("1.0.0-rc.2")),
        format!("{found:?}"),
    )
}

#[test]
fn versions_are_compared_as_numbers_not_as_text() -> TestResult {
    // "0.10.0" sorts before "0.9.0" as text; it is the newer release.
    let scratch = Scratch::new("numeric-order")?;
    let (stub, root) = one_package(&scratch, "0.9.0", &[("0.9.0", false), ("0.10.0", false)])?;
    let outcome = freshness(&root, &["--json", "--index", &stub.url()])?;
    let found = rows(&outcome)?;
    check(outcome.code == Some(1), streams(&outcome))?;
    check(
        found
            .iter()
            .all(|row| row.stale && row.latest.as_deref() == Some("0.10.0")),
        format!("{found:?}"),
    )
}

#[test]
fn each_row_names_the_direct_dependency_it_comes_through() -> TestResult {
    let scratch = Scratch::new("via")?;
    let routes: Vec<(String, String)> = [
        ("al/ph/alpha", "alpha"),
        ("br/av/bravo", "bravo"),
        ("ch/ar/charlie", "charlie"),
        ("de/lt/delta", "delta"),
    ]
    .iter()
    .map(|&(path, name)| (path.to_owned(), index_line(name, "1.0.0", false)))
    .collect();
    let stub = registry(&routes, 0)?;
    // `app` declares alpha and a path library; the library declares bravo; both
    // registry packages reach charlie; nothing reaches delta.
    let root = scratch.write_lock(&[
        Package::local("app", &["alpha", "path-lib"]),
        Package::registry("alpha", "1.0.0", &["charlie"]),
        Package::registry("bravo", "1.0.0", &["charlie"]),
        Package::registry("charlie", "1.0.0", &[]),
        Package::registry("delta", "1.0.0", &[]),
        Package::local("path-lib", &["bravo"]),
    ])?;
    let outcome = freshness(&root, &["--json", "--index", &stub.url()])?;
    check(outcome.code == Some(0), streams(&outcome))?;
    let found: Vec<(String, Vec<String>)> = rows(&outcome)?
        .into_iter()
        .map(|row| (row.name, row.via))
        .collect();
    let owned = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    let expected: Vec<(String, Vec<String>)> = vec![
        ("alpha".to_owned(), owned(&["alpha"])),
        ("bravo".to_owned(), owned(&["bravo"])),
        ("charlie".to_owned(), owned(&["alpha", "bravo"])),
        ("delta".to_owned(), owned(&[])),
    ];
    check(found == expected, format!("{found:?}"))?;

    let table = freshness(&root, &["--index", &stub.url()])?;
    check(
        table
            .stdout
            .lines()
            .any(|line| line.starts_with("charlie") && line.contains("alpha, bravo")),
        format!("the table shows the same: {}", streams(&table)),
    )
}

#[test]
fn a_package_from_another_source_is_counted_and_never_asked_about() -> TestResult {
    let scratch = Scratch::new("sources")?;
    let stub = registry(
        &[
            ("ok/ay/okay".to_owned(), index_line("okay", "1.0.0", false)),
            (
                "in/ho/inhouse".to_owned(),
                index_line("inhouse", "1.0.0", false),
            ),
        ],
        0,
    )?;
    let own_registry = format!("sparse+{}/", stub.url());
    let root = scratch.write_lock(&[
        Package::local("app", &["okay", "inhouse", "gitdep", "elsewhere"]),
        Package::registry("okay", "1.0.0", &[]),
        Package::from_source("inhouse", "1.0.0", &own_registry, &[]),
        Package::from_source(
            "gitdep",
            "0.3.0",
            "git+https://github.com/example/gitdep?rev=0123456#0123456789abcdef0123456789abcdef01234567",
            &[],
        ),
        Package::from_source(
            "elsewhere",
            "2.0.0",
            "sparse+https://registry.example.invalid/index/",
            &[],
        ),
    ])?;
    let outcome = freshness(&root, &["--index", &stub.url()])?;
    check(outcome.code == Some(0), streams(&outcome))?;
    check(
        stub.served() == ["in/ho/inhouse", "ok/ay/okay"],
        format!("asked {:?}", stub.served()),
    )?;
    check(
        outcome
            .stdout
            .contains("2 checked, 0 stale, 0 errors, 2 skipped (not from the queried index)"),
        streams(&outcome),
    )
}

#[test]
fn a_registry_that_is_gone_is_an_unfinished_check_not_a_current_one() -> TestResult {
    let scratch = Scratch::new("gone")?;
    let subjects = vec![Subject::current(0)];
    let root = repository_of(&scratch, &subjects)?;
    let stub = registry_for(&subjects, 0)?;
    let url = stub.url();
    drop(stub);
    let outcome = freshness(&root, &["--json", "--index", &url])?;
    check(outcome.code == Some(2), streams(&outcome))?;
    let found = rows(&outcome)?;
    check(
        found.iter().all(|row| {
            !row.stale
                && row
                    .error
                    .as_deref()
                    .is_some_and(|reason| reason.contains("curl"))
        }),
        format!("{found:?}"),
    )
}

#[test]
fn a_hollow_answer_is_an_unfinished_check_not_a_current_one() -> TestResult {
    // The review of #754 found an empty 200, and a 200 of blank lines, printed
    // as a row with no error and exit 0: a registry that said nothing counted
    // as a dependency that is current.
    let scratch = Scratch::new("hollow")?;
    let (empty, root) = one_package(&scratch, "1.0.0", &[])?;
    let blank = registry(&[("3/l/lib".to_owned(), "\n  \r\n\n".to_owned())], 0)?;
    for (label, url) in [("empty", empty.url()), ("blank lines", blank.url())] {
        let outcome = freshness(&root, &["--json", "--index", &url])?;
        check(
            outcome.code == Some(2),
            format!("{label}: {}", streams(&outcome)),
        )?;
        let found = rows(&outcome)?;
        check(
            found.iter().all(|row| {
                !row.stale
                    && row.latest.is_none()
                    && row
                        .error
                        .as_deref()
                        .is_some_and(|reason| reason.contains("no version record"))
            }),
            format!("{label}: {found:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn every_refusal_exits_two_before_any_lookup() -> TestResult {
    let scratch = Scratch::new("refusals")?;
    let subjects = vec![Subject::current(0)];
    let root = repository_of(&scratch, &subjects)?;
    let stub = registry_for(&subjects, 0)?;
    let url = stub.url();
    let cases: [(&[&str], &str); 8] = [
        (&["--bogus"], "unknown option for `freshness`: --bogus"),
        (&["--jobs"], "--jobs needs a value"),
        (
            &["--jobs", "0"],
            "--jobs needs a whole number from 1 to 256",
        ),
        (
            &["--jobs", "257"],
            "--jobs needs a whole number from 1 to 256",
        ),
        (
            &["--jobs", "2", "--jobs", "3"],
            "--jobs was given more than once",
        ),
        (
            &["--index", "ftp://registry.invalid"],
            "--index needs an http:// or https:// URL",
        ),
        (
            &["--index", &url, "--index", &url],
            "--index was given more than once",
        ),
        (&["one", "two"], "is a second path"),
    ];
    for (args, reason) in cases {
        let outcome = freshness(&root, args)?;
        check(
            outcome.code == Some(2) && outcome.stderr.contains(reason),
            format!(
                "{args:?} must be refused with {reason:?}: {}",
                streams(&outcome)
            ),
        )?;
        check(
            outcome.stdout.is_empty(),
            format!("{args:?} wrote to stdout: {:?}", outcome.stdout),
        )?;
    }
    check(
        stub.served().is_empty(),
        format!(
            "a refused invocation asked the registry: {:?}",
            stub.served()
        ),
    )
}

#[test]
fn a_directory_with_no_lock_is_refused_and_help_is_not_an_error() -> TestResult {
    let scratch = Scratch::new("no-lock")?;
    let outcome = freshness(scratch.path(), &[])?;
    check(
        outcome.code == Some(2) && outcome.stderr.contains("no Cargo.lock"),
        streams(&outcome),
    )?;
    let help = freshness(scratch.path(), &["--help"])?;
    check(
        help.code == Some(0) && help.stdout.contains("[--jobs N]"),
        streams(&help),
    )
}
