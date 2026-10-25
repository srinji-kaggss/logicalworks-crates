//! `freshness`'s arguments, read in one consuming pass.
//!
//! The command used to read every token that did not start with `--` as the
//! repository and to ignore every flag it did not know, so a mistyped option
//! was audited as if it had not been typed. It now refuses what it does not
//! understand, as `check` does, and an option's value can never also be read as
//! the repository.

use std::fmt;
use std::path::PathBuf;
use std::slice::Iter;

/// The sparse index crates.io serves, and the one Cargo reads by default.
pub(super) const DEFAULT_INDEX: &str = "https://index.crates.io";

/// How many lookups run at once when `--jobs` is not given.
pub(super) const DEFAULT_JOBS: usize = 32;

/// The most lookups `--jobs` accepts, well inside the blocking pool's ceiling.
pub(super) const MAX_JOBS: usize = 256;

/// What a run was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Args {
    /// The repository to read, or `None` for the working directory.
    pub(super) target: Option<PathBuf>,
    /// Whether the report is the JSON array rather than the table.
    pub(super) json: bool,
    /// The sparse index to ask, without a trailing slash.
    pub(super) index: String,
    /// The most lookups in flight at once.
    pub(super) jobs: usize,
}

/// What one parse resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Request {
    /// Check a repository.
    Run(Args),
    /// Print the usage block and exit successfully.
    Help,
}

/// Why the arguments were refused. Every variant names the token that caused
/// it, because that is the one the operator must look at next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArgsError {
    /// An option that takes a value was the last argument.
    MissingValue {
        /// The option that needed a value.
        flag: &'static str,
    },
    /// An option that takes a value was handed another option.
    ValueLooksLikeOption {
        /// The option that needed a value.
        flag: &'static str,
        /// The token it was handed instead.
        value: String,
    },
    /// A value-taking option appeared twice.
    DuplicateFlag {
        /// The option that was repeated.
        flag: &'static str,
    },
    /// An option this command does not define.
    UnknownFlag {
        /// The unrecognised token, verbatim.
        flag: String,
    },
    /// A second positional argument.
    SurplusTarget {
        /// The extra token, verbatim.
        value: String,
    },
    /// `--jobs` was not a whole number in range.
    BadJobs {
        /// The value as given.
        value: String,
    },
    /// `--index` was not an `http` or `https` URL.
    BadIndex {
        /// The value as given.
        value: String,
    },
}

impl fmt::Display for ArgsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingValue { flag } => write!(formatter, "{flag} needs a value"),
            Self::ValueLooksLikeOption { flag, ref value } => write!(
                formatter,
                "{flag} was given {value:?}, which is another option, not a value"
            ),
            Self::DuplicateFlag { flag } => {
                write!(formatter, "{flag} was given more than once")
            }
            Self::UnknownFlag { ref flag } => {
                write!(formatter, "unknown option for `freshness`: {flag}")
            }
            Self::SurplusTarget { ref value } => write!(
                formatter,
                "`freshness` reads one repository, and {value:?} is a second path"
            ),
            Self::BadJobs { ref value } => write!(
                formatter,
                "--jobs needs a whole number from 1 to {MAX_JOBS}, got {value:?}"
            ),
            Self::BadIndex { ref value } => write!(
                formatter,
                "--index needs an http:// or https:// URL with no spaces, got {value:?}"
            ),
        }
    }
}

impl std::error::Error for ArgsError {}

/// Parses `freshness`'s arguments in one consuming pass.
///
/// The repository may stand anywhere among the options and means the same thing
/// in each place. `--json` may repeat, because repeating a mode changes
/// nothing; `--jobs` and `--index` may not, because two values would make the
/// run depend on argument order. Both accept `--flag VALUE` and `--flag=VALUE`.
pub(super) fn parse(args: &[String]) -> Result<Request, ArgsError> {
    let mut target: Option<PathBuf> = None;
    let mut json = false;
    let mut index: Option<String> = None;
    let mut jobs: Option<usize> = None;
    let mut cursor = args.iter();
    while let Some(argument) = cursor.next() {
        let (flag, inline) = match argument.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (argument.as_str(), None),
        };
        match flag {
            "--help" | "-h" if inline.is_none() => return Ok(Request::Help),
            "--json" if inline.is_none() => json = true,
            "--jobs" => {
                let text = value_of("--jobs", inline, &mut cursor, jobs.is_some())?;
                jobs = Some(read_jobs(text)?);
            }
            "--index" => {
                let text = value_of("--index", inline, &mut cursor, index.is_some())?;
                index = Some(read_index(text)?);
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return unknown(argument);
            }
            path => {
                if target.is_some() {
                    return surplus(argument);
                }
                target = Some(PathBuf::from(path));
            }
        }
    }
    let index = match index {
        Some(index) => index,
        None => DEFAULT_INDEX.to_owned(),
    };
    let jobs = match jobs {
        Some(jobs) => jobs,
        None => DEFAULT_JOBS,
    };
    Ok(Request::Run(Args {
        target,
        json,
        index,
        jobs,
    }))
}

/// The refusal for an option this command does not define.
fn unknown(argument: &str) -> Result<Request, ArgsError> {
    Err(ArgsError::UnknownFlag {
        flag: argument.to_owned(),
    })
}

/// The refusal for a second repository.
fn surplus(argument: &str) -> Result<Request, ArgsError> {
    Err(ArgsError::SurplusTarget {
        value: argument.to_owned(),
    })
}

/// The value of a value-taking option: the text after `=`, or the next argument.
fn value_of<'a>(
    flag: &'static str,
    inline: Option<&'a str>,
    cursor: &mut Iter<'a, String>,
    seen: bool,
) -> Result<&'a str, ArgsError> {
    match (seen, inline) {
        (true, _) => Err(ArgsError::DuplicateFlag { flag }),
        (false, Some(value)) => Ok(value),
        (false, None) => match cursor.next() {
            None => Err(ArgsError::MissingValue { flag }),
            Some(next) if next.starts_with("--") => Err(ArgsError::ValueLooksLikeOption {
                flag,
                value: next.clone(),
            }),
            Some(next) => Ok(next.as_str()),
        },
    }
}

/// A `--jobs` value: digits only, from 1 to [`MAX_JOBS`].
fn read_jobs(text: &str) -> Result<usize, ArgsError> {
    let digits = text.bytes().all(|byte| byte.is_ascii_digit());
    match text.parse::<usize>() {
        Ok(jobs) if digits && (1..=MAX_JOBS).contains(&jobs) => Ok(jobs),
        _ => Err(ArgsError::BadJobs {
            value: text.to_owned(),
        }),
    }
}

/// An `--index` value: an `http` or `https` URL with a host and nothing that
/// could break out of the one argument it becomes, with its trailing slashes
/// removed.
fn read_index(text: &str) -> Result<String, ArgsError> {
    let trimmed = text.trim_end_matches('/');
    let host = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"));
    let clean = !trimmed.chars().any(|character| {
        character.is_whitespace() || character.is_control() || matches!(character, '"' | '\\')
    });
    match host {
        Some(host) if clean && !host.is_empty() => Ok(trimmed.to_owned()),
        _ => Err(ArgsError::BadIndex {
            value: text.to_owned(),
        }),
    }
}

#[cfg(test)]
mod sim_args;
