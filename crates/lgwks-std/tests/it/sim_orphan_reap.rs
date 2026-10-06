//! Seeded simulation of reaping a dead supervisor's process groups (#318).
//!
//! A supervisor killed with SIGKILL runs no cleanup, so the groups it started
//! keep running. A successor holding the [`ProcessIdentity`] each leader had
//! calls [`reap_orphaned_group`], which must signal the group only while the pid
//! still names the process that was recorded. Three answers are possible and
//! each is a promise: `Signalled` stops the leader and every member, `LeaderReused`
//! touches nothing, and a record whose leader is gone touches nothing.
//!
//! One seed draws each case — how many members a group holds and whether the
//! record is genuine, forged to look like a reused pid, or outlived by its
//! leader — and the model written from the contract says what the reap must
//! answer and which processes must still be running after it. Only the model's
//! answers are folded into the trace, so a seed replays the same trace although
//! pids and timing are the OS's. The processes are real: a pid's start instant
//! and a group signal are not observable through a stub.
//!
//! A second family drives the stored form: a record read back from a database
//! row may be damaged, and the parser must refuse it or return exactly the
//! identity it renders, never something in between.

#![cfg(all(unix, feature = "process"))]

use std::error::Error;
use std::process::Child;
use std::time::{Duration, Instant};

use lgwks_std::process::{
    MAX_START_TOKEN_BYTES, OrphanReap, ProcessIdentity, identify_process, kill_process_group,
    reap_orphaned_group, running_processes,
};

use crate::group_leader::spawn_group_leader;
use crate::rng::Rng;
use crate::seeded_sweep::{SWEEP_SEEDS, fold, fold_usize, initial_trace};

/// What every test here returns: a fixture that cannot be built fails the test
/// with its cause instead of panicking.
type TestResult = Result<(), Box<dyn Error>>;

/// Most members one group may hold besides its leader.
const MAX_MEMBERS: usize = 3;

/// Live cases per seeded sweep.
const CASES: usize = 6;

/// Stored-form cases per seed; parsing is pure, so the family runs many.
const TEXT_CASES: usize = 2_500;

/// How long a group may take to record its members, or a signalled member to
/// stop running.
const SETTLE: Duration = Duration::from_secs(10);

/// What the seed drew for one record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Record {
    /// The identity read while the leader ran, unchanged.
    Genuine,
    /// The leader's pid with a start that differs, as a reissued pid reads.
    Forged,
    /// The genuine identity of a leader that has since been stopped and reaped.
    Outlived,
}

impl Record {
    /// Draw a record kind.
    fn draw(rng: &mut Rng) -> Self {
        match rng.below(3) {
            0 => Self::Genuine,
            1 => Self::Forged,
            _ => Self::Outlived,
        }
    }

    /// The word the trace folds for this kind.
    const fn word(self) -> u64 {
        match self {
            Self::Genuine => 1,
            Self::Forged => 2,
            Self::Outlived => 3,
        }
    }
}

/// A group the case started: a shell leader and the sleeping members it forked.
struct Group {
    /// The leader, unreaped until the case reaps it.
    leader: Child,
    /// The leader's pid, which is also the group id.
    root: i32,
    /// The members' pids, sorted.
    members: Vec<i32>,
    /// The file the leader records its members in.
    roster: std::path::PathBuf,
    /// The file whose existence says the roster is whole.
    ready: std::path::PathBuf,
}

