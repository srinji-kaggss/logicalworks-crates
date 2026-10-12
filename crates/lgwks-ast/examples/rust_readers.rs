//! What reading Rust costs through `lgwks_ast`: the checked parse alone, and the
//! zero-gate scan that runs the five detectors over it (#387, #408).
//!
//! ```sh
//! git ls-files '*.rs' | cargo run --locked --release -p lgwks_ast --features scan \
//!     --example rust_readers -- [both|tree-sitter|scan] [rounds]
//! ```
//!
//! Paths arrive one per line on standard input, so the corpus is whatever the
//! caller names: this repository's own tracked files, for the records in
//! `docs/adr/`. Each file is read once and read `rounds` times by each selected
//! reader (default 5); every read is one latency sample, in microseconds.
//!
//! `tree-sitter` is `lgwks_ast::try_parse`, the checked boundary structural
//! inspection uses: byte ceiling, deadline, node budget and depth bound, so its
//! cost includes the validation walk. `scan` is `lgwks_ast::scan::scan_source`,
//! which is that parse and then the five detectors. A refusal is classed by
//! cause: a syntax refusal is a gap in the grammar, while a node, depth, byte or
//! time budget is the boundary's own ceiling and says nothing about the grammar.
//! One reader per process (`tree-sitter` or `scan`) makes a peak RSS read around
//! the process (`/usr/bin/time -l`) that reader's own. The per-path listing of
//! refused files is capped at [`LISTED`] lines and says how many it left out.
//!
//! `nest <reader> <depth>` is the hostile row: one generated function whose body
//! is a balanced parenthesised expression `depth` levels deep, read once by one
//! reader on the main thread's stack. The caller runs one process per depth and
//! reads the exit status, so a crash would be a recorded verdict rather than a
//! lost run; the checked parse answers a typed depth refusal instead.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{BufRead, Write};
use std::time::Instant;

use lgwks_ast::scan::{ScanError, scan_source};
use lgwks_ast::{Language, ParseError, try_parse};

#[path = "support/measure.rs"]
mod measure;
use measure::spread;

/// Reads of each file per reader when the command line names no count.
const DEFAULT_ROUNDS: usize = 5;

/// Paths printed per listing before the rest are only counted.
const LISTED: usize = 64;

/// One way of reading Rust through `lgwks_ast`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reader {
    /// `lgwks_ast::try_parse` with the Rust grammar, inspection's reader.
    TreeSitter,
    /// `lgwks_ast::scan::scan_source`: the same parse, then the five detectors.
    Scan,
}

impl Reader {
    /// The name the report and the command line use.
    const fn name(self) -> &'static str {
        match self {
            Self::TreeSitter => "tree-sitter",
            Self::Scan => "scan",
        }
    }

    /// Read `source`: `Ok` when this reader accepts it, else its refusal.
    fn read(self, source: &str) -> Result<(), String> {
        match self {
            Self::TreeSitter => try_parse(source, Language::Rust)
                .map(drop)
                .map_err(|error| refusal_class(&error)),
            Self::Scan => scan_source(source, "corpus.rs")
                .map(drop)
                .map_err(|error| match error {
                    ScanError::Refused { ref refusal, .. } => refusal_class(refusal),
                    other => format!("syntax: {other}"),
                }),
        }
    }
}

/// A parser refusal named by its cause, so a budget is never read as a grammar
/// gap.
fn refusal_class(error: &ParseError) -> String {
    let class = match *error {
        ParseError::InvalidSyntax { .. } => "syntax",
        ParseError::AstTooLarge { .. } => "node budget",
        ParseError::AstTooDeep { .. } => "depth bound",
        ParseError::SourceTooLarge { .. } => "byte ceiling",
        ParseError::TimedOut { .. } => "deadline",
        _ => "other",
    };
    format!("{class}: {error}")
}

/// What one reader made of the corpus.
#[derive(Default)]
struct Tally {
    /// Every read's latency, in microseconds.
    samples: Vec<u128>,
    /// Each file's first-round answer, by path.
    verdicts: BTreeMap<String, Result<(), String>>,
}

impl Tally {
    /// Files this reader accepted.
    fn accepted(&self) -> usize {
        self.verdicts
            .values()
            .filter(|verdict| verdict.is_ok())
            .count()
    }
}

