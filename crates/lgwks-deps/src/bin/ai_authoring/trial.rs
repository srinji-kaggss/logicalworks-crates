//! One trial: author, compile, repair and grade.
//!
//! The loop here asks an [`Author`] and a [`Judge`] and nothing else, so it runs
//! unchanged over a model under a sandbox and cargo, or over a scripted author
//! and a judge that never builds anything.

use std::collections::BTreeMap;

use super::author::{Author, extract_rust_block, repair_prompt};
use super::bench::{Bench, Task};
use super::cargo::{Built, Tested};
use super::grade::{Verdict, consumer_lines, orchestration_sites, read_verdicts};
use super::plan::Job;
use super::record::{Record, round_millis};
use super::BenchError;

/// What it takes to compile a candidate and grade it.
pub(super) trait Judge {
    /// Builds `code` as the trial's solution.
    fn build(&self, code: &str) -> Result<Built, BenchError>;
    /// Runs the oracle against the solution last built.
    fn grade(&self) -> Result<Tested, BenchError>;
}

/// Where a trial's first candidate comes from.
pub(super) enum Start<'run> {
    /// A reference solution or a mutant, standing in for an author.
    Reference(String),
    /// A model, asked with `prompt`, and asked again to repair what fails.
    Model {
        /// The model.
        author: &'run dyn Author,
        /// The prompt it is asked first.
        prompt: String,
    },
}

/// What a trial runs against.
pub(super) struct Inputs<'run> {
    /// The benchmark.
    pub(super) bench: &'run Bench,
    /// The trial.
    pub(super) job: &'run Job,
    /// The task.
    pub(super) task: &'run Task,
    /// The trial's directory name.
    pub(super) trial_id: String,
    /// The oracle's clauses, in source order.
    pub(super) clauses: Vec<String>,
    /// How many repairs a failing compile gets.
    pub(super) max_repairs: u32,
    /// The commit the trial ran at.
    pub(super) commit: &'run str,
    /// Where the first candidate comes from.
    pub(super) start: Start<'run>,
}

/// A graded trial and the text that belongs beside its record.
pub(super) struct Finished {
    /// The record.
    pub(super) record: Record,
    /// The solution as last compiled.
    pub(super) code: String,
    /// Everything the model calls printed.
    pub(super) raw: String,
    /// The last build's log.
    pub(super) build_log: String,
    /// The oracle's output.
    pub(super) test_output: String,
}

/// What the model calls of a trial have cost so far.
#[derive(Default)]
struct Spent {
    /// Tokens read.
    tokens_in: u64,
    /// Tokens written.
    tokens_out: u64,
    /// The model's own wall time.
    model_wall_ms: u64,
    /// Milliseconds spent building.
    compile_wall_ms: f64,
    /// The raw output of each call.
    raw: Vec<String>,
}

/// The note a failed attempt carries when the reply held no code.
const NO_BLOCK: &str = "the reply contained no fenced rust block\n";

/// Compiles `code`; an empty candidate is a failed attempt, not an empty crate.
fn attempt(judge: &dyn Judge, code: &str, spent: &mut Spent) -> Result<(bool, String), BenchError> {
    if code.is_empty() {
        return Ok((false, NO_BLOCK.to_owned()));
    }
    let built = judge.build(code)?;
    spent.compile_wall_ms += built.wall_ms;
    Ok((built.ok, built.log))
}

/// Asks `author` and books the cost; the code of its reply, empty when it held none.
fn ask(author: &dyn Author, prompt: &str, spent: &mut Spent) -> Result<String, BenchError> {
    let reply = author.ask(prompt)?;
    spent.tokens_in = spent.tokens_in.saturating_add(reply.tokens_in);
    spent.tokens_out = spent.tokens_out.saturating_add(reply.tokens_out);
    spent.model_wall_ms = spent.model_wall_ms.saturating_add(reply.wall_ms);
    spent.raw.push(reply.raw);
    Ok(extract_rust_block(&reply.text))
}

/// Runs one trial to its record.
pub(super) fn run_trial(inputs: Inputs<'_>, judge: &dyn Judge) -> Result<Finished, BenchError> {
    let mut spent = Spent::default();
    let mut repairs = 0_u32;
    let (mut code, model) = match inputs.start {
        Start::Reference(code) => (code, None),
        Start::Model { author, prompt } => {
            let first = ask(author, &prompt, &mut spent)?;
            (first, Some((author, prompt)))
        }
    };
    let (mut compiled, mut build_log) = attempt(judge, &code, &mut spent)?;
    while !compiled && repairs < inputs.max_repairs {
        let Some((author, ref original)) = model else {
            break;
        };
        repairs = repairs.saturating_add(1);
        let reply = ask(author, &repair_prompt(original, &code, &build_log), &mut spent)?;
        if !reply.is_empty() {
            code = reply;
        }
        (compiled, build_log) = attempt(judge, &code, &mut spent)?;
    }
    let mut oracle: BTreeMap<String, Verdict> = inputs
        .clauses
        .iter()
        .map(|name| (name.clone(), Verdict::NotRun))
        .collect();
    let mut test_output = String::new();
    let mut oracle_wall_ms = 0.0;
    let mut peak_rss = None;
    if compiled {
        let tested = judge.grade()?;
        read_verdicts(&tested.output, &mut oracle);
        if tested.timed_out {
            for verdict in oracle.values_mut() {
                if *verdict == Verdict::NotRun {
                    *verdict = Verdict::Timeout;
                }
            }
        }
        oracle_wall_ms = tested.wall_ms;
        peak_rss = tested.peak_rss;
        test_output = tested.output;
    }
    let (sites, site_tokens) = orchestration_sites(&code, &inputs.bench.tokens);
    let cleanup_ok = oracle.get(&inputs.task.drop_clause) == Some(&Verdict::Pass);
    let record = Record {
        model: inputs.job.model.clone(),
        arm: inputs.job.arm.clone(),
        task: inputs.job.task.clone(),
        profile: inputs.job.profile.clone(),
        trial: inputs.job.trial,
        trial_id: inputs.trial_id,
        mode: inputs.job.mode.record_label().to_owned(),
        compiled,
        repairs,
        cleanup_ok,
        oracle,
        oracle_wall_ms: round_millis(oracle_wall_ms),
        oracle_peak_rss_bytes: peak_rss,
        tokens_in: spent.tokens_in,
        tokens_out: spent.tokens_out,
        model_wall_ms: spent.model_wall_ms,
        compile_wall_ms: round_millis(spent.compile_wall_ms),
        consumer_lines: consumer_lines(&code),
        orchestration_sites: sites,
        orchestration_site_tokens: site_tokens,
        orchestration_token_list: inputs.bench.tokens.clone(),
        lockfile: inputs.bench.lockfile_note.clone(),
        lgwks_bot_sha: inputs.commit.to_owned(),
        lib_rs: code.clone(),
        error: None,
    };
    Ok(Finished {
        record,
        code,
        raw: spent.raw.join("\n"),
        build_log,
        test_output,
    })
}
