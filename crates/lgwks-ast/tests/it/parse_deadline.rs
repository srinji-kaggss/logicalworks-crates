//! The parse deadline against a real slow parse (#277).
//!
//! The byte ceiling bounds what the parser is handed, not how long it works on
//! it. The source here is the parse-budget rig's `longline` shape for Go — the
//! grammar's own valid source tiled to the byte ceiling with its newlines
//! removed — which the rig measured at 1.08 s of bare parse on an M5 Pro, and
//! which is slower on every host this suite runs on. Go is a default grammar, so
//! this runs in the default-feature build the gate's test lane uses.
//!
//! What is asserted here, on the host's real clock, is that an input that takes
//! longer than the deadline when unbounded answers [`ParseError::TimedOut`]
//! naming that deadline, and that the thread is free for the next parse, which
//! the test proves by parsing again on the same thread. *When* the parse stops
//! is proved on a clock the proof drives (`deadline_tests` in `src/lib.rs`):
//! exactly at the first progress check past the deadline. Timed on the host's
//! clock that claim measured the scheduler, and a loaded runner answered a
//! 100 ms deadline after 392 ms although the parser had stopped on time.

#![cfg(feature = "lang-go")]

use std::error::Error;
use std::time::Duration;

use lgwks_ast::{Language, MAX_SOURCE_BYTES, ParseError, try_parse, try_parse_within};

/// What every test returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The rig's Go `longline` shape at the byte ceiling.
fn go_longline() -> String {
    let fragment = "package main\n\nfunc main() {}\n";
    let mut source = String::with_capacity(MAX_SOURCE_BYTES);
    while source.len().saturating_add(fragment.len()) <= MAX_SOURCE_BYTES {
        source.push_str(fragment);
    }
    source.replace('\n', "")
}

#[test]
/// An unbounded parse longer than the deadline is stopped at the deadline, and
/// the same thread then parses the next source cleanly.
fn a_slow_parse_is_stopped_at_its_deadline_and_the_thread_parses_again() -> TestResult {
    let source = go_longline();

    // First, that this input really is longer than the deadlines below when
    // unbounded: a 300 ms deadline still stops it. Without this, a host fast
    // enough to finish the parse inside 50 ms would make the next assertion
    // pass for the wrong reason, or fail it for no reason at all.
    let long = Duration::from_millis(300);
    let refusal = try_parse_within(&source, Language::Go, long);
    if !matches!(refusal, Err(ParseError::TimedOut { .. })) {
        return Err(format!(
            "the witness must outlast {long:?} unbounded, got {:?}",
            refusal.as_ref().err()
        )
        .into());
    }

    for deadline in [
        Duration::from_millis(25),
        Duration::from_millis(50),
        Duration::from_millis(100),
    ] {
        let refusal = try_parse_within(&source, Language::Go, deadline);
        match refusal {
            Err(ParseError::TimedOut { language, after }) => {
                assert_eq!(language, "go", "the refusal names the grammar");
                assert_eq!(after, deadline, "and the deadline it applied");
            }
            other => {
                return Err(
                    format!("{deadline:?}: expected TimedOut, got {:?}", other.err()).into(),
                );
            }
        }
        // The same thread, straight away: a stopped parse leaves its parser
        // holding state for a resume, and a cached parser that kept it would
        // hand this source a continuation of the one above.
        let next = try_parse("package main\n\nfunc main() {}\n", Language::Go);
        assert!(
            next.is_ok(),
            "the next parse on the same thread must start clean, got {:?}",
            next.err()
        );
    }
    Ok(())
}

#[test]
/// A deadline long enough for the parse changes nothing: the answer is the one
/// the default deadline gives.
fn a_deadline_the_parse_fits_inside_answers_like_try_parse() -> TestResult {
    let source = "package main\n\nfunc main() {}\n".repeat(64);
    let bounded = try_parse_within(&source, Language::Go, Duration::from_secs(60));
    let default = try_parse(&source, Language::Go);
    match (bounded, default) {
        (Ok(bounded), Ok(default)) => {
            assert_eq!(
                bounded.root().text(),
                default.root().text(),
                "the same source parses to the same text"
            );
            assert_eq!(
                lgwks_ast::inspect_ast(&bounded.root(), None),
                lgwks_ast::inspect_ast(&default.root(), None),
                "and the same metrics"
            );
            Ok(())
        }
        (bounded, default) => Err(format!(
            "both must parse: within={:?} default={:?}",
            bounded.err(),
            default.err()
        )
        .into()),
    }
}

#[test]
/// A deadline past what the clock can represent never arrives.
fn an_unrepresentable_deadline_runs_the_parse_to_completion() -> TestResult {
    let parsed = try_parse_within(
        "package main\n\nfunc main() {}\n",
        Language::Go,
        Duration::MAX,
    );
    assert!(
        parsed.is_ok(),
        "Duration::MAX is no deadline, got {:?}",
        parsed.err()
    );
    Ok(())
}
