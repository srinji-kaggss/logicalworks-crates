//! The fault space of the subject, coverage and partial-submission family
//! (T31, T33, T34): the faults, the seeded draw, the scenario each describes and
//! the run that returns the receiver's record.
//!
//! Split out of `sim_review_path.rs` by responsibility (the size law). The sweeps
//! and per-arm properties that consume these live in `subject_arms.rs`.

use lgwks_bot::domain::gh::ReviewComment;
use lgwks_bot::review::ReviewOutcome;

use super::fake_gh::{FakeGh, Scenario};
use super::{
    CAPTURE, HEAD, Run, error_text, gh_for, host, normalized_argv, request, review_task, sim,
};

// ── Subject, coverage and partial-submission family (T31, T33, T34) ─────────

/// The fault family the subject/coverage/partial/permission rows sweep.
///
/// Named rather than indexed so a family's fault and its expectation read
/// together, and exhaustive over the states those rows name: a rename, an
/// unavailable diff, each diff ceiling, an unsubmitted draft, a partial
/// submission, and a lost read permission.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) enum SubjectFault {
    /// Everything works; the create answers and the read-back finds it whole.
    Clean,
    /// The repository answers with a renamed-repository redirect.
    Moved,
    /// The server declines to render the diff.
    DiffUnavailable,
    /// The changed-file inventory is past its file-count ceiling.
    DiffOverFiles,
    /// The changed-file patch text is past its byte ceiling.
    DiffOverBytes,
    /// The create landed as an unsubmitted draft and its response was lost.
    PendingDraft,
    /// The create landed with fewer inline comments than intended, response lost.
    PartialComments,
    /// The create landed and the read-back lost permission.
    ReadDenied,
}

impl SubjectFault {
    /// How many faults the family contains.
    const VARIANTS: u64 = 8;

    /// Every fault's label, so a coverage test does not re-list the enum.
    pub(super) const LABELS: [&'static str; 8] = [
        "clean",
        "moved",
        "diff-unavailable",
        "diff-over-files",
        "diff-over-bytes",
        "pending-draft",
        "partial-comments",
        "read-denied",
    ];

    /// The fault for `index`.
    const fn for_index(index: u64) -> Self {
        match index % Self::VARIANTS {
            0 => Self::Clean,
            1 => Self::Moved,
            2 => Self::DiffUnavailable,
            3 => Self::DiffOverFiles,
            4 => Self::DiffOverBytes,
            5 => Self::PendingDraft,
            6 => Self::PartialComments,
            _ => Self::ReadDenied,
        }
    }

    /// A short label, for the trace and for the failure message.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Moved => "moved",
            Self::DiffUnavailable => "diff-unavailable",
            Self::DiffOverFiles => "diff-over-files",
            Self::DiffOverBytes => "diff-over-bytes",
            Self::PendingDraft => "pending-draft",
            Self::PartialComments => "partial-comments",
            Self::ReadDenied => "read-denied",
        }
    }

    /// Whether a run under this fault may report a verified publication.
    ///
    /// Only the clean world can: every other fault is a refusal, a coverage
    /// decision or an unverified effect, and a run that reported `Published`
    /// under one would be reporting an effect it did not observe.
    pub(super) const fn can_be_verified(self) -> bool {
        matches!(self, Self::Clean)
    }

    /// How much of a call's answer the family must retain for this fault.
    ///
    /// The two ceiling faults build an answer larger than the default capture,
    /// so their envelope is raised: the refusal under test is the *diff*
    /// ceiling, not the capture bound.
    pub(super) const fn capture(self) -> usize {
        match self {
            Self::DiffOverFiles | Self::DiffOverBytes => 4 * 1024 * 1024,
            _ => CAPTURE,
        }
    }
}

/// The two inline comments a partial-submission run intends.
fn comments(count: u32) -> Vec<ReviewComment> {
    (1..=count)
        .map(|line| {
            ReviewComment::new(
                format!("src/f{line}.rs"),
                u64::from(line),
                format!("comment {line}"),
            )
        })
        .collect()
}

