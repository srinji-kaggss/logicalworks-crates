//! Continue-as-new over ten million attempts: the lifecycle measured, not modelled.
//!
//! Issue #267's acceptance, run against the shipped [`FileJournal`] at the shipped
//! trigger ([`ContinuationPolicy::declared`], 80% of either ceiling):
//!
//! 1. **The long run.** Ten million attempts (or the count given as the first
//!    argument) in a seeded mix of verified, verification-failed and
//!    outcome-unknown, written through the group-commit door and continued every
//!    time the journal's own watermark says so. An oracle ledger records what every
//!    attempt meant; at every continuation the successor's fold must agree with it,
//!    and at the end every attempt is presented again and must be refused. The
//!    resident set is read at one, five and ten million attempts.
//! 2. **Reopen cost.** A live successor behind 1,000, 100,000 and 1,000,000
//!    historical attempts is reopened fifty times each; the report is p50/p95/p99
//!    reopen time, the successor's size on disk and the resident set after.
//!
//! Sealed predecessors are deleted once their successor is established. That is a
//! host's retention policy, not the journal's: a sealed file is read-only history
//! the journal never reads again, and a run of ten million attempts keeps ten
//! gigabytes of it otherwise.
//!
//! ```text
//! /usr/bin/time -l cargo run --locked --release -p lgwks_bot --example journal_continuation
//! ```
//!
//! Resident set is read from `/proc/self/status` on Linux and from `ps` elsewhere,
//! because the peak counter is `getrusage` and this crate has no FFI edge to reach
//! it; `/usr/bin/time -l` wrapping the run reports
//! the process's true peak beside it. Output goes through `std::io::Write` because
//! `clippy::print_stdout` is forbidden workspace-wide.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, EnvironmentId,
    FlowRevision, Id128, RunId,
};
use lgwks_bot::journal::{
    AttemptStatus, EffectEvent, EffectEvidence, EffectJournal, FileJournal, Verification,
    VerificationResult,
};

#[path = "support/measure.rs"]
mod measure;
#[path = "../tests/support/scratch.rs"]
mod scratch;

use scratch::Scratch;

/// Every fallible step reports its cause rather than a code.
type Outcome<T> = Result<T, Box<dyn Error>>;

/// The run the acceptance names.
const DEFAULT_ATTEMPTS: u64 = 10_000_000;
/// Actions the attempts are spread across. Every continuation folds the settled
/// history to one record per action, so this is the size of every checkpoint.
const ACTIONS: u64 = 64;
/// Attempts per group commit: one flush per batch, as a controller with a
/// backlog to record would write them.
const BATCH: u64 = 1_024;
/// The in-flight set the run reconciles down to after each batch.
const LOW_WATER: usize = 16;
/// Where the long run reads its resident set.
const RSS_AT: [u64; 3] = [1_000_000, 5_000_000, 10_000_000];
/// The histories the reopen measurement stands a successor behind.
const REOPEN_HISTORIES: [u64; 3] = [1_000, 100_000, 1_000_000];
/// Reopens per history.
const REOPEN_SAMPLES: usize = 50;
/// Seed for the fate of every attempt, so two runs of one build are one run.
const SEED: u64 = 0x5eed_0267;

/// What one attempt was meant to end as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Applied, and the verification held.
    Verified,
    /// Applied, and the verification did not hold.
    VerificationFailed,
    /// Dispatched with no outcome: carried across continuations until evidence.
    Unknown,
}

impl Fate {
    /// The status a fold reports for an attempt walked to this fate.
    const fn status(self) -> AttemptStatus {
        match self {
            Self::Verified => AttemptStatus::Verified,
            Self::VerificationFailed => AttemptStatus::VerificationFailed,
            Self::Unknown => AttemptStatus::OutcomeUnknown,
        }
    }
}

/// splitmix64: the fate source. Deterministic, so the run replays.
struct Fates(u64);

impl Fates {
    /// The next fate: one in a hundred unknown, two in a hundred failing
    /// verification, the rest verified.
    fn next(&mut self) -> Fate {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        match (mixed ^ (mixed >> 31)) % 100 {
            0 => Fate::Unknown,
            1 | 2 => Fate::VerificationFailed,
            _ => Fate::Verified,
        }
    }
}

