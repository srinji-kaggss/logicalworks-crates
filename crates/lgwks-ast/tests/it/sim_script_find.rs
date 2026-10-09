//! Deterministic simulation of finding every `script!` in a file (#384).
//!
//! One seed writes one Rust source file the way a repository holds them: real
//! `script!` invocations, some the language accepts and some it refuses, at a
//! seeded depth of nesting (top level, in a `mod`, in a `fn`), behind a path
//! prefix or not, between decoys that spell `script!` where the compiler would
//! never expand it (a line comment, a block comment, a doc comment, a string,
//! a `script` that is not followed by `!` and a group), and sometimes with an
//! unterminated string at the end, so the file is not Rust tokens at all.
//!
//! The generator records where each real invocation's header, first body line
//! and refused token are, as it writes them, so the expected answer is decided
//! by the writer and never read back out of the parser. The shipped
//! [`read_source`] then reads the file through the public API and is held to
//! that answer exactly: the same invocations in the same order, each at its
//! line, an accepted one mapped with its flow and first step at their lines, a
//! refused one refused at the offending token's line with the refusal's text,
//! no decoy found, and a broken file refused at the line of its stray quote.
//!
//! # Replay
//!
//! Every seed's scenario and answer are recorded into a trace and hashed; the
//! same seed replays to the same hash, and distinct seeds are required to
//! produce distinct traces, so a generator that stopped varying fails here
//! rather than passing as a determinism test.

#![cfg(all(feature = "script", feature = "lang-rust"))]

use std::collections::BTreeSet;

use std::error::Error;

use lgwks_ast::script::{Invocation, read_source};

use crate::seed::{Rng, Trace};

type TestResult = Result<(), Box<dyn Error>>;

/// Seeds the sweep reads: every one writes a different file.
const SEEDS: u64 = 4_000;

/// What the refused invocation's message says, whatever else it adds.
const REFUSED_SAYS: &str = "ending the process from a flow";

/// One real invocation the writer planted, and where it put each part.
struct Planted {
    /// The line of the macro's name.
    line: usize,
    /// The line of the `flow` header.
    header: usize,
    /// The line of the first line inside the flow.
    body: usize,
    /// Whether the language accepts it.
    accepted: bool,
}

/// A file the seed wrote, and the answer decided while writing it.
struct Written {
    /// The source text.
    source: String,
    /// Every real invocation, in source order.
    planted: Vec<Planted>,
    /// The line of the unterminated string, when the file is broken.
    broken_at: Option<usize>,
}

/// Appends lines and knows the number of the next one.
struct Writer {
    /// The text so far.
    text: String,
    /// The 1-based number of the line the next `line` call writes.
    next: usize,
}

impl Writer {
    /// An empty file.
    const fn new() -> Self {
        Self {
            text: String::new(),
            next: 1,
        }
    }

    /// Write one line at `indent` spaces, and return its number.
    fn line(&mut self, indent: usize, text: &str) -> usize {
        let number = self.next;
        self.text.push_str(&" ".repeat(indent));
        self.text.push_str(text);
        self.text.push('\n');
        self.next = self.next.saturating_add(1);
        number
    }
}

