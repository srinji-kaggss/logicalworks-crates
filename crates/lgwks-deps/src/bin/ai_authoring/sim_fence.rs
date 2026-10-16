//! Simulated text: replies with fenced blocks, `cargo test` logs, oracle sources,
//! `time -l` reports and the model command's NDJSON are generated from a seed and
//! read by the real parsers, and an independent model says what each must read.
//!
//! These readers are the runner's whole view of the world outside it: what the
//! author wrote, what the oracle said, what it cost. A misreading here is a
//! wrong number in the table, so each is held to a model over generated text, not
//! to the one example it was written against.

use std::collections::BTreeMap;

use lgwks_std::json::{self, Value};

use super::author::{extract_rust_block, read_reply};
use super::grade::{Verdict, oracle_test_names, peak_rss_bytes, read_verdicts};
use super::replay::{Failure, replay_sweep};
use super::seed::Rng;
use super::sim_kit::{count, ensure, one_of};

/// Languages a block can be fenced as.
const LANGS: [&str; 5] = ["rust", "toml", "text", "", "json"];

/// Words a reply's prose is made of; none holds a backtick.
const WORDS: [&str; 6] = ["here", "is", "the", "crate", "you", "asked"];

/// One piece of a generated reply.
enum Segment {
    /// A line of words.
    Prose(String),
    /// A fenced block.
    Block {
        /// The language word after the opening fence.
        lang: &'static str,
        /// The lines between the fences, each ended by the reply's line ending.
        body: String,
        /// Backticks in the opening fence.
        opener: usize,
        /// Backticks in the closing fence.
        closer: usize,
        /// Whether the closing fence is written.
        closed: bool,
    },
}

/// A line of prose.
fn prose(rng: &mut Rng) -> Result<String, Failure> {
    let mut words = Vec::new();
    for _ in 0..count(rng, 1, 5)? {
        words.push(*one_of(rng, "words", &WORDS)?);
    }
    Ok(words.join(" "))
}

/// A block of `lang` with one to three lines, each ended by `eol`.
fn block(rng: &mut Rng, eol: &str, index: usize, closed: bool) -> Result<Segment, Failure> {
    let mut lines = Vec::new();
    for line in 0..count(rng, 1, 3)? {
        let indent = if rng.coin() { "    " } else { "" };
        lines.push(format!("{indent}fn item_{index}_{line}() {{}}{eol}"));
    }
    let body = lines.concat();
    Ok(Segment::Block {
        lang: one_of(rng, "languages", &LANGS)?,
        body,
        opener: count(rng, 3, 5)?,
        closer: count(rng, 3, 5)?,
        closed,
    })
}

/// A reply's pieces: up to five of prose and blocks, and sometimes a last block
/// the model never closed.
fn draw_segments(rng: &mut Rng, eol: &str) -> Result<Vec<Segment>, Failure> {
    let mut segments = Vec::new();
    for index in 0..count(rng, 0, 5)? {
        if rng.chance(400) {
            segments.push(Segment::Prose(prose(rng)?));
        } else {
            segments.push(block(rng, eol, index, true)?);
        }
    }
    if rng.chance(200) {
        segments.push(block(rng, eol, 9, false)?);
    }
    Ok(segments)
}

/// The reply the pieces make, lines ended by `eol`.
fn render(segments: &[Segment], eol: &str) -> String {
    let mut text = String::new();
    for segment in segments {
        match *segment {
            Segment::Prose(ref words) => {
                text.push_str(words);
                text.push_str(eol);
            }
            Segment::Block {
                lang,
                ref body,
                opener,
                closer,
                closed,
            } => {
                text.push_str(&"`".repeat(opener));
                text.push_str(lang);
                text.push_str(eol);
                text.push_str(body);
                if closed {
                    text.push_str(&"`".repeat(closer));
                    text.push_str(eol);
                }
            }
        }
    }
    text
}

/// What the author's solution is: the last closed rust block, else the last
/// closed block of any language, else nothing; one trailing newline.
fn solution(segments: &[Segment]) -> String {
    let closed: Vec<(&str, &str)> = segments
        .iter()
        .filter_map(|segment| match *segment {
            Segment::Block {
                lang,
                ref body,
                closed: true,
                ..
            } => Some((lang, body.as_str())),
            Segment::Prose(_) | Segment::Block { .. } => None,
        })
        .collect();
    let pick = closed
        .iter()
        .rev()
        .find(|pair| pair.0 == "rust")
        .or_else(|| closed.last());
    match pick {
        Some(pair) => format!("{}\n", pair.1.trim_end()),
        None => String::new(),
    }
}

