//! Simulated run directories: a run is begun, appended to, torn by a kill at a
//! random byte, resumed, refused, and finished, on a real disk, one seed per
//! scenario.
//!
//! The evidence of a long matrix is the log every finished trial is appended to;
//! these scenarios hold the promise a kill at hour fourteen keeps every finished
//! trial, loses at most the one being written, and never lets a run reuse or
//! overwrite another's directory.

use std::fs::{self, OpenOptions};

use super::BenchError;
use super::bench::Bench;
use super::plan::{Job, Mode, split_done, trial_id};
use super::record::{Record, read_results};
use super::replay::{Failure, refuse, replay_sweep};
use super::seed::{Rng, Trace};
use super::sim_kit::{Scratch, committed, count, ensure, job, one_of, record};
use super::store;

/// Every mode a run can be.
const MODES: [Mode; 3] = [Mode::Dry, Mode::Mutants, Mode::Models];

/// A commit id, drawn from the sequence.
fn commit_of(rng: &mut Rng) -> String {
    format!(
        "{:016x}{:016x}{:08x}",
        rng.next_u64(),
        rng.next_u64(),
        rng.below(u32::MAX)
    )
}

/// `made` trials of `mode`, each with its record, under distinct trial ids.
fn trials(
    rng: &mut Rng,
    bench: &Bench,
    mode: Mode,
    made: usize,
) -> Result<Vec<(Job, Record)>, Failure> {
    let mut found = Vec::new();
    for index in 0..made {
        let mut one = job(rng, bench, mode)?;
        one.trial = u32::try_from(index)?;
        let line = record(rng, bench, &one)?;
        found.push((one, line));
    }
    Ok(found)
}

/// The text of the refusal `outcome` is, or a failure when it was not one.
fn refusal<T>(outcome: Result<T, BenchError>) -> Result<String, Failure> {
    match outcome {
        Ok(_) => refuse("the run was allowed and should have been refused"),
        Err(error) => Ok(error.to_string()),
    }
}

/// One seed of beginning: a second begin over the same directories is refused
/// and leaves the first run's manifest as it was, and a begin whose work root
/// exists makes no results directory.
fn begin_under(rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scratch = Scratch::new("store-begin")?;
    let (dir, work) = (scratch.path().join("run"), scratch.path().join("work"));
    let mode = *one_of(rng, "modes", &MODES)?;
    let commit = commit_of(rng);
    store::begin(&dir, &work, mode, &commit)?;
    let manifest = fs::read_to_string(dir.join("run.json"))?;
    ensure(
        manifest.contains(&commit) && manifest.contains(mode.run_label()),
        || "the manifest names the commit and the mode".to_owned(),
    )?;
    refusal(store::begin(&dir, &work, mode, &commit))?;
    ensure(
        fs::read_to_string(dir.join("run.json"))? == manifest,
        || "a refused begin rewrote the first run's manifest".to_owned(),
    )?;
    let elsewhere = scratch.path().join("elsewhere");
    refusal(store::begin(&elsewhere, &work, mode, &commit))?;
    ensure(!elsewhere.exists(), || {
        "a begin over an existing work root still made a results directory".to_owned()
    })?;
    // The manifest names the scratch directory, which is random; the commit and
    // the mode are what the seed chose.
    trace.record(&commit);
    trace.record(mode.run_label());
    Ok(())
}

/// Requirement: evidence is never reused. A run's results directory and work
/// root are new or the run is refused (the retired runner's "an existing one is
/// a refusal" rule, and POSIX `mkdir(2)` EEXIST, which is the atomic claim).
#[test]
fn a_run_is_refused_the_directories_of_another_and_leaves_them_as_they_were() -> Result<(), Failure>
{
    replay_sweep("begin", 30, begin_under)
}