/// Write one seed's file.
fn write(seed: u64, trace: &mut Trace) -> Written {
    let mut rng = Rng::new(seed);
    let mut out = Writer::new();
    let mut planted = Vec::new();
    out.line(0, "use std::time::Duration;");
    let items = rng.between(2, 8);
    trace.record_number("items", items);
    for item in 0..items {
        let depth = rng.below(3);
        let indent = match depth {
            1 => 4,
            2 => 8,
            _ => 0,
        };
        match depth {
            1 => {
                out.line(0, &format!("mod m{item} {{"));
            }
            2 => {
                out.line(0, &format!("mod m{item} {{"));
                out.line(4, &format!("fn host{item}() {{"));
            }
            _ => {}
        }
        let choice = rng.below(10);
        trace.record(&format!("item {item}: depth {depth}, choice {choice}"));
        match choice {
            0..=3 => planted.push(invocation(&mut out, &mut rng, indent, item, true)),
            4..=5 => planted.push(invocation(&mut out, &mut rng, indent, item, false)),
            6 => {
                out.line(indent, &format!("// script! {{ flow c{item}(): 1 }}"));
            }
            7 => {
                out.line(indent, &format!("/* script! {{ flow b{item}(): 1 }} */"));
            }
            8 => {
                out.line(indent, &format!("/// script! {{ flow d{item}(): 1 }}"));
                out.line(
                    indent,
                    &format!("const S{item}: &str = \"script! {{ flow s{item}(): 1 }}\";"),
                );
            }
            _ => {
                out.line(
                    indent,
                    &format!("const LOCAL{item}: u8 = {{ let script = 1; script }};"),
                );
                out.line(
                    indent,
                    &format!("macro_rules! not_script{item} {{ () => {{}} }}"),
                );
            }
        }
        match depth {
            1 => {
                out.line(0, "}");
            }
            2 => {
                out.line(4, "}");
                out.line(0, "}");
            }
            _ => {}
        }
    }
    let broken_at = rng
        .chance(150)
        .then(|| out.line(0, "const BROKEN: &str = \"unterminated;"));
    trace.record(&format!("broken at {broken_at:?}"));
    Written {
        source: out.text,
        planted,
        broken_at,
    }
}

/// Write one real `script!`, accepted or refused, and say where its parts are.
fn invocation(
    out: &mut Writer,
    rng: &mut Rng,
    indent: usize,
    item: u32,
    accepted: bool,
) -> Planted {
    let prefix = if rng.chance(500) { "lgwks_bot::" } else { "" };
    let (open, close) = match rng.below(3) {
        0 => ("(", ");"),
        1 => ("[", "];"),
        _ => ("{", "}"),
    };
    let line = out.line(indent, &format!("{prefix}script! {open}"));
    let inner = indent.saturating_add(4);
    if rng.chance(300) {
        out.line(inner, &format!("/// Flow {item}, documented."));
    }
    let (header, body) = if accepted {
        let header = out.line(inner, &format!("flow f{item}(pause: Duration) -> u32:"));
        let body = out.line(inner.saturating_add(4), "within 20ms:");
        out.line(inner.saturating_add(8), &format!("{item}"));
        (header, body)
    } else {
        let header = out.line(inner, &format!("flow g{item}():"));
        let body = out.line(inner.saturating_add(4), "std::process::exit(1)");
        (header, body)
    };
    out.line(indent, close);
    Planted {
        line,
        header,
        body,
        accepted,
    }
}

/// Hold one seed's reading to the answer its writer decided, recording both.
///
/// # Errors
///
/// The first way the reading differs from the answer, naming the seed and
/// carrying the whole file, so the failure replays from its message alone.
fn check(seed: u64, trace: &mut Trace) -> TestResult {
    let written = write(seed, trace);
    let source = written.source.as_str();
    let decided = match (read_source(source), written.broken_at) {
        (Err(refusal), Some(line)) => {
            trace.record_number("broken refusal line", refusal.line());
            if refusal.line() == line && refusal.message().contains("not Rust tokens") {
                Ok(None)
            } else {
                Err(format!(
                    "seed {seed:#x}: the broken file is refused at {}, `{}`; its stray quote is \
                     on line {line}\n{source}",
                    refusal.line(),
                    refusal.message(),
                ))
            }
        }
        (Err(refusal), None) => Err(format!(
            "seed {seed:#x}: a file of Rust tokens was refused: {refusal}\n{source}"
        )),
        (Ok(found), Some(line)) => Err(format!(
            "seed {seed:#x}: the unterminated string on line {line} was read as {} \
             invocations\n{source}",
            found.len()
        )),
        (Ok(found), None) if found.len() != written.planted.len() => Err(format!(
            "seed {seed:#x}: found {} script!, planted {} (a decoy was found or a real one \
             missed)\n{source}",
            found.len(),
            written.planted.len()
        )),
        (Ok(found), None) => Ok(Some(found)),
    };
    let found = match decided {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(()),
        Err(failure) => {
            lgwks_std::trace::debug!(error = %failure, "check: returning an error to the caller");
            return Err(failure.into());
        }
    };
    for (invocation, planted) in found.iter().zip(&written.planted) {
        if let Some(failure) = differs(seed, invocation, planted, source) {
            lgwks_std::trace::debug!(error = %failure, "check: returning an error to the caller");
            return Err(failure.into());
        }
        trace.record(&format!(
            "found at {}:{} accepted {}",
            invocation.line(),
            invocation.column(),
            invocation.read().is_ok()
        ));
        match invocation.read() {
            Ok(script) => trace.record(&script.to_string()),
            Err(refusal) => trace.record(refusal.message()),
        }
    }
    Ok(())
}

