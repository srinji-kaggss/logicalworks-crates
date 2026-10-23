//! A hostile peer controls the shape of a JSON document: how deep it nests, where
//! it stops, which keys repeat, how a reader delivers it.
//!
//! The failures this layer has produced are the ones that crash the process
//! (unbounded recursion, stack exhaustion on deeply nested input), that let two
//! readers of one text disagree about a key (CVE-2017-12635 in CouchDB: duplicate
//! keys, and a key spelled with escapes), and that confuse a truncated stream with
//! a broken one so a caller retries what it should have reported. The attacks: a
//! million brackets, a document cut at every byte, a key repeated sixty-four
//! times, a reader that delivers one byte at a time and one that fails after any
//! number of bytes, and eight threads sharing one hostile corpus.
//!
//! The references are the facade's documented behaviour (the recursion limit and
//! last-key-wins), RFC 8259 for what a document is, and the document itself: a
//! tree encoded and decoded must be the tree.
#![cfg(feature = "json")]

use std::io::{self, Read};

use lgwks_std::json::{
    Error, Value, from_reader, from_slice, from_str, to_string, to_string_pretty, to_vec,
};

use crate::adversary::{
    Attacker, TestResult, as_text, decoder_campaign, diverges_between_seeds,
    replays_under_every_seed, sweep,
};
use crate::json_attacks::{accepts, draw_text, draw_value, encode};

/// A document that exercises every value kind and an escape of each class.
const DOCUMENT: &str = r#"{"k":[1,-2.5e3,true,null,{"z":"sé\n\"\\"}],"n":{"a":[],"b":{}}}"#;

/// A reader that hands out at most `chunk` bytes per call and then either ends
/// cleanly or fails, so a parser's reading of a truncated stream and of a broken
/// one can be told apart.
struct Choppy<'bytes> {
    /// The bytes the reader will deliver.
    data: &'bytes [u8],
    /// How many have been delivered.
    at: usize,
    /// The most any one call delivers.
    chunk: usize,
    /// Whether the reader fails, instead of ending, once the data is spent.
    fails: bool,
}

impl<'bytes> Choppy<'bytes> {
    /// A reader over `data` that delivers `chunk` bytes at a time.
    fn new(data: &'bytes [u8], chunk: usize, fails: bool) -> Self {
        Self {
            data,
            at: 0,
            chunk,
            fails,
        }
    }
}

impl Read for Choppy<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(left) = self.data.get(self.at..).filter(|left| !left.is_empty()) else {
            return if self.fails {
                Err(io::Error::other("injected failure"))
            } else {
                Ok(0)
            };
        };
        let take = left.len().min(self.chunk).min(buf.len());
        for (slot, &byte) in buf.iter_mut().zip(left.iter().take(take)) {
            *slot = byte;
        }
        self.at = self.at.saturating_add(take);
        Ok(take)
    }
}

/// The document `depth` arrays deep, closed.
fn nested(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

/// The document `depth` objects deep, closed, around the number one.
fn nested_objects(depth: usize) -> String {
    format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth))
}

/// The kind of an error as `(end of input, I/O failure)`.
fn kind(error: &Error) -> (bool, bool) {
    (error.is_eof(), error.is_io())
}

/// One campaign: hostile JSON through `from_slice`, outcomes folded.
fn campaign(seed: u64) -> u64 {
    decoder_campaign(
        seed,
        |attacker| encode(&draw_value(attacker, 3)),
        |hostile| match from_slice::<Value>(hostile) {
            Ok(value) => format!("ok {value}"),
            Err(error) => format!("err {error}"),
        },
    )
}

/// Any tree the attacker builds survives encoding and decoding, compact and
/// pretty, through text and through bytes, and the compact text holds no raw
/// control byte (RFC 8259 section 7).
#[test]
fn sim_adversary_json_drawn_trees_round_trip() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let value = draw_value(attacker, 4);
        let compact = to_string(&value)?;
        assert!(
            compact.bytes().all(|byte| byte >= 0x20),
            "seed {seed:#x} round {round}: a raw control byte in {compact:?}"
        );
        assert_eq!(
            from_str::<Value>(&compact)?,
            value,
            "seed {seed:#x} round {round}: compact"
        );
        assert_eq!(
            from_str::<Value>(&to_string_pretty(&value)?)?,
            value,
            "seed {seed:#x} round {round}: pretty"
        );
        assert_eq!(
            from_slice::<Value>(&to_vec(&value)?)?,
            value,
            "seed {seed:#x} round {round}: bytes"
        );
        Ok(())
    })
}

