//! What the runner's simulations share: the committed benchmark, a directory per
//! test, a scripted author, a judge that builds nothing, and the generators that
//! turn one seed into the same record, job and reply every time.
//!
//! One seed drives every draw (`crate::seed::Rng`, the estate's one substrate), so
//! a failing seed is a complete reproduction: the same seed gives the same trial,
//! the same disk state and the same trace hash.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::author::{Author, Reply};
use super::bench::Bench;
use super::cargo::{Built, Tested};
use super::children::guard;
use super::console::Console;
use super::grade::Verdict;
use super::plan::{Job, Mode, trial_id};
use super::record::Record;
use super::replay::{Failure, refuse};
use super::seed::Rng;
use super::trial::Judge;
use super::{BenchError, fail};

/// The benchmark's own manifest, as committed.
pub(super) const COMMITTED: &str = include_str!("../../../../../bench/ai-authoring/bench.toml");

/// The benchmark directory in this checkout.
pub(super) const BENCH_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/ai-authoring");

/// The text a candidate carries when the scripted judge compiles it.
pub(super) const COMPILES: &str = "// compiles";

/// The committed benchmark, parsed.
pub(super) fn committed() -> Result<Bench, Failure> {
    Ok(Bench::parse(Path::new(BENCH_DIR), COMMITTED)?)
}

/// A directory made for one test and removed when the test ends.
pub(super) struct Scratch {
    /// The directory.
    root: PathBuf,
}

impl Scratch {
    /// A new, empty directory whose name no other test or run holds.
    pub(super) fn new(label: &str) -> Result<Self, Failure> {
        let bytes = lgwks_std::random::bytes::<8>()
            .map_err(|error| format!("no entropy for a scratch name: {error}"))?;
        let root = std::env::temp_dir().join(format!(
            "ai-authoring-sim-{label}-{:016x}",
            u64::from_le_bytes(bytes)
        ));
        fs::create_dir(&root)?;
        Ok(Self { root })
    }

    /// The directory.
    pub(super) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            lgwks_std::trace::debug!(%error, path = ?self.root, "a test directory could not be removed");
        }
    }
}

/// A writer whose bytes the test can read back.
pub(super) struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    /// A writer with nothing written.
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    /// Another handle on the same bytes.
    fn handle(&self) -> Self {
        Self(Arc::clone(&self.0))
    }

    /// Everything written so far, as text.
    pub(super) fn text(&self) -> String {
        String::from_utf8_lossy(&guard(&self.0)).into_owned()
    }
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        guard(&self.0).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A console whose two streams the test can read: the console, then what it said
/// on standard output, then what it said on standard error.
pub(super) fn console() -> (Console, Sink, Sink) {
    let (out, err) = (Sink::new(), Sink::new());
    (
        Console::new(Box::new(out.handle()), Box::new(err.handle())),
        out,
        err,
    )
}

/// A reply of `text` that cost `tokens_in` and `tokens_out` and took `wall_ms`.
pub(super) fn reply(text: &str, tokens_in: u64, tokens_out: u64, wall_ms: u64) -> Reply {
    Reply {
        text: text.to_owned(),
        tokens_in,
        tokens_out,
        wall_ms,
        raw: format!("raw:{text}"),
    }
}

/// An author that answers from a list and remembers what it was asked.
pub(super) struct Scripted {
    /// The replies still to give, oldest first.
    replies: Mutex<VecDeque<Reply>>,
    /// Every prompt it was asked, in order.
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    /// An author that gives `replies` in order, then refuses.
    pub(super) fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            asked: Mutex::new(Vec::new()),
        }
    }

    /// Every prompt it was asked.
    pub(super) fn prompts(&self) -> Vec<String> {
        guard(&self.asked).clone()
    }
}

impl Author for Scripted {
    fn ask(&self, prompt: &str) -> Result<Reply, BenchError> {
        guard(&self.asked).push(prompt.to_owned());
        match guard(&self.replies).pop_front() {
            Some(next) => Ok(next),
            None => Err(BenchError::Refused(
                "the script has no reply left".to_owned(),
            )),
        }
    }
}