/// One seed of a kill: the log is cut at a random byte of its last line, as an
/// interrupted append leaves it, and the resume must keep every whole record and
/// at most drop the one being written.
fn torn_under(bench: &Bench, rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scratch = Scratch::new("store-torn")?;
    let (dir, work) = (scratch.path().join("run"), scratch.path().join("work"));
    let commit = commit_of(rng);
    store::begin(&dir, &work, Mode::Models, &commit)?;
    let made = count(rng, 1, 6)?;
    let kept = trials(rng, bench, Mode::Models, made)?;
    for pair in &kept {
        store::append(&dir, &pair.1)?;
    }
    let partial = dir.join("results.partial.jsonl");
    let text = fs::read_to_string(&partial)?;
    let body_len = text.trim_end_matches('\n').len();
    let last_start = match text.get(..body_len).and_then(|body| body.rfind('\n')) {
        Some(at) => at.saturating_add(1),
        None => 0,
    };
    let cut = count(rng, u32::try_from(last_start)?, u32::try_from(text.len())?)?;
    OpenOptions::new()
        .write(true)
        .open(&partial)?
        .set_len(u64::try_from(cut)?)?;
    let resumed = store::resume(&dir, Mode::Models, &commit)?;
    // The last record survives when its JSON is whole, whether or not its
    // newline was written; a cut at the line's first byte leaves nothing torn.
    let whole = made
        .saturating_sub(1)
        .saturating_add(usize::from(cut >= body_len));
    let torn = usize::from(cut > last_start && cut < body_len);
    ensure(resumed.done.len() == whole && resumed.torn == torn, || {
        format!(
            "cut {cut} of {} (last line from {last_start}): {} kept and {} torn, expected {whole} and {torn}",
            text.len(),
            resumed.done.len(),
            resumed.torn
        )
    })?;
    ensure(resumed.work == work, || {
        "the resume lost the work root".to_owned()
    })?;
    for pair in kept.iter().take(whole) {
        ensure(
            resumed.done.get(&trial_id(&pair.0)) == Some(&pair.1),
            || format!("{} was not read back as it was written", trial_id(&pair.0)),
        )?;
    }
    let jobs: Vec<Job> = kept.iter().map(|pair| pair.0.clone()).collect();
    let (finished, remaining) = split_done(jobs, &resumed.done);
    let tail: Vec<Job> = kept.iter().skip(whole).map(|pair| pair.0.clone()).collect();
    ensure(finished.len() == whole && remaining == tail, || {
        "the jobs to run are not exactly the ones with no whole record".to_owned()
    })?;
    trace.record_number("made", made);
    trace.record_number("cut", cut);
    trace.record_number("torn", torn);
    Ok(())
}

/// Source: SQLite's atomic-commit notes and the write-ahead-log practice of every
/// database that survives `kill -9`: a record is whole or it is not there.
/// Requirement: a kill at any byte of the last append loses at most that record
/// (Director, #270: "launched once, with no pilot" makes a lost trial a lost run).
#[test]
fn a_kill_at_any_byte_of_the_last_append_loses_at_most_that_trial() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("torn log", 100, |rng, trace| torn_under(&bench, rng, trace))
}