/// Nesting is bounded: arrays closed properly are read up to a depth and refused
/// past it, the boundary is sharp (never accepted again deeper), and the error
/// is a syntax error rather than a crash. The boundary is found, not assumed.
#[test]
fn sim_adversary_json_nesting_depth_is_bounded() -> TestResult {
    let verdicts: Vec<bool> = (1..=300_usize)
        .map(|depth| accepts(&nested(depth)))
        .collect();
    let boundary = verdicts
        .iter()
        .position(|&ok| !ok)
        .ok_or("a document 300 levels deep was accepted")?;
    assert!(
        boundary >= 100,
        "the recursion limit must sit between 100 and 300 levels: first refused at {}",
        boundary.saturating_add(1)
    );
    assert!(
        verdicts.iter().skip(boundary).all(|&ok| !ok),
        "once refused at depth {}, no deeper document may be accepted",
        boundary.saturating_add(1)
    );
    assert!(
        accepts(&nested_objects(boundary)),
        "objects within the array limit"
    );
    assert!(!accepts(&nested_objects(300)), "objects past the limit");
    let error = from_str::<Value>(&nested(300))
        .err()
        .ok_or("a document 300 levels deep was accepted")?;
    assert!(error.is_syntax(), "too deep is a syntax error: {error}");
    Ok(())
}

/// A million open brackets, closed or not, and ten million bytes of them, end
/// in an error at once: the parser neither recurses a million deep nor reads on
/// looking for a closer.
#[test]
fn sim_adversary_json_a_million_open_brackets_end_in_an_error() -> TestResult {
    for text in [
        "[".repeat(1_000_000),
        nested(1_000_000),
        "[".repeat(10_000_000),
        "{\"a\":".repeat(1_000_000),
    ] {
        let error = from_str::<Value>(&text)
            .err()
            .ok_or("a million open brackets were accepted")?;
        assert!(
            error.is_syntax(),
            "{} bytes of brackets: {error}",
            text.len()
        );
    }
    Ok(())
}

/// A document cut at any byte is refused, and told apart from a malformed one:
/// every proper prefix of a valid object is an end-of-input error.
#[test]
fn sim_adversary_json_every_proper_prefix_is_refused() -> TestResult {
    assert!(accepts(DOCUMENT), "the control document must parse");
    for cut in 0..DOCUMENT.len() {
        let Some(prefix) = DOCUMENT.get(..cut) else {
            continue;
        };
        let error = from_str::<Value>(prefix)
            .err()
            .ok_or("a truncated document was accepted")?;
        assert!(
            error.is_eof(),
            "the first {cut} bytes must be an end-of-input error: {error}"
        );
    }
    Ok(())
}

