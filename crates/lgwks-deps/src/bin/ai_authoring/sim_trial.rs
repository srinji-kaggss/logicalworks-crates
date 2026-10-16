//! Simulated trials: the author, the compiler and the oracle are scripted, one
//! seed drives every draw, and the real repair loop runs over them.
//!
//! Each seed builds a different trial (how many repairs the budget allows, which
//! replies carry code, which of that code compiles, what the oracle log says of
//! each clause, whether the oracle timed out) and an independent model of the
//! loop says what the record must hold. The same seed must then replay to the
//! same trace hash, so the loop has no clock, hash-order or thread dependence.

use std::collections::{BTreeMap, BTreeSet};

use super::BenchError;
use super::bench::Bench;
use super::grade::Verdict;
use super::plan::{Job, Mode};
use super::replay::{Failure, refuse, replay_sweep};
use super::scaffold::oracle_for;
use super::seed::{Rng, Trace};
use super::sim::receipt;
use super::sim_kit::{
    COMPILES, Floor, Graded, Scripted, close, committed, count, ensure, job, one_of, reply,
};
use super::trial::{Finished, Inputs, Start, run_trial};

/// The prompt every simulated author is first asked.
const ORIGINAL: &str = "ORIGINAL PROMPT";

/// The commit every simulated trial claims to have run at.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// What the benchmark declares and what its committed oracles say.
struct World {
    /// The committed benchmark.
    bench: Bench,
    /// Each task's oracle clauses, in source order.
    clauses: BTreeMap<String, Vec<String>>,
}

/// The committed benchmark and every task's oracle clauses.
fn world() -> Result<World, Failure> {
    let bench = committed()?;
    let mut clauses = BTreeMap::new();
    for task in &bench.tasks {
        let (_, names) = oracle_for(&bench, &task.name, "ai_trial_sim")?;
        clauses.insert(task.name.clone(), names);
    }
    Ok(World { bench, clauses })
}

/// One reply the scripted author gives.
struct Attempt {
    /// The reply's text.
    text: String,
    /// The solution the reply carries, when it carries one.
    code: Option<String>,
    /// Whether the scripted judge compiles that solution.
    compiles: bool,
    /// Code lines the solution has.
    lines: usize,
    /// Orchestration tokens the solution has.
    sites: usize,
    /// Tokens the call read.
    tokens_in: u64,
    /// Tokens the call wrote.
    tokens_out: u64,
    /// The model's own wall time.
    wall_ms: u64,
}

/// The `index`th reply: prose about one time in four, otherwise a crate that
/// compiles about one time in three.
fn draw_attempt(rng: &mut Rng, index: usize) -> Result<Attempt, Failure> {
    let tokens_in = u64::from(rng.between(0, 20_000));
    let tokens_out = u64::from(rng.between(0, 5_000));
    let wall_ms = u64::from(rng.between(0, 90_000));
    if rng.chance(250) {
        return Ok(Attempt {
            text: "I would rather explain than write code.\n".to_owned(),
            code: None,
            compiles: false,
            lines: 0,
            sites: 0,
            tokens_in,
            tokens_out,
            wall_ms,
        });
    }
    let compiles = rng.chance(350);
    let body_lines = count(rng, 0, 6)?;
    let sites = count(rng, 0, 3)?;
    let body: String = (0..body_lines)
        .map(|line| format!("    let value_{line} = {line};\n"))
        .collect();
    let burden = "    JoinSet::new();\n".repeat(sites);
    let marker = if compiles { COMPILES } else { "// broken" };
    let code = format!("fn attempt_{index}() {{\n{body}{burden}}}\n{marker}\n");
    Ok(Attempt {
        text: format!("Here is the crate.\n```rust\n{code}```\nThat is all.\n"),
        code: Some(code),
        compiles,
        // The signature, the closing brace, the body and the burden; the marker
        // is a comment and counts for nothing.
        lines: body_lines.saturating_add(sites).saturating_add(2),
        sites,
        tokens_in,
        tokens_out,
        wall_ms,
    })
}

/// How an oracle log mentions one clause.
#[derive(Clone, Copy)]
enum Mention {
    /// `test <clause> ... ok`
    Passed,
    /// `test <clause> ... FAILED`
    Failed,
    /// `test <clause> ... ignored`
    Ignored,
    /// The log never names it.
    Silent,
}

/// Every way a log can mention a clause.
const MENTIONS: [Mention; 4] = [
    Mention::Passed,
    Mention::Failed,
    Mention::Ignored,
    Mention::Silent,
];

