//! The drain's decision table as a seeded simulation.

use super::super::MAX_PANIC_MESSAGE_CHARS;
use super::super::outcome::CleanupReceipt;
#[cfg(feature = "process")]
use super::super::outcome::{Containment, ResidualRisk};
#[cfg(all(unix, feature = "process"))]
use super::super::owners::GroupSignaller;
use super::super::repeat::truncate_panic_message;
#[cfg(all(unix, feature = "process"))]
use super::super::{CONTAINMENT_ROUNDS, EPERM, ESRCH};
#[cfg(all(unix, feature = "process"))]
use super::{ScriptedCapture, capture_group, sequence};
use crate::rt::runtime::block_on;
#[cfg(feature = "process")]
use lgwks_std::process::ContainmentMechanism;
use std::num::NonZeroU64;
use std::sync::atomic::AtomicUsize;

// ── The drain's decision table, as a seeded simulation ───────────────────
//
// The arms above are the cases a reader thinks of. This family is the rest:
// one seed draws a whole world — how many descendants a reading names, which
// of them have already ended, which ignore every signal, whether the table
// can be read at all, whether the reading is truncated, whether the leader had
// already exited on its own, what the group signal answers and when the group
// is observed absent — and a model written from
// the documented contract says what the drain must then report. Every world
// is driven through the real `ProcessGroup`, so the arms the simulation
// covers are the arms production runs.

#[cfg(all(unix, feature = "process"))]
/// Reduce `value` into `0..bound` without a modulo operator.
///
/// `clippy::modulo_arithmetic` is forbidden workspace-wide, and a bound drawn
/// as a `%` is exactly the "bound I did not think about" this module forbids
/// elsewhere. Multiply-shift keeps it in one place: the high half of a
/// 64×64 product is below `bound`, so the narrowing cannot refuse, and it is
/// still propagated rather than defaulted.
fn reduce(value: u64, bound: NonZeroU64) -> Result<u64, std::num::TryFromIntError> {
    let wide = u128::from(value).wrapping_mul(u128::from(bound.get()));
    u64::try_from(wide >> 64)
}

#[cfg(all(unix, feature = "process"))]
/// One seeded world's draws, keyed by field rather than drawn as a stream.
///
/// A stream hides which choice a failure came from; a field name is the seed
/// and the decision in one line, so a reported seed reproduces the world.
struct Seed(u64);

#[cfg(all(unix, feature = "process"))]
impl Seed {
    /// The draw for `field` in `0..bound`.
    ///
    /// The seed and field are combined and passed through the splitmix64
    /// finalizer, so two fields of one seed are independent draws. A
    /// combination that was only multiplied made every field a fixed offset
    /// of every other, and some pairs of choices could never be drawn
    /// together: across 4,096 seeds no leader that exited on its own was
    /// ever paired with a refused group signal.
    fn draw(&self, field: u64, bound: NonZeroU64) -> Result<u64, std::num::TryFromIntError> {
        let mut mixed = self
            .0
            .wrapping_add(field.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        reduce(mixed ^ (mixed >> 31), bound)
    }

    /// A draw in `0..=max`.
    fn below(&self, field: u64, max: u64) -> Result<u64, std::num::TryFromIntError> {
        self.draw(field, NonZeroU64::MIN.saturating_add(max))
    }
}

/// How the group answers the one signal the drain may need.
#[cfg(all(unix, feature = "process"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorldGroup {
    /// Delivered, group still present.
    Delivered,
    /// Delivered and the group is already gone.
    Absent,
    /// Present but unsignalable, as an unreaped zombie leader is.
    Present,
    /// Refused.
    Refused,
}

/// One seeded world: the scripted capture, the scripted group, and the model.
#[cfg(all(unix, feature = "process"))]
struct World {
    /// The pids the reading names.
    tree: Vec<i32>,
    /// The pids that have already ended, so a signal answers `ESRCH`.
    gone: Vec<i32>,
    /// The pids that ignore every signal.
    immortal: Vec<i32>,
    /// Whether the process table cannot be read.
    unreadable: bool,
    /// Whether the reading stopped at its bound.
    truncated: bool,
    /// Whether the leader exited on its own before the cleanup began.
    exited: bool,
    /// What the group signal answers.
    group: WorldGroup,
    /// How many post-reap probes answer "present" before one answers "absent".
    present_probes: usize,
    /// The receipt the pinned phase must produce.
    expected: CleanupReceipt,
    /// The containment report the pinned phase must produce.
    containment: Containment,
}

