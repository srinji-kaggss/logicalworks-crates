//! The deadline's stopping rule, on a clock the test drives.

use std::time::Instant;

use super::check::run_parser;
use super::{Duration, LanguageExt, ParseError};

/// A source the Go grammar needs many thousands of progress checks to parse:
/// its own valid source tiled to the byte ceiling with the newlines removed.
fn go_longline() -> String {
    let fragment = "package main\n\nfunc main() {}\n";
    let mut source = String::with_capacity(super::MAX_SOURCE_BYTES);
    while source.len().saturating_add(fragment.len()) <= super::MAX_SOURCE_BYTES {
        source.push_str(fragment);
    }
    source.replace('\n', "")
}

/// A parse is stopped at the first progress check that reads the deadline or
/// later, never one check after, and the next parse on the same parser runs
/// to completion.
///
/// The clock moves one millisecond per reading, so a deadline of `d` ms is
/// crossed on the `d`-th progress check: the parse must have read the clock
/// exactly `d + 1` times, the start included. Exact, whatever the host is
/// doing, which is what a bound on the host's own clock could not be.
#[test]
fn a_parse_stops_at_the_first_check_past_its_deadline() -> Result<(), String> {
    let grammar: super::TSLanguage = super::SupportLang::Go.get_ts_language();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|error| error.to_string())?;
    let source = go_longline();
    let origin = Instant::now();
    for millis in [1_u64, 25, 50, 100, 1_000] {
        let mut readings = 0_u64;
        let mut now = origin;
        let mut overflowed = false;
        let mut clock = || {
            let reading = now;
            match now.checked_add(Duration::from_millis(1)) {
                Some(next) => now = next,
                None => overflowed = true,
            }
            readings = readings.saturating_add(1);
            reading
        };
        let deadline = Duration::from_millis(millis);
        let outcome = run_parser(&mut parser, &source, "go", deadline, &mut clock);
        match outcome {
            Err(ParseError::TimedOut { language, after }) => {
                assert_eq!(language, "go", "the refusal names the grammar");
                assert_eq!(after, deadline, "and the deadline it applied");
            }
            other => {
                return Err(format!(
                    "{deadline:?}: a parse longer than its deadline must be stopped, got {:?}",
                    other.err()
                ));
            }
        }
        assert!(
            !overflowed,
            "the test clock ran past what an Instant can hold"
        );
        assert_eq!(
            readings,
            millis.saturating_add(1),
            "a {millis} ms deadline must stop the parse on its {millis}th check, \
             not before and not after"
        );
        let next = run_parser(
            &mut parser,
            "package main\n\nfunc main() {}\n",
            "go",
            Duration::MAX,
            &mut Instant::now,
        );
        assert!(
            next.is_ok(),
            "the parser must start the next source clean after a stop, got {:?}",
            next.err()
        );
    }
    Ok(())
}
