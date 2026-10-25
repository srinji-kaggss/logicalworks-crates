//! The path of a name, the reading of a file, and the choice of the newest
//! version, against crates.io's real layout and against properties drawn
//! from seeds.

use std::error::Error;

use lgwks_std::seeded::Seeded;

use super::{Entry, IndexError, Newest, Version, newest, parse, path};
use crate::freshness_cmd::SimError;

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// An index line with the keys the registry always writes.
fn line(version: &str, yanked: bool) -> String {
    format!(
        "{{\"name\":\"fixture\",\"vers\":\"{version}\",\"deps\":[],\"cksum\":\"00\",\"features\":{{}},\"yanked\":{yanked}}}"
    )
}

/// Entries for `versions`, each marked yanked or not.
fn entries(versions: &[(&str, bool)]) -> Result<Vec<Entry>, SimError> {
    let body = versions
        .iter()
        .map(|&(version, yanked)| line(version, yanked))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(parse(&body)?)
}

#[test]
fn a_name_lands_where_the_registry_keeps_it() -> TestResult {
    // The layout of the Cargo Book's "Index files" section, with real crates.
    for (name, expected) in [
        ("a", "1/a"),
        ("cc", "2/cc"),
        ("syn", "3/s/syn"),
        ("rand", "ra/nd/rand"),
        ("serde", "se/rd/serde"),
        ("tokio", "to/ki/tokio"),
        ("bevy_ecs", "be/vy/bevy_ecs"),
        ("lgwks_std", "lg/wk/lgwks_std"),
        ("Serde", "se/rd/serde"),
        ("serde-json", "se/rd/serde-json"),
    ] {
        assert_eq!(path(name)?, expected, "{name} is filed elsewhere");
    }
    Ok(())
}

#[test]
fn a_name_that_could_leave_the_index_is_refused() {
    let long = "a".repeat(65);
    for name in [
        "",
        "..",
        "../etc/passwd",
        "a/b",
        "a b",
        "a.b",
        "a?b",
        "a%2fb",
        "na\u{ef}ve",
        long.as_str(),
    ] {
        assert_eq!(
            path(name),
            Err(IndexError::Name {
                name: name.to_owned()
            }),
            "{name:?} must not become part of a URL"
        );
    }
}

#[test]
fn a_file_reads_as_its_versions_and_their_yanked_flags() -> TestResult {
    let body = format!(
        "{}\r\n\r\n{}\n{}\n",
        line("1.0.0", false),
        line("1.1.0", true),
        line("2.0.0-rc.1", false)
    );
    let read = parse(&body)?;
    let seen: Vec<(&str, bool)> = read
        .iter()
        .map(|entry| (entry.text.as_str(), entry.yanked))
        .collect();
    assert_eq!(
        seen,
        [("1.0.0", false), ("1.1.0", true), ("2.0.0-rc.1", false)],
        "blank lines and CRLF must not change what is read"
    );
    Ok(())
}

#[test]
fn an_empty_file_is_a_crate_with_no_versions() -> TestResult {
    assert!(parse("")?.is_empty(), "an empty body has no entries");
    assert!(parse("\n\n")?.is_empty(), "blank lines have no entries");
    Ok(())
}

#[test]
fn one_bad_line_refuses_the_file_and_names_the_line() {
    let good = line("1.0.0", false);
    for (bad, expected_line) in [
        ("{", 2_usize),
        ("[]", 2),
        ("{\"yanked\":false}", 2),
        ("{\"vers\":\"1.0.0\"}", 2),
        ("{\"vers\":1,\"yanked\":false}", 2),
        ("{\"vers\":\"1.0.0\",\"yanked\":\"no\"}", 2),
        ("{\"vers\":\"1.0\",\"yanked\":false}", 2),
    ] {
        let body = format!("{good}\n{bad}\n{good}");
        let refused = parse(&body);
        assert!(
            matches!(refused, Err(IndexError::Line { line, .. }) if line == expected_line),
            "{bad:?} must refuse the file at line {expected_line}, got {refused:?}"
        );
    }
}

#[test]
fn the_newest_is_the_highest_by_precedence_not_the_last_line() -> TestResult {
    let read = entries(&[("1.4.0", false), ("1.10.0", false), ("1.9.0", false)])?;
    let verdict = newest(&read, &Version::parse("1.4.0")?);
    assert_eq!(
        verdict,
        Some(Newest {
            text: "1.10.0".to_owned(),
            stale: true
        }),
        "1.10.0 is above 1.9.0 whichever line it is on"
    );
    Ok(())
}

#[test]
fn a_yanked_newest_is_never_offered() -> TestResult {
    let read = entries(&[("1.0.0", false), ("1.1.0", false), ("1.2.0", true)])?;
    let verdict = newest(&read, &Version::parse("1.0.0")?);
    assert_eq!(
        verdict.map(|found| found.text),
        Some("1.1.0".to_owned()),
        "the yanked 1.2.0 cannot be what the lock moves to"
    );
    Ok(())
}

