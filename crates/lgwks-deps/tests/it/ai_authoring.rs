//! The `ai-authoring` runner driven as its users drive it: the shipped binary, a
//! benchmark on disk, a real `cargo build --locked --offline` and `cargo test` per
//! trial, and the failures #270 names: a killed run, an oracle past its bound, a
//! mutant the oracle cannot catch, and no orphan process after any of them.
//!
//! The benchmark is a fixture written at run time: two tasks over a crate with no
//! registry dependency, so a trial builds offline in about a second. The runner's
//! unit and simulation tests live beside its source (`sim_*` in the binary); this
//! file holds what only the binary can show. Nothing here calls a model: a dry run
//! compiles reference solutions and a mutant run compiles deliberate mistakes.
//!
//! Synchronisation is by blocking, never by sleeping or polling. A gated oracle
//! connects to a listener this test owns and then blocks reading from it; the
//! test's blocking `accept` is the rendezvous, and the end of the oracle's
//! connection is the proof that its process is gone, because the kernel closes a
//! dead process's sockets. If the runner dies before any oracle connects, a thread
//! that waits for the runner connects to the same listener so the `accept` returns
//! and the test fails with the runner's own words instead of parking.

use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;

use lgwks_std::json::{self, Value};
use lgwks_std::{hex, random};

mod fixture;

use fixture::{
    ALPHA_MUTANT, ALPHA_ORACLE, ALPHA_REFERENCE, BETA_ORACLE, BETA_REFERENCE, LOCK, MANIFEST,
    SUPPORT_MANIFEST, SUPPORT_SOURCE, TEMPLATE,
};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The shipped binary.
const RUNNER: &str = env!("CARGO_BIN_EXE_ai-authoring");

/// The environment variable that gates the second task's oracle: the address of the
/// listener it announces itself to and then blocks on.
const GATE: &str = "AI_AUTH_ANNOUNCE";

/// How long past the oracle bound the gated oracle may block before it gives up on a
/// runner that never stopped it, so a runner that fails to stop it fails a test
/// instead of parking one.
const VALVE_SECONDS: u32 = 60;

/// The oracle bound of the tests that do not exercise it, in seconds.
const ORACLE_BOUND: u32 = 120;

/// The least oracle bound of the test that does, in seconds: what `measured_bound`
/// starts from, long enough for a cold compile of one oracle on an idle host and
/// short enough to wait out.
const SHORT_ORACLE_BOUND: u32 = 12;

/// The most that test will wait out, in seconds.
const LONGEST_SHORT_BOUND: u32 = 90;

/// How many times the cost of one un-gated oracle, measured on this host a moment
/// before, the test's oracle bound is.
const BOUND_MARGIN: f64 = 4.0;

/// A directory removed when the test ends.
struct Dir {
    /// The directory.
    path: PathBuf,
}

impl Dir {
    /// A new directory no other test or run holds.
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let nonce = hex::encode(random::bytes::<8>()?);
        let path = std::env::temp_dir().join(format!("ai-authoring-it-{nonce}-{label}"));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        // Best effort: the tree is this process's own, below the OS temp root, and
        // `Drop` has no failure channel for a test that has already decided.
        if fs::remove_dir_all(&self.path).is_err() {
            // Nothing to report.
        }
    }
}

/// Writes `text` to `relative` under `root`, making its directories.
fn put(root: &Path, relative: &str, text: &str) -> TestResult {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(target, text)?;
    Ok(())
}