/// Every key the run mints, under one run identity.
struct Keys {
    /// The run, environment and flow every key shares.
    identity: EffectIdentity,
    /// One action id per action number.
    actions: Vec<ActionId>,
    /// The digest every key binds.
    digest: ActionDigest,
    /// The epoch every key is fenced at.
    epoch: EnvironmentEpoch,
    /// The predicate every verification names.
    predicate: Id128,
}

impl Keys {
    /// The shared identity and the action table.
    fn new() -> Outcome<Self> {
        let run = RunId::from_hex("02670267026702670267026702670267")?;
        let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
        let flow = FlowRevision::from_tagged("blake3_256", &"0f".repeat(32))?;
        let mut actions = Vec::new();
        for action in 1..=ACTIONS {
            actions.push(ActionId::from_hex(&format!("{action:032x}"))?);
        }
        let digest = ActionDigest::from_tagged("blake3_256", &"a5".repeat(32))?;
        let epoch = EnvironmentEpoch::from_decimal("1")?;
        let predicate = Id128::from_hex(&"42".repeat(16))?;
        Ok(Self {
            identity: EffectIdentity::new(run, environment, flow),
            actions,
            digest,
            epoch,
            predicate,
        })
    }

    /// The key of the `ordinal`-th attempt the run made, counted from zero.
    ///
    /// Attempts go round the actions, so each action's attempt numbers rise by one
    /// per lap: the journal refuses an attempt at or below the latest its action
    /// walked, and this is the order that never asks it to.
    fn of(&self, ordinal: u64) -> Outcome<(u64, u64, EffectKey)> {
        let action = ordinal
            .checked_rem(ACTIONS)
            .ok_or("no actions to spread over")?;
        let lap = ordinal
            .checked_div(ACTIONS)
            .ok_or("no actions to spread over")?;
        let attempt = lap.saturating_add(1);
        let index = usize::try_from(action)?;
        let id = *self
            .actions
            .get(index)
            .ok_or("an action number past the table")?;
        let attempt_id = AttemptId::from_decimal(&attempt.to_string())?;
        let key = self.identity.key(id, attempt_id, self.digest, self.epoch);
        Ok((action, attempt, key))
    }

    /// The rungs an attempt with `fate` walks, appended to `batch`.
    fn ladder(&self, key: EffectKey, fate: Fate, batch: &mut Vec<EffectEvent>) {
        batch.push(EffectEvent::IntentAdmitted { key });
        batch.push(EffectEvent::DispatchPrepared { key });
        let held = match fate {
            Fate::Unknown => return,
            Fate::Verified => VerificationResult::Satisfied,
            Fate::VerificationFailed => VerificationResult::NotSatisfied,
        };
        batch.push(EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        });
        batch.push(EffectEvent::Verified {
            key,
            verification: Verification::new(
                self.predicate,
                1,
                lgwks_std::hash::blake3(b"journal_continuation's postcondition"),
                held,
            ),
        });
    }
}

/// What the run meant to do, independent of the journal's own fold.
#[derive(Default)]
struct Oracle {
    /// The latest attempt of every action and the status it should fold to.
    latest: BTreeMap<u64, (EffectKey, AttemptStatus)>,
    /// Every attempt with no outcome yet, by key.
    unknown: HashSet<EffectKey>,
}

/// The long run's moving parts.
struct Run {
    /// The live generation.
    journal: FileJournal,
    /// Key minting and ladders.
    keys: Keys,
    /// The seeded fates.
    fates: Fates,
    /// The independent record.
    oracle: Oracle,
    /// Unknown attempts, oldest first, waiting for evidence.
    stranded: VecDeque<(u64, EffectKey)>,
    /// The batch being filled, reused.
    batch: Vec<EffectEvent>,
    /// Attempts made so far.
    dispatched: u64,
    /// Continuations so far.
    continuations: u64,
}

impl Run {
    /// Open the base journal at the shipped trigger.
    fn open(dir: &Path) -> Outcome<Self> {
        Ok(Self {
            journal: FileJournal::open_continuing(dir.join("run.jrnl"))?,
            keys: Keys::new()?,
            fates: Fates(SEED),
            oracle: Oracle::default(),
            stranded: VecDeque::new(),
            batch: Vec::new(),
            dispatched: 0,
            continuations: 0,
        })
    }

