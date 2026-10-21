//! Seeded sweeps over the closed estate list (#701).
//!
//! The rule under test is `is_estate_crate`: an edge is admitted by provenance
//! only when its name is one of the five crates this repository publishes, in
//! either spelling Cargo folds. The properties below hold the list to that
//! sentence from the outside: a published name is admitted, and a name that is
//! not one — another case, an extension, a prefix, a random word — is refused.
//!
//! Every property runs over a band of 64 seeds, and a seed replays the same
//! names, so a failure names the seed that reproduces it. The refused-name
//! sweeps are what a prefix rule or a case fold would fail: they would admit
//! `lgwks_stdx`, `LGWKS_STD` or a sixteen-letter draw, none of which anyone
//! published.

use std::fmt::{self, Display, Formatter};

use lgwks_std::seeded::{DrawError, Seeded};

use crate::estate::is_estate_crate;

/// Seeds per band.
const SEEDS: u64 = 64;

/// Bands per property.
const BANDS: u64 = 12;

/// The five published crates, in both spellings Cargo folds.
const MEMBERS: [&str; 10] = [
    "lgwks_std",
    "lgwks_bot",
    "lgwks_ast",
    "lgwks_deps",
    "lgwks_macros",
    "lgwks-std",
    "lgwks-bot",
    "lgwks-ast",
    "lgwks-deps",
    "lgwks-macros",
];

/// What a refused-name sweep appends to a published name.
const SUFFIXES: [&str; 8] = ["x", "_x", "s", "_std", "-extra", "0", "_", "2"];

/// The bytes a name is built from when it is meant to be arbitrary.
const ALPHABET: [u8; 37] = *b"abcdefghijklmnopqrstuvwxyz0123456789-";

/// Why a sweep could not draw its names.
#[derive(Debug)]
enum SweepError {
    /// The stream refused a draw: an empty range, or one wider than a word.
    Draw(DrawError),
}

impl Display for SweepError {
    /// Names the refusal the stream reported.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Draw(ref cause) => write!(formatter, "the stream refused a draw: {cause}"),
        }
    }
}

impl std::error::Error for SweepError {
    /// The stream's own refusal, so a caller can match on it.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Draw(ref cause) => Some(cause),
        }
    }
}

impl From<DrawError> for SweepError {
    /// A refused draw is the whole of this error.
    fn from(cause: DrawError) -> Self {
        Self::Draw(cause)
    }
}

/// A property of one seed's draws.
type Property = fn(&mut Seeded) -> Result<(), SweepError>;

/// The seed `index` of `band`.
fn seed_of(band: u64, index: u64) -> u64 {
    band.saturating_mul(SEEDS).saturating_add(index)
}