impl Mention {
    /// The log line that says it, when it says anything.
    fn line(self, clause: &str) -> Option<String> {
        match self {
            Self::Passed => Some(format!("test {clause} ... ok")),
            Self::Failed => Some(format!("test {clause} ... FAILED")),
            Self::Ignored => Some(format!("test {clause} ... ignored")),
            Self::Silent => None,
        }
    }

    /// The verdict a reader of that line must reach: an ignored clause proved
    /// nothing and fails, and a clause the log never names has not run.
    fn verdict(self) -> Verdict {
        match self {
            Self::Passed => Verdict::Pass,
            Self::Failed | Self::Ignored => Verdict::Fail,
            Self::Silent => Verdict::NotRun,
        }
    }
}

/// One generated trial.
struct Scenario {
    /// The trial.
    job: Job,
    /// The replies the author gives, one more than the repair budget.
    attempts: Vec<Attempt>,
    /// The repair budget.
    max_repairs: u32,
    /// The oracle's output.
    log: String,
    /// The verdicts a reader of that log must reach if the oracle runs.
    graded: BTreeMap<String, Verdict>,
    /// Whether the oracle was still running at its bound.
    timed_out: bool,
}

/// A trial drawn from the sequence.
fn draw(rng: &mut Rng, world: &World) -> Result<Scenario, Failure> {
    let job = job(rng, &world.bench, Mode::Models)?;
    let max_repairs = rng.between(0, 4);
    let mut attempts = Vec::new();
    for index in 0..=usize::try_from(max_repairs)? {
        attempts.push(draw_attempt(rng, index)?);
    }
    let clauses = world
        .clauses
        .get(&job.task)
        .ok_or("the task has no oracle clauses")?;
    let timed_out = rng.chance(250);
    let mut lines = vec![format!("running {} tests", clauses.len())];
    let mut graded = BTreeMap::new();
    for clause in clauses {
        let mention = *one_of(rng, "mentions", &MENTIONS)?;
        lines.extend(mention.line(clause));
        if rng.coin() {
            let noise = lines.len();
            lines.push(format!("test unrelated_{noise} ... ok"));
        }
        let verdict = match (mention.verdict(), timed_out) {
            (Verdict::NotRun, true) => Verdict::Timeout,
            (found, _) => found,
        };
        graded.insert(clause.clone(), verdict);
    }
    lines.push("test result: done".to_owned());
    Ok(Scenario {
        job,
        attempts,
        max_repairs,
        log: format!("{}\n", lines.join("\n")),
        graded,
        timed_out,
    })
}

/// What an independent reading of the loop says a trial comes to.
struct Expected {
    /// Whether a candidate compiled within the budget.
    compiled: bool,
    /// How many times the author was asked.
    asked: usize,
    /// How many candidates were built.
    builds: usize,
    /// How many times the author had been asked when each build happened.
    build_asks: Vec<usize>,
    /// The code the author was shown before each repair, when it had any.
    shown: Vec<Option<String>>,
    /// The solution the trial ends with.
    code: String,
    /// Its code lines.
    lines: usize,
    /// Its orchestration tokens.
    sites: usize,
    /// Tokens read over every call.
    tokens_in: u64,
    /// Tokens written over every call.
    tokens_out: u64,
    /// The model's wall time over every call.
    wall_ms: u64,
}

/// The loop, read afresh: a reply with code replaces the candidate; a reply
/// without keeps the last one, which is built again; the first compile ends it.
fn expect(attempts: &[Attempt]) -> Expected {
    let mut current: Option<&Attempt> = None;
    let mut expected = Expected {
        compiled: false,
        asked: 0,
        builds: 0,
        build_asks: Vec::new(),
        shown: Vec::new(),
        code: String::new(),
        lines: 0,
        sites: 0,
        tokens_in: 0,
        tokens_out: 0,
        wall_ms: 0,
    };
    for attempt in attempts {
        if expected.asked > 0 {
            expected
                .shown
                .push(current.and_then(|held| held.code.clone()));
        }
        expected.asked = expected.asked.saturating_add(1);
        expected.tokens_in = expected.tokens_in.saturating_add(attempt.tokens_in);
        expected.tokens_out = expected.tokens_out.saturating_add(attempt.tokens_out);
        expected.wall_ms = expected.wall_ms.saturating_add(attempt.wall_ms);
        if attempt.code.is_some() {
            current = Some(attempt);
        }
        if let Some(held) = current {
            expected.builds = expected.builds.saturating_add(1);
            expected.build_asks.push(expected.asked);
            if held.compiles {
                expected.compiled = true;
                break;
            }
        }
    }
    if let Some(held) = current {
        expected.code = match held.code {
            Some(ref code) => code.clone(),
            None => String::new(),
        };
        expected.lines = held.lines;
        expected.sites = held.sites;
    }
    expected
}

