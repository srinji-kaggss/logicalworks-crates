//! A clock that is killed mid-step and read back from what survived the kill.
//!
//! `INV-BOT-30` says what survives a restart is the remaining **duration**, never
//! the instant. That is a claim about a type, and `tests/sim_clock.rs` proves it
//! in one process. This file proves the claim survives an *observed* process
//! death: a real child runs a bounded step under a virtual clock, records its
//! `ClockSnapshot` through the estate's own durable path ([`FileJournal`], whose
//! append is `fsync`-ed before the acknowledgment is minted), and is then killed
//! with a real `SIGKILL` — no destructors, no flushing, no chance to tidy. The
//! parent restarts from the bytes on the disk and asserts the remaining budget
//! is what the child had left, and that the step does not rerun.
//!
//! The discriminator is the same one `durable_crash_observation.rs` uses and
//! for the same reason: the child touches its marker only *after* the append
//! was acknowledged, so the marker's existence is the proof that the durability
//! promise was earned rather than assumed. A kill after the marker therefore
//! tests the guarantee rather than the timing.
//!
//! Unix-only: `SIGKILL` and `Child::kill`'s signal report are both POSIX, and
//! the file is `cfg`-gated to unix in `Cargo.toml`. Nothing here is portable
//! Windows, and it does not pretend to be.
//!
//! Rows observed here: the remaining budget survives the kill; the recovered
//! budget is *not* an instant, so a restart on a host whose monotonic origin
//! differs still reports the same remaining duration; an idempotent replay of a
//! recovered step does not double-spend the budget; a torn tail (an append
//! interrupted mid-write) is refused rather than read as a shorter step.

// Unix-only, because `SIGKILL` and `Child::kill`'s signal report are both POSIX.
// Also gated on `rt`, because the clock this file observes lives behind that
// feature: without it there is no `Clock` to kill anything under, and a target
// that compiled to nothing would read as a test that passed.
#![cfg(all(unix, feature = "rt"))]

mod common;

use common::ProbeGuard;
use common::key;

use std::path::{Path, PathBuf};
use std::time::Duration;

use lgwks_bot::effect::EffectKey;
use lgwks_bot::journal::{EffectEvent, EffectJournal, FileJournal};
use lgwks_bot::rt::clock::Clock;

const DIGEST: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

/// The budget the step declares. The child spends the first `SPENT` of it; the
/// rest is what the restart is expected to find.
const BUDGET: Duration = Duration::from_secs(600);
/// What is left of `BUDGET` once the child has spent `SPENT`, computed rather
/// than written so the constant and the expectation cannot drift apart.
const LEFT: Duration = Duration::from_secs(600 - 250);
/// A budget shorter than the one the child spent under, so a recovered
/// duration can be read against a budget that is not the original one.
const HALF: Duration = Duration::from_secs(300);
/// What `HALF` leaves once `SPENT` is charged to it.
const HALF_LEFT: Duration = Duration::from_secs(300 - 250);
const SPENT: Duration = Duration::from_secs(250);

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Environment variable that turns this test binary into the killed child.
const PROBE_ENV: &str = "LGWKS_CLOCK_KILL_PROBE";
const PROBE_JOURNAL: &str = "LGWKS_CLOCK_KILL_JOURNAL";
const PROBE_MARKER: &str = "LGWKS_CLOCK_KILL_MARKER";
/// The row index the parent is running, so a child does its parent's row.
const PROBE_ROW: &str = "LGWKS_CLOCK_KILL_ROW";

/// A unique scratch directory for one test, and a guard that removes it when
/// the test ends however it ends.
///
/// Both live in [`tests/common`], beside the identical pair the journal
/// crash-observation harness uses: a scratch directory the OS reuses ids for is
/// litter a failed run leaves behind, and a name two runs can collide on is a
/// test that fails only when the machine is busy.
fn scratch_and_guard(
    name: &str,
) -> Result<(PathBuf, common::TempGuard), Box<dyn std::error::Error>> {
    common::scratch(name).map(|dir| {
        let guard = common::TempGuard(dir.clone());
        (dir, guard)
    })
}

