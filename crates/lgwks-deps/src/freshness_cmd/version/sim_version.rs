//! Section 11 of SemVer 2.0.0, checked against the specification's own
//! chain, against an oracle that shares no code with the parser, and over
//! seeded version sets for the laws of a total order.

use std::cmp::Ordering;
use std::error::Error;

use lgwks_std::seeded::Seeded;

use super::{Version, VersionError};
use crate::freshness_cmd::SimError;

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// semver.org section 11.4's example, lowest first.
const SPEC_CHAIN: [&str; 8] = [
    "1.0.0-alpha",
    "1.0.0-alpha.1",
    "1.0.0-alpha.beta",
    "1.0.0-beta",
    "1.0.0-beta.2",
    "1.0.0-beta.11",
    "1.0.0-rc.1",
    "1.0.0",
];

/// The identifiers a drawn pre-release is built from: numerics that tell a
/// numeric order from a text order (`2` and `10`), a leading `0`, mixed case
/// that tells byte order from a folded one, and a hyphenated identifier.
const POOL: [&str; 9] = ["0", "1", "2", "10", "alpha", "beta", "rc", "x-1", "Alpha"];

/// The key the specification's rules give a version, written from the text
/// directly: core numbers, then release-over-pre-release, then identifiers
/// as (kind, number, text) so numerics sort below alphanumerics.
type Key = (Vec<u64>, u8, Vec<(u8, u64, String)>);

/// The oracle: section 11 applied to the text without the parser.
fn key(text: &str) -> Result<Key, SimError> {
    let without_build = text.split('+').next().ok_or("empty version")?;
    let (core, pre) = match without_build.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (without_build, None),
    };
    let numbers = core
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()?;
    let mut identifiers = Vec::new();
    for part in pre.into_iter().flat_map(|pre| pre.split('.')) {
        identifiers.push(match part.parse::<u64>() {
            Ok(value) => (0, value, String::new()),
            Err(_) => (1, 0, part.to_owned()),
        });
    }
    Ok((numbers, u8::from(pre.is_none()), identifiers))
}

#[test]
fn the_specification_chain_is_strictly_increasing() -> TestResult {
    for (at, earlier) in SPEC_CHAIN.iter().enumerate() {
        for later in SPEC_CHAIN.iter().skip(at.saturating_add(1)) {
            assert!(
                Version::parse(earlier)? < Version::parse(later)?,
                "{earlier} must precede {later}"
            );
        }
    }
    Ok(())
}

#[test]
fn core_numbers_compare_as_numbers_not_as_text() -> TestResult {
    let chain = [
        "1.0.0", "2.0.0", "2.1.0", "2.1.1", "2.1.10", "2.10.0", "10.0.0",
    ];
    for pair in chain.windows(2) {
        let (low, high) = match pair {
            &[low, high] => (low, high),
            _ => return Err("windows(2) yields pairs".into()),
        };
        assert!(
            Version::parse(low)? < Version::parse(high)?,
            "{low} must precede {high}"
        );
    }
    Ok(())
}

#[test]
fn build_metadata_is_ignored_in_precedence() -> TestResult {
    let plain = Version::parse("1.0.0")?;
    for spelling in ["1.0.0+build", "1.0.0+20130313144700", "1.0.0+a.b-c.01"] {
        let with_build = Version::parse(spelling)?;
        assert_eq!(
            plain.cmp(&with_build),
            Ordering::Equal,
            "{spelling} differs from 1.0.0 only in build metadata"
        );
        assert_eq!(plain, with_build, "{spelling} must equal 1.0.0");
    }
    let pre = Version::parse("1.0.0-rc.1+left")?;
    let other = Version::parse("1.0.0-rc.1+right")?;
    assert_eq!(pre, other, "build metadata does not split a pre-release");
    Ok(())
}

#[test]
fn a_pre_release_is_recognised_and_a_release_is_not() -> TestResult {
    assert!(Version::parse("0.4.0-alpha.1")?.is_prerelease(), "alpha");
    assert!(Version::parse("2.0.0-beta.3+meta")?.is_prerelease(), "beta");
    assert!(!Version::parse("1.0.0")?.is_prerelease(), "release");
    assert!(
        !Version::parse("1.0.0+meta-1")?.is_prerelease(),
        "build hyphen"
    );
    Ok(())
}

