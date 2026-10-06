//! Deterministic simulation of the parse deadline (#277).
//!
//! A deadline is a clock, and a clock is the one input a replay cannot hold
//! still. The family is deterministic anyway because it drives the deadline only
//! at its two ends, where the outcome does not depend on how fast the host is:
//!
//! - **`Duration::ZERO`.** The deadline has passed at the parser's first
//!   progress check, whatever the host. Every source here is drawn large enough
//!   to reach that check (a hundred parser operations), so every one is stopped
//!   and answers [`ParseError::TimedOut`] — on a fast laptop and on a loaded CI
//!   runner alike.
//! - **A deadline no seeded source can reach.** The answer is then exactly what
//!   [`try_parse`] gives, arm for arm and tree for tree.
//!
//! Between the two, every seed parses again **on the same thread** straight
//! after it was stopped. A stopped tree-sitter parse keeps its partial state for
//! a resume, and the parser is cached per thread; a reset that went missing
//! would hand the next source a continuation of the stopped one. Comparing that
//! answer with a parse on a fresh thread, which has never stopped anything, is
//! what catches it.
//!
//! Every seed's scenario is recorded into a trace — the shape, the bytes, and
//! the three answers by arm — with no clock reading in it, and the same seed
//! replays to the same hash.

#![cfg(feature = "lang-rust")]

use crate::seed;
use std::collections::BTreeSet;
use std::error::Error;
use std::time::Duration;

use lgwks_ast::{Language, ParseError, inspect_ast, try_parse, try_parse_within};
use seed::{Rng, Trace};

/// What every family returns.
type TestResult = Result<(), Box<dyn Error>>;

/// What a scenario step returns: the value it drew, or the refusal that stopped
/// it. A draw this target cannot hold is reported rather than replaced by a
/// count, because a source written from a substituted count is still a trace.
type Scenario<T> = Result<T, Box<dyn Error>>;

/// Sources written per family.
const SEEDS: u64 = 64;

/// The base every family's seeds are derived from.
const BASE_SEED: u64 = 0x0da5_2770_dead_0001;

/// A deadline no source this generator writes can reach on any host this suite
/// runs on: the largest is 16 KiB, which the slowest grammar measured parses in
/// milliseconds.
const UNREACHABLE: Duration = Duration::from_secs(600);

/// The seed for run `index` of `family`; families take disjoint streams.
fn seed_for(family: u64, index: u64) -> u64 {
    BASE_SEED
        ^ family.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ index.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// Statements a mixed source interleaves, so the grammar has real work.
const CLEAN: [&str; 4] = ["let a = 1;", "let b: u32 = 2;", "return;", "let c = a + b;"];

/// A statement from [`CLEAN`], drawn from it.
///
/// The generator draws `u32` and this is a four-element table, so the draw is
/// converted rather than assumed: a host whose `usize` cannot hold it is
/// reported, because a mixed source written from a substituted statement is
/// still a trace and would still replay.
fn drawn_statement(rng: &mut Rng) -> Scenario<&'static str> {
    let bound = u32::try_from(CLEAN.len())?;
    let at = usize::try_from(rng.below(bound))?;
    CLEAN
        .get(at)
        .copied()
        .ok_or_else(|| format!("a draw from {} statements names none of them", CLEAN.len()).into())
}

/// A seeded source and the name of its shape.
///
/// The nesting runs from 64 to 2 048 levels. Sixty-four is the floor because
/// it is past a hundred parser operations for every shape — `fn f() {` is five
/// tokens — so a zero deadline is always reached; the ceiling keeps one seed to
/// 16 KiB. Both ends are inside sixteen bits, so the draw fits a `usize` on
/// every target that can run this suite.
fn write_source(rng: &mut Rng) -> Scenario<(String, &'static str)> {
    let levels = usize::try_from(1_u32 << rng.between(6, 11))?;
    Ok(match rng.below(4) {
        0 => (
            format!("{}fn f() {{}}", "fn f() {".repeat(levels)),
            "unbalanced-openers",
        ),
        1 => (
            format!(
                "{}fn f() {{}}{}",
                "fn f() {".repeat(levels),
                "}".repeat(levels)
            ),
            "balanced",
        ),
        2 => ("fn f() {}\n".repeat(levels), "flat"),
        _ => {
            let mut source = String::new();
            for level in 0..levels {
                source.push_str("fn f() {");
                if level % 4 == 0 {
                    source.push_str(drawn_statement(rng)?);
                }
            }
            source.push_str(&"}".repeat(levels));
            (source, "mixed")
        }
    })
}

