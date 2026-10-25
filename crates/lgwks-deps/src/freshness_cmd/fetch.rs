//! One HTTP GET through `curl`, answered as a body or as a typed reason.
//!
//! `curl` stays the one transport: the crate's manifest keeps the admission
//! gate free of a TLS stack (INV-GATE-ZERO-DEPS), and the command already
//! required `curl`. The status code travels on the last line of the output,
//! after the body, so a 404 is told apart from a refused connection without a
//! second request and without a temporary file.
//!
//! The output is read through a cap. A registry sends whatever it likes, and a
//! lookup that buffered all of it would hold a hostile or broken answer in
//! memory once per request in flight.

use std::fmt;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};

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

/// The most bytes of `curl`'s output one lookup reads, 16 MiB, trailer included.
///
/// The largest index file crates.io serves is `web-sys`'s at 4,487,047 bytes
/// decoded (measured 2026-10-25), so this is 3.7 times the largest real file.
/// The count is of decoded bytes, after `--compressed`, so a small body that
/// inflates is stopped too. At the default 32 lookups in flight the worst case
/// is 512 MiB.
const MAX_OUTPUT_BYTES: usize = 16_777_216;

/// The most bytes of `curl`'s complaint kept: only its first line is quoted.
const MAX_COMPLAINT_BYTES: usize = 4096;

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
    /// The response was longer than a lookup reads, and `curl` was ended.
    TooLarge {
        /// The most bytes a lookup reads.
        limit: usize,
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
            Self::TooLarge { limit } => {
                write!(formatter, "response larger than {limit} bytes")
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
    let mut child = Command::new("curl")
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| FetchError::Spawn {
            reason: error.to_string(),
        })?;
    let outcome = collect(&mut child);
    // Every path ends with the child gone. A lookup that stopped reading leaves
    // `curl` blocked on a full pipe, so it is ended and collected here and not
    // left to the pipe closing; after a clean `wait` both calls change nothing.
    if let Err(error) = child.kill() {
        lgwks_std::trace::debug!(error = %error, "curl had already exited");
    }
    if let Err(error) = child.wait() {
        lgwks_std::trace::debug!(error = %error, "curl could not be collected twice");
    }
    outcome
}

/// Reads `child` to its end through the cap and says what it came to.
fn collect(child: &mut Child) -> Result<String, FetchError> {
    let stdout = child.stdout.take().ok_or_else(|| FetchError::Malformed {
        reason: "no stdout pipe".to_owned(),
    })?;
    let output = read_up_to(stdout, MAX_OUTPUT_BYTES).map_err(|error| FetchError::Transport {
        reason: format!("reading curl's output: {error}"),
    })?;
    if output.more {
        let error = FetchError::TooLarge {
            limit: MAX_OUTPUT_BYTES,
        };
        lgwks_std::trace::debug!(error = %error, "curl is ended at the read cap");
        return Err(error);
    }
    let complaint = match child.stderr.take() {
        Some(pipe) => match read_up_to(pipe, MAX_COMPLAINT_BYTES) {
            Ok(read) => read.bytes,
            Err(error) => {
                lgwks_std::trace::debug!(error = %error, "curl's complaint was not readable");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let status = child.wait().map_err(|error| FetchError::Transport {
        reason: format!("waiting for curl: {error}"),
    })?;
    interpret(status.code(), &output.bytes, &complaint)
}

/// What a bounded read found.
struct Capped {
    /// The first bytes of the source, at most the limit.
    bytes: Vec<u8>,
    /// Whether the source held more than the limit.
    more: bool,
}

/// Reads `source` until it ends or holds more than `limit` bytes.
///
/// One byte past the limit is read, to tell a source that ends at the limit
/// from one that goes on; that byte is dropped. Memory is bounded by `limit`
/// whatever the source sends.
fn read_up_to(source: impl Read, limit: usize) -> io::Result<Capped> {
    let ceiling = match u64::try_from(limit) {
        Ok(bytes) => bytes.saturating_add(1),
        Err(_) => u64::MAX,
    };
    let mut bytes = Vec::new();
    source.take(ceiling).read_to_end(&mut bytes)?;
    let more = bytes.len() > limit;
    bytes.truncate(limit);
    Ok(Capped { bytes, more })
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