/// How one found invocation differs from the one planted, if it does.
fn differs(seed: u64, invocation: &Invocation, planted: &Planted, source: &str) -> Option<String> {
    if invocation.line() != planted.line {
        return Some(format!(
            "seed {seed:#x}: an invocation planted on line {} was found on line {}\n{source}",
            planted.line,
            invocation.line()
        ));
    }
    match (invocation.read(), planted.accepted) {
        (Ok(script), true) => {
            let header = script.flows().iter().map(|flow| flow.shape().line()).next();
            let first = script
                .flows()
                .iter()
                .filter_map(|flow| flow.shape().steps().first())
                .map(lgwks_ast::script::StepShape::line)
                .next();
            let map = script.to_string();
            if script.flows().len() != 1
                || (header, first) != (Some(planted.header), Some(planted.body))
            {
                Some(format!(
                    "seed {seed:#x}: {} flows, the first at {header:?} with its first step at \
                     {first:?}; one flow was written at {} and {}\n{source}",
                    script.flows().len(),
                    planted.header,
                    planted.body
                ))
            } else if !map.contains(&format!("  @{}\n", planted.header)) {
                Some(format!(
                    "seed {seed:#x}: the rendered map lost the header's line:\n{map}"
                ))
            } else {
                None
            }
        }
        (Err(refusal), false)
            if refusal.line() == planted.body && refusal.message().contains(REFUSED_SAYS) =>
        {
            None
        }
        (Err(refusal), false) => Some(format!(
            "seed {seed:#x}: refused at line {} with `{}`; the exit is on line {}\n{source}",
            refusal.line(),
            refusal.message(),
            planted.body
        )),
        (Ok(_), false) => Some(format!(
            "seed {seed:#x}: the process exit on line {} was accepted\n{source}",
            planted.body
        )),
        (Err(refusal), true) => Some(format!(
            "seed {seed:#x}: the flow on line {} was refused: {refusal}\n{source}",
            planted.header
        )),
    }
}

#[test]
fn sim_every_seed_finds_exactly_what_it_planted() -> TestResult {
    let mut accepted = 0_usize;
    let mut refused = 0_usize;
    let mut broken = 0_usize;
    for seed in 0..SEEDS {
        let mut trace = Trace::new();
        check(seed, &mut trace)?;
        let written = write(seed, &mut Trace::new());
        broken = broken.saturating_add(usize::from(written.broken_at.is_some()));
        for planted in &written.planted {
            if planted.accepted {
                accepted = accepted.saturating_add(1);
            } else {
                refused = refused.saturating_add(1);
            }
        }
    }
    assert!(
        accepted > 1_000 && refused > 500 && broken > 100,
        "the sweep reached every arm: {accepted} accepted, {refused} refused, {broken} broken"
    );
    Ok(())
}

#[test]
fn sim_the_same_seed_replays_to_the_same_trace() -> TestResult {
    for seed in [0, 1, 0x5eed, SEEDS.saturating_sub(1)] {
        let mut first = Trace::new();
        check(seed, &mut first)?;
        let mut second = Trace::new();
        check(seed, &mut second)?;
        assert_eq!(
            first.hash(),
            second.hash(),
            "seed {seed:#x} replays to its own trace"
        );
    }
    Ok(())
}

#[test]
fn sim_distinct_seeds_diverge() -> TestResult {
    let mut hashes = BTreeSet::new();
    for seed in 0..100 {
        let mut trace = Trace::new();
        check(seed, &mut trace)?;
        hashes.insert(trace.hash());
    }
    assert!(
        hashes.len() > 90,
        "100 seeds wrote {} distinct traces",
        hashes.len()
    );
    Ok(())
}