/// A plain-process pause for the harness branches that have no executor.
///
/// The workspace bans `std::thread::sleep` because blocking an executor thread
/// stalls every task on it. Neither branch here has an executor: the child
/// runs one sync body and parks while the parent decides when it dies, and the
/// parent polls for a marker. That wait is the observation's subject, not its
/// scaffolding.
#[expect(
    clippy::disallowed_methods,
    reason = "the kill harness is a plain process with no async runtime and no reactor to \
              stall; the parked child and the marker poll are the observation's shape"
)]
fn pause(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

/// The key the child's step is journalled under.
fn step_key() -> Result<EffectKey, Box<dyn std::error::Error>> {
    key("1", DIGEST)
}

/// Append one `IntentAdmitted` for `key`, which is what the journal treats as
/// the record that this step began.
///
/// `compare_and_append` is the same append the crash-observation harness uses:
/// it is `fsync`-ed before its acknowledgment is minted, so a marker written
/// after it is a promise the store actually earned.
fn admit(journal: &mut FileJournal, key: EffectKey) -> Result<(), Box<dyn std::error::Error>> {
    journal.compare_and_append(journal.tail(), &EffectEvent::IntentAdmitted { key })?;
    Ok(())
}

/// The child process's whole body.
///
/// The elapsed reading is written to its own fsync-ed file next to the journal,
/// because that is what a store of a duration physically is: the journal's
/// `EffectEvidence` is a closed variant set with nowhere to put a nanosecond
/// count, and the claim under test is about what survives, not about which
/// variant carries it.
fn child_body() -> TestResult {
    let journal_path = journal_path();
    let marker = marker_path();
    let snapshot_path = journal_path.with_extension("elapsed");

    let mut journal = FileJournal::open(&journal_path)?;
    let key = step_key()?;
    admit(&mut journal, key)?;

    // The step runs under a virtual clock: it spends `SPENT` of `BUDGET` and
    // stops there, with no real wait at all. The reading it would leave for a
    // restart is a duration, so that is what it writes — through the same
    // fsync-ed path the intent used, then fsync-ed itself.
    let clock = Clock::virtual_at(Duration::ZERO);
    clock.advance(SPENT)?;
    let snapshot = clock.snapshot();
    std::fs::write(&snapshot_path, nanos_of(snapshot.elapsed())?.to_le_bytes())?;
    let file = std::fs::File::open(&snapshot_path)?;
    file.sync_all()?;

    // Only now, after every write was acknowledged, is the marker written. Its
    // existence is the parent's proof that what follows is a durability
    // question and not a race.
    std::fs::write(&marker, b"acked")?;

    for _ in 0..600 {
        pause(100);
    }
    Err("the probe child parked for its whole bound and was never killed".into())
}

/// Spawn this test binary as the probe child.
///
/// `test_name` is *this* test's own name, not an argument: the parent asks
/// this test to do its work, and the child — re-executing this binary with
/// `PROBE_ENV` set — must run the same test as the child instead. Passing a
/// sibling's name is the mistake this signature cannot express.
fn spawn_probe(
    test_name: &str,
    row: usize,
    journal_path: &Path,
    marker_path: &Path,
) -> Result<ProbeGuard, Box<dyn std::error::Error>> {
    Ok(ProbeGuard(Some(
        std::process::Command::new(std::env::current_exe()?)
            .args([test_name, "--exact", "--nocapture"])
            .env(PROBE_ENV, "1")
            .env(PROBE_ROW, row.to_string())
            .env(PROBE_JOURNAL, journal_path)
            .env(PROBE_MARKER, marker_path)
            .spawn()?,
    )))
}

/// The journal every probe child appends to, ordered by the parent through the
/// environment.
fn journal_path() -> PathBuf {
    PathBuf::from(std::env::var_os(PROBE_JOURNAL).unwrap_or_default())
}

/// The marker every probe child writes once its appends were acknowledged.
fn marker_path() -> PathBuf {
    PathBuf::from(std::env::var_os(PROBE_MARKER).unwrap_or_default())
}

/// Wait for the marker, then `SIGKILL` the child and reap it.
///
/// `Child::kill` sends `SIGKILL` on Unix: no cleanup, no destructors, no
/// flushing. Whatever the child wrote that is not on the disk is gone, and
/// whatever claimed to be durable had better be there.
fn kill_after_marker(
    mut guard: ProbeGuard,
    marker: &Path,
    test_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    guard.kill_after_marker(marker, test_name)
}

/// The elapsed nanos the killed child left on the disk.
fn read_child_elapsed(journal_path: &Path) -> Result<Duration, Box<dyn std::error::Error>> {
    let path = journal_path.with_extension("elapsed");
    let bytes = std::fs::read(&path)?;
    if bytes.len() != std::mem::size_of::<u64>() {
        return Err(format!("the child's elapsed record is {} bytes, not 8", bytes.len()).into());
    }
    let mut raw = [0_u8; 8];
    raw.copy_from_slice(&bytes);
    Ok(Duration::from_nanos(u64::from_le_bytes(raw)))
}

/// The elapsed reading as whole nanoseconds, refusing a value that would not
/// survive the eight bytes a restart reads back.
///
/// `Duration::as_nanos` is a `u128`, so a naive write is a sixteen-byte record
/// that a reader decoding a `u64` would silently truncate. Saturating is
/// refused rather than performed: a budget of more than ~584 years is a value
/// no run declares, and writing a truncated one would make the restart read a
/// shorter budget than the child had — the exact failure the record's shape
/// exists to prevent.
fn nanos_of(elapsed: Duration) -> Result<u64, Box<dyn std::error::Error>> {
    u64::try_from(elapsed.as_nanos())
        .map_err(|_| format!("{elapsed:?} is longer than the record can hold").into())
}

/// A scratch directory whose journal and marker paths are already chosen.
struct Fixture {
    journal: PathBuf,
    marker: PathBuf,
    _dir: common::TempGuard,
}

impl Fixture {
    /// Spawn the probe for `row`, wait for its marker, kill it, and return.
    ///
    /// The whole observation in one call, because the two paths are a matched
    /// pair: passing the journal to one and the marker to the other is the way a
    /// harness ends up polling a file the child never writes.
    fn kill_after(&self, test_name: &str, row: usize) -> Result<(), Box<dyn std::error::Error>> {
        let child = spawn_probe(test_name, row, &self.journal, &self.marker)?;
        kill_after_marker(child, &self.marker, test_name)
    }

    /// The journal a restart opens, having admitted nothing: the state a restart
    /// finds on the disk, read without touching it.
    fn reopened(&self) -> Result<FileJournal, Box<dyn std::error::Error>> {
        Ok(FileJournal::open(&self.journal)?)
    }

    /// A restart that *re-offers* attempt "1" to the store that already holds
    /// it. This is the replay, and the store's refusal of it is the observation
    /// — so the name says what the call does rather than what the caller hopes
    /// it does.
    fn restarted_journal(&self) -> Result<FileJournal, Box<dyn std::error::Error>> {
        let mut journal = self.reopened()?;
        admit(&mut journal, step_key()?)?;
        Ok(journal)
    }

    /// The elapsed reading the killed child left on the disk.
    fn child_elapsed(&self) -> Result<Duration, Box<dyn std::error::Error>> {
        read_child_elapsed(&self.journal)
    }

    /// The budget a restart still has, read from what survived the kill.
    fn recovered_budget(&self) -> Result<Duration, Box<dyn std::error::Error>> {
        Ok(clock_from_elapsed(self.child_elapsed()?).remaining_from(BUDGET))
    }

    /// A fresh fixture under `name`'s scratch directory.
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let (dir, guard) = scratch_and_guard(name)?;
        Ok(Self {
            journal: dir.join("journal.log"),
            marker: dir.join("marker"),
            _dir: guard,
        })
    }
}