/// Source: CommonMark 0.31 section 4.5 (fenced code blocks: an opening fence of
/// three or more backticks, an info string, a closing fence of at least as many,
/// an unclosed block running to the end of the document) and the benchmark's own
/// reply contract: "exactly one fenced rust block". Requirement: whatever mix of
/// prose, languages, fence lengths, line endings and unclosed tails a model
/// writes, the solution is the last closed rust block.
#[test]
fn the_solution_is_the_last_closed_rust_block_of_any_reply() -> Result<(), Failure> {
    replay_sweep("fences", 600, |rng, trace| {
        let eol = if rng.coin() { "\n" } else { "\r\n" };
        let segments = draw_segments(rng, eol)?;
        let reply = render(&segments, eol);
        let wanted = solution(&segments);
        let found = extract_rust_block(&reply);
        ensure(found == wanted, || {
            format!("read {found:?} from {reply:?}, expected {wanted:?}")
        })?;
        trace.record(&reply);
        trace.record(&found);
        Ok(())
    })
}

/// Clause names, three of which are prefixes of another.
const CLAUSES: [&str; 6] = [
    "holds",
    "holds_twice",
    "holds_twice_over",
    "drop_clause",
    "dr",
    "x_clause",
];

/// The three words libtest ends a clause line with.
const STATUSES: [(&str, Verdict); 3] = [
    ("ok", Verdict::Pass),
    ("FAILED", Verdict::Fail),
    ("ignored", Verdict::Fail),
];

/// Lines of a `cargo test` log that name no clause.
const NOISE: [&str; 6] = [
    "running 3 tests",
    "warning: unused variable: `value`",
    "test unrelated_thing ... ok",
    "test result: FAILED. 1 passed; 1 failed; 1 ignored",
    "",
    "failures:",
];

/// Source: the libtest pretty formatter (rust-lang/rust, `library/test/src/
/// formatters/pretty.rs`): one `test <name> ... <status>` line per test. Requirement:
/// each clause reads the status of its last line, a clause never named has not
/// run, an ignored clause proved nothing and fails, and a name that is the prefix
/// of another is never read as it.
#[test]
fn a_clause_reads_the_status_of_its_last_log_line_whatever_surrounds_it() -> Result<(), Failure> {
    replay_sweep("verdict lines", 600, |rng, trace| {
        let mut clauses: BTreeMap<String, Verdict> = BTreeMap::new();
        for name in CLAUSES {
            if rng.coin() {
                clauses.insert(name.to_owned(), Verdict::NotRun);
            }
        }
        let names: Vec<String> = clauses.keys().cloned().collect();
        let mut wanted = clauses.clone();
        let mut lines = Vec::new();
        for _ in 0..count(rng, 0, 10)? {
            if rng.chance(400) {
                lines.push((*one_of(rng, "noise", &NOISE)?).to_owned());
                continue;
            }
            if names.is_empty() {
                continue;
            }
            let name = one_of(rng, "clauses", &names)?;
            let status = one_of(rng, "statuses", &STATUSES)?;
            let indent = if rng.coin() { "  " } else { "" };
            lines.push(format!("{indent}test {name} ... {}", status.0));
            wanted.insert(name.clone(), status.1);
        }
        let log = format!("{}\n", lines.join("\n"));
        read_verdicts(&log, &mut clauses);
        ensure(clauses == wanted, || {
            format!("read {clauses:?} from {log:?}, expected {wanted:?}")
        })?;
        trace.record(&log);
        Ok(())
    })
}

/// Source lines an oracle file holds that are not clauses.
const NOT_CLAUSES: [&str; 5] = [
    "#[cfg(test)]\nfn helper() {}",
    "// a note about #[test] attributes\nfn noted() {}",
    "fn plain() {}",
    "#[test]\nasync fn asynchronous() {}",
    "#[test]\n#[ignore]\nfn attributed() {}",
];

/// The ways a clause is declared.
const DECLARATIONS: [&str; 3] = [
    "#[test]\nfn {name}() {}",
    "#[test]\n    pub fn {name}() {}",
    "#[test] fn {name}() {}",
];

/// Source: the Rust reference, "Testing attributes": `#[test]` marks the function
/// that follows. Requirement: the clause names are the `#[test] fn` names in
/// source order, and a function that only looks like one (a comment, an async
/// test, a test with another attribute between) is not a clause.
#[test]
fn oracle_clauses_are_the_test_functions_in_source_order() -> Result<(), Failure> {
    replay_sweep("oracle names", 400, |rng, trace| {
        let mut source = String::new();
        let mut wanted = Vec::new();
        for index in 0..count(rng, 0, 8)? {
            if rng.chance(350) {
                source.push_str(one_of(rng, "non-clauses", &NOT_CLAUSES)?);
            } else {
                let name = format!("clause_{index}");
                source.push_str(
                    &one_of(rng, "declarations", &DECLARATIONS)?.replace("{name}", &name),
                );
                wanted.push(name);
            }
            source.push_str("\n\n");
        }
        let found = oracle_test_names(&source);
        ensure(found == wanted, || {
            format!("read {found:?} from {source:?}, expected {wanted:?}")
        })?;
        trace.record(&source);
        Ok(())
    })
}