/// One seed of refusals: a resume names what is wrong with the run it was asked
/// to reopen, and reopens it when nothing is.
fn refusals_under(rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scratch = Scratch::new("store-refuse")?;
    let (dir, work) = (scratch.path().join("run"), scratch.path().join("work"));
    let mode = *one_of(rng, "modes", &MODES)?;
    let commit = commit_of(rng);
    store::begin(&dir, &work, mode, &commit)?;
    let others: Vec<Mode> = MODES
        .iter()
        .copied()
        .filter(|found| *found != mode)
        .collect();
    let other = *one_of(rng, "other modes", &others)?;
    let said = refusal(store::resume(&dir, other, &commit))?;
    ensure(
        said.contains(&format!(
            "is a {} run, not {}",
            mode.run_label(),
            other.run_label()
        )),
        || format!("a resume as the wrong mode said: {said}"),
    )?;
    let elsewhere = commit_of(rng);
    let said = refusal(store::resume(&dir, mode, &elsewhere))?;
    ensure(said.contains(&commit) && said.contains(&elsewhere), || {
        format!("a resume at another commit said: {said}")
    })?;
    let reopened = store::resume(&dir, mode, &commit)?;
    ensure(
        reopened.done.is_empty() && reopened.torn == 0 && reopened.work == work,
        || "a resume of an untouched run found something".to_owned(),
    )?;
    store::finish(&dir, &[], "{}\n")?;
    let said = refusal(store::resume(&dir, mode, &commit))?;
    ensure(said.contains("finished"), || {
        format!("a resume of a finished run said: {said}")
    })?;
    let broken = scratch.path().join("broken");
    fs::create_dir(&broken)?;
    refusal(store::resume(&broken, mode, &commit))?;
    fs::write(broken.join("run.json"), "{\"mode\": \"dry\"")?;
    refusal(store::resume(&broken, Mode::Dry, &commit))?;
    fs::write(
        broken.join("run.json"),
        format!("{{\"mode\": \"{}\"}}", mode.run_label()),
    )?;
    let said = refusal(store::resume(&broken, mode, &commit))?;
    ensure(said.contains("lgwks_bot_sha"), || {
        format!("a manifest with no commit said: {said}")
    })?;
    trace.record(&commit);
    trace.record(&elsewhere);
    Ok(())
}

/// Requirement: trials graded against two different crate trees are not one
/// measurement, so a resume at another commit, as another mode, of a finished
/// run, or of a run with no readable manifest is refused with its reason.
#[test]
fn a_resume_is_refused_for_another_mode_another_commit_or_a_finished_run() -> Result<(), Failure> {
    replay_sweep("refusals", 40, refusals_under)
}

/// One seed of setting aside: each unfinished trial's directory is renamed, never
/// deleted and never overwritten, however many resumes it has been through.
fn aside_under(rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scratch = Scratch::new("store-aside")?;
    let work = scratch.path().join("work");
    fs::create_dir(&work)?;
    let total = count(rng, 1, 6)?;
    let ids: Vec<String> = (0..total).map(|index| format!("trial_{index}")).collect();
    let present: Vec<bool> = ids.iter().map(|_| rng.coin()).collect();
    let alive = present.iter().filter(|here| **here).count();
    for round in 1..=2_usize {
        for (id, here) in ids.iter().zip(&present) {
            if *here {
                fs::create_dir(work.join(id))?;
                fs::write(
                    work.join(id).join("evidence"),
                    format!("{id} round {round}"),
                )?;
            }
        }
        let moved = store::set_aside(&work, &ids)?;
        ensure(moved == alive, || {
            format!("{moved} directories set aside, {alive} were there")
        })?;
        for (id, here) in ids.iter().zip(&present) {
            ensure(!work.join(id).exists(), || {
                format!("{id} is still where its rerun would start")
            })?;
            for earlier in 1..=round {
                let kept = work
                    .join(format!("{id}.interrupted-{earlier}"))
                    .join("evidence");
                ensure(kept.is_file() == *here, || {
                    format!(
                        "{id} round {earlier}: evidence {} but present is {here}",
                        kept.is_file()
                    )
                })?;
                if *here {
                    ensure(
                        fs::read_to_string(&kept)? == format!("{id} round {earlier}"),
                        || format!("{id}: the evidence of round {earlier} was overwritten"),
                    )?;
                }
            }
        }
    }
    trace.record_number("alive", alive);
    trace.record_number("total", total);
    Ok(())
}

/// Requirement: what an interrupted trial left is evidence of the interruption.
/// It is renamed with a counter so a second interruption of the same trial
/// cannot overwrite the first's (the retired runner's `.interrupted-N` rule).
#[test]
fn an_interrupted_trials_directory_is_set_aside_and_never_deleted_or_overwritten()
-> Result<(), Failure> {
    replay_sweep("set aside", 40, aside_under)
}