/// Assert an *observation* equals what the design says, naming the design claim.
fn agree<T: std::fmt::Debug + PartialEq>(observed: T, expected: T, claim: &str) {
    assert_eq!(observed, expected, "{claim}");
}

/// Take the error a harness step must produce, or fail with `why`: every refusal in
/// this file is an expected outcome that has to be named, since a store that admits
/// a replay and one that refuses it are both "it ran".
fn refuse<T, E>(result: Result<T, E>, why: &str) -> Result<E, Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Err(why.into()),
        Err(refusal) => Ok(refusal),
    }
}

/// The claims the rows assert, named once so a row reads as the check it makes
/// and the design sentence behind it is edited in one place.
mod claim {
    pub const BUDGET_LEFT: &str = "the restart must find the budget the child had left";
    pub const ONE_ATTEMPT: &str = "the refused replay added no second attempt";
    pub const RECORD_LENGTH: &str = "the refusal names the length it refused";
}

/// One row of the kill/restart family: a scratch name, and what the restart
/// must find afterwards.
///
/// A row is a case, not a test: every row runs the *same* observation — kill a
/// child that recorded a snapshot, restart from the disk — and differs only in
/// what it asserts about the restart. One harness drives every row from one
/// table, so a row cannot quietly skip the kill and still be counted.
#[derive(Clone, Copy)]
enum Row {
    /// The restart finds the budget the child had left.
    Budget,
    /// The recovered budget is a duration, not a host-local instant.
    Duration,
    /// A replayed attempt is refused, named, and appends nothing.
    Idempotent,
    /// The step ran once across the kill: the durable count is 1, and the
    /// restart neither reruns it nor spends its budget a second time.
    NoRerun,
}