/// Source: `time(1)` on macOS (`-l`: bytes before the label) and GNU time (`-v`:
/// kibibytes after it). Requirement: the peak is in bytes under either spelling,
/// the first report wins, and a report with no number is no measurement.
#[test]
fn the_peak_resident_set_is_read_in_bytes_from_either_spelling_of_the_report() -> Result<(), Failure>
{
    replay_sweep("time reports", 600, |rng, trace| {
        let bytes = u64::from(rng.between(1, u32::MAX));
        let kibibytes = u64::from(rng.between(1, u32::MAX));
        let apple = format!(
            "        1.85 real         0.31 user         0.12 sys\n{bytes:>12}  maximum resident set size\n     912  page reclaims\n"
        );
        let gnu = format!(
            "\tCommand being timed: \"cargo test\"\n\tMaximum resident set size (kbytes): {kibibytes}\n\tPage size (bytes): 4096\n"
        );
        ensure(peak_rss_bytes(&apple) == Some(bytes), || {
            format!("read {:?} from {apple:?}", peak_rss_bytes(&apple))
        })?;
        ensure(peak_rss_bytes(&gnu) == kibibytes.checked_mul(1024), || {
            format!("read {:?} from {gnu:?}", peak_rss_bytes(&gnu))
        })?;
        ensure(
            peak_rss_bytes(&format!("{apple}{gnu}")) == Some(bytes),
            || "the first report must win".to_owned(),
        )?;
        ensure(
            peak_rss_bytes("  many  maximum resident set size\n").is_none(),
            || "a label with no number is a measurement".to_owned(),
        )?;
        ensure(peak_rss_bytes("1.85 real 0.31 user\n").is_none(), || {
            "a report with no label is a measurement".to_owned()
        })?;
        trace.record_number("bytes", bytes);
        trace.record_number("kibibytes", kibibytes);
        Ok(())
    })
}

/// One line of a model command's output and what it must read as.
enum Output {
    /// A progress object.
    Progress,
    /// Text that is not JSON.
    Chatter,
    /// JSON that is not an object.
    Array,
    /// A result: its text, tokens read, tokens written and wall time.
    Result(String, u64, u64, u64),
    /// A result that names itself by `type` and carries nothing else.
    Bare,
}

/// A model command's output of up to eight lines, drawn from the sequence.
fn draw_output(rng: &mut Rng) -> Result<Vec<Output>, Failure> {
    let mut lines = Vec::new();
    for index in 0..count(rng, 0, 8)? {
        lines.push(match rng.below(5) {
            0 => Output::Progress,
            1 => Output::Chatter,
            2 => Output::Array,
            3 => Output::Bare,
            _ => Output::Result(
                format!("```rust\nfn done_{index}() {{}}\n```\n\"quoted\" \\ \u{2603}"),
                u64::from(rng.between(0, 200_000)),
                u64::from(rng.between(0, 50_000)),
                u64::from(rng.between(0, 900_000)),
            ),
        });
    }
    Ok(lines)
}

/// The text the command printed for `line`.
fn print(line: &Output) -> Result<String, Failure> {
    Ok(match *line {
        Output::Progress => "{\"type\":\"progress\",\"step\":3}".to_owned(),
        Output::Chatter => "not json at all".to_owned(),
        Output::Array => "[1, 2, 3]".to_owned(),
        Output::Bare => "{\"type\":\"result\"}".to_owned(),
        Output::Result(ref text, consumed, written, millis) => {
            let quoted = json::to_string(&Value::from(text.clone()))
                .map_err(|error| format!("a reply text could not be quoted: {error}"))?;
            format!(
                "{{\"type\":\"result\",\"finalText\":{quoted},\"usage\":{{\"inputTokens\":{consumed},\"outputTokens\":{written}}},\"durationMs\":{millis}}}"
            )
        }
    })
}

/// Source: the model command's `--output-format json` stream (one JSON object per
/// line, the last `result` being the answer). Requirement: the reply is the last
/// result line whatever progress, chatter or malformed lines surround it, with its
/// text and its counts; no result line is no reply.
#[test]
fn the_reply_is_the_last_result_line_of_whatever_the_command_printed() -> Result<(), Failure> {
    replay_sweep("ndjson", 600, |rng, trace| {
        let lines = draw_output(rng)?;
        let mut printed = Vec::new();
        for line in &lines {
            printed.push(print(line)?);
        }
        let text = format!("{}\n", printed.join("\n"));
        let last = lines
            .iter()
            .rev()
            .find(|line| matches!(**line, Output::Result(..) | Output::Bare));
        let read = read_reply(&text);
        match (last, read) {
            (None, None) => {}
            (Some(&Output::Bare), Some(found)) => ensure(
                found.text.is_empty()
                    && found.tokens_in == 0
                    && found.tokens_out == 0
                    && found.wall_ms == 0,
                || "a bare result is an empty reply".to_owned(),
            )?,
            (Some(&Output::Result(ref words, consumed, written, millis)), Some(found)) => ensure(
                found.text == *words
                    && found.tokens_in == consumed
                    && found.tokens_out == written
                    && found.wall_ms == millis,
                || format!("the reply read from {text:?} is not the last result"),
            )?,
            (Some(_), None) | (None, Some(_)) | (Some(_), Some(_)) => {
                return Err(format!("a reply was found or missed wrongly in {text:?}").into());
            }
        }
        trace.record(&text);
        Ok(())
    })
}