/// Runs `scenario` through the real loop, from a model or from `reference`.
fn play(
    world: &World,
    scenario: &Scenario,
    judge: &Floor,
    reference: Option<String>,
) -> (Result<Finished, BenchError>, Scripted) {
    let author = Scripted::new(
        scenario
            .attempts
            .iter()
            .map(|attempt| {
                reply(
                    &attempt.text,
                    attempt.tokens_in,
                    attempt.tokens_out,
                    attempt.wall_ms,
                )
            })
            .collect(),
    );
    let (Some(task), Some(clauses)) = (
        world.bench.task(&scenario.job.task),
        world.clauses.get(&scenario.job.task),
    ) else {
        return (
            Err(BenchError::Usage("the task is not declared".to_owned())),
            author,
        );
    };
    let start = match reference {
        Some(code) => Start::Reference(code),
        None => Start::Model {
            author: &author,
            prompt: ORIGINAL.to_owned(),
        },
    };
    let run = run_trial(
        Inputs {
            bench: &world.bench,
            job: &scenario.job,
            task,
            trial_id: "sim__trial".to_owned(),
            clauses: clauses.clone(),
            max_repairs: scenario.max_repairs,
            commit: COMMIT,
            start,
        },
        judge,
    );
    (run, author)
}

/// A judge that reports what `scenario`'s oracle printed.
fn judge_for(scenario: &Scenario) -> Floor {
    Floor::new(Graded {
        output: scenario.log.clone(),
        timed_out: scenario.timed_out,
    })
}

/// One seed of the repair loop: the author is asked as often as the model says,
/// every candidate is built, and the record and its cost add up.
fn repair_under(world: &World, rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scenario = draw(rng, world)?;
    let expected = expect(&scenario.attempts);
    let judge = judge_for(&scenario);
    let (run, author) = play(world, &scenario, &judge, None);
    let finished = run?;
    let record = &finished.record;
    let repairs = u32::try_from(expected.asked.saturating_sub(1))?;
    ensure(record.compiled == expected.compiled, || {
        format!(
            "compiled {} but the model says {}",
            record.compiled, expected.compiled
        )
    })?;
    ensure(record.repairs == repairs, || {
        format!("{} repairs but the model says {repairs}", record.repairs)
    })?;
    ensure(
        record.tokens_in == expected.tokens_in
            && record.tokens_out == expected.tokens_out
            && record.model_wall_ms == expected.wall_ms,
        || "the cost of the calls is not the sum of the calls".to_owned(),
    )?;
    ensure(
        record.lib_rs == expected.code && finished.code == expected.code,
        || "the solution is not the last code the author wrote".to_owned(),
    )?;
    ensure(
        record.consumer_lines == expected.lines && record.orchestration_sites == expected.sites,
        || "the solution's size is not the size of that code".to_owned(),
    )?;
    ensure(judge.builds().len() == expected.builds, || {
        format!(
            "{} builds but the model says {}",
            judge.builds().len(),
            expected.builds
        )
    })?;
    ensure(
        judge.grade_calls() == usize::from(expected.compiled),
        || "the oracle runs once if something compiled, and never otherwise".to_owned(),
    )?;
    let builds = f64::from(u32::try_from(expected.builds)?);
    ensure(close(record.compile_wall_ms, 1.5 * builds), || {
        format!(
            "compile time {} is not {builds} builds of 1.5 ms",
            record.compile_wall_ms
        )
    })?;
    let prompts = author.prompts();
    ensure(prompts.len() == expected.asked, || {
        format!(
            "{} calls but the model says {}",
            prompts.len(),
            expected.asked
        )
    })?;
    ensure(
        prompts.first().map(String::as_str) == Some(ORIGINAL),
        || "the first call is the original prompt".to_owned(),
    )?;
    for (prompt, shown) in prompts.iter().skip(1).zip(&expected.shown) {
        let told = match *shown {
            Some(ref code) => prompt.contains(code.as_str()),
            None => prompt.contains("contained no fenced ```rust block"),
        };
        ensure(
            prompt.starts_with(ORIGINAL) && prompt.contains("=== PREVIOUS ATTEMPT ===") && told,
            || "a repair shows the original prompt, then the code or the lack of it".to_owned(),
        )?;
    }
    let raws: Vec<String> = scenario
        .attempts
        .iter()
        .take(expected.asked)
        .map(|attempt| format!("raw:{}", attempt.text))
        .collect();
    ensure(finished.raw == raws.join("\n"), || {
        "the raw output is every call's, in order".to_owned()
    })?;
    let log_says = if expected.builds == 0 {
        finished.build_log == "the reply contained no fenced rust block\n"
    } else if expected.compiled {
        finished.build_log.starts_with("Finished")
    } else {
        finished.build_log.starts_with("error[E0308]")
    };
    ensure(log_says, || {
        "the build log is the last build's, or says no code was found".to_owned()
    })?;
    trace.record_number("asked", expected.asked);
    trace.record_number("builds", expected.builds);
    trace.record(&record.to_line());
    Ok(())
}

