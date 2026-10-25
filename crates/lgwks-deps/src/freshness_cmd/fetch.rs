//! One HTTP GET through `curl`, answered as a body or as a typed reason.
//!
//! `curl` stays the one transport: the crate's manifest keeps the admission
//! gate free of a TLS stack (INV-GATE-ZERO-DEPS), and the command already
//! required `curl`. The status code travels on the last line of the output,
//! after the body, so a 404 is told apart from a refused connection without a
//! second request and without a temporary file.

use std::fmt;
use std::process::Command;

/// The `User-Agent` crates.io's crawler policy asks for: this tool's name and
/// version and where to reach its maintainers, read from the crate's own
/// manifest. Compiled from `Cargo.toml` rather than written here, so the
/// version cannot go stale and no person's address is baked into every copy
/// of a published binary.
const USER_AGENT: &str = concat!(
    "User-Agent: lgwks-deps/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

/// The cap on one request, so an unreachable registry cannot hang the command.
const TIMEOUT_SECONDS: &str = "10";

/// How many redirects a request may follow, as Cargo's own client allows.
const MAX_REDIRECTS: &str = "3";

/// `curl --write-out` template: a newline, then the final status code.
const STATUS_TRAILER: &str = "\n%{http_code}";

/// Why a request did not produce a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FetchError {
    /// `curl` could not be started.
    Spawn {
        /// The operating system's reason.
        reason: String,
    },
    /// `curl` ran and failed before a response: DNS, connect, TLS or timeout.
    Transport {
        /// Its exit status and the first line it wrote to stderr.
        reason: String,
    },
    /// The server has no such file: HTTP 404 or 410.
    Missing,
    /// The server answered with a status other than 200, 404 and 410.
    Status {
        /// The HTTP status code.
        code: u16,
    },
    /// `curl` exited cleanly and its output was not a body and a status.
    Malformed {
        /// What was wrong with the output.
        reason: String,
    },
}

impl fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Spawn { ref reason } => write!(formatter, "cannot start curl: {reason}"),
            Self::Transport { ref reason } => formatter.write_str(reason),
            Self::Missing => formatter.write_str("not in the index (HTTP 404)"),
            Self::Status { code } => write!(formatter, "HTTP {code}"),
            Self::Malformed { ref reason } => {
                write!(formatter, "curl returned no readable status: {reason}")
            }
        }
    }
}

impl std::error::Error for FetchError {}

/// Fetches `url` and returns its body when the server answers 200.
///
/// Only `http` and `https` are followed, including through redirects, so an
/// index URL cannot be turned into a read of a local file.
pub(super) fn get(url: &str) -> Result<String, FetchError> {
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--compressed",
            "--location",
            "--max-redirs",
            MAX_REDIRECTS,
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--max-time",
            TIMEOUT_SECONDS,
            "--header",
            USER_AGENT,
            "--write-out",
            STATUS_TRAILER,
            url,
        ])
        .output()
        .map_err(|error| FetchError::Spawn {
            reason: error.to_string(),
        })?;
    interpret(output.status.code(), &output.stdout, &output.stderr)
}

/// What `curl`'s exit status, stdout and stderr mean.
fn interpret(exit: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<String, FetchError> {
    match exit {
        Some(0) => read_response(stdout),
        _ => Err(transport_failure(exit, stderr)),
    }
}

/// The failure of a `curl` that did not exit cleanly: its status, and the first
/// line it complained with when it said anything.
fn transport_failure(exit: Option<i32>, stderr: &[u8]) -> FetchError {
    let status = match exit {
        Some(code) => format!("curl exit {code}"),
        None => "curl was ended by a signal".to_owned(),
    };
    let complaint = String::from_utf8_lossy(stderr);
    let reason = match complaint.lines().next().map(str::trim) {
        Some(first) if !first.is_empty() => format!("{status}: {first}"),
        _ => status,
    };
    FetchError::Transport { reason }
}

/// A clean `curl` run's output: the body, a newline and the status code.
fn read_response(stdout: &[u8]) -> Result<String, FetchError> {
    let text = std::str::from_utf8(stdout).map_err(|error| FetchError::Malformed {
        reason: error.to_string(),
    })?;
    let (body, code) = text
        .rsplit_once('\n')
        .ok_or_else(|| FetchError::Malformed {
            reason: "no status line".to_owned(),
        })?;
    match code.trim().parse::<u16>() {
        Ok(200) => Ok(body.to_owned()),
        Ok(404 | 410) => Err(FetchError::Missing),
        Ok(other) => Err(FetchError::Status { code: other }),
        Err(error) => Err(FetchError::Malformed {
            reason: format!("status {code:?}: {error}"),
        }),
    }
}

#[cfg(test)]
mod sim_fetch;
