//! Deterministic simulation of the evidence rule of unlogged-err-return:
//! generated functions held to a model written from the rule's contract rather
//! than from the detector (#699).
//!
//! The model is three statements. A `return Err(..)` is evidenced only by a log
//! statement that runs on the way into it: in its own block before it, or in an
//! enclosing block before the statement that encloses it; a log inside a sibling
//! branch is on no path to the return, and a statement between a log and the
//! return does not take the log away. A log evidences an error that carries
//! values when it names one of them (a format capture, a field, an argument) or
//! carries a value of its own beside its message; its macro path, the prose of
//! its message, a doubled brace, a literal and a keyword name nothing. An error
//! that carries no value is evidenced by any log on the path.
//!
//! Sources: the field and argument syntax of `tracing::event!` and
//! `tracing::error!` (tokio-rs/tracing, `tracing::field`); implicit named
//! arguments in format strings, `{e}` and `{{` (Rust RFC 2795, `std::fmt`); and
//! the reviewed defect of #699 where a log was forgotten by the next statement
//! and a macro path or a word of prose was counted as a mention.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `unlogged_band_00..03` | 1,024 seeded functions agree with the model, replay to one trace, and reach every outcome |
//! | `the_bands_cover_every_seed_exactly_once` | the bands partition the seed space |
//! | `one_line_or_many_the_verdict_is_the_same` | reflowing a program never changes what it reports |
//! | `two_tenants_scan_their_own_functions` | no state crosses between calls |
//! | `a_seed_replays_and_distinct_seeds_diverge` | same seed, same trace; different seeds, different programs |

#![cfg(feature = "scan")]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use lgwks_ast::scan::{Rule, scan_source_with};

use crate::sim::{Rng, Trace, receipt};

/// Seeds the bands sweep.
const SEEDS: u64 = 1_024;

/// How many bands the family is cut into, one declared test each.
const BANDS: u64 = 4;

/// The odd multiplier that spreads consecutive indices across the seed space.
const SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// One log statement a generated function may write: its text, the values the
/// rule's contract says it names, and whether it carries a value of its own
/// beside its message.
struct Log {
    /// The statement as written.
    text: &'static str,
    /// The values it names.
    names: &'static [&'static str],
    /// Whether it carries context of its own.
    context: bool,
}