/// Which readers this run times.
fn selection(argument: Option<&str>) -> Result<Vec<Reader>, String> {
    match argument {
        None | Some("both") => Ok(vec![Reader::TreeSitter, Reader::Scan]),
        Some("tree-sitter") => Ok(vec![Reader::TreeSitter]),
        Some("scan") => Ok(vec![Reader::Scan]),
        Some(other) => Err(format!(
            "unknown reader {other:?}: name both, tree-sitter or scan"
        )),
    }
}

/// Read one generated `depth`-deep expression with `reader` and print the
/// verdict, or die trying: see the module documentation.
fn nest(reader: Option<&str>, depth: Option<&str>) -> Result<(), Box<dyn Error>> {
    let readers = selection(reader)?;
    let &[reader] = readers.as_slice() else {
        let refusal = Err("nest reads with one reader: name tree-sitter or scan".into());
        tracing::debug!(error = ?refusal.as_ref().err(), "nest: returning an error to the caller");
        return refusal;
    };
    let depth: usize = depth.ok_or("nest needs a depth")?.parse()?;
    let source = format!(
        "fn f() {{ let _ = {}1{}; }}\n",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    let started = Instant::now();
    let verdict = reader.read(&source);
    let elapsed = started.elapsed().as_micros();
    let answer = match verdict {
        Ok(()) => "accepted".to_owned(),
        Err(why) => format!("refused ({})", why.replace('\n', " ")),
    };
    writeln!(
        std::io::stdout().lock(),
        "nest reader={} depth={depth} us={elapsed} {answer}",
        reader.name()
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("nest") {
        return nest(
            arguments.get(1).map(String::as_str),
            arguments.get(2).map(String::as_str),
        );
    }
    let readers = selection(arguments.first().map(String::as_str))?;
    let rounds = match arguments.get(1) {
        Some(count) => count.parse::<usize>()?.max(1),
        None => DEFAULT_ROUNDS,
    };

    let mut tallies: BTreeMap<Reader, Tally> = BTreeMap::new();
    let mut unreadable: Vec<String> = Vec::new();
    let mut files = 0_usize;
    let mut bytes = 0_usize;
    for line in std::io::stdin().lock().lines() {
        let path = line?;
        if path.is_empty() {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            unreadable.push(path);
            continue;
        };
        files = files.saturating_add(1);
        bytes = bytes.saturating_add(source.len());
        for &reader in &readers {
            let tally = tallies.entry(reader).or_default();
            for round in 0..rounds {
                let started = Instant::now();
                let verdict = reader.read(&source);
                tally.samples.push(started.elapsed().as_micros());
                if round == 0 {
                    tally.verdicts.insert(path.clone(), verdict);
                }
            }
        }
    }

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "files={files} bytes={bytes} unreadable={} rounds={rounds}",
        unreadable.len()
    )?;
    let per_round = u128::try_from(rounds)?;
    for (&reader, tally) in &mut tallies {
        let parses = tally.samples.len();
        let total: u128 = tally.samples.iter().sum();
        let (p50, p99) = spread(&mut tally.samples)?;
        let worst = tally
            .samples
            .last()
            .copied()
            .ok_or("a reader that ran no read has no slowest one")?;
        let corpus_us = total
            .checked_div(per_round)
            .ok_or("a run of zero rounds measured nothing")?;
        writeln!(
            out,
            "reader={} accepted={} refused={} reads={parses} p50_us={p50} p99_us={p99} max_us={worst} corpus_ms_per_round={}",
            reader.name(),
            tally.accepted(),
            tally.verdicts.len().saturating_sub(tally.accepted()),
            corpus_us.div_euclid(1_000),
        )?;
        let refused: Vec<(&String, &String)> = tally
            .verdicts
            .iter()
            .filter_map(|(path, verdict)| verdict.as_ref().err().map(|why| (path, why)))
            .collect();
        for &(path, why) in refused.iter().take(LISTED) {
            writeln!(
                out,
                "{}_refused\t{path}\t{}",
                reader.name(),
                why.replace('\n', " ")
            )?;
        }
        if refused.len() > LISTED {
            writeln!(
                out,
                "{}_refused\t({} more not listed)",
                reader.name(),
                refused.len().saturating_sub(LISTED)
            )?;
        }
    }
    for path in unreadable.iter().take(LISTED) {
        writeln!(out, "unreadable\t{path}")?;
    }
    Ok(())
}
