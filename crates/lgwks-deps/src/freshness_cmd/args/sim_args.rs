//! The grammar of `freshness`'s arguments: every refusal, and the fact that
//! no ordering of a valid set of arguments changes what they mean.

use std::error::Error;
use std::path::PathBuf;

use lgwks_std::seeded::Seeded;

use super::{Args, ArgsError, DEFAULT_INDEX, DEFAULT_JOBS, MAX_JOBS, Request, parse};
use crate::freshness_cmd::SimError;

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// `words` as the owned argument list the command receives.
fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The run `words` ask for, or a failure naming why they were refused.
fn run(words: &[&str]) -> Result<Args, SimError> {
    match parse(&argv(words))? {
        Request::Run(args) => Ok(args),
        Request::Help => Err(SimError::Missing("a run, but help was asked for")),
    }
}

#[test]
fn no_arguments_is_the_working_directory_at_the_defaults() -> TestResult {
    let args = run(&[])?;
    assert_eq!(
        args.target, None,
        "an omitted path is the working directory"
    );
    assert!(!args.json, "the table is the default");
    assert_eq!(args.index, DEFAULT_INDEX, "the crates.io sparse index");
    assert_eq!(args.jobs, DEFAULT_JOBS, "32 lookups in flight");
    Ok(())
}

#[test]
fn every_option_has_both_spellings() -> TestResult {
    let spaced = run(&[
        "--json",
        "--jobs",
        "4",
        "--index",
        "http://127.0.0.1:9/",
        "repo",
    ])?;
    let joined = run(&["--json", "--jobs=4", "--index=http://127.0.0.1:9/", "repo"])?;
    assert_eq!(
        spaced, joined,
        "`--flag VALUE` and `--flag=VALUE` are one thing"
    );
    assert_eq!(spaced.jobs, 4, "the bound");
    assert_eq!(
        spaced.index, "http://127.0.0.1:9",
        "trailing slashes are dropped"
    );
    assert_eq!(spaced.target, Some(PathBuf::from("repo")), "the path");
    assert!(spaced.json, "the mode");
    Ok(())
}

#[test]
fn help_is_a_request_not_a_run() -> TestResult {
    for words in [["--help"], ["-h"]] {
        assert_eq!(parse(&argv(&words))?, Request::Help, "{words:?}");
    }
    Ok(())
}

#[test]
fn a_refused_invocation_names_the_token() {
    let cases: [(&[&str], ArgsError); 11] = [
        (
            &["--bogus"],
            ArgsError::UnknownFlag {
                flag: "--bogus".to_owned(),
            },
        ),
        (
            &["-x"],
            ArgsError::UnknownFlag {
                flag: "-x".to_owned(),
            },
        ),
        (
            &["--json=yes"],
            ArgsError::UnknownFlag {
                flag: "--json=yes".to_owned(),
            },
        ),
        (&["--jobs"], ArgsError::MissingValue { flag: "--jobs" }),
        (&["--index"], ArgsError::MissingValue { flag: "--index" }),
        (
            &["--jobs", "--json"],
            ArgsError::ValueLooksLikeOption {
                flag: "--jobs",
                value: "--json".to_owned(),
            },
        ),
        (
            &["--jobs", "2", "--jobs", "3"],
            ArgsError::DuplicateFlag { flag: "--jobs" },
        ),
        (
            &["--index=http://a.test", "--index=http://b.test"],
            ArgsError::DuplicateFlag { flag: "--index" },
        ),
        (
            &["one", "two"],
            ArgsError::SurplusTarget {
                value: "two".to_owned(),
            },
        ),
        (
            &["--jobs=0"],
            ArgsError::BadJobs {
                value: "0".to_owned(),
            },
        ),
        (
            &["--index", "ftp://index.test"],
            ArgsError::BadIndex {
                value: "ftp://index.test".to_owned(),
            },
        ),
    ];
    for (words, expected) in cases {
        assert_eq!(parse(&argv(words)), Err(expected), "{words:?}");
    }
}

#[test]
fn the_job_bound_is_a_whole_number_from_one_to_the_ceiling() -> TestResult {
    assert_eq!(run(&["--jobs", "1"])?.jobs, 1, "serial is allowed");
    let ceiling = MAX_JOBS.to_string();
    assert_eq!(
        run(&["--jobs", ceiling.as_str()])?.jobs,
        MAX_JOBS,
        "the ceiling"
    );
    let over = MAX_JOBS.saturating_add(1).to_string();
    for text in [
        "0",
        "-1",
        "1.5",
        "many",
        "",
        " 4",
        "4 ",
        "+4",
        over.as_str(),
    ] {
        assert!(
            matches!(
                parse(&argv(&["--jobs", text])),
                Err(ArgsError::BadJobs { .. })
            ),
            "--jobs {text:?} must be refused"
        );
    }
    Ok(())
}

#[test]
fn an_index_url_cannot_break_out_of_its_argument() {
    for text in [
        "",
        "https://",
        "http:///",
        "index.test",
        "file:///etc/passwd",
        "https://a b.test",
        "https://a.test/\"x",
        "https://a.test/\\x",
        "https://a.test/\nx",
        "https://a.test/\u{7f}",
    ] {
        assert!(
            matches!(
                parse(&argv(&["--index", text])),
                Err(ArgsError::BadIndex { .. })
            ),
            "--index {text:?} must be refused"
        );
    }
}

/// The valid units of an invocation, each a group of tokens that must stay
/// together.
const UNITS: [&[&str]; 4] = [
    &["--json"],
    &["--jobs", "7"],
    &["--index=http://127.0.0.1:8080"],
    &["somewhere/repo"],
];

#[test]
fn no_ordering_of_valid_arguments_changes_what_they_mean() -> TestResult {
    let reference = run(&[
        "--json",
        "--jobs",
        "7",
        "--index=http://127.0.0.1:8080",
        "somewhere/repo",
    ])?;
    for seed in 0..200_u64 {
        let mut stream = Seeded::from_seed(seed);
        let mut units = UNITS.to_vec();
        // Fisher-Yates, drawn from the seed.
        for position in (1..units.len()).rev() {
            let other = stream.index(position.saturating_add(1))?;
            units.swap(position, other);
        }
        let words: Vec<&str> = units.iter().flat_map(|unit| unit.iter().copied()).collect();
        assert_eq!(
            run(&words)?,
            reference,
            "seed {seed}: {words:?} read differently from the reference order"
        );
    }
    Ok(())
}