/// A log row: the statement, the values it names, and its context.
const fn log(text: &'static str, names: &'static [&'static str], context: bool) -> Log {
    Log {
        text,
        names,
        context,
    }
}

/// Every log the generator writes. The first eight name the error or carry a
/// value of their own; the rest are shapes that look like evidence and are not.
static LOGS: [Log; 19] = [
    log("tracing::error!(\"failed: {e}\");", &["e"], false),
    log("tracing::error!(%e, \"failed\");", &["e"], true),
    log("tracing::error!(error = ?e);", &["e"], true),
    log("tracing::warn!(\"{e:?}\");", &["e"], false),
    log("log::warn!(\"failed {}\", e.kind());", &["e"], true),
    log("tracing::event!(Level::ERROR, %e, \"x\");", &["e"], true),
    log("tracing::debug!(index, \"decoded\");", &["index"], true),
    log("tracing::warn!(found = e[0], \"refused\");", &["e"], true),
    log("eprintln!(\"zero\");", &[], false),
    log("eprintln!(\"saw {other}\");", &["other"], false),
    log("eprintln!(\"no errors today\");", &[], false),
    log("tracing::error!(\"request failed\");", &[], false),
    log("tracing::error!(error = \"boom\");", &[], false),
    log("eprintln!(\"failed {}\", 0);", &[], false),
    log("eprintln!(\"failed {}\", true);", &[], false),
    log("eprintln!(\"{{e}}\");", &[], false),
    log("tracing::event!(Level::ERROR, \"x\");", &[], false),
    log(
        "tracing::debug!(target: \"frames\", \"decoded\");",
        &[],
        false,
    ),
    log("tracing::warn!(found = count(), \"refused\");", &[], false),
];

/// Statements that are not logs and say nothing about the failure.
static OTHERS: [&str; 6] = [
    "let count = 1_u32;",
    "drop(count);",
    "step();",
    "let hint = \"tracing::error!(%e)\";",
    "my_info!(\"x\");",
    "catalog::emit!(\"x\");",
];

/// The returns a generated function ends in, with the values each carries.
static RETURNS: [(&str, &[&str]); 3] = [
    ("return Err(e);", &["e"]),
    ("return Err(E::Wrapped(e));", &["e"]),
    ("return Err(E::Flat);", &[]),
];

/// One generated function: its text and the lines the model expects a finding on.
struct Program {
    /// The file's text.
    text: String,
    /// The finding lines, ascending.
    expected: Vec<usize>,
    /// Whether a log was on the path to the return.
    had_logs: bool,
}

/// A source text under construction, one numbered line at a time.
#[derive(Default)]
struct Builder {
    /// The text so far.
    text: String,
    /// Lines written.
    lines: usize,
}

impl Builder {
    /// Append a line at `indent` levels and return its 1-based number.
    fn push(&mut self, indent: usize, line: &str) -> usize {
        self.lines = self.lines.saturating_add(1);
        for _ in 0..indent {
            self.text.push_str("    ");
        }
        self.text.push_str(line);
        self.text.push('\n');
        self.lines
    }
}

/// Whether the model says a return carrying `carried` is evidenced by `path`.
fn evidenced(path: &[&Log], carried: &[&str]) -> bool {
    if path.is_empty() {
        return false;
    }
    if carried.is_empty() {
        return true;
    }
    path.iter()
        .any(|entry| entry.context || carried.iter().any(|name| entry.names.contains(name)))
}

/// Write one statement of a block at `indent`. A log written directly in the
/// block joins `path`; a log inside a branch or loop is written beside it and
/// does not.
fn statement(
    rng: &mut Rng,
    out: &mut Builder,
    indent: usize,
    path: &mut Vec<&'static Log>,
) -> Result<(), Box<dyn Error>> {
    let inner = indent.saturating_add(1);
    match rng.below(10) {
        0..=3 | 9 => {
            let entry = rng.pick_named("logs", &LOGS)?;
            out.push(indent, entry.text);
            path.push(entry);
        }
        4..=6 => {
            let other = *rng.pick_named("others", &OTHERS)?;
            out.push(indent, other);
        }
        7 => {
            let entry = rng.pick_named("logs", &LOGS)?;
            out.push(indent, "if flag {");
            out.push(inner, entry.text);
            out.push(indent, "}");
        }
        _ => {
            let other = *rng.pick_named("others", &OTHERS)?;
            out.push(indent, "while flag {");
            out.push(inner, other);
            out.push(indent, "}");
        }
    }
    Ok(())
}

/// One seed's function.
fn generate(seed: u64) -> Result<Program, Box<dyn Error>> {
    let mut rng = Rng::new(seed);
    let mut out = Builder::default();
    out.push(0, "fn f(flag: bool) -> Result<(), E> {");
    out.push(1, "let e = E::Flat;");
    let mut path: Vec<&'static Log> = Vec::new();
    for _ in 0..rng.between(0, 4) {
        statement(&mut rng, &mut out, 1, &mut path)?;
    }
    let nested = rng.coin();
    let indent = if nested { 2 } else { 1 };
    if nested {
        out.push(1, "if flag {");
        for _ in 0..rng.between(0, 2) {
            statement(&mut rng, &mut out, indent, &mut path)?;
        }
    }
    let &(text, carried) = rng.pick_named("returns", &RETURNS)?;
    let line = out.push(indent, text);
    if nested {
        out.push(1, "}");
    }
    out.push(1, "Ok(())");
    out.push(0, "}");
    let had_logs = !path.is_empty();
    let expected = if evidenced(&path, carried) {
        Vec::new()
    } else {
        vec![line]
    };
    Ok(Program {
        text: out.text,
        expected,
        had_logs,
    })
}

/// The lines the detector reports for `text`, with no other rule running.
fn scanned(text: &str) -> Result<Vec<usize>, Box<dyn Error>> {
    let hits = scan_source_with(text, "sim.rs", &[Rule::UnloggedError])?;
    Ok(hits.iter().map(|hit| hit.line()).collect())
}

/// Scans one generated function and refuses any disagreement with the model.
fn agree(seed: u64, program: &Program) -> Result<Vec<usize>, Box<dyn Error>> {
    let observed = scanned(&program.text)?;
    if observed == program.expected {
        return Ok(observed);
    }
    Err(format!(
        "seed {seed:#x}: the detector reported {observed:?}\nthe model expects {:?}\nsource:\n{}",
        program.expected, program.text
    )
    .into())
}

/// The seed of the family's `index`-th scenario.
const fn seed_of(index: u64) -> u64 {
    0x5ca5_0000_0000_0000 ^ index.wrapping_mul(SPREAD)
}

/// The index range `band` sweeps.
fn band_seeds(band: u64) -> Result<std::ops::Range<u64>, Box<dyn Error>> {
    let per = SEEDS
        .checked_div(BANDS)
        .ok_or("the family is cut into a band")?;
    let first = band.saturating_mul(per);
    Ok(first..first.saturating_add(per))
}

/// One seed folded into a trace hash: the function and every finding.
fn receipt_of(seed: u64) -> Result<u64, Box<dyn Error>> {
    let program = generate(seed)?;
    let found = agree(seed, &program)?;
    let mut trace = Trace::new();
    trace.record(&program.text);
    for line in &found {
        trace.record_number("unlogged-err-return", line);
    }
    Ok(receipt(&trace)?)
}

/// Every scenario in `band`: each agrees with the model, replays to one trace,
/// and the band reaches each outcome.
fn unlogged_band(band: u64) -> Result<(), Box<dyn Error>> {
    let mut cleared = 0_usize;
    let mut reported = 0_usize;
    let mut misread = 0_usize;
    for index in band_seeds(band)? {
        let seed = seed_of(index);
        let program = generate(seed)?;
        let found = agree(seed, &program)?;
        if found.is_empty() {
            cleared = cleared.saturating_add(1);
        } else {
            reported = reported.saturating_add(1);
            if program.had_logs {
                // A log was on the path and still did not excuse the return.
                misread = misread.saturating_add(1);
            }
        }
        assert_eq!(
            receipt_of(seed)?,
            receipt_of(seed)?,
            "seed {seed:#x} replays to one trace"
        );
    }
    assert!(cleared > 0, "band {band} reached an evidenced return");
    assert!(reported > 0, "band {band} reached an unevidenced return");
    assert!(
        misread > 0,
        "band {band} reached a log that is not evidence for the return it precedes"
    );
    Ok(())
}

macro_rules! bands {
    ($($name:ident => $band:expr);+ $(;)?) => {
        $(
            /// A seeded band of generated functions, each agreeing with the model.
            #[test]
            fn $name() -> Result<(), Box<dyn Error>> {
                unlogged_band($band)
            }
        )+
    };
}

bands!(
    unlogged_band_00 => 0; unlogged_band_01 => 1; unlogged_band_02 => 2; unlogged_band_03 => 3;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_bands_cover_every_seed_exactly_once() -> Result<(), Box<dyn Error>> {
    let mut covered = BTreeSet::new();
    for band in 0..BANDS {
        for index in band_seeds(band)? {
            assert!(covered.insert(index), "seed {index} is in two bands");
        }
    }
    assert_eq!(
        u64::try_from(covered.len())?,
        SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}

/// Formatting is not evidence: the same function on one line reports the same
/// number of findings, because the verdict reads statements and not lines.
#[test]
fn one_line_or_many_the_verdict_is_the_same() -> Result<(), Box<dyn Error>> {
    for index in 0..256_u64 {
        let seed = seed_of(index ^ 0x0d0d);
        let program = generate(seed)?;
        let wrapped = program.text.replace('\n', " ");
        let found = scanned(&wrapped)?;
        assert_eq!(
            found.len(),
            program.expected.len(),
            "seed {seed:#x}: one line changed the verdict of\n{}",
            program.text
        );
    }
    Ok(())
}

/// Sixty-four functions scanned on their own threads report exactly their own
/// findings: the detector keeps no state between calls.
#[test]
fn two_tenants_scan_their_own_functions() -> Result<(), Box<dyn Error>> {
    let mut programs = Vec::new();
    for index in 0..64_u64 {
        let seed = seed_of(index ^ 0x7ea4_0000);
        programs.push((seed, generate(seed)?));
    }
    let failures = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for entry in &programs {
            let seed = entry.0;
            let program = &entry.1;
            handles.push(scope.spawn(move || -> Result<(), String> {
                for _ in 0..8 {
                    let observed = scanned(&program.text).map_err(|failure| failure.to_string())?;
                    if observed != program.expected {
                        return Err(format!("seed {seed:#x}: {observed:?}"));
                    }
                }
                Ok(())
            }));
        }
        let mut failures = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(failure)) => failures.push(failure),
                Err(_) => failures.push("a detector thread panicked".to_owned()),
            }
        }
        failures
    });
    assert!(failures.is_empty(), "tenants disagreed: {failures:#?}");
    Ok(())
}

/// The same seed replays its trace; one text never yields two answers, and
/// distinct seeds write distinct programs.
#[test]
fn a_seed_replays_and_distinct_seeds_diverge() -> Result<(), Box<dyn Error>> {
    let mut by_text: BTreeMap<String, u64> = BTreeMap::new();
    for index in 0..64_u64 {
        let seed = seed_of(index ^ 0x5eed);
        let program = generate(seed)?;
        let first = receipt_of(seed)?;
        assert_eq!(
            first,
            receipt_of(seed)?,
            "seed {seed:#x} replayed differently"
        );
        if let Some(prior) = by_text.insert(program.text.clone(), first) {
            assert_eq!(prior, first, "seed {seed:#x}: one text, two receipts");
        }
    }
    assert!(
        by_text.len() >= 40,
        "64 seeds wrote only {} distinct functions",
        by_text.len()
    );
    Ok(())
}