/// A repeated key keeps its last value, whatever the count and whatever sits
/// between the copies, so every reader of the same text agrees on one value.
#[test]
fn sim_adversary_json_a_repeated_key_keeps_its_last_value() -> TestResult {
    for copies in 1..=64_u32 {
        let body = (0..copies)
            .map(|value| format!("\"a\":{value}"))
            .collect::<Vec<_>>()
            .join(",");
        let value = from_str::<Value>(&format!("{{{body}}}"))?;
        let expected = copies.saturating_sub(1);
        assert_eq!(
            value.get("a").and_then(Value::as_u64),
            Some(u64::from(expected)),
            "{copies} copies"
        );
        assert_eq!(
            value.as_object().map(|map| map.len()),
            Some(1),
            "{copies} copies make one key"
        );
    }
    let mixed = from_str::<Value>(r#"{"a":1,"b":2,"a":3,"b":4,"c":5}"#)?;
    assert_eq!(
        mixed,
        from_str::<Value>(r#"{"a":3,"b":4,"c":5}"#)?,
        "interleaved repeats"
    );
    Ok(())
}

/// A key spelled with escapes and the same key spelled plainly are one key, and
/// keys that differ by a NUL or a code point are two: the parser compares
/// decoded keys, not raw bytes.
#[test]
fn sim_adversary_json_escaped_and_plain_keys_are_one_key() -> TestResult {
    let same = from_str::<Value>(r#"{"a":1,"a":2,"ab":3,"ab":4}"#)?;
    assert_eq!(
        same,
        from_str::<Value>(r#"{"a":2,"ab":4}"#)?,
        "escaped spellings merge"
    );
    let apart = from_str::<Value>(r#"{"a":1,"a\u0000":2,"a\u0000b":3,"á":4,"a":5}"#)?;
    assert_eq!(
        apart.as_object().map(|map| map.len()),
        Some(4),
        "NUL and accents keep keys apart"
    );
    assert_eq!(
        apart.get("a\u{0}").and_then(Value::as_u64),
        Some(2),
        "the NUL key"
    );
    sweep(512, |seed, round, attacker| {
        let key = to_string(&draw_text(attacker, 6))?;
        let value = from_str::<Value>(&format!("{{{key}:1,{key}:2}}"))?;
        assert_eq!(
            value.as_object().and_then(|map| map.values().next()),
            Some(&Value::from(2)),
            "seed {seed:#x} round {round}: {key}"
        );
        Ok(())
    })
}

/// However a reader chops the stream, one byte at a time or in chunks of any
/// size, the value read is the value `from_slice` reads.
#[test]
fn sim_adversary_json_reader_chunking_never_changes_the_value() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let value = draw_value(attacker, 3);
        let bytes = to_vec(&value)?;
        for chunk in [1, 2, 3, 7, 64, 4_096] {
            assert_eq!(
                from_reader::<_, Value>(Choppy::new(&bytes, chunk, false))?,
                value,
                "seed {seed:#x} round {round}: chunks of {chunk}"
            );
        }
        Ok(())
    })
}

/// A stream that ends early and a stream that breaks are different faults: at
/// every cut a clean end is an end-of-input error and a failing reader is an I/O
/// error, and a reader that fails only after the whole document still fails.
#[test]
fn sim_adversary_json_truncation_and_io_failure_are_told_apart() -> TestResult {
    let bytes = DOCUMENT.as_bytes();
    for cut in 0..bytes.len() {
        let Some(data) = bytes.get(..cut) else {
            continue;
        };
        for chunk in [1, 5, 4_096] {
            let ends = from_reader::<_, Value>(Choppy::new(data, chunk, false))
                .err()
                .ok_or("a truncated stream was accepted")?;
            assert_eq!(
                kind(&ends),
                (true, false),
                "clean end after {cut} bytes, chunks of {chunk}"
            );
            let breaks = from_reader::<_, Value>(Choppy::new(data, chunk, true))
                .err()
                .ok_or("a failed stream was accepted")?;
            assert_eq!(
                kind(&breaks),
                (false, true),
                "failure after {cut} bytes, chunks of {chunk}"
            );
        }
    }
    assert!(
        from_reader::<_, Value>(Choppy::new(bytes, 8, true)).is_err(),
        "a failure after the document is still a failure"
    );
    assert!(
        from_reader::<_, Value>(Choppy::new(bytes, 8, false)).is_ok(),
        "the same stream ending cleanly is the document"
    );
    Ok(())
}

/// Large flat documents parse to the right shape in bounded time and memory: half
/// a million array elements, a four-megabyte string, and ten thousand keys.
#[test]
fn sim_adversary_json_large_flat_documents_parse() -> TestResult {
    let items = format!("[{}0]", "0,".repeat(500_000));
    assert_eq!(
        from_str::<Value>(&items)?.as_array().map(Vec::len),
        Some(500_001),
        "elements"
    );
    let long = "x".repeat(4 * 1_048_576);
    assert_eq!(
        from_str::<String>(&to_string(&long)?)?.len(),
        long.len(),
        "a four megabyte string"
    );
    let keys = (0..10_000)
        .map(|index| format!("\"k{index}\":{index}"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        from_str::<Value>(&format!("{{{keys}}}"))?
            .as_object()
            .map(|map| map.len()),
        Some(10_000),
        "keys"
    );
    Ok(())
}

/// Eight threads parsing one hostile corpus each reach the answers one thread
/// reaches, in the same order: the parser holds no state between calls.
#[test]
fn sim_adversary_json_one_corpus_gives_one_answer_on_every_thread() {
    let mut attacker = Attacker::new(0x5eed);
    let corpus: Vec<Vec<u8>> = (0..400)
        .map(|_| {
            let value = draw_value(&mut attacker, 3);
            attacker.mutate_n(&encode(&value), 2)
        })
        .collect();
    let answers = |documents: &[Vec<u8>]| -> Vec<Option<Value>> {
        documents
            .iter()
            .map(|document| from_slice::<Value>(document).ok())
            .collect()
    };
    let expected = answers(&corpus);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| answers(&corpus))).collect();
        for worker in workers {
            assert_eq!(
                worker.join().ok().as_ref(),
                Some(&expected),
                "a thread read the corpus differently"
            );
        }
    });
}

/// An error names a place inside the input, on one line: its line is at least
/// one and no later than the lines the input has, so a message cannot point
/// outside what was sent.
#[test]
fn sim_adversary_json_error_positions_stay_inside_the_input() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let value = draw_value(attacker, 3);
        let hostile = as_text(&attacker.mutate_n(&encode(&value), 3));
        if let Err(error) = from_str::<Value>(&hostile) {
            let lines = hostile.split('\n').count();
            assert!(
                (1..=lines).contains(&error.line()),
                "seed {seed:#x} round {round}: line {} of {lines}",
                error.line()
            );
            let message = error.to_string();
            assert!(
                !message.contains('\n'),
                "seed {seed:#x} round {round}: {message:?}"
            );
        }
        Ok(())
    })
}

/// The same seed mounts the same attack on the parser.
#[test]
fn sim_adversary_json_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_json_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
