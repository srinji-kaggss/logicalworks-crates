//! Row 1 of issue #278, on the real surface: a store that is written while it
//! is read.
//!
//! The shipped reader is `domain::data::JsonStore`, driven through its own
//! `Observe` and `Query` implementations with a proof minted by `GrantSet`. The
//! subject is a real file on a real filesystem, and the writer is a real child
//! process — not a thread inside this one — so the reads contend with a writer
//! the operating system schedules independently of this test.
//!
//! What these tests claim is exactly what the protocol claims: a reading is
//! reported only when two independent reads agreed, and a subject that never
//! settles is reported as [`BotError::UnstableObservation`] rather than as a
//! value. They do **not** claim that a settled reading is valid JSON — a writer
//! that pauses between two writes produces a stable prefix, which is the
//! writer's discipline to fix and is stated as such in `lgwks_bot::stability`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::domain::data::JsonStore;
use lgwks_bot::error::BotError;
use lgwks_bot::stability::Drift;
use lgwks_bot::verb::{Observe, Query};
use lgwks_bot::{Cap, GrantSet};

type TestResult = Result<(), Box<dyn Error>>;

/// How long each sweep polls a subject that is being rewritten.
///
/// A window rather than a poll count, because the property under test is a
/// rate: a window long enough to contain many writes makes "at least one
/// unsettled reading" a fact about the protocol rather than about how fast
/// this host happens to read a file. Fifty milliseconds holds about fifty
/// writes from the fixture below and costs the same on a laptop and on a busy
/// CI box.
const SWEEP_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

/// How long a store nobody is writing is given to settle.
const SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(200);

/// A scratch directory named by this process, a monotone sequence and the
/// clock's own reading, removed when the test ends.
///
/// Not the estate's `shared::Scratch`, which draws its tag from
/// `lgwks_std::random` behind this crate's `ephemeral` feature: this file has
/// to run in the default-feature build the gate runs, and a fixed name would be
/// a directory two concurrent runs share (INV-BOT-116).
struct Scratch {
    /// The directory itself.
    path: PathBuf,
}

/// The per-process sequence, so two scratch directories in one binary never
/// share a path.
static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl Scratch {
    /// Create a scratch directory for a test named `tag`.
    fn new(tag: &str) -> Result<Self, Box<dyn Error>> {
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "lgwks-stability-{tag}-{}-{now}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// A path inside the directory.
    fn join(&self, tail: &str) -> PathBuf {
        self.path.join(tail)
    }
}

impl Drop for Scratch {
    /// Remove the directory, best effort: every assertion has already been
    /// made by the time this runs.
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.path);
    }
}

/// Return a typed test failure, emitting it first.
///
/// The emission is what the `scan` lane requires of every `return Err`: a
/// caller that sees only a value has no signal. Building it in one place keeps
/// every scenario's refusal identical rather than one line per call site.
fn refuse(cause: impl std::fmt::Display) -> Result<(), Box<dyn Error>> {
    let refusal: Result<(), Box<dyn Error>> = Err(cause.to_string().into());
    lgwks_std::trace::debug!(
        error = ?refusal.as_ref().err(),
        "scenario: returning an error to the caller"
    );
    refusal
}

/// Proof this store's poll is admitted with, minted the way a bot's build mints
/// it.
fn proof() -> Result<lgwks_bot::Auth, BotError> {
    GrantSet::empty().grant(Cap::fs()).issue(&[Cap::fs()])
}

/// The two whole documents a rewriting writer alternates between.
///
/// A rewrite in place is the real-world shape of this failure: a tool that
/// saves a document by truncating it and writing the new bytes, which leaves a
/// window in which the file is empty and another in which it is half a
/// document. Both documents are complete, so "settled" has a precise meaning
/// here and a settled reading can be checked against them exactly.
const BEFORE: &str = "{\"revision\":1}";
const AFTER: &str = "{\"revision\":2,\"note\":\"rewritten\"}";