/// Runs `git` in `dir`, deaf to the host's configuration, and returns what it printed.
fn git(dir: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

/// A benchmark on disk inside a repository with one commit, and where its run goes.
struct Fixture {
    /// The directory holding everything; removed at the end.
    root: Dir,
    /// The benchmark directory, a repository.
    bench: PathBuf,
    /// Where the run's results go.
    out: PathBuf,
    /// Where the run's trials go.
    work: PathBuf,
    /// The commit the benchmark's repository is at.
    commit: String,
}

impl Fixture {
    /// A benchmark whose oracle bound is `oracle_seconds`.
    fn new(label: &str, oracle_seconds: u32) -> Result<Self, Box<dyn Error>> {
        let root = Dir::new(label)?;
        let bench = root.path.join("bench");
        fs::create_dir(&bench)?;
        put(
            &bench,
            "bench.toml",
            &MANIFEST.replace("ORACLE_SECONDS", &oracle_seconds.to_string()),
        )?;
        put(&bench, "api/sheet.md", "The API sheet.\n")?;
        put(&bench, "template/Cargo.toml", TEMPLATE)?;
        put(&bench, "Cargo.lock", LOCK)?;
        put(&bench, "support/Cargo.toml", SUPPORT_MANIFEST)?;
        put(&bench, "support/src/lib.rs", SUPPORT_SOURCE)?;
        put(&bench, "tasks/alpha/prompt.md", "Answer.\n")?;
        put(&bench, "tasks/alpha/oracle.rs", ALPHA_ORACLE)?;
        put(&bench, "tasks/beta/prompt.md", "Double.\n")?;
        let beta_oracle = BETA_ORACLE.replace("__GATE__", GATE).replace(
            "__VALVE__",
            &oracle_seconds.saturating_add(VALVE_SECONDS).to_string(),
        );
        put(&bench, "tasks/beta/oracle.rs", &beta_oracle)?;
        put(&bench, "reference/solo-alpha.rs", ALPHA_REFERENCE)?;
        put(&bench, "reference/solo-beta.rs", BETA_REFERENCE)?;
        put(&bench, "reference/mutant-alpha.rs", ALPHA_MUTANT)?;
        put(&bench, "reference/mutant-beta.rs", BETA_REFERENCE)?;
        git(&bench, &["init", "-q"])?;
        git(&bench, &["add", "-A", "."])?;
        let tree = git(&bench, &["write-tree"])?;
        let commit = git(&bench, &["commit-tree", &tree, "-m", "fixture"])?;
        git(&bench, &["update-ref", "HEAD", &commit])?;
        let out = root.path.join("out");
        let work = root.path.join("work");
        Ok(Self {
            root,
            bench,
            out,
            work,
            commit,
        })
    }

    /// The command line of a run of `mode` into this fixture's own directories.
    fn run_args(&self, mode: &str) -> Vec<String> {
        let shown = |path: &Path| path.display().to_string();
        vec![
            mode.to_owned(),
            "--bench".to_owned(),
            shown(&self.bench),
            "--out".to_owned(),
            shown(&self.out),
            "--work".to_owned(),
            shown(&self.work),
        ]
    }

    /// Runs the binary with `args` and waits for it.
    fn run(&self, args: &[String]) -> Result<Outcome, Box<dyn Error>> {
        let output = Command::new(RUNNER)
            .args(args)
            .current_dir(&self.root.path)
            .stdin(Stdio::null())
            .output()?;
        Ok(Outcome::of(&output))
    }
}

/// What a finished run said and how it ended.
struct Outcome {
    /// The exit code, or `None` when a signal ended the process.
    code: Option<i32>,
    /// Standard output.
    stdout: String,
    /// Standard error.
    stderr: String,
}

impl Outcome {
    /// The outcome of a finished process.
    fn of(output: &Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// The records of a run's file, one per line.
fn records(file: &Path) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut found = Vec::new();
    for line in fs::read_to_string(file)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        found.push(
            json::from_str::<Value>(line)
                .map_err(|error| format!("{}: {error}", file.display()))?,
        );
    }
    Ok(found)
}

/// The text field `key` of `record`, or a refusal naming it.
fn text<'record>(record: &'record Value, key: &str) -> Result<&'record str, Box<dyn Error>> {
    record
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("the record has no text field {key:?}").into())
}

