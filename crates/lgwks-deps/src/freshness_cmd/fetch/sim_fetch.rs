//! How `curl`'s three outputs are read, over the statuses a registry sends
//! and over seeded bodies, including bodies that end the way a trailer does,
//! and how the read is capped, at the limit, one under and one over.

use std::error::Error;
use std::io::{self, Read};

use lgwks_std::seeded::Seeded;

use super::{FetchError, MAX_OUTPUT_BYTES, interpret, read_up_to};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// A source that hands out one byte per call, as a slow pipe does.
struct Trickle<'a> {
    /// What it has not handed out yet.
    rest: &'a [u8],
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (Some((&byte, tail)), Some(slot)) = (self.rest.split_first(), buf.first_mut()) else {
            return Ok(0);
        };
        *slot = byte;
        self.rest = tail;
        Ok(1)
    }
}

/// A source that hands out `good` bytes and then fails, as a dropped pipe does.
struct Dropping {
    /// How many bytes it still hands out.
    good: usize,
}

impl Read for Dropping {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match (self.good, buf.first_mut()) {
            (0, _) => Err(io::Error::other("link dropped")),
            (_, Some(slot)) => {
                *slot = b'x';
                self.good = self.good.saturating_sub(1);
                Ok(1)
            }
            (_, None) => Ok(0),
        }
    }
}

/// `curl`'s stdout for `body` answered with `status`.
fn stdout(body: &str, status: &str) -> Vec<u8> {
    format!("{body}\n{status}").into_bytes()
}

#[test]
fn a_200_is_its_body_and_nothing_else() {
    assert_eq!(
        interpret(Some(0), &stdout("{\"vers\":\"1.0.0\"}", "200"), b""),
        Ok("{\"vers\":\"1.0.0\"}".to_owned()),
        "the trailer is not part of the body"
    );
    assert_eq!(
        interpret(Some(0), &stdout("", "200"), b""),
        Ok(String::new()),
        "an empty 200 is an empty body"
    );
}

#[test]
fn a_missing_file_is_told_apart_from_a_failing_server() {
    for status in ["404", "410"] {
        assert_eq!(
            interpret(Some(0), &stdout("not found", status), b""),
            Err(FetchError::Missing),
            "{status} means the index has no such crate"
        );
    }
    for (status, code) in [("429", 429_u16), ("500", 500), ("503", 503), ("304", 304)] {
        assert_eq!(
            interpret(Some(0), &stdout("x", status), b""),
            Err(FetchError::Status { code }),
            "{status} is not an answer"
        );
    }
}

#[test]
fn a_curl_failure_keeps_its_exit_status_and_its_first_stderr_line() {
    assert_eq!(
        interpret(
            Some(6),
            b"\n000",
            b"curl: (6) Could not resolve host: index.invalid\nsecond line\n"
        ),
        Err(FetchError::Transport {
            reason: "curl exit 6: curl: (6) Could not resolve host: index.invalid".to_owned()
        }),
        "a DNS failure names the host"
    );
    assert_eq!(
        interpret(None, b"", b""),
        Err(FetchError::Transport {
            reason: "curl was ended by a signal".to_owned()
        }),
        "a signal has no exit status to quote"
    );
    assert_eq!(
        interpret(Some(28), b"", b"  \n"),
        Err(FetchError::Transport {
            reason: "curl exit 28".to_owned()
        }),
        "a blank complaint adds nothing to the status"
    );
}

#[test]
fn output_that_is_not_a_body_and_a_status_is_malformed() {
    for output in [
        b"200".as_slice(),
        b"body\nabc".as_slice(),
        b"body\n".as_slice(),
        b"body\n99999".as_slice(),
        b"\xff\xfe\n200".as_slice(),
    ] {
        let read = interpret(Some(0), output, b"");
        assert!(
            matches!(read, Err(FetchError::Malformed { .. })),
            "{output:?} has no readable status, got {read:?}"
        );
    }
}