/// The spec is `trial.py` of the retired Python runner (2026-10-11): up to
/// `max_repairs` re-prompts, each showing the original prompt and the compiler's
/// complaint, ending at the first compile; a reply with no fenced block is a
/// failed attempt. Requirement: the record is the sum of what the loop did.
#[test]
fn a_trial_repairs_until_a_candidate_compiles_or_the_budget_ends() -> Result<(), Failure> {
    let world = world()?;
    replay_sweep("repair loop", 400, |rng, trace| {
        repair_under(&world, rng, trace)
    })
}

/// One seed of grading: a clause's verdict is what the oracle log says of it, a
/// timeout marks only the clauses that had not finished, and nothing is graded
/// when nothing compiled.
fn grade_under(world: &World, rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scenario = draw(rng, world)?;
    let expected = expect(&scenario.attempts);
    let judge = judge_for(&scenario);
    let (run, _) = play(world, &scenario, &judge, None);
    let finished = run?;
    let record = &finished.record;
    let clauses = world
        .clauses
        .get(&scenario.job.task)
        .ok_or("the task has no oracle clauses")?;
    let drop_clause = &world
        .bench
        .task(&scenario.job.task)
        .ok_or("the task is not declared")?
        .drop_clause;
    if expected.compiled {
        ensure(record.oracle == scenario.graded, || {
            format!(
                "verdicts {:?} but the log says {:?}",
                record.oracle, scenario.graded
            )
        })?;
        ensure(
            record.cleanup_ok == (scenario.graded.get(drop_clause) == Some(&Verdict::Pass)),
            || "cleanup_ok is the drop clause passing".to_owned(),
        )?;
        ensure(
            close(record.oracle_wall_ms, 2.25)
                && record.oracle_peak_rss_bytes == Some(4096)
                && finished.test_output == scenario.log,
            || "the oracle's time, peak and output are the run's".to_owned(),
        )?;
    } else {
        ensure(
            record.oracle.len() == clauses.len()
                && record
                    .oracle
                    .values()
                    .all(|verdict| *verdict == Verdict::NotRun),
            || "a crate that never compiled has every clause not run".to_owned(),
        )?;
        ensure(
            !record.cleanup_ok
                && record.oracle_peak_rss_bytes.is_none()
                && finished.test_output.is_empty()
                && close(record.oracle_wall_ms, 0.0),
            || "a crate that never compiled has no cleanup result and no measurement".to_owned(),
        )?;
    }
    trace.record_number("compiled", expected.compiled);
    trace.record_number("timed_out", scenario.timed_out);
    trace.record(&record.to_line());
    Ok(())
}

/// libtest's pretty format (`test <name> ... ok|FAILED|ignored`) is what
/// `cargo test` prints. Requirement: a clause's verdict is the log's word for it,
/// and `cleanup_ok` is only ever the drop clause's pass (the Python runner's
/// `cleanup_ok`, #270).
#[test]
fn a_clause_takes_the_verdict_its_log_line_states_and_a_timeout_marks_the_rest()
-> Result<(), Failure> {
    let world = world()?;
    replay_sweep("grading", 400, |rng, trace| grade_under(&world, rng, trace))
}

/// A canned solution is not an author. Requirement: a dry run or a mutant asks
/// no model, spends no tokens and gets no repair, whatever the budget says.
#[test]
fn a_reference_start_asks_no_author_and_gets_no_repair() -> Result<(), Failure> {
    let world = world()?;
    replay_sweep("reference start", 200, |rng, trace| {
        let scenario = draw(rng, &world)?;
        let compiles = rng.coin();
        let marker = if compiles { COMPILES } else { "// broken" };
        let code = format!("fn reference() {{}}\n{marker}\n");
        let judge = judge_for(&scenario);
        let (run, author) = play(&world, &scenario, &judge, Some(code.clone()));
        let finished = run?;
        let record = &finished.record;
        ensure(author.prompts().is_empty(), || {
            "a reference was handed an author's question".to_owned()
        })?;
        ensure(
            record.repairs == 0
                && record.tokens_in == 0
                && record.tokens_out == 0
                && record.model_wall_ms == 0,
            || "a reference costs nothing and is never repaired".to_owned(),
        )?;
        ensure(
            record.compiled == compiles
                && record.lib_rs == code
                && judge.builds().len() == 1
                && judge.grade_calls() == usize::from(compiles),
            || "a reference is built once and graded only if it compiled".to_owned(),
        )?;
        trace.record_number("compiles", compiles);
        trace.record(&record.to_line());
        Ok(())
    })
}