/// The arm a checked parse answered with, plus the tree's shape when it built
/// one, so two answers compare on more than their variant.
fn answer_of(answer: &Result<lgwks_ast::Parsed, ParseError>) -> String {
    match *answer {
        Ok(ref tree) => {
            let metrics = inspect_ast(&tree.root(), None);
            format!(
                "accepted nodes={} depth={}",
                metrics.nodes, metrics.max_depth
            )
        }
        Err(ParseError::TimedOut { after, .. }) => format!("timed-out after={after:?}"),
        Err(ref other) => format!("refused {other}"),
    }
}

/// `source` parsed on a thread that has never stopped a parse, owned and joined
/// by the scope before this returns.
fn on_a_fresh_thread(source: &str) -> Result<String, Box<dyn Error>> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| answer_of(&try_parse(source, Language::Rust)))
            .join()
            .map_err(|panic| format!("the fresh-thread parse panicked: {panic:?}").into())
    })
}

/// One seed's whole scenario: stopped, then parsed again on this thread, then
/// parsed under a deadline it cannot reach.
fn scenario(seed: u64) -> Scenario<Trace> {
    let mut rng = Rng::new(seed);
    let (source, shape) = write_source(&mut rng)?;
    let mut trace = Trace::new();
    trace.record(shape);
    trace.record_number("bytes", source.len());

    let stopped = try_parse_within(&source, Language::Rust, Duration::ZERO);
    trace.record(&answer_of(&stopped));
    let named = match stopped {
        Err(ParseError::TimedOut { language, after }) => {
            language == "rust" && after == Duration::ZERO
        }
        _ => false,
    };
    let checked = stopped_as_named(named, shape, source.len());
    checked?;

    // Straight after the stop, on the same thread and so on the same cached
    // parser, against a thread that never stopped anything.
    let same_thread = answer_of(&try_parse(&source, Language::Rust));
    let fresh = on_a_fresh_thread(&source);
    let agreed = same_answer("the parse after a stop", &same_thread, &fresh?);
    agreed?;
    trace.record(&same_thread);

    let generous = answer_of(&try_parse_within(&source, Language::Rust, UNREACHABLE));
    let agreed = same_answer("a deadline the parse fits inside", &generous, &same_thread);
    agreed?;
    trace.record(&generous);
    Ok(trace)
}

/// Refuse a seed whose zero-deadline parse was not stopped as `rust` at zero.
fn stopped_as_named(named: bool, shape: &str, bytes: usize) -> Result<(), Box<dyn Error>> {
    if named {
        Ok(())
    } else {
        Err(format!(
            "a {shape} source of {bytes} bytes was not stopped by a zero deadline as `rust` at zero"
        )
        .into())
    }
}

/// Refuse two answers that should be one.
fn same_answer(what: &str, got: &str, expected: &str) -> Result<(), Box<dyn Error>> {
    if got == expected {
        Ok(())
    } else {
        Err(format!("{what} answered {got:?}, expected {expected:?}").into())
    }
}

#[test]
/// Every seeded source is stopped by a zero deadline, the same thread then
/// parses it exactly as a fresh thread does, and a deadline it fits inside
/// changes nothing.
fn every_seed_is_stopped_then_parses_clean_on_the_same_thread() -> TestResult {
    let mut shapes = BTreeSet::new();
    for index in 0..SEEDS {
        let seed = seed_for(1, index);
        let mut rng = Rng::new(seed);
        shapes.insert(write_source(&mut rng)?.1);
        scenario(seed).map_err(|error| format!("seed {seed:#x}: {error}"))?;
    }
    assert_eq!(
        shapes.len(),
        4,
        "the family must reach every shape, reached {shapes:?}"
    );
    Ok(())
}

#[test]
/// The same seed replays to the same trace hash.
fn the_same_seed_replays_to_the_same_deadline_trace() -> TestResult {
    for index in 0..SEEDS {
        let seed = seed_for(2, index);
        let first = scenario(seed).map_err(|error| format!("seed {seed:#x}: {error}"))?;
        let second = scenario(seed).map_err(|error| format!("seed {seed:#x}: {error}"))?;
        assert_eq!(
            first.hash(),
            second.hash(),
            "seed {seed:#x} replayed to a different trace"
        );
    }
    Ok(())
}

#[test]
/// Distinct seeds diverge, so a generator that stopped varying cannot pass as a
/// deterministic one.
fn distinct_seeds_diverge_in_their_deadline_trace() -> TestResult {
    let mut hashes = BTreeSet::new();
    for index in 0..SEEDS {
        let seed = seed_for(3, index);
        let trace = scenario(seed).map_err(|error| format!("seed {seed:#x}: {error}"))?;
        hashes.insert(trace.hash());
    }
    assert!(
        hashes.len() > 8,
        "{SEEDS} seeds produced only {} distinct traces",
        hashes.len()
    );
    Ok(())
}