#[test]
fn malformed_versions_are_refused_not_ordered() {
    let corpus = [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-",
        "1.2.3-01",
        "1.2.3-a..b",
        "1.2.3-.a",
        "1.2.3+",
        "1.2.3+a..b",
        "1.2.3+a_b",
        "a.b.c",
        "1.2.x",
        "1.2.3-\u{3b1}",
        " 1.2.3",
        "1.2.3 ",
        "v1.2.3",
        "-1.2.3",
        "+1.2.3",
        "1.+2.3",
        "18446744073709551616.0.0",
        "1.2.3-18446744073709551616",
        "1..3",
    ];
    for text in corpus {
        assert!(
            Version::parse(text).is_err(),
            "{text:?} is outside the grammar and must be refused"
        );
    }
}

#[test]
fn each_refusal_names_the_part_that_broke_the_grammar() {
    assert_eq!(
        Version::parse("1.2"),
        Err(VersionError::Core {
            text: "1.2".to_owned()
        }),
        "two components are a core error"
    );
    assert_eq!(
        Version::parse("01.2.3"),
        Err(VersionError::Number {
            part: "01".to_owned()
        }),
        "a leading zero is a number error"
    );
    assert_eq!(
        Version::parse("1.2.3-01"),
        Err(VersionError::Number {
            part: "01".to_owned()
        }),
        "a numeric identifier keeps the no-leading-zero rule"
    );
    assert_eq!(
        Version::parse("1.2.3-"),
        Err(VersionError::Identifier {
            part: String::new()
        }),
        "an empty pre-release is an identifier error"
    );
    assert_eq!(
        Version::parse("1.2.3+a_b"),
        Err(VersionError::Identifier {
            part: "a_b".to_owned()
        }),
        "build metadata keeps the alphabet"
    );
}

/// One drawn version text. The ranges are small so equal cores and equal
/// identifier prefixes are common: that is where the rules differ.
fn draw(stream: &mut Seeded) -> Result<String, SimError> {
    let mut text = format!(
        "{}.{}.{}",
        stream.below(3)?,
        stream.below(3)?,
        stream.below(3)?
    );
    if stream.below(5)? < 2 {
        let count = stream.below(3)?.saturating_add(1);
        let mut parts = Vec::new();
        for _ in 0..count {
            parts.push(*POOL.get(stream.index(POOL.len())?).ok_or("pool index")?);
        }
        text.push('-');
        text.push_str(&parts.join("."));
    }
    if stream.below(5)? == 0 {
        text.push_str("+b.1");
    }
    Ok(text)
}

/// The drawn set for `seed`, in draw order.
fn drawn(seed: u64) -> Result<Vec<String>, SimError> {
    let mut stream = Seeded::from_seed(seed);
    let mut texts = Vec::new();
    for _ in 0..12 {
        texts.push(draw(&mut stream)?);
    }
    Ok(texts)
}

#[test]
fn the_parser_agrees_with_the_oracle_and_orders_totally_over_seeds() -> TestResult {
    for seed in 0..200_u64 {
        let texts = drawn(seed)?;
        let parsed = texts
            .iter()
            .map(|text| Version::parse(text))
            .collect::<Result<Vec<_>, _>>()?;
        for (left_at, left) in parsed.iter().enumerate() {
            for (right_at, right) in parsed.iter().enumerate() {
                let expected = key(texts.get(left_at).ok_or("left")?)?
                    .cmp(&key(texts.get(right_at).ok_or("right")?)?);
                assert_eq!(
                    left.cmp(right),
                    expected,
                    "seed {seed}: {:?} against {:?} left the specification",
                    texts.get(left_at),
                    texts.get(right_at)
                );
                assert_eq!(
                    left.cmp(right),
                    right.cmp(left).reverse(),
                    "seed {seed}: the order is not antisymmetric"
                );
                for middle in &parsed {
                    if left <= right && right <= middle {
                        assert!(left <= middle, "seed {seed}: the order is not transitive");
                    }
                }
            }
        }
    }
    Ok(())
}

/// The set for `seed` sorted by the parser's order, as the texts.
fn sorted(seed: u64) -> Result<Vec<String>, SimError> {
    let mut pairs = Vec::new();
    for text in drawn(seed)? {
        pairs.push((Version::parse(&text)?, text));
    }
    pairs.sort();
    Ok(pairs.into_iter().map(|(_, text)| text).collect())
}

#[test]
fn one_seed_sorts_to_one_order_every_time() -> TestResult {
    for seed in [0_u64, 7, 99, 4_096] {
        assert_eq!(
            sorted(seed)?,
            sorted(seed)?,
            "seed {seed} must replay to the same order"
        );
    }
    Ok(())
}
