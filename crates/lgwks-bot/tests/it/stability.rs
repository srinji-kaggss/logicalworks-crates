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
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::scratch::Scratch;

use lgwks_bot::domain::data::JsonStore;
use lgwks_bot::error::BotError;
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

/// How long a sweep polls a subject that moves on every read before it gives
/// up waiting for a pending poll.
///
/// The first poll of such a subject is pending, so the budget never decides a
/// passing run; it bounds a regression in which the protocol stopped refusing,
/// so that run fails on its assertion instead of polling for ever.
const PENDING_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// Feed a named pipe a new revision on every open: the subject that never
    /// settles, *whatever the scheduler does*.
    ///
    /// A poll is pending only when the subject moved between every one of
    /// [`lgwks_bot::stability::MAX_STABILITY_READS`] consecutive reads. Against
    /// a file rewritten in place that is a race the reader wins whenever the
    /// writer is descheduled for a moment, which on a loaded host is most of
    /// the time: at load 68 on 15 cores the in-place writer let eight reads in
    /// a row agree, and the pending assertion measured the scheduler. A pipe's
    /// open waits for its writer, and each open is handed a later revision, so
    /// no two reads can agree and every poll is pending — a fact about the
    /// protocol, reached through the shipped verb with a real child writer.
    Fifo,
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
        // The open blocks until a reader opens, and the pause is a real gap with
        // the pipe closed, so a read reaches its end of file instead of being
        // fed revision after revision into one unbounded read.
        Protocol::Fifo => (
            format!(
                "printf '{{\"revision\":%s}}' \"$i\" > {target}",
                target = path.display()
            ),
            "sleep 0.001; ",
        ),
    };
    // The round budget is the writer's bound and nothing else may be relied on:
    // the rewrite loop rewrites the same handful of bytes, so a budget of twenty
    // million rounds costs no disk and cannot finish before the test kills it —
    // which is the whole reason the property under test survives a loaded host,
    // where a smaller budget can complete before the first poll.
    //
    // The parent check is what stops a *killed* test from orphaning the loop.
    // `Drop` handles every path this process takes, and it cannot handle the one
    // where the process itself is gone: a `SIGKILL`ed test binary runs no
    // destructors, and a twenty-million-round writer would then outlive it by
    // hours. So the writer checks every thousand rounds whether its parent is
    // still there, and stops when it is not. Sampled rather than every round
    // because `kill -0` is a syscall and the loop's whole job is to be fast.
    //
    // **Known limit:** the check identifies the parent by pid, and a pid the
    // operating system has since handed to some other process reads as alive.
    // `Drop` is the primary defence and this is the one that survives losing it.
    let script = format!(
        "i=0; while [ $i -lt {rounds} ]; do {step}; {pause}i=$((i+1)); \
         if [ $((i % 1000)) -eq 0 ]; then kill -0 $PPID 2>/dev/null || exit 0; fi; done",
        rounds = 20_000_000u32
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
    ///
    /// Carries the refusal itself rather than a copy of its fields, because the
    /// refusal *is* what a caller matches on, and a test that read a
    /// classification would be asserting about its own fixture instead.
    Pending(BotError),
    /// The subject could not be read at all.
    Unreadable(BotError),
}

/// Classify a refused poll once, so the two verbs below share the reading of a
/// pending answer rather than each decoding it.
fn classify(failure: BotError) -> Result<Polled, Box<dyn Error>> {
    match failure {
        failure @ BotError::UnstableObservation { .. } => Ok(Polled::Pending(failure)),
        other => Ok(Polled::Unreadable(other)),
    }
}

/// Poll the store once through `Observe`.
async fn poll_once(store: &JsonStore) -> Result<Polled, Box<dyn Error>> {
    match Observe::poll(store, (proof()?, ())).await {
        Ok(state) => Ok(Polled::Settled(state.raw().to_owned())),
        Err(failure) => classify(failure),
    }
}