/// The record of `task`, or a refusal naming it.
fn of_task<'found>(found: &'found [Value], task: &str) -> Result<&'found Value, Box<dyn Error>> {
    for record in found {
        if text(record, "task")? == task {
            return Ok(record);
        }
    }
    Err(format!("the run has no record of task {task:?}").into())
}

/// The verdicts of `record`'s oracle clauses, by clause.
fn verdicts(record: &Value) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let clauses = record
        .get("oracle")
        .and_then(Value::as_object)
        .ok_or("the record has no oracle object")?;
    let mut found = Vec::new();
    for clause in clauses {
        let verdict = clause.1.as_str().ok_or("a verdict is not text")?;
        found.push((clause.0.clone(), verdict.to_owned()));
    }
    Ok(found)
}

/// Whether every clause of `record` passed.
fn all_pass(record: &Value) -> Result<bool, Box<dyn Error>> {
    let found = verdicts(record)?;
    Ok(!found.is_empty() && found.iter().all(|clause| clause.1 == "pass"))
}

/// Whether any live process's command line names `needle`.
fn process_names(needle: &str) -> Result<bool, Box<dyn Error>> {
    let listing = Command::new("ps")
        .args(["-A", "-ww", "-o", "command="])
        .output()?;
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .any(|line| line.contains(needle)))
}