#[cfg(all(unix, feature = "process"))]
impl World {
    /// Draw the world `seed` names, with the model's answers beside it.
    fn draw(seed: u64) -> Result<Self, std::num::TryFromIntError> {
        let draws = Seed(seed);
        let length = draws.below(0, 3)?;
        let mut tree = Vec::with_capacity(usize::try_from(length)?);
        let mut gone = Vec::new();
        let mut immortal = Vec::new();
        for index in 0..length {
            let pid_field = index.saturating_add(1);
            let pid = i32::try_from(draws.below(pid_field, 60)?)?;
            if tree.contains(&pid) {
                continue;
            }
            tree.push(pid);
            // Two fields per pid, so "already ended" and "ignores signals" are
            // disjoint by construction: a pid cannot be both, and a world
            // that could say otherwise would be testing an impossible state.
            let ends = draws.below(index.saturating_add(20), 1)? == 0;
            let ignores = draws.below(index.saturating_add(40), 1)? == 0;
            if ends {
                gone.push(pid);
            } else if ignores {
                immortal.push(pid);
            }
        }
        tree.sort_unstable();
        gone.sort_unstable();
        immortal.sort_unstable();
        let unreadable = draws.below(3, 3)? == 0;
        let truncated = !unreadable && draws.below(4, 3)? == 0;
        let group = match draws.below(5, 3)? {
            0 => WorldGroup::Absent,
            1 => WorldGroup::Present,
            2 => WorldGroup::Refused,
            _ => WorldGroup::Delivered,
        };
        let present_probes = usize::try_from(draws.below(6, 2)?)?;
        let exited = draws.below(7, 3)? == 0;

        // The model is the drain's documented contract, phase by phase.
        //
        // Phase 1, the reading: none when the leader had already exited (its
        // children were re-parented at the exit, so a walk names nothing) or
        // when the table cannot be read. Either way the tree is empty, and a
        // model that still counted the drawn pids would report a capture that
        // never happened.
        let read = !unreadable && !exited;
        let named: Vec<i32> = if read { tree.clone() } else { Vec::new() };
        // Phase 2, the group signal: a refusal ends the drain at once, after
        // the reading and before any pid is signalled or observed.
        let refused = group == WorldGroup::Refused;
        // Phases 3 and 4, signal and observe each named pid, run only on a
        // named tree under a group signal the OS accepted.
        let per_pid = read && !refused;
        let survivors: Vec<i32> = if per_pid {
            named
                .iter()
                .copied()
                .filter(|pid| immortal.contains(pid))
                .collect()
        } else {
            Vec::new()
        };
        // One round when nothing is left running, every round while something
        // is: that is the loop's own exit condition, not a choice. A pid is
        // signalled the first round it is named, unless it had already ended,
        // and again each later round only while it is observed running.
        let rounds = if survivors.is_empty() {
            1
        } else {
            CONTAINMENT_ROUNDS
        };
        let delivered = named.iter().filter(|pid| !gone.contains(pid)).count();
        let signalled = if per_pid {
            delivered.saturating_add(survivors.len().saturating_mul(rounds.saturating_sub(1)))
        } else {
            0
        };
        // The first limit noted wins, and the reading is where each is noted.
        let residual = if exited {
            Some(ResidualRisk::LeaderExited)
        } else if unreadable {
            Some(ResidualRisk::TableUnreadable)
        } else if truncated {
            Some(ResidualRisk::CaptureTruncated)
        } else {
            None
        };
        let expected = match group {
            WorldGroup::Refused => CleanupReceipt::CleanupFailed,
            _ if !survivors.is_empty() => CleanupReceipt::CleanupSurvivors {
                survivors: survivors.clone(),
            },
            WorldGroup::Absent => CleanupReceipt::CleanupConfirmed,
            WorldGroup::Delivered | WorldGroup::Present => CleanupReceipt::CleanupPending,
        };
        Ok(Self {
            tree,
            gone,
            immortal,
            unreadable,
            truncated,
            exited,
            group,
            present_probes,
            expected,
            containment: Containment {
                mechanism: if read {
                    ContainmentMechanism::ProcessTableSnapshot
                } else {
                    ContainmentMechanism::ProcessGroupOnly
                },
                captured: named.len(),
                signalled,
                survivors,
                residual,
            },
        })
    }