#[test]
fn a_pre_release_is_not_the_newest_for_a_release() -> TestResult {
    let read = entries(&[
        ("0.103.15", false),
        ("0.104.0-alpha.8", false),
        ("1.16.2", false),
        ("2.0.0-beta.3", false),
    ])?;
    // Issue #702: rustls-webpki 0.103.15 -> 0.104.0-alpha.8 and smallvec
    // 1.16.2 -> 2.0.0-beta.3 were reported stale; cargo never selects either.
    assert_eq!(
        newest(&read, &Version::parse("0.103.15")?),
        Some(Newest {
            text: "1.16.2".to_owned(),
            stale: true
        }),
        "a release is compared against releases"
    );
    assert_eq!(
        newest(&read, &Version::parse("1.16.2")?),
        Some(Newest {
            text: "1.16.2".to_owned(),
            stale: false
        }),
        "smallvec 1.16.2 is current: 2.0.0-beta.3 is not on offer"
    );
    Ok(())
}

#[test]
fn a_pre_release_lock_is_compared_against_pre_releases() -> TestResult {
    let read = entries(&[
        ("2.0.0-beta.1", false),
        ("2.0.0-beta.3", false),
        ("2.0.0-beta.2", true),
    ])?;
    let behind = newest(&read, &Version::parse("2.0.0-beta.1")?);
    assert_eq!(
        behind,
        Some(Newest {
            text: "2.0.0-beta.3".to_owned(),
            stale: true
        }),
        "a lock on a beta has asked for pre-releases"
    );
    let current = newest(&read, &Version::parse("2.0.0-beta.3")?);
    assert_eq!(
        current.map(|found| found.stale),
        Some(false),
        "the newest beta is current"
    );
    Ok(())
}

#[test]
fn a_lock_ahead_of_the_registry_is_not_stale() -> TestResult {
    // The old test was `latest != resolved`, which called this stale.
    let read = entries(&[("1.0.0", false), ("1.1.0", true)])?;
    let verdict = newest(&read, &Version::parse("1.1.0")?);
    assert_eq!(
        verdict,
        Some(Newest {
            text: "1.0.0".to_owned(),
            stale: false
        }),
        "1.1.0 is yanked, the newest on offer is 1.0.0, and the lock is ahead of it"
    );
    Ok(())
}

#[test]
fn nothing_on_offer_is_no_answer_not_a_stale_one() -> TestResult {
    let resolved = Version::parse("1.0.0")?;
    assert_eq!(newest(&[], &resolved), None, "an empty file offers nothing");
    let all_yanked = entries(&[("1.0.0", true), ("1.1.0", true)])?;
    assert_eq!(
        newest(&all_yanked, &resolved),
        None,
        "yanked is not on offer"
    );
    let only_pre = entries(&[("2.0.0-alpha.1", false)])?;
    assert_eq!(
        newest(&only_pre, &resolved),
        None,
        "a release lock is not offered a pre-release"
    );
    Ok(())
}

/// A drawn index: unique versions, a quarter yanked, a quarter pre-release.
fn draw(stream: &mut Seeded) -> Result<Vec<(String, bool)>, SimError> {
    let count = stream.below(8)?.saturating_add(1);
    let mut drawn: Vec<(String, bool)> = Vec::new();
    for ordinal in 0..count {
        let pre = if stream.below(4)? == 0 {
            format!("-rc.{}", stream.below(3)?)
        } else {
            String::new()
        };
        let text = format!("{}.{}.{ordinal}{pre}", stream.below(3)?, stream.below(3)?);
        drawn.push((text, stream.below(4)? == 0));
    }
    Ok(drawn)
}

#[test]
fn the_verdict_satisfies_its_specification_over_seeds() -> TestResult {
    for seed in 0..400_u64 {
        let mut stream = Seeded::from_seed(seed);
        let drawn = draw(&mut stream)?;
        let spelled: Vec<(&str, bool)> =
            drawn.iter().map(|pair| (pair.0.as_str(), pair.1)).collect();
        let read = entries(&spelled)?;
        let resolved_text = format!("{}.{}.0", stream.below(3)?, stream.below(3)?);
        let resolved_text = if stream.below(4)? == 0 {
            format!("{resolved_text}-rc.1")
        } else {
            resolved_text
        };
        let resolved = Version::parse(&resolved_text)?;
        let admit = resolved.is_prerelease();
        let eligible: Vec<&Entry> = read
            .iter()
            .filter(|entry| !entry.yanked && (admit || !entry.version.is_prerelease()))
            .collect();
        match newest(&read, &resolved) {
            None => assert!(
                eligible.is_empty(),
                "seed {seed}: an eligible version existed but none was offered"
            ),
            Some(found) => {
                let chosen = eligible
                    .iter()
                    .find(|entry| entry.text == found.text)
                    .ok_or("the offered version is not an eligible entry")?;
                for other in &eligible {
                    assert!(
                        other.version <= chosen.version,
                        "seed {seed}: {} is above the offered {}",
                        other.text,
                        found.text
                    );
                }
                assert_eq!(
                    found.stale,
                    chosen.version > resolved,
                    "seed {seed}: staleness is the order, not inequality"
                );
            }
        }
    }
    Ok(())
}