/// Source: the issue's acceptance, "the run is the full planned run, launched once":
/// the command line is the user's whole interface, so every refusal must leave the
/// disk untouched and say what was wrong. Requirement: help prints the usage; no
/// intent, two intents, an unknown task, a missing run and a benchmark with no
/// manifest are each refused with exit 2, a message naming the cause, and nothing on
/// standard output.
#[test]
fn help_and_every_refusal_exit_as_documented() -> TestResult {
    let fixture = Fixture::new("usage", ORACLE_BOUND)?;
    let say = |args: &[&str]| -> Result<Outcome, Box<dyn Error>> {
        fixture.run(
            &args
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    let help = say(&["--help"])?;
    assert!(
        help.code == Some(0)
            && help.stdout.contains("--dry-run")
            && help.stdout.contains("--matrix")
            && help.stderr.is_empty(),
        "help must print the usage and nothing else: {:?} {:?}",
        help.stdout,
        help.stderr
    );
    let bench = fixture.bench.display().to_string();
    let root = fixture.root.path.display().to_string();
    let refusals: [(Vec<&str>, &[&str]); 5] = [
        (vec!["--trials", "3"], &["say what to run"]),
        (vec!["--dry-run", "--matrix"], &["separate runs"]),
        (
            vec![
                "--dry-run",
                "--bench",
                bench.as_str(),
                "--tasks",
                "nonesuch",
            ],
            &["unknown task \"nonesuch\"", "alpha, beta"],
        ),
        (vec!["--tables", bench.as_str()], &["results.jsonl"]),
        (vec!["--dry-run", "--bench", root.as_str()], &["bench.toml"]),
    ];
    for refusal in refusals {
        let said = say(&refusal.0)?;
        assert!(
            said.code == Some(2) && said.stdout.is_empty(),
            "{:?} must be refused with exit 2 and nothing on stdout: {:?} {:?} {:?}",
            refusal.0,
            said.code,
            said.stdout,
            said.stderr
        );
        for needle in refusal.1 {
            assert!(
                said.stderr.contains(needle),
                "{:?} must be refused naming `{needle}`: {:?}",
                refusal.0,
                said.stderr
            );
        }
    }
    assert!(
        !fixture.bench.join("runs").exists(),
        "a refused run created the benchmark's runs directory"
    );
    Ok(())
}

/// Source: #270 ("the harness keeps ... per-trial sandbox, ... committed `runs/`"):
/// a dry run is the whole pipeline with reference solutions standing in for a model.
/// Requirement: every cell compiles and passes through real cargo; results come
/// out sorted and whole, the log is retired, the commit is recorded, each trial
/// leaves its solution; the tables read the run back; a second run into the same
/// directories is refused with the first untouched; no process outlives the run.
#[test]
fn a_dry_run_grades_every_reference_through_real_cargo_and_writes_a_whole_run() -> TestResult {
    let fixture = Fixture::new("dry", ORACLE_BOUND)?;
    let args = fixture.run_args("--dry-run");
    let run = fixture.run(&args)?;
    assert_eq!(
        run.code,
        Some(0),
        "the dry run must finish clean: {:?} {:?}",
        run.stdout,
        run.stderr
    );
    let results = fixture.out.join("results.jsonl");
    let found = records(&results)?;
    let tasks: Vec<&str> = found
        .iter()
        .map(|record| text(record, "task"))
        .collect::<Result<_, _>>()?;
    assert_eq!(tasks, ["alpha", "beta"], "one record per task, sorted");
    for record in &found {
        assert!(
            all_pass(record)? && record.get("compiled").and_then(Value::as_bool) == Some(true),
            "the reference of {} must compile and pass every clause: {:?}",
            text(record, "task")?,
            verdicts(record)?
        );
        assert_eq!(
            text(record, "lgwks_bot_sha")?,
            fixture.commit,
            "a record names the commit it ran at"
        );
        assert_eq!(text(record, "mode")?, "dry", "a dry run's records say so");
    }
    assert!(
        !fixture.out.join("results.partial.jsonl").exists(),
        "a finished run retires its log"
    );
    let summary = json::from_str::<Value>(&fs::read_to_string(fixture.out.join("summary.json"))?)
        .map_err(|error| format!("summary.json is not JSON: {error}"))?;
    assert_eq!(
        text(&summary, "generated_by")?,
        "ai-authoring",
        "the summary names its generator"
    );
    let kept = fs::read_to_string(
        fixture
            .work
            .join("__dry-run____solo__beta__plain__t0")
            .join("lib.rs"),
    )?;
    assert_eq!(
        kept, BETA_REFERENCE,
        "the trial keeps the solution it graded"
    );
    let tables = fixture.run(&["--tables".to_owned(), fixture.out.display().to_string()])?;
    assert!(
        tables.code == Some(0) && tables.stdout.contains("| solo | plain | 2 | 2/2 |"),
        "the tables must read the run back: {:?} {:?}",
        tables.stdout,
        tables.stderr
    );
    let before = fs::read_to_string(&results)?;
    let again = fixture.run(&args)?;
    assert!(
        again.code == Some(2) && again.stdout.is_empty() && again.stderr.contains("File exists"),
        "a second run into the same directories must be refused: {:?} {:?}",
        again.stdout,
        again.stderr
    );
    assert_eq!(
        fs::read_to_string(&results)?,
        before,
        "a refused run left the first run's results alone"
    );
    assert!(
        !process_names(&fixture.work.display().to_string())?,
        "a process of the finished run is still alive"
    );
    Ok(())
}

/// Source: #270's negative controls ("negative controls for every oracle"). Requirement:
/// a mutant the oracle fails is reported as caught with the clause that caught it;
/// a mutant no clause fails is reported as not caught and the run exits 1, so a hole
/// in an oracle is never a green run.
#[test]
fn a_mutant_run_names_the_clause_that_caught_each_mutant_and_exits_one_for_a_survivor() -> TestResult
{
    let fixture = Fixture::new("mutants", ORACLE_BOUND)?;
    let run = fixture.run(&fixture.run_args("--mutants"))?;
    assert_eq!(
        run.code,
        Some(1),
        "a surviving mutant must fail the run: {:?} {:?}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.stdout.contains("did NOT catch the mutant"),
        "the survivor must be named in the report: {:?}",
        run.stdout
    );
    let found = records(&fixture.out.join("results.jsonl"))?;
    let alpha = of_task(&found, "alpha")?;
    let beta = of_task(&found, "beta")?;
    let caught: Vec<String> = verdicts(alpha)?
        .into_iter()
        .filter(|clause| clause.1 == "fail")
        .map(|clause| clause.0)
        .collect();
    assert_eq!(
        caught,
        ["answers_forty_two"],
        "the wrong answer fails exactly the clause that checks it"
    );
    assert!(
        all_pass(beta)?,
        "the surviving mutant passes every clause: {:?}",
        verdicts(beta)?
    );
    assert_eq!(
        text(alpha, "mode")?,
        "mutant",
        "a mutant run's records say so"
    );
    Ok(())
}

/// A runner started with the second oracle gated, and what the gate showed.
struct Gated {
    /// How the runner ended.
    runner: Outcome,
    /// What the oracle sent after announcing itself; it holds `VALVE` only if the
    /// oracle outlived its runner's stop and gave up by itself.
    after_announcement: String,
}

/// The runner ended before the gated oracle announced itself: the oracle was stopped
/// at its bound before it reached its gate, or the runner failed. It is a type of its
/// own so that a caller can tell it from every other refusal.
#[derive(Debug)]
struct Unannounced {
    /// What the test saw, with the runner's own words.
    message: String,
}

impl std::fmt::Display for Unannounced {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(&self.message)
    }
}

impl Error for Unannounced {}

/// Kills `pid` with `KILL`.
fn kill_nine(pid: u32) -> TestResult {
    let status = Command::new("kill")
        .args(["-s", "KILL", &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -s KILL {pid} failed").into())
    }
}

/// Waits for the gated oracle to announce itself, optionally kills the runner then,
/// and reads until the oracle's connection ends: the end is its process gone.
fn observe(listener: &TcpListener, runner: u32, kill: bool) -> Result<String, Box<dyn Error>> {
    let (stream, _) = listener.accept()?;
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    if first.trim().parse::<u32>().is_err() {
        lgwks_std::trace::debug!(announcement = ?first, "no oracle announced itself");
        return Err(Box::new(Unannounced {
            message: format!("the runner ended before an oracle announced itself; read {first:?}"),
        }));
    }
    if kill {
        kill_nine(runner)?;
    }
    let mut rest = String::new();
    reader.read_to_string(&mut rest)?;
    Ok(rest)
}

/// Runs the binary with the second oracle gated; with `kill`, kills it with `KILL`
/// the moment that oracle is running.
fn gated(fixture: &Fixture, args: &[String], kill: bool) -> Result<Gated, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let child = Command::new(RUNNER)
        .args(args)
        .env(GATE, address.to_string())
        .current_dir(&fixture.root.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let runner = child.id();
    thread::scope(|scope| -> Result<Gated, Box<dyn Error>> {
        let waiter = scope.spawn(move || {
            let finished = child.wait_with_output();
            if TcpStream::connect(address).is_err() {
                // The listener is gone already, which is what the connect was for.
            }
            finished
        });
        let observed = observe(&listener, runner, kill);
        let finished = waiter.join().map_err(|payload| {
            format!("the thread waiting for the runner panicked: {payload:?}")
        })??;
        let ended = Outcome::of(&finished);
        match observed {
            Ok(after_announcement) => Ok(Gated {
                runner: ended,
                after_announcement,
            }),
            Err(error) => {
                let message = format!(
                    "{error}; the runner printed {:?} and {:?}",
                    ended.stdout, ended.stderr
                );
                if error.downcast_ref::<Unannounced>().is_some() {
                    Err(Box::new(Unannounced { message }))
                } else {
                    Err(message.into())
                }
            }
        }
    })
}

/// Runs the dry run with the second oracle gated under an oracle bound of `first`
/// seconds, and under twice that while the runner ended before the oracle announced
/// itself, up to `LONGEST_SHORT_BOUND`. The bound covers the oracle's whole `cargo
/// test`, compile included, so an oracle cut off before its gate says the host's load
/// moved after the bound was measured, which the stop under test did not cause; one
/// that announced itself and then misbehaves is never run again. Returns the fixture,
/// what the gate showed, and the bound that reached the gate.
fn gated_within(first: u32) -> Result<(Fixture, Gated, u32), Box<dyn Error>> {
    let mut seconds = first;
    loop {
        let fixture = Fixture::new("timeout", seconds)?;
        match gated(&fixture, &fixture.run_args("--dry-run"), false) {
            Ok(shown) => return Ok((fixture, shown, seconds)),
            Err(error)
                if error.downcast_ref::<Unannounced>().is_some()
                    && seconds < LONGEST_SHORT_BOUND =>
            {
                lgwks_std::trace::debug!(bound = ?seconds, "the oracle was cut off before its gate");
                seconds = seconds.saturating_mul(2).min(LONGEST_SHORT_BOUND);
            }
            Err(error) => {
                lgwks_std::trace::debug!(bound = ?seconds, failure = ?error, "the gated run failed");
                return Err(format!("under a bound of {seconds} s: {error}").into());
            }
        }
    }
}

/// The oracle bound, in seconds, that the un-gated task fits inside on this host
/// now: `BOUND_MARGIN` times the wall time one dry-run oracle of that task takes,
/// from `SHORT_ORACLE_BOUND` up to `LONGEST_SHORT_BOUND`. An oracle run is a whole
/// `cargo test`, compile included, so what it costs is the host's load and not a
/// constant a test can write down.
fn measured_bound() -> Result<u32, Box<dyn Error>> {
    let fixture = Fixture::new("measure", ORACLE_BOUND)?;
    let mut args = fixture.run_args("--dry-run");
    args.extend(["--tasks".to_owned(), "alpha".to_owned()]);
    let run = fixture.run(&args)?;
    assert_eq!(
        run.code,
        Some(0),
        "the measuring dry run must finish clean: {:?} {:?}",
        run.stdout,
        run.stderr
    );
    let found = records(&fixture.out.join("results.jsonl"))?;
    let cost = of_task(&found, "alpha")?
        .get("oracle_wall_ms")
        .and_then(Value::as_f64)
        .ok_or("the measuring run recorded no oracle_wall_ms for alpha")?;
    let mut seconds = SHORT_ORACLE_BOUND;
    while seconds < LONGEST_SHORT_BOUND && f64::from(seconds) * 1000.0 < cost * BOUND_MARGIN {
        seconds = seconds.saturating_add(1);
    }
    Ok(seconds)
}

/// Source: #270's "process-group kill ... a 1,800 s oracle timeout recorded as a failure"
/// and "no orphan process after a run"; CI run 38985141189, where the un-gated task's
/// own `cargo test` ran past 12 s at a host load of 70 and read `timeout`, and main run
/// 39006985065, where the gated task's oracle never announced itself (`read ""`) inside
/// a bound measured moments earlier, on a host whose load had run from 3.5 to 27. A
/// bound that cut the oracle off before its gate is widened and the run repeated. Requirement:
/// an oracle still running at its bound is stopped as a group (its process is gone
/// when the runner returns), its clauses are recorded `timeout` after at least the
/// bound, the other task is graded as usual under a bound measured on this host, the
/// run is not a crash and exits 0.
#[test]
fn an_oracle_past_its_bound_is_stopped_with_its_group_and_recorded_as_a_timeout() -> TestResult {
    let (fixture, shown, bound) = gated_within(measured_bound()?)?;
    assert!(
        !shown.after_announcement.contains("VALVE"),
        "the oracle outlived its bound and was stopped only by its own valve"
    );
    assert_eq!(
        shown.runner.code,
        Some(0),
        "a timeout is a failure of the solution, not a crash: {:?}",
        shown.runner.stderr
    );
    let found = records(&fixture.out.join("results.jsonl"))?;
    let alpha = of_task(&found, "alpha")?;
    let beta = of_task(&found, "beta")?;
    assert!(
        all_pass(alpha)?,
        "the task that was not gated passes: {:?}",
        verdicts(alpha)?
    );
    let stopped = verdicts(beta)?;
    assert!(
        !stopped.is_empty() && stopped.iter().all(|clause| clause.1 == "timeout"),
        "every clause the stopped oracle had not finished is a timeout: {stopped:?}"
    );
    assert!(
        beta.get("error").is_none(),
        "a timeout is a recorded failure, not a crashed trial"
    );
    assert_eq!(
        beta.get("cleanup_ok").and_then(Value::as_bool),
        Some(false),
        "a timed-out drop clause is not a clean one"
    );
    assert!(
        beta.get("oracle_wall_ms")
            .and_then(Value::as_f64)
            .is_some_and(|wall| wall >= f64::from(bound) * 1000.0),
        "the oracle ran its whole bound before it was stopped: {:?}",
        beta.get("oracle_wall_ms")
    );
    assert!(
        !process_names(&fixture.work.display().to_string())?,
        "a process of the run is still alive"
    );
    Ok(())
}

/// Source: the Director's contract ("`kill -9` and restart") and #270's "no orphan
/// process after a run". Requirement: a runner killed with `KILL` while an oracle is
/// running leaves no oracle behind (its watchdog stops the group), keeps the
/// finished trial in its log, and `--resume` runs only the interrupted one, sets its
/// directory aside rather than deleting it, and ends with the same results a clean
/// run has, the kept record byte for byte.
#[test]
fn a_killed_run_leaves_no_oracle_behind_and_resumes_to_a_whole_run() -> TestResult {
    let fixture = Fixture::new("killed", ORACLE_BOUND)?;
    let killed = gated(&fixture, &fixture.run_args("--dry-run"), true)?;
    assert!(
        !killed.after_announcement.contains("VALVE"),
        "the oracle survived its runner's death until its own valve fired"
    );
    assert_eq!(killed.runner.code, None, "the runner was ended by a signal");
    assert!(
        !process_names(&fixture.work.display().to_string())?,
        "an oracle outlived the runner that was killed"
    );
    let partial = fixture.out.join("results.partial.jsonl");
    let kept = fs::read_to_string(&partial)?;
    assert_eq!(
        kept.lines().count(),
        1,
        "exactly the finished trial is in the log: {kept:?}"
    );
    assert!(
        !fixture.out.join("results.jsonl").exists(),
        "an interrupted run has no results yet"
    );
    let resume = fixture.run(&[
        "--dry-run".to_owned(),
        "--bench".to_owned(),
        fixture.bench.display().to_string(),
        "--resume".to_owned(),
        fixture.out.display().to_string(),
    ])?;
    assert_eq!(
        resume.code,
        Some(0),
        "the resumed run must finish clean: {:?} {:?}",
        resume.stdout,
        resume.stderr
    );
    assert!(
        resume.stdout.contains("resuming: 1 trial(s) kept, 1 to run, 1 interrupted directory(ies) set aside, 0 torn log line(s) dropped"),
        "the resume must say what it kept and set aside: {:?}",
        resume.stdout
    );
    let found = records(&fixture.out.join("results.jsonl"))?;
    assert_eq!(found.len(), 2, "both trials are in the finished run");
    for record in &found {
        assert!(
            all_pass(record)?,
            "{} must pass after the resume: {:?}",
            text(record, "task")?,
            verdicts(record)?
        );
    }
    let results = fs::read_to_string(fixture.out.join("results.jsonl"))?;
    assert_eq!(
        results.lines().next(),
        kept.lines().next(),
        "the kept trial's record is carried over byte for byte"
    );
    assert!(
        fixture
            .work
            .join("__dry-run____solo__beta__plain__t0.interrupted-1")
            .is_dir(),
        "the interrupted trial's directory is set aside, not deleted"
    );
    assert!(!partial.exists(), "the finished run retired its log");
    Ok(())
}