    /// Write `count` attempts in one flush, then continue if the watermark is due.
    ///
    /// The evidence that settles the oldest unknown attempts rides in the same
    /// flush, as a controller that reconciles while it works writes it: one
    /// `sync_all` per batch, not one per settled unknown.
    fn batch(&mut self, count: u64) -> Outcome<()> {
        self.batch.clear();
        for _ in 0..count {
            self.admit_one()?;
        }
        self.reconcile();
        self.journal.compare_and_append_all(&self.batch)?;
        if !self.journal.continuation_watermark()?.is_due() {
            return Ok(());
        }
        self.continue_now()?;
        self.continuations = self.continuations.saturating_add(1);
        agrees(&self.journal, &self.oracle)
    }

    /// Add one attempt's ladder to the batch and to the oracle.
    fn admit_one(&mut self) -> Outcome<()> {
        let fate = self.fates.next();
        let (action, _attempt, key) = self.keys.of(self.dispatched)?;
        self.keys.ladder(key, fate, &mut self.batch);
        self.oracle.latest.insert(action, (key, fate.status()));
        if fate == Fate::Unknown {
            self.oracle.unknown.insert(key);
            self.stranded.push_back((action, key));
        }
        self.dispatched = self.dispatched.saturating_add(1);
        Ok(())
    }

    /// Continue the live journal and retire the sealed predecessor.
    fn continue_now(&mut self) -> Outcome<()> {
        let successor = self
            .journal
            .continue_as_file()?
            .ok_or("a continuing journal must hand back a successor")?;
        let sealed = std::mem::replace(&mut self.journal, successor);
        let retired = sealed.path().to_path_buf();
        drop(sealed);
        std::fs::remove_file(retired)?;
        Ok(())
    }

    /// Evidence settles the oldest unknown attempts down to the low water.
    ///
    /// Into the batch about to be flushed, after every batch's own attempts. A run
    /// that only reconciled at its continuations would carry a whole generation's
    /// unknowns into each checkpoint, and the checkpoint's declared carry bound
    /// refuses that rather than truncating it.
    fn reconcile(&mut self) {
        while self.stranded.len() > LOW_WATER {
            let Some((action, key)) = self.stranded.pop_front() else {
                break;
            };
            self.batch.push(EffectEvent::OutcomeObserved {
                key,
                evidence: EffectEvidence::Applied,
            });
            self.oracle.unknown.remove(&key);
            if let Some(latest) = self.oracle.latest.get_mut(&action)
                && latest.0 == key
            {
                latest.1 = AttemptStatus::Applied;
            }
        }
    }
}

/// Refuse with `why`, logged where the refusal is made.
fn refuse<T>(why: String) -> Outcome<T> {
    let refusal: Outcome<T> = Err(why.into());
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "journal_continuation: refusing");
    refusal
}

/// The journal's fold agrees with the oracle: every action's latest attempt has
/// the status the oracle recorded, and the uncertain set is exactly the oracle's.
fn agrees(journal: &FileJournal, oracle: &Oracle) -> Outcome<()> {
    let recovered = journal.recover();
    for (action, &(key, status)) in &oracle.latest {
        let folded = recovered.status(key);
        if folded != Some(status) {
            return refuse(format!(
                "generation {}: action {action}'s latest attempt folds to {folded:?}, the \
                 oracle recorded {status:?}",
                journal.generation()
            ));
        }
    }
    let uncertain: HashSet<EffectKey> = recovered.uncertain().into_iter().collect();
    if uncertain != oracle.unknown {
        return refuse(format!(
            "generation {}: the journal is uncertain about {} attempts, the oracle about {}",
            journal.generation(),
            uncertain.len(),
            oracle.unknown.len()
        ));
    }
    Ok(())
}

