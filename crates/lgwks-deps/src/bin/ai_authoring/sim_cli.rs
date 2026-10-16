//! Simulated command lines: every flag the runner has is written in a random
//! order with random values and read back, every way to get one wrong is refused
//! with the flag's name, and the whole entry point is driven through its public
//! `execute` over a console the test reads.
//!
//! A run of this benchmark can cost real money (`--matrix`), so the command line
//! is the first safety: it must never read two intents as one, and it must refuse
//! what it does not understand instead of guessing.

use std::fs;
use std::path::{Path, PathBuf};

use super::BenchError;
use super::cli::{Action, USAGE, parse};
use super::execute;
use super::plan::Mode;
use super::replay::{Failure, replay_sweep};
use super::seed::Rng;
use super::sim_kit::{BENCH_DIR, Scratch, console, count, ensure, one_of, shuffle};

/// Words the names in a list are drawn from.
const NAMES: [&str; 5] = ["alpha", "beta", "gamma", "delta", "epsilon"];

/// Every flag that takes a value.
const VALUED: [&str; 13] = [
    "--tables",
    "--bench",
    "--resume",
    "--out",
    "--work",
    "--cmd",
    "--models",
    "--apis",
    "--tasks",
    "--profiles",
    "--trials",
    "--max-repairs",
    "--parallel",
];

/// The benchmark directory a line that names none is read as.
const DEFAULT_BENCH: &str = "bench/ai-authoring";

/// The flags that choose a run, and the mode each one is.
const RUNS: [(&str, Mode); 3] = [
    ("--dry-run", Mode::Dry),
    ("--mutants", Mode::Mutants),
    ("--matrix", Mode::Models),
];

/// The flags whose value is a count.
const COUNTED: [&str; 3] = ["--trials", "--parallel", "--max-repairs"];

/// Values a count must refuse: zero, negative, text, empty, fractional, too large.
const BAD_COUNTS: [&str; 6] = ["0", "-1", "x", "", "1.5", "99999999999"];

/// What a drawn command line must be read as.
struct Wanted {
    /// Whether help was asked for.
    help: bool,
    /// The run asked for, when one was.
    mode: Option<Mode>,
    /// The directory `--tables` named.
    tables: Option<String>,
    /// The benchmark directory the line named.
    bench: Option<String>,
    /// The models, arms, tasks and profiles the line listed, in that order.
    lists: [Vec<String>; 4],
    /// Trials per cell.
    trials: Option<u32>,
    /// Trials in flight.
    parallel: Option<usize>,
    /// Repairs per trial.
    max_repairs: Option<u32>,
    /// The model command.
    command: Option<String>,
    /// The results directory.
    out: Option<String>,
    /// The work root.
    work: Option<String>,
    /// The run to resume.
    resume: Option<String>,
}

/// A comma-separated list the way a hurried user writes one: stray blanks and
/// empty items. The text, then the names it holds.
fn list_text(rng: &mut Rng) -> Result<(String, Vec<String>), Failure> {
    let mut names = Vec::new();
    let mut text = String::new();
    for _ in 0..count(rng, 0, 3)? {
        let name = (*one_of(rng, "names", &NAMES)?).to_owned();
        if rng.coin() {
            text.push(' ');
        }
        text.push_str(&name);
        if rng.coin() {
            text.push(' ');
        }
        text.push(',');
        if rng.chance(300) {
            text.push(',');
        }
        names.push(name);
    }
    Ok((text, names))
}

/// A value for a flag that wants a path or a word.
fn word(rng: &mut Rng, stem: &str) -> String {
    format!("{stem}-{}", rng.below(1000))
}

/// What a line with no flags asks for.
fn nothing() -> Wanted {
    Wanted {
        help: false,
        mode: None,
        tables: None,
        bench: None,
        lists: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        trials: None,
        parallel: None,
        max_repairs: None,
        command: None,
        out: None,
        work: None,
        resume: None,
    }
}