impl Group {
    /// Start a group of `count` members and wait until the leader has recorded
    /// every one of them.
    fn start(count: usize, tag: &str) -> Result<Self, Box<dyn Error>> {
        let roster = std::env::temp_dir().join(format!(
            "lgwks-std-orphan-{}-{tag}.pids",
            std::process::id()
        ));
        let _cleared = std::fs::remove_file(&roster);
        let ready = roster.with_extension("ready");
        let _cleared = std::fs::remove_file(&ready);
        let fork = format!("sleep 30 & echo $! >> {}; ", roster.display());
        let mut script = vec![fork; count];
        // Everything after the forks is a shell builtin, so the group never
        // holds a process besides the leader and its members: an external `mv`
        // publishing the roster was a child of the leader, still running when the
        // roster appeared, and a capture taken then named it as a fourth member.
        // The marker is written after every `echo` returned, so a reader that
        // sees it reads a whole roster.
        script.push(format!(": > {}; exec sleep 30", ready.display()));
        let leader = spawn_group_leader(&script.concat())?;
        let root = i32::try_from(leader.id())?;
        let mut group = Self {
            leader,
            root,
            members: Vec::new(),
            roster,
            ready,
        };
        group.members = group.read_roster(count)?;
        Ok(group)
    }

    /// The `count` member pids the leader recorded, within [`SETTLE`].
    fn read_roster(&self, count: usize) -> Result<Vec<i32>, Box<dyn Error>> {
        let deadline = Instant::now()
            .checked_add(SETTLE)
            .ok_or("the roster deadline overflows the clock")?;
        while Instant::now() < deadline {
            if self.ready.exists() {
                let text = std::fs::read_to_string(&self.roster)?;
                let mut pids = text
                    .lines()
                    .map(|line| line.trim().parse::<i32>())
                    .collect::<Result<Vec<i32>, _>>()?;
                if pids.len() != count {
                    let refusal: Result<Vec<i32>, Box<dyn Error>> =
                        Err(format!("the roster holds {} of {count} members", pids.len()).into());
                    #[cfg(feature = "trace")]
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_roster: returning an error to the caller");
                    return refusal;
                }
                pids.sort_unstable();
                return Ok(pids);
            }
            std::thread::park_timeout(Duration::from_millis(5));
        }
        Err("the leader never wrote its roster".into())
    }

    /// Every process in the group, leader first.
    fn everyone(&self) -> Vec<i32> {
        let mut all = vec![self.root];
        all.extend_from_slice(&self.members);
        all
    }
}

impl Drop for Group {
    /// Leave nothing running, whatever the case did.
    fn drop(&mut self) {
        let _killed = kill_process_group(self.root);
        let _reaped = self.leader.wait();
        let _removed = std::fs::remove_file(&self.roster);
        let _removed = std::fs::remove_file(&self.ready);
    }
}