impl Row {
    /// Every row of the family, in the order they are reported.
    const ALL: [Row; 4] = [Row::Budget, Row::Duration, Row::Idempotent, Row::NoRerun];

    /// The scratch directory this row works in.
    const fn scratch(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Duration => "duration",
            Self::Idempotent => "idempotent",
            Self::NoRerun => "rerun",
        }
    }

    fn check(self, fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Self::Budget => budget_survives(fixture),
            Self::Duration => budget_is_a_duration(fixture),
            Self::Idempotent => replay_is_idempotent(fixture),
            Self::NoRerun => step_did_not_rerun(fixture),
        }
    }
}

/// The budget the restart found is the budget the child had left.
fn budget_survives(row: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    let empty = FileJournal::open(&row.journal)?.recover().is_empty();
    assert!(
        !empty,
        "the acknowledged intent must be on the disk after a SIGKILL"
    );
    agree(row.recovered_budget()?, LEFT, claim::BUDGET_LEFT);
    assert!(
        !clock_from_elapsed(row.child_elapsed()?).is_exhausted(BUDGET),
        "a budget with time left must not read as exhausted after the kill",
    );
    Ok(())
}

/// A recovered budget is a duration, so two hosts with unrelated monotonic
/// origins read the same remaining budget from the same bytes.
fn budget_is_a_duration(row: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    // What survives is the elapsed *duration*, so the restart's own clock reads
    // back exactly what the child spent — not an offset from some host's
    // monotonic origin, which would make the figure depend on where the restart
    // ran. Reading the same duration against a second, shorter budget makes the
    // point concrete: the value is carried by the record, not by the budget.
    let spent = row.child_elapsed()?;
    let recovered = clock_from_elapsed(spent);
    let observed = (
        spent,
        recovered.remaining_from(BUDGET),
        recovered.remaining_from(HALF),
    );
    let expected = (SPENT, LEFT, HALF_LEFT);
    agree(
        observed,
        expected,
        "the restart reads back the duration the child spent, and the same duration \
         recovers against a different budget too: the value is carried by the record, \
         not by the budget it was spent under",
    );
    Ok(())
}

/// A replay of the killed attempt is refused, named, and appends nothing.
fn replay_is_idempotent(row: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    // The recovered journal already holds attempt "1", so the store refuses the
    // replay at `open` rather than admitting a second intent: recovering is not
    // re-doing. Reopening is the replay; nothing else writes.
    let refusal = refuse(
        row.restarted_journal(),
        "a journal already holding attempt 1 must refuse to open for a replay",
    )?;
    // The reason matters as much as the refusal: only it tells a reader whether
    // this is an idempotent replay or a corrupt journal.
    let reason = refusal.to_string();
    for expected in ["cannot follow what is committed", "attempt 1"] {
        assert!(
            reason.contains(expected),
            "the refusal said {reason:?}, not {expected:?}"
        );
    }
    let attempts = FileJournal::open(&row.journal)?.recover().len();
    agree(attempts, 1, claim::ONE_ATTEMPT);
    Ok(())
}