/// Query the store once through `Query`, which reaches the same reader.
async fn query_once(store: &JsonStore) -> Result<Polled, Box<dyn Error>> {
    match Query::query(store, (proof()?, &())).await {
        Ok(state) => Ok(Polled::Settled(state.raw().to_owned())),
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
            Polled::Pending(_) => pending = pending.saturating_add(1),
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

/// The whole point of the row, on a subject that never settles: every poll the
/// bot makes while it is being written is pending, never a value, and once the
/// writer has finished — by renaming its final document into place — the next
/// poll settles on that document. A protocol that simply failed would pass the
/// first half and fail the second.
#[test]
fn a_store_being_rewritten_is_pending_until_it_settles() -> TestResult {
    let mut live = LiveStore::new("concurrent-write", Protocol::Fifo)?;
    let readings = live.sweep_until_pending(PENDING_BUDGET, Verb::Observe)?;
    live.quiesce();
    let (settled, pending) = tally(readings)?;
    assert_eq!(
        settled, 0,
        "no two reads of a subject that moves on every read can agree, so nothing may settle"
    );
    assert!(
        pending > 0,
        "a poll of a subject that moves on every read must be pending"
    );

    live.finish_with(AFTER)?;
    let Some(raw) = live.settle(SETTLE_WINDOW)? else {
        return refuse("a store nobody is writing must settle once the writer has finished");
    };
    assert_eq!(
        raw, AFTER,
        "recovery must read the document the writer finished with"
    );
    Ok(())
}

/// The safety half, against the torn read in its purest form: a file rewritten
/// in place, truncate and all. Whatever the bot is given as a value is a state
/// the file actually held, and nothing is reported unreadable.
///
/// Nothing here counts pending polls. Whether any eight reads in a row straddle
/// a write of an in-place writer is the scheduler's decision, not the
/// protocol's; the pending claim is made above, on a subject that moves on
/// every read.
#[test]
fn a_store_rewritten_in_place_is_never_read_as_a_state_it_never_held() -> TestResult {
    let mut live = LiveStore::new("in-place-write", Protocol::Rewrite)?;
    let readings = live.sweep(SWEEP_WINDOW)?;
    live.quiesce();
    assert!(!readings.is_empty(), "the sweep polled nothing");
    for polled in &readings {
        if let Polled::Settled(ref raw) = *polled {
            // `TRUNCATED` is one of the three, and its presence is the caveat
            // above rather than a defect.
            assert!(
                settled_states().contains(&raw.as_str()),
                "a settled reading must be a state the file actually held: {raw:?}"
            );
        }
    }
    tally(readings)?;
    Ok(())
}

/// The other verb over the same subject: a pending reading is pending whoever
/// asked for it.
#[test]
fn a_query_over_a_moving_store_is_pending_too() -> TestResult {
    let mut live = LiveStore::new("concurrent-query", Protocol::Fifo)?;
    let readings = live.sweep_until_pending(PENDING_BUDGET, Verb::Query)?;
    live.quiesce();
    let (settled, pending) = tally(readings)?;
    assert_eq!(
        settled, 0,
        "a query of a subject that moves on every read settled"
    );
    assert!(
        pending > 0,
        "a query of a subject that moves on every read must be pending"
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
    /// The subject's path, for a writer that finishes by renaming into it.
    path: std::path::PathBuf,
    /// The domain the shipped verb path polls.
    store: JsonStore,
    /// The child process rewriting the subject.
    writer: Child,
}

impl LiveStore {
    /// A store holding [`BEFORE`], being rewritten under `protocol`.
    fn new(tag: &str, protocol: Protocol) -> Result<Self, Box<dyn Error>> {
        let scratch = Scratch::new(tag)?;
        let path = scratch.path().join("store.json");
        if let Protocol::Fifo = protocol {
            let made = Command::new("mkfifo")
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !made.success() {
                let refusal: Result<Self, Box<dyn Error>> =
                    Err(format!("mkfifo {} exited {made}", path.display()).into());
                lgwks_std::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "live store: returning an error to the caller"
                );
                return refusal;
            }
        } else {
            std::fs::write(&path, BEFORE)?;
        }
        let writer = start_writer(&path, protocol)?;
        Ok(Self {
            store: JsonStore::new(&path),
            path,
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
            readings.push(lgwks_std::task::block_on(poll_once(&self.store))?);
        }
        Ok(readings)
    }

    /// Poll the store through `verb` until a poll is pending, or give up after
    /// `budget`.
    ///
    /// The refusal is what these scenarios are about, so the loop stops once it
    /// has one rather than sweeping a fixed count. Against a subject that moves
    /// on every read the first poll is pending; the budget bounds a regression
    /// in which it never is, and the assertion still reads what the sweep saw.
    fn sweep_until_pending(
        &self,
        budget: std::time::Duration,
        verb: Verb,
    ) -> Result<Vec<Polled>, Box<dyn Error>> {
        let started = std::time::Instant::now();
        let mut readings = Vec::new();
        while started.elapsed() < budget {
            let polled = match verb {
                Verb::Observe => lgwks_std::task::block_on(poll_once(&self.store))?,
                Verb::Query => lgwks_std::task::block_on(query_once(&self.store))?,
            };
            let pending = matches!(polled, Polled::Pending(_));
            readings.push(polled);
            if pending {
                return Ok(readings);
            }
        }
        Ok(readings)
    }

    /// The writer's last act: write `document` beside the subject and rename it
    /// over the subject, so the next open sees it whole.
    ///
    /// Called after [`Self::quiesce`]. For a pipe this replaces the pipe with a
    /// file, which is what lets a reader that would otherwise wait for a writer
    /// that is gone read a document instead.
    fn finish_with(&self, document: &str) -> Result<(), Box<dyn Error>> {
        let partial = self.path.with_extension("partial");
        std::fs::write(&partial, document)?;
        std::fs::rename(&partial, &self.path)?;
        Ok(())
    }

    /// Poll the store until it settles, or give up after `window`.
    ///
    /// The recovery path: a store nobody is writing has to settle, and a
    /// protocol that simply failed would not.
    fn settle(&self, window: std::time::Duration) -> Result<Option<String>, Box<dyn Error>> {
        let started = std::time::Instant::now();
        while started.elapsed() < window {
            if let Polled::Settled(raw) = lgwks_std::task::block_on(poll_once(&self.store))? {
                return Ok(Some(raw));
            }
        }
        Ok(None)
    }

    /// Stop the writer and wait for it, so the subject is quiescent.
    ///
    /// Kill first, then wait: waiting alone would block for as long as the
    /// writer's own round budget, and the point of this call is that the subject
    /// stops moving *now*. Idempotent, because [`Drop`] calls it too and a
    /// second `kill` on a reaped child is an error this method ignores.
    fn quiesce(&mut self) {
        let _ignored = self.writer.kill();
        let _ignored = self.writer.wait();
    }

    /// The writer's process id, for a test that has to observe the process
    /// itself rather than the file it writes.
    fn writer_id(&self) -> u32 {
        self.writer.id()
    }
}

/// Which shipped verb a sweep reads the store through.
#[derive(Clone, Copy)]
enum Verb {
    /// `Observe::poll`, the path a tick takes.
    Observe,
    /// `Query::query`, the same reader asked for a value.
    Query,
}

/// The writer dies with the store, on every path out of a test.
///
/// Without this the child outlives a test that returns early on a `?`, one that
/// panics on a failed assertion, and — the case `Drop` cannot cover — a test
/// binary that is killed outright, which is what the writer's own parent check
/// exists for. The struct's `Drop` runs before its fields drop, so the writer is
/// stopped and reaped while the scratch directory it writes to still exists,
/// rather than being left writing into a path another run has taken.
impl Drop for LiveStore {
    fn drop(&mut self) {
        self.quiesce();
    }
}

/// How long a dropped store is given to take its writer with it.
const GONE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether a process is still running, probed the way the shell probes it.
///
/// `kill -0` sends no signal and asks only whether the process exists, so this
/// is an observation rather than a second kill. It is answered by a child of its
/// own so the test needs no `unsafe` and no libc edge of its own.
fn alive(pid: u32) -> bool {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A store that is dropped takes its writer with it.
///
/// The regression this pins is a *process* leak: the writer is a twenty-million
/// round loop, so a test that returns through `?`, panics, or loses its process
/// to a signal used to leave one behind writing into a scratch directory nobody
/// owns any more. The assertion is on the pid rather than on a file, because a
/// leaked writer is invisible in the test's own results.
#[test]
fn a_dropped_store_takes_its_writer_with_it() -> TestResult {
    let pid = {
        let live = LiveStore::new("drop-leak", Protocol::Rewrite)?;
        assert!(
            alive(live.writer_id()),
            "the control: the writer is running while the store is alive"
        );
        live.writer_id()
    };

    let started = std::time::Instant::now();
    while alive(pid) && started.elapsed() < GONE_BUDGET {
        std::thread::park_timeout(std::time::Duration::from_millis(10));
    }
    assert!(
        !alive(pid),
        "a dropped store must not leave its writer running: pid {pid} outlived it"
    );
    Ok(())
}

/// Every refusal names at least two reads and an axis the caller can triage on.
#[test]
fn an_unsettled_reading_names_its_reads_and_its_axis() -> TestResult {
    let mut live = LiveStore::new("named-refusal", Protocol::Fifo)?;
    let readings = live.sweep_until_pending(PENDING_BUDGET, Verb::Observe)?;
    let mut named = 0u32;
    for polled in &readings {
        let Polled::Pending(ref failure) = *polled else {
            continue;
        };
        named = named.saturating_add(1);
        let BotError::UnstableObservation { ref unstable, .. } = *failure else {
            return refuse("a pending poll must carry the unsettled reading itself");
        };
        assert!(
            unstable.reads() >= 2,
            "one reading is not evidence of stability, so a refusal must take at least two: \
             {unstable}"
        );
        assert!(
            !unstable.drift().as_str().is_empty(),
            "the axis that moved is what a caller triages on: {unstable}"
        );
    }
    live.quiesce();
    assert!(
        named > 0,
        "a poll of a subject that moves on every read must be pending"
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
    let path = scratch.path().join("never-written.json");
    let store = JsonStore::new(&path);

    match lgwks_std::task::block_on(poll_once(&store))? {
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
    let mut live = LiveStore::new("pending-semantics", Protocol::Fifo)?;
    // One pass: the sweep polls until the protocol fires and hands back the
    // refusal the bot would have been given.
    let readings = live.sweep_until_pending(PENDING_BUDGET, Verb::Observe)?;
    live.quiesce();
    live.finish_with(AFTER)?;
    let recovered = live.settle(SETTLE_WINDOW)?;

    let mut asserted = 0u32;
    for polled in &readings {
        let Polled::Pending(ref failure) = *polled else {
            continue;
        };
        asserted = asserted.saturating_add(1);
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
        break;
    }
    assert!(
        asserted > 0,
        "a poll of a subject that moves on every read must have been refused (the store did \
         settle afterwards, on {recovered:?})"
    );
    assert!(
        recovered.is_some(),
        "and once the writer has stopped, the same store settles"
    );
    Ok(())
}
