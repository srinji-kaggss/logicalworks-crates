//! How `curl`'s three outputs are read, over the statuses a registry sends
//! and over seeded bodies, including bodies that end the way a trailer does.

use std::error::Error;

use lgwks_std::seeded::Seeded;

use super::{FetchError, interpret};

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

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