/// What a scripted judge reports for the oracle.
pub(super) struct Graded {
    /// The oracle run's output.
    pub(super) output: String,
    /// Whether the oracle was still running at its bound.
    pub(super) timed_out: bool,
}

/// A judge that decides from the text of a candidate, builds nothing, and can be
/// made to fail on a chosen build or on grading.
pub(super) struct Floor {
    /// What grading reports.
    graded: Graded,
    /// The build, counted from one, that fails with an error instead of a verdict.
    fault_at_build: Option<usize>,
    /// Whether grading fails with an error.
    fault_on_grade: bool,
    /// Every candidate it was asked to build, in order.
    built: Mutex<Vec<String>>,
    /// How many times it was asked to grade.
    grades: Mutex<usize>,
}

impl Floor {
    /// A judge whose oracle reports `graded`.
    pub(super) fn new(graded: Graded) -> Self {
        Self {
            graded,
            fault_at_build: None,
            fault_on_grade: false,
            built: Mutex::new(Vec::new()),
            grades: Mutex::new(0),
        }
    }

    /// The same judge, failing with an error on build number `at` (from one).
    pub(super) fn failing_build(self, at: usize) -> Self {
        Self {
            fault_at_build: Some(at),
            ..self
        }
    }

    /// The same judge, failing with an error when graded.
    pub(super) fn failing_grade(self) -> Self {
        Self {
            fault_on_grade: true,
            ..self
        }
    }

    /// Every candidate it was asked to build.
    pub(super) fn builds(&self) -> Vec<String> {
        guard(&self.built).clone()
    }

    /// How many times it was asked to grade.
    pub(super) fn grade_calls(&self) -> usize {
        *guard(&self.grades)
    }
}

impl Judge for Floor {
    fn build(&self, code: &str) -> Result<Built, BenchError> {
        let mut built = guard(&self.built);
        built.push(code.to_owned());
        if self.fault_at_build == Some(built.len()) {
            return fail(
                BenchError::Refused("injected build fault".to_owned()),
                "sim build",
            );
        }
        let ok = code.contains(COMPILES);
        Ok(Built {
            ok,
            log: if ok {
                "Finished `dev` profile\n".to_owned()
            } else {
                "error[E0308]: mismatched types\n".to_owned()
            },
            wall_ms: 1.5,
        })
    }

    fn grade(&self) -> Result<Tested, BenchError> {
        let mut grades = guard(&self.grades);
        *grades = grades.saturating_add(1);
        if self.fault_on_grade {
            return fail(
                BenchError::Refused("injected grade fault".to_owned()),
                "sim grade",
            );
        }
        Ok(Tested {
            output: self.graded.output.clone(),
            wall_ms: 2.25,
            peak_rss: Some(4096),
            timed_out: self.graded.timed_out,
        })
    }
}

/// Fails the scenario, saying what was wrong, when `holds` is false.
///
/// A failure goes back through the replay sweep, which names the seed; an
/// assertion that panicked would name only the line.
pub(super) fn ensure(holds: bool, what: impl FnOnce() -> String) -> Result<(), Failure> {
    if holds { Ok(()) } else { refuse(what()) }
}

/// Whether two floats agree to a millionth.
pub(super) fn close(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-6
}

/// One of `values`, drawn from the sequence; an empty table is a fixture defect.
pub(super) fn one_of<'table, T>(
    rng: &mut Rng,
    table: &'static str,
    values: &'table [T],
) -> Result<&'table T, Failure> {
    Ok(rng.pick_named(table, values)?)
}

/// A count in `low..=high`, as a size.
pub(super) fn count(rng: &mut Rng, low: u32, high: u32) -> Result<usize, Failure> {
    Ok(usize::try_from(rng.between(low, high))?)
}

