//! Seeded simulation of the environment a gate lane runs in (`child_env_from`).
//!
//! One seed draws the starting environment and the scratch and target
//! directories. Every property below is checked over a band of 64 seeds, and a
//! seed replays the same world, so a failure names the seed that reproduces it.
//! The properties are the contract the lanes depend on: the caller's
//! `CARGO_INCREMENTAL` is kept (run 38407172423 lost 2 m 43 s to a rebuild when
//! it was not), `PATH` gains the cargo directory in front and keeps what it had,
//! and no variable the caller set is dropped.

use super::child_env_from;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Seeds per band.
const BAND: u64 = 64;

/// The variables a lane may add or replace; nothing else changes.
const KNOWN: [&str; 6] = [
    "CARGO_INCREMENTAL",
    "CARGO_TERM_COLOR",
    "RUST_BACKTRACE",
    "TMPDIR",
    "CARGO_TARGET_DIR",
    "PATH",
];

/// SplitMix64: one seed, one stream.
struct Stream(u64);

impl Stream {
    fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mixed = (self.0 ^ (self.0 >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A draw below `bound`, which is at least one.
    fn below(&mut self, bound: u64) -> u64 {
        self.draw() % bound.max(1)
    }
}

/// What one seed draws.
#[derive(Clone, PartialEq, Debug)]
struct World {
    vars: BTreeMap<String, String>,
    scratch: Option<PathBuf>,
    target: Option<PathBuf>,
}

/// The world a seed draws.
fn world(seed: u64) -> World {
    let mut stream = Stream(seed);
    let mut vars = BTreeMap::new();
    if stream.below(4) != 0 {
        vars.insert("HOME".to_owned(), format!("/home/u{}", stream.below(1000)));
    }
    if stream.below(4) != 0 {
        let path = match stream.below(3) {
            0 => "",
            1 => "/usr/bin",
            _ => "/usr/bin:/bin",
        };
        vars.insert("PATH".to_owned(), path.to_owned());
    }
    match stream.below(5) {
        0 | 1 => {}
        2 => {
            vars.insert("CARGO_INCREMENTAL".to_owned(), "0".to_owned());
        }
        3 => {
            vars.insert("CARGO_INCREMENTAL".to_owned(), "1".to_owned());
        }
        _ => {
            vars.insert("CARGO_INCREMENTAL".to_owned(), "2".to_owned());
        }
    }
    if stream.below(2) == 0 {
        vars.insert("RUST_BACKTRACE".to_owned(), "full".to_owned());
    }
    if stream.below(3) == 0 {
        vars.insert("TMPDIR".to_owned(), "/tmp/old".to_owned());
    }
    if stream.below(3) == 0 {
        vars.insert("CARGO_TARGET_DIR".to_owned(), "/old/target".to_owned());
    }
    for extra in 0..stream.below(6) {
        vars.insert(format!("SIM_EXTRA_{extra}"), format!("v{}", stream.below(100)));
    }
    let scratch = (stream.below(2) == 0).then(|| PathBuf::from(format!("/scratch/{}", stream.below(100))));
    let target = (stream.below(2) == 0).then(|| PathBuf::from(format!("/target/{}", stream.below(100))));
    World {
        vars,
        scratch,
        target,
    }
}

/// The environment a lane receives in `world`.
fn lane_env(world: &World) -> BTreeMap<String, String> {
    child_env_from(world.vars.clone(), world.scratch.as_deref(), world.target.as_deref())
}

/// A property of one seed's world and the environment a lane gets in it.
type Property = fn(u64, &World, &BTreeMap<String, String>);

/// Checks `property` over every seed of band `index`.
fn over_band(index: u64, property: Property) {
    for seed in index * BAND..(index + 1) * BAND {
        let drawn = world(seed);
        let received = lane_env(&drawn);
        property(seed, &drawn, &received);
    }
}

fn incremental_is_kept(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    if let Some(chosen) = drawn.vars.get("CARGO_INCREMENTAL") {
        assert_eq!(received.get("CARGO_INCREMENTAL"), Some(chosen), "seed {seed}");
    }
}

fn incremental_defaults_to_zero(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    if !drawn.vars.contains_key("CARGO_INCREMENTAL") {
        assert_eq!(received.get("CARGO_INCREMENTAL").map(String::as_str), Some("0"), "seed {seed}");
    }
}

fn colour_is_never(seed: u64, _drawn: &World, received: &BTreeMap<String, String>) {
    assert_eq!(received.get("CARGO_TERM_COLOR").map(String::as_str), Some("never"), "seed {seed}");
}

fn backtrace_is_kept_or_one(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    let expected = match drawn.vars.get("RUST_BACKTRACE") {
        Some(kept) => kept.as_str(),
        None => "1",
    };
    assert_eq!(received.get("RUST_BACKTRACE").map(String::as_str), Some(expected), "seed {seed}");
}

fn tmpdir_follows_scratch(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    match drawn.scratch.as_deref() {
        Some(directory) => assert_eq!(
            received.get("TMPDIR"),
            Some(&format!("{}/", directory.display())),
            "seed {seed}"
        ),
        None => assert_eq!(received.get("TMPDIR"), drawn.vars.get("TMPDIR"), "seed {seed}"),
    }
}

fn tmpdir_ends_with_a_slash(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    if drawn.scratch.is_some() {
        assert!(received.get("TMPDIR").is_some_and(|value| value.ends_with('/')), "seed {seed}");
    }
}

fn target_follows_target(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    match drawn.target.as_deref() {
        Some(directory) => assert_eq!(
            received.get("CARGO_TARGET_DIR"),
            Some(&directory.display().to_string()),
            "seed {seed}"
        ),
        None => assert_eq!(
            received.get("CARGO_TARGET_DIR"),
            drawn.vars.get("CARGO_TARGET_DIR"),
            "seed {seed}"
        ),
    }
}

fn path_leads_with_cargo_bin(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    if let Some(home) = drawn.vars.get("HOME") {
        let front = format!("{home}/.cargo/bin:");
        assert!(received.get("PATH").is_some_and(|path| path.starts_with(&front)), "seed {seed}");
    }
}

fn path_keeps_what_it_had(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    if let Some(home) = drawn.vars.get("HOME") {
        let had = match drawn.vars.get("PATH") {
            Some(path) => path.as_str(),
            None => "",
        };
        assert_eq!(received.get("PATH"), Some(&format!("{home}/.cargo/bin:{had}")), "seed {seed}");
    }
}

fn path_without_home_is_unchanged(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    if !drawn.vars.contains_key("HOME") {
        assert_eq!(received.get("PATH"), drawn.vars.get("PATH"), "seed {seed}");
    }
}

fn extras_are_kept(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    for (name, value) in drawn.vars.iter().filter(|(name, _)| name.starts_with("SIM_EXTRA_")) {
        assert_eq!(received.get(name), Some(value), "seed {seed} {name}");
    }
}

fn nothing_the_caller_set_is_dropped(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    for name in drawn.vars.keys() {
        assert!(received.contains_key(name), "seed {seed} lost {name}");
    }
}

fn the_same_world_gives_the_same_environment(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    assert_eq!(&lane_env(drawn), received, "seed {seed}");
}

fn a_seed_replays_its_world(seed: u64, drawn: &World, _received: &BTreeMap<String, String>) {
    assert_eq!(&world(seed), drawn, "seed {seed}");
}

fn only_known_variables_are_added(seed: u64, drawn: &World, received: &BTreeMap<String, String>) {
    for name in received.keys().filter(|name| !drawn.vars.contains_key(*name)) {
        assert!(KNOWN.contains(&name.as_str()), "seed {seed} added {name}");
    }
}

#[test]
fn sim_incremental_is_kept_band_0() {
    over_band(0, incremental_is_kept);
}

#[test]
fn sim_incremental_is_kept_band_1() {
    over_band(1, incremental_is_kept);
}

#[test]
fn sim_incremental_is_kept_band_2() {
    over_band(2, incremental_is_kept);
}

#[test]
fn sim_incremental_defaults_to_zero_band_0() {
    over_band(0, incremental_defaults_to_zero);
}

#[test]
fn sim_incremental_defaults_to_zero_band_1() {
    over_band(1, incremental_defaults_to_zero);
}

#[test]
fn sim_incremental_defaults_to_zero_band_2() {
    over_band(2, incremental_defaults_to_zero);
}

#[test]
fn sim_colour_is_never_band_0() {
    over_band(0, colour_is_never);
}

#[test]
fn sim_colour_is_never_band_1() {
    over_band(1, colour_is_never);
}

#[test]
fn sim_colour_is_never_band_2() {
    over_band(2, colour_is_never);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_0() {
    over_band(0, backtrace_is_kept_or_one);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_1() {
    over_band(1, backtrace_is_kept_or_one);
}

#[test]
fn sim_backtrace_is_kept_or_one_band_2() {
    over_band(2, backtrace_is_kept_or_one);
}

#[test]
fn sim_tmpdir_follows_scratch_band_0() {
    over_band(0, tmpdir_follows_scratch);
}

#[test]
fn sim_tmpdir_follows_scratch_band_1() {
    over_band(1, tmpdir_follows_scratch);
}

#[test]
fn sim_tmpdir_follows_scratch_band_2() {
    over_band(2, tmpdir_follows_scratch);
}

#[test]
fn sim_tmpdir_ends_with_a_slash_band_0() {
    over_band(0, tmpdir_ends_with_a_slash);
}

#[test]
fn sim_tmpdir_ends_with_a_slash_band_1() {
    over_band(1, tmpdir_ends_with_a_slash);
}

#[test]
fn sim_tmpdir_ends_with_a_slash_band_2() {
    over_band(2, tmpdir_ends_with_a_slash);
}

#[test]
fn sim_target_follows_target_band_0() {
    over_band(0, target_follows_target);
}

#[test]
fn sim_target_follows_target_band_1() {
    over_band(1, target_follows_target);
}

#[test]
fn sim_target_follows_target_band_2() {
    over_band(2, target_follows_target);
}

#[test]
fn sim_path_leads_with_cargo_bin_band_0() {
    over_band(0, path_leads_with_cargo_bin);
}

#[test]
fn sim_path_leads_with_cargo_bin_band_1() {
    over_band(1, path_leads_with_cargo_bin);
}

#[test]
fn sim_path_leads_with_cargo_bin_band_2() {
    over_band(2, path_leads_with_cargo_bin);
}

#[test]
fn sim_path_keeps_what_it_had_band_0() {
    over_band(0, path_keeps_what_it_had);
}

#[test]
fn sim_path_keeps_what_it_had_band_1() {
    over_band(1, path_keeps_what_it_had);
}

#[test]
fn sim_path_keeps_what_it_had_band_2() {
    over_band(2, path_keeps_what_it_had);
}

#[test]
fn sim_path_without_home_is_unchanged_band_0() {
    over_band(0, path_without_home_is_unchanged);
}

#[test]
fn sim_path_without_home_is_unchanged_band_1() {
    over_band(1, path_without_home_is_unchanged);
}

#[test]
fn sim_path_without_home_is_unchanged_band_2() {
    over_band(2, path_without_home_is_unchanged);
}

#[test]
fn sim_extras_are_kept_band_0() {
    over_band(0, extras_are_kept);
}

#[test]
fn sim_extras_are_kept_band_1() {
    over_band(1, extras_are_kept);
}

#[test]
fn sim_extras_are_kept_band_2() {
    over_band(2, extras_are_kept);
}

#[test]
fn sim_nothing_the_caller_set_is_dropped_band_0() {
    over_band(0, nothing_the_caller_set_is_dropped);
}

#[test]
fn sim_nothing_the_caller_set_is_dropped_band_1() {
    over_band(1, nothing_the_caller_set_is_dropped);
}

#[test]
fn sim_nothing_the_caller_set_is_dropped_band_2() {
    over_band(2, nothing_the_caller_set_is_dropped);
}

#[test]
fn sim_the_same_world_gives_the_same_environment_band_0() {
    over_band(0, the_same_world_gives_the_same_environment);
}

#[test]
fn sim_the_same_world_gives_the_same_environment_band_1() {
    over_band(1, the_same_world_gives_the_same_environment);
}

#[test]
fn sim_the_same_world_gives_the_same_environment_band_2() {
    over_band(2, the_same_world_gives_the_same_environment);
}

#[test]
fn sim_a_seed_replays_its_world_band_0() {
    over_band(0, a_seed_replays_its_world);
}

#[test]
fn sim_a_seed_replays_its_world_band_1() {
    over_band(1, a_seed_replays_its_world);
}

#[test]
fn sim_a_seed_replays_its_world_band_2() {
    over_band(2, a_seed_replays_its_world);
}

#[test]
fn sim_only_known_variables_are_added_band_0() {
    over_band(0, only_known_variables_are_added);
}

#[test]
fn sim_only_known_variables_are_added_band_1() {
    over_band(1, only_known_variables_are_added);
}

#[test]
fn sim_only_known_variables_are_added_band_2() {
    over_band(2, only_known_variables_are_added);
}