/// The state a rewriting subject is in between its truncate and its first byte.
///
/// Named because it is the documented caveat of the protocol made concrete: a
/// writer that saves by truncating opens a window in which the file is stably
/// empty, and two reads taken in that window agree. The protocol admits the
/// reading, because what it proves is that the file did not move between two
/// reads — not that the file holds a complete document. That guarantee is the
/// writer's, and `rename`-into-place is how a writer takes it.
const TRUNCATED: &str = "";

/// Every state a subject rewritten in place can be *stably* in.
fn settled_states() -> [&'static str; 3] {
    [BEFORE, AFTER, TRUNCATED]
}

/// How a real child process writes the subject.
#[derive(Clone, Copy)]
enum Protocol {
    /// Rewrite the subject in place, alternating [`BEFORE`] and [`AFTER`] with
    /// no pause at all: the writer that never settles, and the torn read in its
    /// purest form.
    ///
    /// Tight, and bounded by rounds rather than by size, because the property
    /// under test is a *rate*: the writer has to move faster than the reader
    /// polls or no read ever straddles a write and the sweep measures nothing.
    /// The subject stays a few hundred bytes however long the loop runs.
    Rewrite,
    /// Write [`AFTER`] to a temporary file and rename it over the subject: the
    /// writer whose discipline makes every open see whole bytes.
    Rename,
}