#[test]
fn a_read_stops_exactly_one_byte_past_the_limit() -> TestResult {
    // Seven, eight and nine bytes against a limit of eight: under and at the
    // limit are whole, one over is cut to the limit and says more followed.
    for (length, kept, more) in [
        (0_usize, 0_usize, false),
        (7, 7, false),
        (8, 8, false),
        (9, 8, true),
    ] {
        let data = vec![b'x'; length];
        let read = read_up_to(data.as_slice(), 8)?;
        assert_eq!(
            read.bytes.len(),
            kept,
            "{length} bytes against a limit of 8"
        );
        assert_eq!(read.more, more, "{length} bytes against a limit of 8");
    }
    Ok(())
}

#[test]
fn a_bounded_read_agrees_with_its_model_for_any_limit_and_any_pipe() -> TestResult {
    for seed in 0..300_u64 {
        let mut stream = Seeded::from_seed(seed);
        let limit = usize::try_from(stream.below(40)?)?;
        let length = usize::try_from(stream.below(60)?)?;
        let data = (0..length)
            .map(|_| Ok(u8::try_from(stream.below(256)?)?))
            .collect::<Result<Vec<u8>, Box<dyn Error>>>()?;
        let kept = data.get(..length.min(limit)).ok_or("the model slice")?;
        let whole = read_up_to(data.as_slice(), limit)?;
        let trickled = read_up_to(Trickle { rest: &data }, limit)?;
        for (pipe, read) in [("a slice", &whole), ("one byte a call", &trickled)] {
            assert_eq!(
                read.bytes, kept,
                "seed {seed}: {pipe}, limit {limit}, {length} bytes"
            );
            assert_eq!(
                read.more,
                length > limit,
                "seed {seed}: {pipe}, limit {limit}, {length} bytes"
            );
        }
    }
    Ok(())
}

#[test]
fn a_limit_past_any_source_never_reports_more() -> TestResult {
    let read = read_up_to(b"abc".as_slice(), usize::MAX)?;
    assert_eq!(read.bytes, b"abc", "the whole source is kept");
    assert!(!read.more, "nothing followed the end");
    Ok(())
}

#[test]
fn a_pipe_that_fails_is_an_error_only_when_the_read_reaches_the_failure() {
    let reached = read_up_to(Dropping { good: 5 }, 100);
    assert!(reached.is_err(), "the read ran into the dropped link");
    // Four bytes are all a limit of three can ask for, so the failure at the
    // sixth is never reached and the answer is the cap, not the fault.
    let capped = read_up_to(Dropping { good: 5 }, 3);
    assert!(
        matches!(capped, Ok(ref read) if read.more && read.bytes.len() == 3),
        "a runaway source is stopped at the cap before it can fail"
    );
}

#[test]
fn the_cap_clears_the_largest_index_file_measured_and_the_text_names_it() {
    // web-sys, 4,487,047 bytes decoded from index.crates.io on 2026-10-25.
    const {
        assert!(
            MAX_OUTPUT_BYTES > 3 * 4_487_047,
            "the cap must leave a real registry's biggest file room to grow"
        );
    }
    assert_eq!(
        FetchError::TooLarge { limit: 4096 }.to_string(),
        "response larger than 4096 bytes",
        "the row names the limit it hit"
    );
}

#[test]
fn a_body_that_ends_like_a_trailer_is_still_a_body() -> TestResult {
    // Only the last newline separates the status, so a body whose own last
    // line is a number is returned whole.
    for seed in 0..300_u64 {
        let mut stream = Seeded::from_seed(seed);
        let lines = stream.below(5)?;
        let mut body = String::new();
        for _ in 0..lines {
            body = format!("{body}{}\n", stream.below(1_000)?);
        }
        body = format!("{body}{}", stream.below(1_000)?);
        assert_eq!(
            interpret(Some(0), &stdout(&body, "200"), b""),
            Ok(body.clone()),
            "seed {seed}: the body {body:?} must come back unchanged"
        );
    }
    Ok(())
}