/// A command line in random order, and what it must be read as.
fn draw_line(rng: &mut Rng) -> Result<(Vec<String>, Wanted), Failure> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut wanted = nothing();
    if rng.chance(150) {
        wanted.help = true;
        groups.push(vec![(*one_of(rng, "help", &["--help", "-h"])?).to_owned()]);
    }
    if rng.chance(250) {
        let dir = word(rng, "runs");
        groups.push(vec!["--tables".to_owned(), dir.clone()]);
        wanted.tables = Some(dir);
    } else {
        let chosen = one_of(rng, "modes", &RUNS)?;
        groups.push(vec![chosen.0.to_owned()]);
        wanted.mode = Some(chosen.1);
    }
    for (index, flag) in ["--models", "--apis", "--tasks", "--profiles"].iter().enumerate() {
        if rng.coin() {
            let (text, names) = list_text(rng)?;
            groups.push(vec![(*flag).to_owned(), text]);
            if let Some(slot) = wanted.lists.get_mut(index) {
                *slot = names;
            }
        }
    }
    if rng.coin() {
        let trials = rng.between(1, 1000);
        groups.push(vec!["--trials".to_owned(), trials.to_string()]);
        wanted.trials = Some(trials);
    }
    if rng.coin() {
        let workers = rng.between(1, 64);
        groups.push(vec!["--parallel".to_owned(), workers.to_string()]);
        wanted.parallel = Some(usize::try_from(workers)?);
    }
    if rng.coin() {
        let repairs = rng.between(1, 20);
        groups.push(vec!["--max-repairs".to_owned(), repairs.to_string()]);
        wanted.max_repairs = Some(repairs);
    }
    for (flag, slot) in [
        ("--bench", &mut wanted.bench),
        ("--cmd", &mut wanted.command),
        ("--out", &mut wanted.out),
        ("--work", &mut wanted.work),
        ("--resume", &mut wanted.resume),
    ] {
        if rng.coin() {
            let value = word(rng, "value");
            groups.push(vec![flag.to_owned(), value.clone()]);
            *slot = Some(value);
        }
    }
    shuffle(rng, &mut groups)?;
    Ok((groups.concat(), wanted))
}

/// A path option as the text it was given.
fn shown(found: &Option<PathBuf>) -> Option<String> {
    found.as_deref().map(|dir| dir.display().to_string())
}

/// Source: POSIX Utility Syntax Guidelines 11 and 12 (options may appear in any
/// order, each option-argument follows its option). Requirement: every flag the
/// runner has is read as written whatever order the line puts them in, a list
/// drops its blanks and empty items, and `--help` outranks everything.
#[test]
fn every_flag_is_read_as_written_in_whatever_order_the_line_puts_them() -> Result<(), Failure> {
    replay_sweep("flags", 500, |rng, trace| {
        let (line, wanted) = draw_line(rng)?;
        let options = parse(&line)?;
        let action_ok = match options.action {
            Action::Help => wanted.help,
            Action::Tables(ref dir) => {
                !wanted.help && wanted.tables.as_deref().map(PathBuf::from).as_ref() == Some(dir)
            }
            Action::Run(mode) => !wanted.help && wanted.tables.is_none() && wanted.mode == Some(mode),
        };
        ensure(action_ok, || format!("{line:?} was read as the wrong action"))?;
        ensure(
            options.models == wanted.lists[0]
                && options.arms == wanted.lists[1]
                && options.tasks == wanted.lists[2]
                && options.profiles == wanted.lists[3],
            || format!("{line:?} lost a list or its names"),
        )?;
        ensure(
            options.trials == wanted.trials
                && options.parallel == wanted.parallel
                && options.max_repairs == wanted.max_repairs
                && options.command == wanted.command,
            || format!("{line:?} lost a number or the command"),
        )?;
        let bench = match wanted.bench {
            Some(ref given) => PathBuf::from(given),
            None => PathBuf::from(DEFAULT_BENCH),
        };
        ensure(
            shown(&options.out) == wanted.out
                && shown(&options.work) == wanted.work
                && shown(&options.resume) == wanted.resume
                && options.bench == bench,
            || format!("{line:?} lost a path"),
        )?;
        trace.record(&line.join(" "));
        Ok(())
    })
}