/// A real child process that rewrites `path` under `protocol`.
///
/// A child rather than a thread because the failure under test is contention
/// with a writer this process does not control: a thread would be scheduled by
/// the same executor the reads run on, and a run that interleaved perfectly
/// would prove less than one that interleaved at random.
fn start_writer(path: &Path, protocol: Protocol) -> Result<Child, Box<dyn Error>> {
    let (step, pause) = match protocol {
        Protocol::Rewrite => (
            format!(
                "printf '%s' '{BEFORE}' > {target}; printf '%s' '{AFTER}' > {target}",
                target = path.display()
            ),
            "",
        ),
        Protocol::Rename => (
            format!(
                "printf '%s' '{AFTER}' > {partial}; mv {partial} {target}",
                partial = path.with_extension("partial").display(),
                target = path.display()
            ),
            "sleep 0.001; ",
        ),
    };
    // Rounds bound the writer in both cases: a rewrite loop that never ended
    // would outlive the test on a host that lost the race to kill it, and a
    // rename loop that never ended would leave the temporary file behind.
    let script = format!(
        "i=0; while [ $i -lt {rounds} ]; do {step}; {pause}i=$((i+1)); done",
        rounds = 200_000u32
    );
    Ok(Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

/// What one poll of a real store produced, classified from the answer itself.
#[derive(Debug)]
enum Polled {
    /// The store reported a value.
    Settled(String),
    /// The subject was moving: no reading was committed, and the observation is
    /// pending the next tick.
    Pending {
        /// Reads the protocol took before giving up.
        reads: u32,
        /// The axis that kept moving.
        drift: Drift,
    },
    /// The subject could not be read at all.
    Unreadable(BotError),
}

/// Classify a refused poll once, so the two verbs below share the reading of a
/// pending answer rather than each decoding it.
fn classify(failure: BotError) -> Result<Polled, Box<dyn Error>> {
    match failure {
        BotError::UnstableObservation { unstable, .. } => Ok(Polled::Pending {
            reads: unstable.reads(),
            drift: unstable.drift(),
        }),
        other => Ok(Polled::Unreadable(other)),
    }
}

/// Poll the store once through `Observe`.
async fn poll_once(store: &JsonStore) -> Result<Polled, Box<dyn Error>> {
    match Observe::poll(store, (proof()?, ())).await {
        Ok(state) => Ok(Polled::Settled(state.raw)),
        Err(failure) => classify(failure),
    }
}

/// Query the store once through `Query`, which reaches the same reader.
async fn query_once(store: &JsonStore) -> Result<Polled, Box<dyn Error>> {
    match Query::query(store, (proof()?, &())).await {
        Ok(state) => Ok(Polled::Settled(state.raw)),
        Err(failure) => classify(failure),
    }
}

/// Count how many of `polls` polls settled, and refuse anything that was
/// neither settled nor pending.
///
/// A third outcome is the failure this row exists to catch, so it is not
/// tolerated anywhere in the file: an unreadable subject mid-write is reported
/// as the test failure it would be, with the platform's own words attached.
fn tally(polls: impl IntoIterator<Item = Polled>) -> Result<(u32, u32), Box<dyn Error>> {
    // Two counts rather than a struct: the two are read at different places and
    // adding a third would be a type change rather than a call-site change.
    let mut settled = 0u32;
    let mut pending = 0u32;
    for polled in polls {
        match polled {
            Polled::Settled(_) => settled = settled.saturating_add(1),
            Polled::Pending { .. } => pending = pending.saturating_add(1),
            Polled::Unreadable(other) => {
                let refusal =
                    Err::<(u32, u32), _>(Box::<dyn Error>::from(std::io::Error::other(format!(
                        "a subject that is being written must never be reported unreadable: {other}"
                    ))));
                lgwks_std::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "tally: returning an error to the caller"
                );
                return refusal;
            }
        }
    }
    Ok((settled, pending))
}

/// The whole point of the row: while a child process rewrites a store, every
/// reading the bot is given either was confirmed by two independent reads or
/// was reported as pending. Nothing torn is ever reported as a value.
///
/// The recovery case is in the same test: once the writer has finished, the
/// next poll settles on the complete document. A protocol that simply failed
/// would pass the first half and fail the second.
#[test]
fn a_store_being_rewritten_is_pending_until_it_settles() -> TestResult {
    let mut live = LiveStore::new("concurrent-write", Protocol::Rewrite)?;
    let readings = live.sweep(SWEEP_WINDOW)?;
    for polled in &readings {
        if let Polled::Settled(ref raw) = *polled {
            // Whatever settled is a state the file actually held. A value the
            // protocol admitted cannot be a fourth thing: that is the claim
            // under test, stated as data. `TRUNCATED` is one of the three, and
            // its presence is the caveat above rather than a defect.
            assert!(
                settled_states().contains(&raw.as_str()),
                "a settled reading must be a state the file actually held: {raw:?}"
            );
        }
    }
    live.quiesce();
    let (settled, pending) = tally(readings)?;

    assert!(
        settled > 0,
        "a live store must produce readings as well as refusals"
    );
    assert!(
        pending > 0,
        "a sweep against a file being rewritten must observe at least one unsettled \
         reading, or the protocol is not being exercised (settled={settled})"
    );

    let Some(raw) = live.settle(SETTLE_WINDOW)? else {
        return refuse("a store nobody is writing must settle once the writer has finished");
    };
    assert!(
        settled_states().contains(&raw.as_str()),
        "recovery must read a state the file actually held, not a prefix of a document: {raw:?}"
    );
    Ok(())
}

/// The other verb over the same subject: a pending reading is pending whoever
/// asked for it, and a refusal names the reads it took and the axis that moved.
#[test]
fn a_query_over_a_moving_store_is_pending_too() -> TestResult {
    let mut live = LiveStore::new("concurrent-query", Protocol::Rewrite)?;
    let mut readings = Vec::new();
    for _ in 0..300 {
        readings.push(lgwks_bot::block_on(query_once(&live.store))?);
    }
    live.quiesce();
    let (_, pending) = tally(readings)?;
    assert!(
        pending > 0,
        "a sweep of queries against a file being rewritten must observe at least one \
         unsettled reading (or the protocol is not being exercised)"
    );
    Ok(())
}

/// A store, the file behind it, and the child process rewriting that file.
///
/// One value because every scenario here needs all three, and a scenario that
/// rebuilt the trio by hand would drift: a writer started at the wrong moment
/// or a sweep of the wrong length is a test that passes for the wrong reason.
struct LiveStore {
    /// The scratch directory, removed when the test ends.
    ///
    /// Held for its `Drop` rather than read, which is why the name says so: the
    /// directory is the guard, and the file inside it is only interesting while
    /// a scenario is writing or reading it.
    _scratch: Scratch,
    /// The domain the shipped verb path polls.
    store: JsonStore,
    /// The child process rewriting the subject.
    writer: Child,
}

impl LiveStore {
    /// A store holding [`BEFORE`], being rewritten under `protocol`.
    fn new(tag: &str, protocol: Protocol) -> Result<Self, Box<dyn Error>> {
        let scratch = Scratch::new(tag)?;
        let path = scratch.join("store.json");
        std::fs::write(&path, BEFORE)?;
        let writer = start_writer(&path, protocol)?;
        Ok(Self {
            store: JsonStore::new(&path),
            _scratch: scratch,
            writer,
        })
    }

    /// Poll the store through `Observe` for `window`, collecting the answers.
    ///
    /// A duration rather than a count of polls, because the property under test
    /// is a *rate*: a window long enough to contain many writes is what makes
    /// "at least one unsettled reading" a fact about the protocol rather than
    /// about how fast this host happens to read a file. The poll cap is the
    /// second bound — a host that can poll a store a million times a second
    /// stops on the cap instead of on the clock, so neither side of the sweep
    /// can run away.
    fn sweep(&self, window: std::time::Duration) -> Result<Vec<Polled>, Box<dyn Error>> {
        const POLL_CAP: u32 = 200_000;
        let started = std::time::Instant::now();
        let mut readings = Vec::new();
        let mut polls = 0u32;
        while started.elapsed() < window && polls < POLL_CAP {
            polls = polls.saturating_add(1);
            readings.push(lgwks_bot::block_on(poll_once(&self.store))?);
        }
        Ok(readings)
    }

    /// Poll the store until it settles, or give up after `window`.
    ///
    /// The recovery path: a store nobody is writing has to settle, and a
    /// protocol that simply failed would not.
    fn settle(&self, window: std::time::Duration) -> Result<Option<String>, Box<dyn Error>> {
        let started = std::time::Instant::now();
        while started.elapsed() < window {
            if let Polled::Settled(raw) = lgwks_bot::block_on(poll_once(&self.store))? {
                return Ok(Some(raw));
            }
        }
        Ok(None)
    }

    /// Stop the writer and wait for it, so the subject is quiescent.
    ///
    /// Kill first, then wait: waiting alone would block for as long as the
    /// writer's own round budget, and the point of this call is that the
    /// subject stops moving *now*.
    fn quiesce(&mut self) {
        let _ignored = self.writer.kill();
        let _ignored = self.writer.wait();
    }
}

/// Every refusal names at least two reads and an axis the caller can triage on.
#[test]
fn an_unsettled_reading_names_its_reads_and_its_axis() -> TestResult {
    let mut live = LiveStore::new("named-refusal", Protocol::Rewrite)?;
    let mut seen = 0u32;
    for polled in live.sweep(SWEEP_WINDOW)? {
        let Polled::Pending { reads, drift } = polled else {
            continue;
        };
        seen = seen.saturating_add(1);
        assert!(
            reads >= 2,
            "one reading is not evidence of stability, so a refusal must take at least two: {reads}"
        );
        assert!(
            !drift.as_str().is_empty(),
            "the axis that moved is what a caller triages on"
        );
    }
    live.quiesce();
    assert!(
        seen > 0,
        "the writer was rewriting the subject for the whole sweep"
    );
    Ok(())
}

/// The writer's half of the discipline: write into place, rename over. Every
/// reading of such a subject is a whole document, which is the guarantee the
/// two-read protocol cannot give on its own and the reason it says so.
#[test]
fn a_store_written_by_rename_is_never_read_half_written() -> TestResult {
    let mut live = LiveStore::new("atomic-write", Protocol::Rename)?;
    let readings = live.sweep(SWEEP_WINDOW)?;
    for polled in &readings {
        if let Polled::Settled(ref raw) = *polled {
            assert!(
                raw == BEFORE || raw == AFTER,
                "a rename-into-place write must be read whole or not at all: {raw}"
            );
        }
    }
    live.quiesce();
    let (settled, _) = tally(readings)?;
    assert!(
        settled > 0,
        "an atomic writer must produce readable subjects while it runs"
    );
    Ok(())
}

/// A store that does not exist is *unreadable*, which is a different fact from a
/// store that is moving: one is a report about the world and the other is a
/// retry.
#[test]
fn an_absent_store_is_unreadable_rather_than_unsettled() -> TestResult {
    let scratch = Scratch::new("absent")?;
    let path = scratch.join("never-written.json");
    let store = JsonStore::new(&path);

    match lgwks_bot::block_on(poll_once(&store))? {
        Polled::Unreadable(BotError::DomainError { ref cause, .. }) => {
            assert!(
                cause.contains("No such file") || cause.contains("not found"),
                "the platform's own report is retained as the cause: {cause}"
            );
        }
        other => {
            return refuse(format!(
                "a file that does not exist must be unreadable, not {other:?}"
            ));
        }
    }
    Ok(())
}

/// The half of the contract the substrate itself reads: an unsettled reading is
/// `NotDelivered`, so a tick keeps the value it holds and reads again, and a
/// retry classifier calls it safe. This is what "pending, never a change" means
/// to the code that decides whether a chain fires.
#[test]
fn an_unsettled_reading_is_pending_rather_than_a_committed_change() -> TestResult {
    let mut live = LiveStore::new("pending-semantics", Protocol::Rewrite)?;
    // First the rate: the protocol has to be firing at all, or the three facts
    // below would be read off a refusal this test provoked some other way.
    let (_, pending) = tally(live.sweep(SWEEP_WINDOW)?)?;
    assert!(
        pending > 0,
        "a sweep against a rewriting writer must observe unsettled readings, or the facts \
         below would be read off a provoked refusal rather than an observed one"
    );
    // Then the refusal itself, polled through the verb rather than through the
    // sweep's classification: the three facts are read off the error the bot
    // would be handed.
    let mut refusal = None;
    let started = std::time::Instant::now();
    while refusal.is_none() && started.elapsed() < SWEEP_WINDOW {
        match lgwks_bot::block_on(Observe::poll(&live.store, (proof()?, ()))) {
            Ok(_) => {}
            Err(failure) => refusal = Some(failure),
        }
    }
    let recovered = live.settle(SETTLE_WINDOW)?;
    // Named so the recovery assertion below can read what the store held, whether or
    // not the sweep ever refused anything.
    live.quiesce();
    let Some(failure) = refusal else {
        return refuse(format!(
            "the writer was rewriting the subject for the whole sweep, so a poll must have \
             been refused (the store did settle afterwards, on {recovered:?})"
        ));
    };
    assert!(
        recovered.is_some(),
        "and once the writer has stopped, the same store settles"
    );
    assert!(
        matches!(failure, BotError::UnstableObservation { .. }),
        "the sweep must produce the unsettled refusal, not another failure: {failure}"
    );
    assert_eq!(
        failure.dispatch_certainty(),
        lgwks_bot::DispatchCertainty::NotDelivered,
        "nothing was read and nothing was committed, so a later tick is a plain retry"
    );
    assert_eq!(
        failure.retry_class(),
        lgwks_bot::RetryClass::Safe,
        "a refused reading is safe to retry precisely because it committed nothing"
    );
    assert!(
        failure.to_string().contains("pending"),
        "the refusal says in its own words that the observation is pending: {failure}"
    );
    Ok(())
}