/// What one seed draws beyond its fault.
///
/// The fault is `index % 8`, so a band of eight seeds reaches every fault; the
/// draw is what makes two seeds with one fault two different worlds — a diff one
/// file or sixty-four files past the ceiling, a partial submission that landed
/// none of five comments or four of them — rather than one scenario replayed.
#[derive(Debug, Clone, Copy)]
pub(super) struct SubjectDraw {
    /// Files past `MAX_DIFF_FILES_PER_PULL` the receiver reports.
    pub(super) files_over: u32,
    /// Patch bytes past `MAX_DIFF_BYTES` the receiver reports.
    pub(super) bytes_over: u32,
    /// Inline comments the run intends.
    pub(super) intended: u32,
    /// How many of them the receiver says landed; always fewer than intended.
    pub(super) applied: u32,
}

impl SubjectDraw {
    /// The draw for seed `index`, from the shared generator.
    pub(super) fn for_seed(index: u64) -> Self {
        let mut rng = sim::Rng::new(index);
        let intended = rng.between(2, 6);
        Self {
            files_over: rng.between(1, 64),
            bytes_over: rng.between(1, 4096),
            intended,
            applied: rng.below(intended),
        }
    }
}

/// The scenario a subject fault describes.
pub(super) fn subject_scenario(
    fault: SubjectFault,
    head: &str,
    draw: SubjectDraw,
) -> Result<Scenario, Box<dyn std::error::Error>> {
    let scenario = Scenario::new(head);
    Ok(match fault {
        SubjectFault::Clean => scenario,
        SubjectFault::Moved => scenario.repository_moved(),
        SubjectFault::DiffUnavailable => scenario.diff_unavailable(),
        SubjectFault::DiffOverFiles => scenario.with_filler_files(
            u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_FILES_PER_PULL)?
                .saturating_add(draw.files_over),
        ),
        SubjectFault::DiffOverBytes => scenario.with_diff_bytes(
            u32::try_from(lgwks_bot::domain::gh::MAX_DIFF_BYTES)?.saturating_add(draw.bytes_over),
        ),
        SubjectFault::PendingDraft => scenario.records_pending_draft(),
        SubjectFault::PartialComments => scenario.records_partial_comments(draw.applied),
        SubjectFault::ReadDenied => scenario.deny_reads(),
    })
}

/// Run one seed's subject fault through the real path.
///
/// The fault is this seed's own, so this is [`run_subject_fault`] with the fault
/// taken from the seed rather than named by the caller. It delegates rather than
/// repeating the run, so the sweep and the arm families cannot drift apart.
pub(super) fn run_subject_seed(
    index: u64,
) -> Result<(SubjectFault, Run), Box<dyn std::error::Error>> {
    run_subject_fault(SubjectFault::for_index(index), index)
}

/// The over-ceiling draws a family sweeps, shared by every arm family below.
///
/// One helper rather than a per-family literal: the arm families differ in what
/// they assert about a draw, not in how a draw is produced, and four copies of
/// the generator would be four chances for one arm's fault space to drift from
/// the others'.
pub(super) fn over_ceiling_draw(index: u64) -> SubjectDraw {
    SubjectDraw::for_seed(index)
}

/// Run one seed under one subject fault and return the receiver's record.
///
/// [`run_subject_seed`] is this with the seed's own fault, so an arm family
/// cannot be testing a different run than the sweep does. The fault is named
/// rather than taken from the seed so an arm family sweeps *one* arm across
/// many draws, which is the sampling the arm properties are about.
pub(super) fn run_subject_fault(
    fault: SubjectFault,
    index: u64,
) -> Result<(SubjectFault, Run), Box<dyn std::error::Error>> {
    let draw = over_ceiling_draw(index);
    let host = host()?;
    let job = review_task()?;
    let fake = FakeGh::install(fault.label(), HEAD)?;
    fake.configure(subject_scenario(fault, HEAD, draw)?)?;

    let mut request = request(7)?;
    if fault == SubjectFault::PartialComments {
        request = request.with_comments(comments(draw.intended));
    }
    let report = host.block_on(&job, (gh_for(&fake, fault.capture())?, request))?;
    let argv = normalized_argv(&fake.calls()?);
    Ok((
        fault,
        Run {
            report,
            creates: fake.creates()?,
            argv,
        },
    ))
}

/// The reason a run recorded, wherever it recorded it.
pub(super) fn reason_of(run: &Run) -> String {
    let unpublished = run
        .report
        .output()
        .filter(|outcome| !matches!(outcome, ReviewOutcome::Published { .. }));
    match unpublished {
        Some(outcome) => match outcome.unknown_reason() {
            Some(reason) => String::from(reason),
            None => format!("{outcome:?}"),
        },
        None => error_text(&run.report),
    }
}