/// Runs `property` over every seed of `band`, reporting every failing seed.
fn over_band(band: u64, property: Property) {
    let mut failures: Vec<String> = Vec::new();
    for index in 0..SEEDS {
        let seed = seed_of(band, index);
        if let Err(error) = property(&mut Seeded::from_seed(seed)) {
            failures.push(format!("band {band} seed {seed}: {error}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The published name a seed draws.
fn a_member(stream: &mut Seeded) -> Result<&'static str, SweepError> {
    Ok(MEMBERS[stream.index(MEMBERS.len())?])
}

/// A name drawn from the alphabet, sixteen letters long, so it can equal no
/// member: every member is at most twelve characters and each holds `_`.
fn arbitrary_name(stream: &mut Seeded) -> Result<String, SweepError> {
    let mut name = String::with_capacity(16);
    for _ in 0..16 {
        let drawn = stream.index(ALPHABET.len())?;
        name.push(char::from(ALPHABET[drawn]));
    }
    Ok(name)
}

/// A published name is admitted, whichever spelling it arrived in.
fn a_published_name_is_admitted(stream: &mut Seeded) -> Result<(), SweepError> {
    let name = a_member(stream)?;
    assert!(
        is_estate_crate(name),
        "{name} is published by this repository"
    );
    Ok(())
}

/// Another case is a different name: the check is byte-exact, as an approval is.
fn another_case_is_refused(stream: &mut Seeded) -> Result<(), SweepError> {
    let name = a_member(stream)?;
    let folded = name.to_ascii_uppercase();
    assert!(
        !is_estate_crate(&folded),
        "{folded} is not a published name"
    );
    Ok(())
}

/// An extension of a published name is refused: the suffix admits nobody.
fn an_extension_is_refused(stream: &mut Seeded) -> Result<(), SweepError> {
    let name = a_member(stream)?;
    let suffix = SUFFIXES[stream.index(SUFFIXES.len())?];
    let extended = format!("{name}{suffix}");
    assert!(
        !is_estate_crate(&extended),
        "{extended} borrows {name} without being it"
    );
    Ok(())
}

/// The prefix on its own is refused, so `lgwks_` places nothing on a rung.
fn a_bare_prefix_is_refused(stream: &mut Seeded) -> Result<(), SweepError> {
    let prefixes = ["lgwks", "lgwks_", "lgwks-", "lgwks_std_", "lgwkss", ""];
    let drawn = stream.index(prefixes.len())?;
    let prefix = prefixes[drawn];
    assert!(!is_estate_crate(prefix), "{prefix:?} is not a crate");
    Ok(())
}

/// An arbitrary word is refused, which is what a prefix rule would fail.
fn an_arbitrary_name_is_refused(stream: &mut Seeded) -> Result<(), SweepError> {
    let name = arbitrary_name(stream)?;
    assert!(!is_estate_crate(&name), "{name} is nobody's crate");
    Ok(())
}

/// A seed replays its names, so the sweeps above are reproducible.
fn a_seed_replays_its_names(stream: &mut Seeded) -> Result<(), SweepError> {
    let mut again = stream.clone();
    let first = arbitrary_name(stream)?;
    let second = arbitrary_name(&mut again)?;
    assert_eq!(first, second, "the same seed drew two names");
    Ok(())
}

#[test]
fn sim_published_name_is_admitted_band_0() {
    over_band(0, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_1() {
    over_band(1, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_2() {
    over_band(2, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_3() {
    over_band(3, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_4() {
    over_band(4, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_5() {
    over_band(5, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_6() {
    over_band(6, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_7() {
    over_band(7, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_8() {
    over_band(8, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_9() {
    over_band(9, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_10() {
    over_band(10, a_published_name_is_admitted);
}

#[test]
fn sim_published_name_is_admitted_band_11() {
    over_band(11, a_published_name_is_admitted);
}

#[test]
fn sim_another_case_is_refused_band_0() {
    over_band(0, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_1() {
    over_band(1, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_2() {
    over_band(2, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_3() {
    over_band(3, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_4() {
    over_band(4, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_5() {
    over_band(5, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_6() {
    over_band(6, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_7() {
    over_band(7, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_8() {
    over_band(8, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_9() {
    over_band(9, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_10() {
    over_band(10, another_case_is_refused);
}

#[test]
fn sim_another_case_is_refused_band_11() {
    over_band(11, another_case_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_0() {
    over_band(0, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_1() {
    over_band(1, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_2() {
    over_band(2, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_3() {
    over_band(3, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_4() {
    over_band(4, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_5() {
    over_band(5, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_6() {
    over_band(6, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_7() {
    over_band(7, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_8() {
    over_band(8, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_9() {
    over_band(9, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_10() {
    over_band(10, an_extension_is_refused);
}

#[test]
fn sim_an_extension_is_refused_band_11() {
    over_band(11, an_extension_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_0() {
    over_band(0, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_1() {
    over_band(1, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_2() {
    over_band(2, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_3() {
    over_band(3, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_4() {
    over_band(4, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_5() {
    over_band(5, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_6() {
    over_band(6, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_7() {
    over_band(7, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_8() {
    over_band(8, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_9() {
    over_band(9, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_10() {
    over_band(10, a_bare_prefix_is_refused);
}

#[test]
fn sim_a_bare_prefix_is_refused_band_11() {
    over_band(11, a_bare_prefix_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_0() {
    over_band(0, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_1() {
    over_band(1, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_2() {
    over_band(2, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_3() {
    over_band(3, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_4() {
    over_band(4, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_5() {
    over_band(5, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_6() {
    over_band(6, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_7() {
    over_band(7, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_8() {
    over_band(8, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_9() {
    over_band(9, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_10() {
    over_band(10, an_arbitrary_name_is_refused);
}

#[test]
fn sim_an_arbitrary_name_is_refused_band_11() {
    over_band(11, an_arbitrary_name_is_refused);
}

#[test]
fn sim_a_seed_replays_its_names_band_0() {
    over_band(0, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_1() {
    over_band(1, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_2() {
    over_band(2, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_3() {
    over_band(3, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_4() {
    over_band(4, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_5() {
    over_band(5, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_6() {
    over_band(6, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_7() {
    over_band(7, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_8() {
    over_band(8, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_9() {
    over_band(9, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_10() {
    over_band(10, a_seed_replays_its_names);
}

#[test]
fn sim_a_seed_replays_its_names_band_11() {
    over_band(11, a_seed_replays_its_names);
}

/// Every band index exists, so a band added to `BANDS` without a test fails here.
#[test]
fn sim_every_band_has_a_property() {
    let named = BANDS;
    assert_eq!(named, 12, "the bands named above");
}