/// One seed of finishing: the sorted results are whole under their final name,
/// read back as written, the log and the staging file are gone, and the summary
/// is what was given.
fn finish_under(bench: &Bench, rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scratch = Scratch::new("store-finish")?;
    let (dir, work) = (scratch.path().join("run"), scratch.path().join("work"));
    let commit = commit_of(rng);
    store::begin(&dir, &work, Mode::Dry, &commit)?;
    let made = count(rng, 0, 6)?;
    let written = trials(rng, bench, Mode::Dry, made)?;
    let records: Vec<Record> = written.iter().map(|pair| pair.1.clone()).collect();
    for line in &records {
        store::append(&dir, line)?;
    }
    store::finish(&dir, &records, "{\"summary\": true}\n")?;
    let text = fs::read_to_string(dir.join("results.jsonl"))?;
    ensure(read_results(&text)? == records, || {
        "the results are not the records, in order".to_owned()
    })?;
    ensure(text.lines().eq(records.iter().map(Record::to_line)), || {
        "a results line is not its record's canonical line".to_owned()
    })?;
    ensure(
        !dir.join("results.partial.jsonl").exists() && !dir.join("results.jsonl.staged").exists(),
        || "the log or the staging file outlived the run".to_owned(),
    )?;
    ensure(
        fs::read_to_string(dir.join("summary.json"))? == "{\"summary\": true}\n",
        || "the summary is not what finish was given".to_owned(),
    )?;
    trace.record_number("made", made);
    trace.record(&text);
    Ok(())
}

/// Requirement: a finished run's results are whole under their final name or not
/// there (write-then-rename, the POSIX `rename(2)` atomicity every editor and
/// package manager relies on), and strict where the log is forgiving.
#[test]
fn a_finished_run_holds_its_results_whole_and_retires_its_log() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("finish", 40, |rng, trace| finish_under(&bench, rng, trace))
}

/// One seed of crashes: a crashed trial is on the log and in the results, is not
/// finished, and is the one a resume runs again.
fn crashed_under(bench: &Bench, rng: &mut Rng, trace: &mut Trace) -> Result<(), Failure> {
    let scratch = Scratch::new("store-crash")?;
    let (dir, work) = (scratch.path().join("run"), scratch.path().join("work"));
    let commit = commit_of(rng);
    store::begin(&dir, &work, Mode::Models, &commit)?;
    let made = count(rng, 1, 6)?;
    let mut jobs = Vec::new();
    let mut crashed = Vec::new();
    for pair in trials(rng, bench, Mode::Models, made)? {
        let broke = rng.coin();
        let line = if broke {
            Record::crashed(
                &pair.0,
                &commit,
                &bench.tokens,
                "the compiler could not start".to_owned(),
            )
        } else {
            pair.1
        };
        store::append(&dir, &line)?;
        crashed.push(broke);
        jobs.push(pair.0);
    }
    let resumed = store::resume(&dir, Mode::Models, &commit)?;
    let alive = crashed.iter().filter(|broke| !**broke).count();
    ensure(resumed.done.len() == alive && resumed.torn == 0, || {
        format!(
            "{} finished and {} torn, expected {alive} and 0",
            resumed.done.len(),
            resumed.torn
        )
    })?;
    let (finished, remaining) = split_done(jobs.clone(), &resumed.done);
    let to_run: Vec<Job> = jobs
        .iter()
        .zip(&crashed)
        .filter(|pair| *pair.1)
        .map(|pair| pair.0.clone())
        .collect();
    ensure(finished.len() == alive && remaining == to_run, || {
        "a resume must run exactly the crashed trials again".to_owned()
    })?;
    trace.record_number("made", made);
    trace.record_number("alive", alive);
    Ok(())
}

/// Requirement: a crash is a recorded trial and not a missing one, and a resume
/// treats it as unfinished (the retired runner's `crashed` record, #270).
#[test]
fn a_crashed_trial_is_recorded_and_is_the_one_a_resume_runs_again() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("crashed", 40, |rng, trace| {
        crashed_under(&bench, rng, trace)
    })
}