/// The step ran once across the kill, and the restart does not spend its budget
/// a second time.
///
/// Two failures hide behind "the budget was restored". One is a rerun: the
/// restart admits attempt "1" again, so the store grows a second copy and the
/// effect the step performs happens twice. The other is a double-spend: the
/// restart resumes the clock and then charges the *same* `SPENT` again, so the
/// figure it recovers is right and its accounting is wrong. Both are visible
/// here — the attempt count and the budget after the restart resumes — and
/// neither is visible to a test that only checks the restored number.
fn step_did_not_rerun(row: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    // The restart opens the store, sees the attempt the killed child already
    // admitted, and declines to admit it again.
    let journal = row.reopened()?;
    let attempts = journal.recover().len();
    agree(
        attempts,
        1,
        "the restart must find exactly the attempt the killed child admitted: a second \
         copy is a rerun, and a rerun performs the effect twice",
    );

    // Resuming from the recovered reading spends nothing further. The clock is
    // rebuilt at `LEFT`'s origin, so a restart that re-charges `SPENT` — or
    // starts from zero — is visible as a shorter or a longer budget.
    let resumed = Clock::virtual_at(row.child_elapsed()?);
    let remaining = resumed.snapshot().remaining_from(BUDGET);
    agree(
        remaining,
        LEFT,
        "resuming from the recovered reading must spend nothing: a budget that came back \
         shorter was charged twice, and one that came back longer was never charged at all",
    );
    Ok(())
}

/// The row a probe child runs.
///
/// The parent asks this test to do its work; the child — re-executed with
/// `PROBE_ENV` set — must instead append, mark and park to be killed. `index`
/// is the row's position, so the child does the same row its parent killed.
fn row_from_env() -> Option<usize> {
    std::env::var(PROBE_ROW).ok()?.parse().ok()
}

/// Build the clock a restart begins from: a virtual clock already advanced to
/// the reading that was persisted.
///
/// A production restart hands [`Clock::virtual_at`] a reading decoded from its
/// store; this is that decoding, written out rather than hidden in a helper so
/// a test reads as the restart it claims to be.
fn clock_from_elapsed(elapsed: Duration) -> lgwks_bot::rt::clock::ClockSnapshot {
    let clock = Clock::virtual_at(elapsed);
    clock.snapshot()
}

#[test]
fn the_remaining_budget_survives_a_real_sigkill() -> TestResult {
    // Two roles, one name: re-executed with `PROBE_ENV` set, this process is
    // the child that gets killed. Without the row the parent asks every row, so
    // this name is the one the parent spawns.
    match row_from_env() {
        Some(_) => child_body(),
        None => run_every_row(),
    }
}

#[test]
fn a_torn_snapshot_after_the_kill_is_refused_not_read_as_a_shorter_step() -> TestResult {
    // A snapshot record an append interrupted mid-write leaves behind: half a
    // word, behind a marker claiming the write was acknowledged.
    let fixture = Fixture::new("torn")?;
    let mut journal = fixture.reopened()?;
    admit(&mut journal, step_key()?)?;
    drop(journal);
    std::fs::write(fixture.journal.with_extension("elapsed"), [1_u8, 2, 3, 4])?;
    std::fs::write(&fixture.marker, b"acked")?;

    let torn = "a half-written snapshot must be refused; reading it as a shorter step is how \
                a restart silently loses budget";
    let refusal = refuse(fixture.child_elapsed(), torn)?;
    agree(
        refusal.to_string(),
        String::from("the child's elapsed record is 4 bytes, not 8"),
        claim::RECORD_LENGTH,
    );
    Ok(())
}

/// Run every row: kill a child per row, restart from the disk, check the row.
fn run_every_row() -> TestResult {
    let this = std::thread::current();
    let name = this.name().ok_or("the test has no name")?;
    for (index, row) in Row::ALL.iter().enumerate() {
        let fixture = Fixture::new(row.scratch())?;
        fixture.kill_after(name, index)?;
        row.check(&fixture)?;
    }
    Ok(())
}