/// Whether `pids` reach the wanted running state within [`SETTLE`].
///
/// Running, not present: a stopped member nobody reaped is a zombie, which is not
/// a survivor.
fn settles(pids: &[i32], want_running: bool) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now()
        .checked_add(SETTLE)
        .ok_or("the settle deadline overflows the clock")?;
    let reached = |running: usize| {
        if want_running {
            running == pids.len()
        } else {
            running == 0
        }
    };
    while Instant::now() < deadline {
        if reached(running_processes(pids)?.len()) {
            return Ok(true);
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    Ok(reached(running_processes(pids)?.len()))
}

/// One seeded sweep of live reaps, as its trace.
fn sweep(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut trace = initial_trace();
    for case in 0..CASES {
        let members = rng.below(MAX_MEMBERS).saturating_add(1);
        let record = Record::draw(&mut rng);
        let mut group = Group::start(members, &format!("{seed:x}-{case}"))?;
        let genuine = identify_process(group.root)?.ok_or("a live leader has no identity")?;
        let held = match record {
            Record::Genuine => genuine.clone(),
            Record::Forged => {
                ProcessIdentity::new(genuine.pid(), &format!("{}x", genuine.started()))?
            }
            Record::Outlived => {
                kill_process_group(group.root)?;
                group.leader.wait()?;
                genuine.clone()
            }
        };
        let answer = match (record, reap_orphaned_group(&held)?) {
            (Record::Genuine, OrphanReap::Signalled { descendants }) => {
                assert_eq!(
                    descendants.pids(),
                    group.members.as_slice(),
                    "seed {seed:#x} case {case}: the reap captures exactly the group's members"
                );
                assert!(
                    settles(&group.everyone(), false)?,
                    "seed {seed:#x} case {case}: a genuine record stops the whole group"
                );
                fold_usize(&mut trace, descendants.pids().len());
                1
            }
            (Record::Forged, OrphanReap::LeaderReused { holder }) => {
                assert_eq!(
                    holder, genuine,
                    "seed {seed:#x} case {case}: the holder is the live leader"
                );
                assert!(
                    settles(&group.everyone(), true)?,
                    "seed {seed:#x} case {case}: a forged record signals nothing"
                );
                2
            }
            // The OS may in principle reissue a reaped pid before the reap reads
            // it; either way nothing may be signalled, which is the promise.
            (Record::Outlived, OrphanReap::LeaderGone | OrphanReap::LeaderReused { .. }) => 3,
            (record, other) => {
                let refusal: Result<u64, Box<dyn Error>> = Err(format!(
                    "seed {seed:#x} case {case}: a {record:?} record was answered {other:?}"
                )
                .into());
                #[cfg(feature = "trace")]
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                return refusal;
            }
        };
        fold(&mut trace, record.word());
        fold(&mut trace, answer);
        fold_usize(&mut trace, members);
    }
    Ok(trace)
}

/// Damage `text` the way the seed draws, or leave it whole.
fn damage(rng: &mut Rng, text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    match rng.below(5) {
        0 => {}
        1 => bytes.truncate(rng.below(bytes.len())),
        2 => {
            let at = rng.below(bytes.len());
            if let Some(byte) = bytes.get_mut(at) {
                *byte = rng.next().to_le_bytes()[0];
            }
        }
        3 => bytes.extend(std::iter::repeat_n(
            b'z',
            rng.below(MAX_START_TOKEN_BYTES.saturating_mul(2)),
        )),
        _ => bytes.insert(rng.below(bytes.len()), b'/'),
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One seeded sweep of damaged stored records, as its trace.
///
/// The model is the parser's contract: whatever it accepts renders back to the
/// exact text it was given, and an intact record is never refused.
fn text_sweep(seed: u64) -> Result<u64, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut trace = initial_trace();
    let starts = ["ps:Mon-Oct-6-12:00:00-2026", "proc:3f1e-boot:123456"];
    for case in 0..TEXT_CASES {
        let pid = i32::try_from(rng.below(4_194_304).saturating_add(1))?;
        let started = starts[rng.below(starts.len())];
        let whole = ProcessIdentity::new(pid, started)?.to_string();
        let text = damage(&mut rng, &whole);
        let accepted = match text.parse::<ProcessIdentity>() {
            Ok(identity) => {
                assert_eq!(
                    identity.to_string(),
                    text,
                    "seed {seed:#x} case {case}: an accepted record renders as it was read"
                );
                1
            }
            Err(error) => {
                assert_ne!(
                    text, whole,
                    "seed {seed:#x} case {case}: an intact record was refused: {error}"
                );
                2
            }
        };
        fold(&mut trace, accepted);
        fold_usize(&mut trace, text.len());
    }
    Ok(trace)
}

#[test]
fn sim_every_seed_reaps_only_the_records_that_still_name_their_leader() -> TestResult {
    for seed in SWEEP_SEEDS {
        let first = sweep(seed)?;
        let replay = sweep(seed)?;
        assert_eq!(first, replay, "seed {seed:#x} must replay its own trace");
    }
    Ok(())
}

#[test]
fn sim_a_damaged_record_is_refused_or_read_back_exactly() -> TestResult {
    let mut traces = Vec::with_capacity(SWEEP_SEEDS.len());
    for seed in SWEEP_SEEDS {
        let first = text_sweep(seed)?;
        assert_eq!(
            first,
            text_sweep(seed)?,
            "seed {seed:#x} must replay its own trace"
        );
        traces.push(first);
    }
    traces.sort_unstable();
    traces.dedup();
    assert_eq!(
        traces.len(),
        SWEEP_SEEDS.len(),
        "every named seed draws its own records"
    );
    Ok(())
}
