//! The attacker every adversarial family shares is itself under test.
//!
//! A family's verdict is only as good as the attacker's stream: an attacker that
//! repeats itself, leaves its range, or panics on an empty input would make a
//! family pass for the wrong reason. These tests pin the properties the families
//! lean on: same seed, same attack (INV-STD-SEEDED-1); distinct seeds, distinct
//! attacks; every draw inside its range; every operator total on any input.
//!
//! Source: the seeded-stream contract of `lgwks_std::seeded`, and the mutation
//! operators of AFL (bit flip, delete, duplicate, insert, truncate, splice).

use crate::adversary::{Attacker, TestResult, as_text, sweep};
use crate::seeded_sweep::SWEEP_SEEDS;

/// The sentinel the stamp-shaped probes start from.
const STAMP: &[u8] = b"2023-11-14T22:13:20Z";

/// One seed mounts one attack, whatever the seed.
#[test]
fn sim_adversary_fixture_one_seed_mounts_one_attack() {
    for seed in SWEEP_SEEDS {
        let first = Attacker::new(seed).mutate_n(STAMP, 5);
        let second = Attacker::new(seed).mutate_n(STAMP, 5);
        assert_eq!(first, second, "seed {seed:#x} must replay its own attack");
    }
}

/// Distinct seeds mount distinct attacks on the same base.
#[test]
fn sim_adversary_fixture_distinct_seeds_mount_distinct_attacks() {
    let base = [0x55_u8; 64];
    let one = Attacker::new(SWEEP_SEEDS[0]).mutate_n(&base, 6);
    let other = Attacker::new(SWEEP_SEEDS[1]).mutate_n(&base, 6);
    assert_ne!(one, other, "two seeds mounted one attack");
}

/// A field draw stays inside its half-open range, and an empty range yields its
/// floor without drawing.
#[test]
fn sim_adversary_fixture_a_field_draw_stays_inside_its_range() {
    let mut attacker = Attacker::new(SWEEP_SEEDS[2]);
    for _ in 0..2_048 {
        let drawn = attacker.field(3, 11);
        assert!((3..11).contains(&drawn), "{drawn} left 3..11");
    }
    assert_eq!(attacker.field(7, 7), 7, "an empty range yields its floor");
    assert_eq!(
        attacker.field(9, 2),
        9,
        "an inverted range yields its floor"
    );
}

/// An empty input survives every operator, and a mutation grows it by at most
/// one tail: the attacker never panics on nothing.
#[test]
fn sim_adversary_fixture_every_operator_survives_an_empty_input() {
    let mut attacker = Attacker::new(SWEEP_SEEDS[3]);
    for _ in 0..512 {
        let mutated = attacker.mutate(&[]);
        assert!(
            mutated.len() <= 16,
            "an empty input grew by more than a tail"
        );
    }
}

/// Lossy text keeps ASCII exactly and replaces what is not UTF-8.
#[test]
fn sim_adversary_fixture_lossy_text_keeps_ascii_and_replaces_the_rest() {
    assert_eq!(as_text(b"abc"), "abc", "ASCII passes through");
    assert_eq!(
        as_text(&[0x61, 0xff]),
        "a\u{fffd}",
        "a lone 0xff is replaced"
    );
}

/// A bounded draw never reaches its bound, and a zero bound draws zero.
#[test]
fn sim_adversary_fixture_a_bounded_draw_never_reaches_its_bound() {
    let mut attacker = Attacker::new(SWEEP_SEEDS[0]);
    for bound in 1_usize..64 {
        for _ in 0..64 {
            assert!(attacker.below(bound) < bound, "a draw reached {bound}");
        }
    }
    assert_eq!(attacker.below(0), 0, "a zero bound yields zero");
}

/// The operators change something often enough to be an attack: over many
/// rounds on a long base, most mutations differ from the base.
#[test]
fn sim_adversary_fixture_most_mutations_change_the_input() -> TestResult {
    let base = [0x41_u8; 48];
    let (mut changed, mut total) = (0_usize, 0_usize);
    sweep(256, |_, _, attacker| {
        total = total.saturating_add(1);
        if attacker.mutate(&base) != base {
            changed = changed.saturating_add(1);
        }
        Ok(())
    })?;
    assert!(
        changed.saturating_mul(10) > total.saturating_mul(8),
        "only {changed} of {total} mutations changed the input"
    );
    Ok(())
}

/// Every boundary byte the attacker names is reachable, so an attack on a byte
/// grammar will meet each of its edges.
#[test]
fn sim_adversary_fixture_every_boundary_byte_is_reachable() {
    let mut attacker = Attacker::new(SWEEP_SEEDS[1]);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..4_096 {
        seen.insert(attacker.boundary_byte());
    }
    assert_eq!(seen.len(), 8, "boundary bytes reached: {seen:02x?}");
}

/// A pick returns every element of a table in time, and only elements of it.
#[test]
fn sim_adversary_fixture_a_pick_covers_its_table() {
    let table = ["a", "b", "c", "d", "e"];
    let mut attacker = Attacker::new(SWEEP_SEEDS[2]);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..512 {
        seen.insert(*attacker.pick(&table));
    }
    assert_eq!(seen.len(), table.len(), "elements picked: {seen:?}");
}

/// Generated text is printable ASCII of the requested length.
#[test]
fn sim_adversary_fixture_generated_text_is_printable_ascii() {
    let mut attacker = Attacker::new(SWEEP_SEEDS[3]);
    for len in 0_usize..96 {
        let text = attacker.text(len);
        assert_eq!(text.len(), len, "length of {text:?}");
        assert!(
            text.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
            "{text:?} is not printable ASCII"
        );
    }
}