    /// The receipt the post-reap pass must reach, from the world alone.
    ///
    /// A survivor is observed again and never promoted. Otherwise the group
    /// is settled absent when the pinned phase already observed it gone, or
    /// when the post-reap probes reach their first "absent" inside the bounded
    /// rounds. It is read from the world rather than by probing the scripted
    /// observer again: a pass that returned at its first probe has not
    /// consumed the world's "present" answers, so a fresh probe would report a
    /// group the drain had correctly settled as still there.
    fn settled(&self) -> CleanupReceipt {
        if !self.containment.survivors.is_empty() {
            return CleanupReceipt::CleanupSurvivors {
                survivors: self.containment.survivors.clone(),
            };
        }
        if self.group == WorldGroup::Absent || self.present_probes < CONTAINMENT_ROUNDS {
            CleanupReceipt::CleanupConfirmed
        } else {
            CleanupReceipt::CleanupPending
        }
    }

    /// The scripted capture this world drives.
    fn capture(&self) -> ScriptedCapture {
        ScriptedCapture {
            tree: self.tree.clone(),
            gone: self.gone.clone(),
            immortal: self.immortal.clone(),
            dying: Vec::new(),
            dying_for: 0,
            observed: AtomicUsize::new(0),
            unreadable: self.unreadable,
            truncated: self.truncated,
            reads: AtomicUsize::new(0),
            signals: AtomicUsize::new(0),
        }
    }

    /// The scripted signaller this world drives.
    fn signaller(&self) -> WorldSignaller {
        WorldSignaller(self.group)
    }
}

/// A group signal that answers whatever the seeded world says.
#[cfg(all(unix, feature = "process"))]
struct WorldSignaller(WorldGroup);

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for WorldSignaller {
    fn signal(&self, _group: i32) -> std::io::Result<()> {
        match self.0 {
            WorldGroup::Absent => Err(std::io::Error::from_raw_os_error(ESRCH)),
            WorldGroup::Present => Err(std::io::Error::from_raw_os_error(EPERM)),
            WorldGroup::Refused => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "injected refusal",
            )),
            WorldGroup::Delivered => Ok(()),
        }
    }
}

/// The receipt's arm as a number, for the coverage check.
#[cfg(all(unix, feature = "process"))]
fn receipt_arm(receipt: &CleanupReceipt) -> u64 {
    match *receipt {
        CleanupReceipt::CleanupConfirmed => 1,
        CleanupReceipt::CleanupPending => 2,
        CleanupReceipt::CleanupFailed => 3,
        CleanupReceipt::CleanupSurvivors { .. } => 4,
    }
}

/// The receipt's arm and survivor count as one number, for the trace.
///
/// Both, because a family's replay oracle is only meaningful if two worlds
/// that differ only in *how many* pids survived cannot collapse into one
/// trace.
#[cfg(all(unix, feature = "process"))]
fn receipt_code(receipt: &CleanupReceipt) -> Result<u64, std::num::TryFromIntError> {
    Ok(match *receipt {
        CleanupReceipt::CleanupSurvivors { ref survivors } => {
            u64::try_from(survivors.len())?.saturating_add(4)
        }
        _ => receipt_arm(receipt),
    })
}

/// Drive one seeded world through the real drain and check it against the model.
#[cfg(all(unix, feature = "process"))]
fn sim_drain(seed: u64) -> Result<u64, Box<dyn std::error::Error>> {
    let world = World::draw(seed)?;
    let at = format!("seed {seed:#018x}");
    let capture = world.capture();
    let signaller = world.signaller();
    let observer = sequence(world.present_probes);
    let mut group = capture_group(&signaller, &observer, &capture);
    if world.exited {
        group.mark_exited();
    }
    let pinned = block_on(group.cleanup());
    assert_eq!(
        pinned, world.expected,
        "{at}: the pinned phase must report the model's receipt"
    );
    assert_eq!(
        *group.containment(),
        world.containment,
        "{at}: the containment report must carry the model's counts and limits"
    );
    assert_eq!(
        capture.signals(),
        world.containment.signalled(),
        "{at}: the drain must deliver exactly the signals the model counts"
    );
    if world.exited {
        assert_eq!(
            capture.reads(),
            0,
            "{at}: a leader that had exited has no tree to walk, so no table is read"
        );
        assert!(
            !group.containment().is_complete(),
            "{at}: a cleanup that never saw the tree must not claim it"
        );
    }
    // The post-reap pass decides the group, and a survivor is never promoted.
    group.mark_reaped();
    let settled = block_on(group.confirm_absence());
    let expected = world.settled();
    assert_eq!(
        settled, expected,
        "{at}: only an observed absence settles the group, and a survivor is never \
             promoted to a clean cleanup"
    );
    let captured = u64::try_from(group.containment().captured())?;
    let signalled = u64::try_from(group.containment().signalled())?;
    let pinned_code = receipt_code(&pinned)?;
    let settled_code = receipt_code(&settled)?;
    Ok(pinned_code
        .wrapping_add(captured)
        .wrapping_add(signalled)
        .wrapping_add(settled_code))
}