/// `items` in an order drawn from the sequence (Fisher–Yates, 1964).
pub(super) fn shuffle<T>(rng: &mut Rng, items: &mut [T]) -> Result<(), Failure> {
    for top in (1..items.len()).rev() {
        let other = count(rng, 0, u32::try_from(top)?)?;
        items.swap(top, other);
    }
    Ok(())
}

/// Model ids as vendors write them: a bare name, a vendor path and a dotted version.
pub(super) const MODELS: [&str; 3] = ["model-a", "vendor/model-b", "model_c.1"];

/// Every verdict a clause can come to.
const VERDICTS: [Verdict; 4] = [
    Verdict::Pass,
    Verdict::Fail,
    Verdict::NotRun,
    Verdict::Timeout,
];

/// A job of `mode` over the committed benchmark, drawn from the sequence.
pub(super) fn job(rng: &mut Rng, bench: &Bench, mode: Mode) -> Result<Job, Failure> {
    let arm = one_of(rng, "arms", &bench.arms)?;
    let task = one_of(rng, "tasks", &bench.tasks)?;
    let profile = one_of(rng, "profiles", &bench.profiles)?;
    let model = one_of(rng, "models", &MODELS)?;
    Ok(Job {
        mode,
        model: (*model).to_owned(),
        arm: arm.name.clone(),
        task: task.name.clone(),
        profile: profile.name.clone(),
        trial: rng.between(0, 3),
    })
}

/// Source fragments that exercise every escape a JSON string has.
const FRAGMENTS: [&str; 6] = [
    "fn main() {}\n",
    "let quoted = \"a \\\"b\\\" c\";\n",
    "\ttabbed\tline\r\n",
    "// snowman \u{2603} and crab \u{1f980}\n",
    "let back = '\\\\';\n",
    "/* \u{0} control \u{1f} bytes */\n",
];

/// A record of a trial of `job`, every field drawn from the sequence.
pub(super) fn record(rng: &mut Rng, bench: &Bench, job: &Job) -> Result<Record, Failure> {
    let clauses = count(rng, 1, 7)?;
    let mut oracle = BTreeMap::new();
    for index in 0..clauses {
        let verdict = one_of(rng, "verdicts", &VERDICTS)?;
        oracle.insert(format!("clause_{index}"), *verdict);
    }
    let mut lib_rs = String::new();
    for _ in 0..rng.between(1, 4) {
        lib_rs.push_str(one_of(rng, "fragments", &FRAGMENTS)?);
    }
    let mut sites = BTreeMap::new();
    let mut total = 0_usize;
    for token in &bench.tokens {
        let found = count(rng, 0, 3)?;
        total = total.saturating_add(found);
        sites.insert(token.clone(), found);
    }
    let rss = if rng.coin() {
        Some(u64::from(rng.between(1, u32::MAX)))
    } else {
        None
    };
    Ok(Record {
        model: job.model.clone(),
        arm: job.arm.clone(),
        task: job.task.clone(),
        profile: job.profile.clone(),
        trial: job.trial,
        trial_id: trial_id(job),
        mode: job.mode.record_label().to_owned(),
        compiled: rng.coin(),
        repairs: rng.between(0, 4),
        cleanup_ok: rng.coin(),
        oracle,
        oracle_wall_ms: f64::from(rng.between(0, 4_000_000)) / 1000.0,
        oracle_peak_rss_bytes: rss,
        tokens_in: u64::from(rng.between(0, 200_000)),
        tokens_out: u64::from(rng.between(0, 50_000)),
        model_wall_ms: u64::from(rng.between(0, 900_000)),
        compile_wall_ms: f64::from(rng.between(0, 900_000)) / 1000.0,
        consumer_lines: count(rng, 0, 400)?,
        orchestration_sites: total,
        orchestration_site_tokens: sites,
        orchestration_token_list: bench.tokens.clone(),
        lockfile: bench.lockfile_note.clone(),
        lgwks_bot_sha: format!(
            "{:016x}{:016x}{:08x}",
            rng.next_u64(),
            rng.next_u64(),
            rng.below(u32::MAX)
        ),
        lib_rs,
        error: None,
    })
}