/// This process's resident set in KiB.
///
/// `VmRSS` on Linux. Elsewhere `ps`, addressed through the shell's `$PPID`, which
/// is this process because the shell is spawned directly by it: the same read
/// `tests/it/journal_scale.rs` makes, and for the same reason, since
/// `std::process::id` is an OS-reused identifier the std-first gate refuses.
fn rss_kib() -> Outcome<u64> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .ok_or("/proc/self/status has no VmRSS line")?;
        let value = line
            .split_whitespace()
            .next()
            .ok_or("an empty VmRSS line")?;
        return Ok(value.parse()?);
    }
    let output = std::process::Command::new("sh")
        .args(["-c", "ps -o rss= -p $PPID"])
        .output()?;
    let text = String::from_utf8(output.stdout)?;
    Ok(text.trim().parse()?)
}

/// One line of the report.
fn say(line: &str) -> Outcome<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    Ok(())
}

/// The long run: attempts, continuations, the oracle at every boundary, the
/// resident set at each mark, every attempt refused a second time, and a reopen.
fn long_run(attempts: u64) -> Outcome<()> {
    let scratch = Scratch::new("journal-continuation-long")?;
    let started = Instant::now();
    let mut run = Run::open(scratch.path())?;
    let mut marks = RSS_AT.iter().copied().peekable();
    while run.dispatched < attempts {
        run.batch(BATCH.min(attempts.saturating_sub(run.dispatched)))?;
        if let Some(&mark) = marks.peek()
            && run.dispatched >= mark
        {
            marks.next();
            let rss = rss_kib()?;
            say(&format!(
                "attempts={} generation={} continuations={} rss_kib={rss} elapsed={:?}",
                run.dispatched,
                run.journal.generation(),
                run.continuations,
                started.elapsed()
            ))?;
        }
    }
    agrees(&run.journal, &run.oracle)?;
    if run.continuations < 100 {
        return refuse(format!(
            "{attempts} attempts reached only {} continuations",
            run.continuations
        ));
    }
    let written = started.elapsed();
    let refused = refuse_every_attempt(&mut run, attempts)?;
    let live = run.journal.path().to_path_buf();
    let Run {
        journal, oracle, ..
    } = run;
    drop(journal);
    let reopened = FileJournal::open_continuing(&live)?;
    agrees(&reopened, &oracle)?;
    say(&format!(
        "long run: attempts={attempts} continuations={} generation={} still_unknown={} \
         re-presentations_refused={refused} write_wall={written:?} total_wall={:?}",
        reopened.generation().saturating_sub(1),
        reopened.generation(),
        oracle.unknown.len(),
        started.elapsed()
    ))
}

/// Present every attempt the run made a second time; every one must be refused.
fn refuse_every_attempt(run: &mut Run, attempts: u64) -> Outcome<u64> {
    let mut refused = 0_u64;
    for ordinal in 0..attempts {
        let (_action, _attempt, key) = run.keys.of(ordinal)?;
        let again = EffectEvent::IntentAdmitted { key };
        if run
            .journal
            .compare_and_append(run.journal.tail(), &again)
            .is_ok()
        {
            return refuse(format!("attempt {ordinal} was admitted a second time"));
        }
        refused = refused.saturating_add(1);
    }
    Ok(refused)
}

/// Reopen a live successor behind `history` attempts and report what it costs.
fn reopen_cost(history: u64) -> Outcome<()> {
    let scratch = Scratch::new("journal-continuation-reopen")?;
    let mut run = Run::open(scratch.path())?;
    while run.dispatched < history {
        run.batch(BATCH.min(history.saturating_sub(run.dispatched)))?;
    }
    run.continue_now()?;
    let live = run.journal.path().to_path_buf();
    drop(run);
    let mut samples = Vec::with_capacity(REOPEN_SAMPLES);
    for _ in 0..REOPEN_SAMPLES {
        let started = Instant::now();
        let reopened = FileJournal::open_continuing(&live)?;
        samples.push(started.elapsed().as_micros());
        drop(reopened);
    }
    let summary = measure::Summary::of(&mut samples);
    let bytes = std::fs::metadata(&live)?.len();
    let rss = rss_kib()?;
    say(&format!(
        "{} successor_bytes={bytes} rss_kib={rss}",
        summary.line(&format!("reopen behind {history} attempts"))
    ))
}

fn main() -> Outcome<()> {
    let attempts = match std::env::args().nth(1) {
        Some(count) => count.parse()?,
        None => DEFAULT_ATTEMPTS,
    };
    for history in REOPEN_HISTORIES {
        reopen_cost(history)?;
    }
    long_run(attempts)
}