#[cfg(all(unix, feature = "process"))]
/// The seeds this family replays, named so a failure names one.
const DRAIN_SEEDS: [u64; 12] = [
    0x5EED_2630_0000_0001,
    0x5EED_2630_0000_0002,
    0x5EED_2630_0000_0003,
    0x5EED_2630_0000_0004,
    0x5EED_2630_0000_0005,
    0x5EED_2630_0000_0006,
    0x5EED_2630_0000_000E,
    0x5EED_2630_0000_0019,
    0x5EED_2630_0000_0025,
    0x5EED_2630_FFFF_FFFF,
    0xDEAD_BEEF_0263_0001,
    0xC0FF_EE00_2630_0001,
];

#[cfg(all(unix, feature = "process"))]
#[test]
fn sim_a_seeded_drain_reaches_the_models_receipt() -> Result<(), Box<dyn std::error::Error>> {
    for seed in DRAIN_SEEDS {
        sim_drain(seed)?;
    }
    Ok(())
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn sim_the_same_seed_replays_the_same_drain_trace() -> Result<(), Box<dyn std::error::Error>> {
    for seed in DRAIN_SEEDS {
        assert_eq!(
            sim_drain(seed)?,
            sim_drain(seed)?,
            "seed {seed:#018x}: the same seed must drive the same world and the same report"
        );
    }
    Ok(())
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn sim_every_receipt_arm_is_reachable_in_the_family() -> Result<(), Box<dyn std::error::Error>> {
    let arms = DRAIN_SEEDS
        .iter()
        .map(|seed| World::draw(*seed).map(|world| receipt_arm(&world.expected)))
        .collect::<Result<Vec<u64>, _>>()?;
    for arm in [1_u64, 2, 3, 4] {
        assert!(
            arms.contains(&arm),
            "the family must reach receipt arm {arm}; it drew {arms:?} over {} seeds",
            DRAIN_SEEDS.len()
        );
    }
    Ok(())
}

/// How many consecutive seeds the exited-leader sweep draws.
#[cfg(all(unix, feature = "process"))]
const EXITED_SWEEP_SEEDS: u64 = 4_096;

/// Every world in a wide seeded sweep reaches the model, and the sweep holds
/// both leaders that exited on their own and leaders that did not, under
/// every group answer, so the exited arm is exercised against each receipt
/// rather than once.
#[cfg(all(unix, feature = "process"))]
#[test]
fn sim_an_exited_leader_reads_no_table_and_claims_no_tree() -> Result<(), Box<dyn std::error::Error>>
{
    let mut exited_arms = std::collections::BTreeSet::new();
    let mut alive = 0_u64;
    for offset in 0..EXITED_SWEEP_SEEDS {
        let seed = 0x5EED_0347_0000_0000_u64.wrapping_add(offset);
        sim_drain(seed)?;
        let world = World::draw(seed)?;
        if world.exited {
            exited_arms.insert(receipt_arm(&world.expected));
        } else {
            alive = alive.saturating_add(1);
        }
    }
    assert_eq!(
        exited_arms,
        std::collections::BTreeSet::from([1_u64, 2, 3]),
        "an exited leader must be driven to the confirmed, pending and failed receipts; \
             it can never have a named survivor, because nothing was named"
    );
    assert!(
        alive > 0,
        "the sweep must still hold leaders that were alive at the cleanup"
    );
    Ok(())
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn sim_distinct_seeds_drive_distinct_drains() -> Result<(), Box<dyn std::error::Error>> {
    let [first, second, ..] = DRAIN_SEEDS;
    assert_ne!(
        sim_drain(first)?,
        sim_drain(second)?,
        "seeds {first:#018x} and {second:#018x} must draw different worlds"
    );
    Ok(())
}

#[test]
fn an_over_long_panic_message_is_truncated_not_dropped() {
    let long = "x".repeat(MAX_PANIC_MESSAGE_CHARS.saturating_add(1));
    let kept = truncate_panic_message(&long);
    assert_eq!(
        kept.chars().count(),
        MAX_PANIC_MESSAGE_CHARS.saturating_add(1),
        "a truncated message keeps the cap plus the single-character marker"
    );
    assert!(
        kept.ends_with('…'),
        "a truncated message says so, so it is never read as complete"
    );
    assert_eq!(
        truncate_panic_message("short"),
        "short",
        "a message within the cap is carried unchanged"
    );
}