/// One seed of faults: a build that errors on a chosen build, and an oracle that
/// errors, each stop the trial with that error and nothing after it runs.
fn fault_under(world: &World, rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scenario = draw(rng, world)?;
    let expected = expect(&scenario.attempts);
    trace.record_number("builds", expected.builds);
    if expected.builds > 0 {
        let at = count(rng, 1, u32::try_from(expected.builds)?)?;
        let judge = judge_for(&scenario).failing_build(at);
        let (run, author) = play(world, &scenario, &judge, None);
        let Err(error) = run else {
            return refuse(format!("build {at} failed and the trial went on"));
        };
        ensure(error.to_string().contains("injected build fault"), || {
            format!("the error is {error}, not the build's")
        })?;
        ensure(
            judge.builds().len() == at && judge.grade_calls() == 0,
            || "nothing was built or graded after the fault".to_owned(),
        )?;
        ensure(
            expected.build_asks.get(at.saturating_sub(1)) == Some(&author.prompts().len()),
            || "the author was asked past the failing build".to_owned(),
        )?;
        trace.record_number("build_fault_at", at);
    }
    if expected.compiled {
        let judge = judge_for(&scenario).failing_grade();
        let (run, _) = play(world, &scenario, &judge, None);
        ensure(run.is_err() && judge.grade_calls() == 1, || {
            "an oracle that errors ends the trial and is asked once".to_owned()
        })?;
        trace.record("grade_fault");
    }
    Ok(())
}

/// Requirement: a fault in the compiler or the oracle is the trial's error, not
/// a quiet failed verdict; the caller records it as a crash and a resume runs it
/// again (SQLite's "a failed write is an error, never a short row" rule).
#[test]
fn a_fault_in_the_build_or_the_oracle_stops_the_trial_with_that_error() -> Result<(), Failure> {
    let world = world()?;
    replay_sweep("faults", 300, |rng, trace| fault_under(&world, rng, trace))
}

/// Requirement: an author that stops answering is an error at the call that went
/// unanswered, after every reply it did give was used.
#[test]
fn an_author_that_runs_out_of_replies_is_an_error_at_the_unanswered_call() -> Result<(), Failure> {
    let world = world()?;
    replay_sweep("exhausted script", 300, |rng, trace| {
        let mut scenario = draw(rng, &world)?;
        let kept = count(rng, 1, u32::try_from(scenario.attempts.len())?)?;
        scenario.attempts.truncate(kept);
        let expected = expect(&scenario.attempts);
        let needs_more = !expected.compiled && kept <= usize::try_from(scenario.max_repairs)?;
        let judge = judge_for(&scenario);
        let (run, author) = play(&world, &scenario, &judge, None);
        match run {
            Ok(_) => ensure(!needs_more, || {
                "a script ran dry and the trial finished".to_owned()
            })?,
            Err(error) => {
                ensure(
                    needs_more && error.to_string().contains("no reply left"),
                    || format!("the trial failed with {error} though the script was enough"),
                )?;
                ensure(author.prompts().len() == kept.saturating_add(1), || {
                    "the unanswered call is the one after the last reply".to_owned()
                })?;
            }
        }
        trace.record_number("kept", kept);
        trace.record_number("needs_more", needs_more);
        Ok(())
    })
}

/// Requirement: the sweep is not degenerate. Two hundred seeds must tell
/// well over a hundred and eighty different trials, or the draws are not
/// reaching the loop and "replays to the same hash" would prove nothing.
#[test]
fn different_seeds_tell_different_trials() -> Result<(), Failure> {
    let world = world()?;
    let mut seen = BTreeSet::new();
    for seed in 1..=200_u64 {
        let mut trace = Trace::new();
        repair_under(&world, &mut Rng::new(seed), &mut trace)?;
        seen.insert(receipt(&trace)?);
    }
    ensure(seen.len() > 180, || {
        format!("200 seeds told only {} different trials", seen.len())
    })
}