/// A wrong command line, what the refusal must say, and what the line got wrong.
struct Refusal {
    /// The line.
    line: Vec<String>,
    /// Every phrase the refusal must hold.
    needles: Vec<String>,
    /// What the line got wrong, for the failure message.
    what: &'static str,
}

/// The words as owned text.
fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// The refusal of `line`, as a usage error's text; a failure when it is allowed or
/// is another kind of error.
fn refused(line: &[String]) -> Result<String, Failure> {
    match parse(line) {
        Ok(_) => Err(format!("{line:?} was allowed").into()),
        Err(BenchError::Usage(said)) => Ok(said),
        Err(other) => Err(format!("{line:?} was refused as {other:?}, not as a usage error").into()),
    }
}

/// Six wrong lines drawn from the sequence, each with the refusal it must get.
fn wrong_lines(rng: &mut Rng) -> Result<Vec<Refusal>, Failure> {
    let stray = format!("--nonesuch{}", rng.below(1000));
    let flag = *one_of(rng, "valued flags", &VALUED)?;
    let counted = *one_of(rng, "counted flags", &COUNTED)?;
    let bad = *one_of(rng, "bad counts", &BAD_COUNTS)?;
    let run = one_of(rng, "run flags", &RUNS)?.0;
    let other = one_of(rng, "other run flags", &RUNS)?.0;
    Ok(vec![
        Refusal {
            line: owned(&[run, &stray]),
            needles: vec![format!("unknown argument {stray:?}")],
            what: "a flag it does not know",
        },
        Refusal {
            line: owned(&[run, flag]),
            needles: vec![format!("{flag} needs a value")],
            what: "a flag with no value",
        },
        Refusal {
            line: owned(&[run, counted, bad]),
            needles: vec![
                counted.to_owned(),
                "whole number above zero".to_owned(),
                format!("{bad:?}"),
            ],
            what: "a count that is not a whole number above zero",
        },
        Refusal {
            line: owned(&[run, other]),
            needles: vec!["separate runs".to_owned()],
            what: "two runs at once",
        },
        Refusal {
            line: owned(&["--tables", "dir", run]),
            needles: vec!["separate runs".to_owned()],
            what: "tables and a run at once",
        },
        Refusal {
            line: owned(&[counted, "3"]),
            needles: vec!["say what to run".to_owned()],
            what: "no intent at all",
        },
    ])
}

/// Source: the runner's own contract, since `--matrix` calls paid models.
/// Requirement: a flag it does not know, a value it was not given, a count that
/// is not a whole number above zero, two intents at once, and no intent at all
/// are each refused as a usage error that names what was wrong.
#[test]
fn a_command_line_that_is_wrong_is_refused_naming_what_was_wrong() -> Result<(), Failure> {
    replay_sweep("refusals", 400, |rng, trace| {
        for case in wrong_lines(rng)? {
            let said = refused(&case.line)?;
            ensure(
                case.needles.iter().all(|needle| said.contains(needle.as_str())),
                || format!("{} was refused as `{said}`", case.what),
            )?;
            trace.record(&said);
        }
        Ok(())
    })
}

/// The committed runs that hold results, by name.
fn committed_runs() -> Result<Vec<PathBuf>, Failure> {
    let mut runs = Vec::new();
    for entry in fs::read_dir(PathBuf::from(BENCH_DIR).join("runs"))? {
        let path = entry?.path();
        if path.join("results.jsonl").is_file() {
            runs.push(path);
        }
    }
    runs.sort();
    Ok(runs)
}

