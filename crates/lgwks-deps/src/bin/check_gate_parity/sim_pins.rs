//! Simulated pin drift: one seed picks a workflow and one of its `toolchain:`
//! pins, sets it to a version the manifest does not name, and the checker must
//! name that version in that workflow and say nothing about the other one.
//!
//! Source: the drift rule 4 exists for (dependabot PR 730 moved
//! `rust-toolchain.toml` alone and left twenty pins behind) and the one pin the
//! checker could not see, `pages.yml`, which it never read. The draw is
//! `lgwks_std::seeded`, so a failing seed replays exactly: no clock, no hash
//! order and no thread is in the sweep.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

use lgwks_std::seeded::{DrawError, Seeded};

use super::{MANIFEST_REL, PAGES_REL, WORKFLOW_REL, pin_problems, read_manifest};

/// Why a simulation could not read or draw what it needs.
#[derive(Debug)]
enum SimError {
    /// A repository file could not be read.
    Read(io::Error),
    /// The seeded stream refused a draw.
    Draw(DrawError),
    /// The workflow text is not shaped as a drift assumes.
    Shape(&'static str),
}

impl fmt::Display for SimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Read(ref cause) => write!(formatter, "reading a repository file: {cause}"),
            Self::Draw(ref cause) => write!(formatter, "the seeded stream refused: {cause}"),
            Self::Shape(what) => {
                write!(formatter, "the workflow is not shaped for a drift: {what}")
            }
        }
    }
}

impl Error for SimError {}

impl From<io::Error> for SimError {
    fn from(cause: io::Error) -> Self {
        Self::Read(cause)
    }
}

impl From<DrawError> for SimError {
    fn from(cause: DrawError) -> Self {
        Self::Draw(cause)
    }
}

/// What every test here returns.
type TestResult = Result<(), SimError>;

/// Seeds each simulation sweeps.
const SEEDS: u64 = 64;

/// The text that opens a pin's version in a workflow.
const MARKER: &str = "toolchain: \"";

/// A drawn drift: the workflow it landed in, the workflow's text with one pin
/// changed, and the version that pin now names.
struct Drift {
    /// The workflow's file name.
    workflow: &'static str,
    /// The workflow text with one pin set to `bogus`.
    mutated: String,
    /// The version no manifest names.
    bogus: String,
}

/// A committed file of the repository, by its path from the root.
fn read_repo(rel: &str) -> Result<String, SimError> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    Ok(std::fs::read_to_string(root.join(rel))?)
}

/// A workflow the checker holds to the manifest.
struct Workflow {
    /// The workflow's file name.
    name: &'static str,
    /// The committed text.
    text: String,
}

/// The two workflows the checker holds to the manifest.
fn workflows() -> Result<[Workflow; 2], SimError> {
    Ok([
        Workflow {
            name: "ci.yml",
            text: read_repo(WORKFLOW_REL)?,
        },
        Workflow {
            name: "pages.yml",
            text: read_repo(PAGES_REL)?,
        },
    ])
}

/// The offset of every pin's first version character in `text`.
fn pin_starts(text: &str) -> Vec<usize> {
    text.match_indices(MARKER)
        .map(|(start, marker)| start.saturating_add(marker.len()))
        .collect()
}

/// The drift `seed` draws over `workflows`: a workflow, one of its pins, and a
/// version for it.
fn drift(seed: u64, workflows: &[Workflow]) -> Result<Drift, SimError> {
    let mut stream = Seeded::from_seed(seed);
    let drawn = workflows
        .get(stream.index(workflows.len())?)
        .ok_or(SimError::Shape("no workflow was drawn"))?;
    let starts = pin_starts(&drawn.text);
    let start = *starts
        .get(stream.index(starts.len())?)
        .ok_or(SimError::Shape("the workflow holds no pin"))?;
    let width = drawn
        .text
        .get(start..)
        .and_then(|rest| rest.find('"'))
        .ok_or(SimError::Shape("the pin is unterminated"))?;
    let bogus = format!("0.{}.0-drift", stream.below(1_000_000)?);
    let head = drawn
        .text
        .get(..start)
        .ok_or(SimError::Shape("the pin offset is cut off"))?;
    let tail = drawn
        .text
        .get(start.saturating_add(width)..)
        .ok_or(SimError::Shape("the pin is cut off"))?;
    Ok(Drift {
        workflow: drawn.name,
        mutated: format!("{head}{bogus}{tail}"),
        bogus,
    })
}

/// What one seed's drift made the checker say.
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    /// The workflow the drift landed in.
    workflow: &'static str,
    /// The version the drifted pin was set to.
    bogus: String,
    /// What the checker said about that workflow.
    problems: Vec<String>,
}

/// Everything one sweep observed, in seed order.
fn sweep(rust: &str, workflows: &[Workflow]) -> Result<Vec<Verdict>, SimError> {
    let mut trace = Vec::new();
    for seed in 0..SEEDS {
        let case = drift(seed, workflows)?;
        let problems = pin_problems(rust, case.workflow, &case.mutated);
        trace.push(Verdict {
            workflow: case.workflow,
            bogus: case.bogus,
            problems,
        });
    }
    Ok(trace)
}

#[test]
fn sim_one_drifted_pin_in_either_workflow_is_named_and_the_other_stays_clean() -> TestResult {
    let rust = read_manifest(&read_repo(MANIFEST_REL)?).rust;
    let workflows = workflows()?;
    for workflow in &workflows {
        assert_eq!(
            pin_problems(&rust, workflow.name, &workflow.text),
            Vec::<String>::new(),
            "the committed {} pins the manifest's toolchain",
            workflow.name
        );
    }
    let mut reached = BTreeSet::new();
    for seed in 0..SEEDS {
        let case = drift(seed, &workflows)?;
        let problems = pin_problems(&rust, case.workflow, &case.mutated);
        assert_eq!(
            problems.len(),
            1,
            "seed {seed}: one drift, one problem: {problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains(&case.bogus) && problem.contains(case.workflow)),
            "seed {seed}: the drifted pin is named in {}: {problems:?}",
            case.workflow
        );
        reached.insert(case.workflow);
    }
    assert_eq!(
        reached.len(),
        workflows.len(),
        "the sweep drifted a pin in every workflow"
    );
    Ok(())
}

#[test]
fn sim_the_same_seed_replays_the_same_verdicts_and_distinct_seeds_draw_distinct_pins() -> TestResult
{
    let rust = read_manifest(&read_repo(MANIFEST_REL)?).rust;
    let workflows = workflows()?;
    let first = sweep(&rust, &workflows)?;
    let second = sweep(&rust, &workflows)?;
    assert_eq!(first, second, "one seed, one trace");
    let versions: BTreeSet<&String> = first.iter().map(|seen| &seen.bogus).collect();
    assert!(
        versions.len() > 1,
        "distinct seeds draw distinct versions, not one: {versions:?}"
    );
    Ok(())
}