/// What `execute` did for `parts`: its code, then what it said on each stream.
fn executed(parts: &[String]) -> (u8, String, String) {
    let (shown_to, out, err) = console();
    let code = execute(parts, &shown_to);
    (code, out.text(), err.text())
}

/// What `--tables` said for `run`: the code, then standard output and standard error.
fn reported(run: &Path) -> (u8, String, String) {
    executed(&["--tables".to_owned(), run.display().to_string()])
}

/// Source: the process contract of `execute` (0 clean, 2 refused). Requirement:
/// help prints the usage and nothing else; a refusal prints to standard error and
/// exits 2 with standard output untouched; the newest committed run is reported;
/// and every committed run is either reported with the same bytes each time or
/// refused by its first unreadable line, never read with invented values (a run
/// from before the `profile` field existed has no profile to report).
#[test]
fn the_entry_point_prints_help_refuses_on_stderr_and_reports_or_refuses_each_committed_run() -> Result<(), Failure> {
    let runs = committed_runs()?;
    let newest = runs.last().ok_or("the benchmark commits no run to report")?;
    ensure(reported(newest).0 == 0, || format!("the newest run, {}, is not reported", newest.display()))?;
    replay_sweep("execute", 60, |rng, trace| {
        let run = one_of(rng, "committed runs", &runs)?;
        let help = executed(&owned(&["--help"]));
        ensure(help.0 == 0 && help.1 == format!("{USAGE}\n") && help.2.is_empty(), || {
            "help is the usage and nothing else".to_owned()
        })?;
        let wrong = executed(&owned(&["--nonesuch"]));
        ensure(wrong.0 == 2 && wrong.1.is_empty() && wrong.2.contains("unknown argument"), || {
            "a refusal must go to standard error with code 2".to_owned()
        })?;
        let first = reported(run);
        ensure(first == reported(run), || format!("two reads of {} differ", run.display()))?;
        let sound = if first.0 == 0 {
            first.1.starts_with("## Per-(api, profile)") && first.2.is_empty()
        } else {
            first.0 == 2 && first.1.is_empty() && first.2.contains("is not a whole trial record")
        };
        ensure(sound, || format!("{} was reported as {first:?}", run.display()))?;
        trace.record_number("code", first.0);
        trace.record(&first.1);
        Ok(())
    })
}

/// Requirement: a run directory that is missing, empty, or corrupt is refused
/// with its path or its line, never reported as an empty table; a benchmark
/// directory with no manifest is refused naming the manifest.
#[test]
fn a_missing_or_corrupt_run_and_a_missing_manifest_are_refused_by_name() -> Result<(), Failure> {
    replay_sweep("bad inputs", 40, |rng, trace| {
        let scratch = Scratch::new("cli")?;
        let dir = scratch.path().display().to_string();
        let empty = executed(&["--tables".to_owned(), dir.clone()]);
        ensure(empty.0 == 2 && empty.1.is_empty() && empty.2.contains("results.jsonl"), || {
            format!("an empty run directory was reported: {}", empty.2)
        })?;
        fs::write(scratch.path().join("results.jsonl"), format!("{{\"half\": {}\n", rng.below(100)))?;
        let corrupt = executed(&["--tables".to_owned(), dir.clone()]);
        ensure(
            corrupt.0 == 2 && corrupt.1.is_empty() && corrupt.2.contains("results line 1"),
            || format!("a corrupt run was reported: {}", corrupt.2),
        )?;
        let bare = executed(&["--dry-run".to_owned(), "--bench".to_owned(), dir]);
        ensure(bare.0 == 2 && bare.2.contains("bench.toml"), || {
            format!("a missing manifest was not named: {}", bare.2)
        })?;
        trace.record_number("codes", format!("{}{}{}", empty.0, corrupt.0, bare.0));
        Ok(())
    })
}
