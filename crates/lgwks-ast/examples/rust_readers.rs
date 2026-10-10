//! Rust is read twice: by `syn` (the `lgwks-deps scan` detectors) and by
//! tree-sitter (`lgwks_ast`, behind `lgwks_bot::inspect`). This reads the same
//! files with both and reports where they disagree and what each costs (#387).
//!
//! ```sh
//! git ls-files '*.rs' | cargo run --locked --release -p lgwks_ast \
//!     --example rust_readers -- [both|syn|tree-sitter] [rounds]
//! ```
//!
//! Paths arrive one per line on standard input, so the corpus is whatever the
//! caller names: this repository's own tracked files, for the ADR in
//! `docs/adr/0001-one-rust-reader.md`. Each file is read once and parsed
//! `rounds` times by each selected reader (default 5); every parse is one
//! latency sample, in microseconds.
//!
//! `both` prints the fidelity table: files both readers accept, files only one
//! accepts and why the other refused, and files both refuse. tree-sitter's
//! answer is `lgwks_ast::try_parse`, the checked boundary inspection uses, so a
//! refusal is classed by cause: a syntax refusal is a disagreement about the
//! grammar, while a node, depth, byte or time budget is the boundary's own
//! ceiling and says nothing about the grammar. `syn` or `tree-sitter` alone runs
//! one reader, so a peak RSS read around the process (`/usr/bin/time -l`) is
//! that reader's own; `syn-released` is `syn` releasing the span source map
//! `proc-macro2`'s `span-locations` keeps per thread, after every parse, which
//! the scan does not do. The per-path listings are capped at [`LISTED`] lines each
//! and say how many they left out.
//!
//! `nest <reader> <depth>` is the hostile row: one generated function whose
//! body is a balanced parenthesised expression `depth` levels deep, read once
//! by one reader on the main thread's stack. A reader that recurses without a
//! bound overflows that stack and the process dies by signal, which is why it
//! is a mode of its own: the caller runs one process per depth and reads the
//! exit status, so a crash is a recorded verdict rather than a lost run.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{BufRead, Write};
use std::time::Instant;

use lgwks_ast::{Language, ParseError, try_parse};
use lgwks_deps::syn;

#[path = "support/measure.rs"]
mod measure;
use measure::spread;

/// Parses of each file per reader when the command line names no count.
const DEFAULT_ROUNDS: usize = 5;

/// Paths printed per listing before the rest are only counted.
const LISTED: usize = 64;

/// One of the two Rust readers.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reader {
    /// `syn::parse_file`, the scan detectors' reader.
    Syn,
    /// `lgwks_ast::try_parse` with the Rust grammar, inspection's reader.
    TreeSitter,
    /// `syn::parse_file` with the thread's span source map released after
    /// each parse: what the scan's peak RSS would be if it released it.
    SynReleased,
}

impl Reader {
    /// The name the report and the command line use.
    const fn name(self) -> &'static str {
        match self {
            Self::Syn => "syn",
            Self::TreeSitter => "tree-sitter",
            Self::SynReleased => "syn-released",
        }
    }

    /// Read `source`: `Ok` when this reader accepts it, else its refusal.
    fn read(self, source: &str) -> Result<(), String> {
        match self {
            Self::Syn => syn::parse_file(source)
                .map(drop)
                .map_err(|error| format!("syntax: {error}")),
            Self::TreeSitter => try_parse(source, Language::Rust)
                .map(drop)
                .map_err(|error| refusal_class(&error)),
            Self::SynReleased => {
                let verdict = Self::Syn.read(source);
                // `span-locations` keeps every parsed source in a per-thread
                // map until this call; an example is never a proc macro, the
                // one context in which it refuses.
                lgwks_deps::proc_macro2::extra::invalidate_current_thread_spans();
                verdict
            }
        }
    }
}

/// A tree-sitter refusal named by its cause, so a budget is never read as a
/// grammar disagreement.
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
    /// Every parse's latency, in microseconds.
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
        None | Some("both") => Ok(vec![Reader::Syn, Reader::TreeSitter]),
        Some("syn") => Ok(vec![Reader::Syn]),
        Some("tree-sitter") => Ok(vec![Reader::TreeSitter]),
        Some("syn-released") => Ok(vec![Reader::SynReleased]),
        Some(other) => Err(format!(
            "unknown reader {other:?}: name both, syn, syn-released or tree-sitter"
        )),
    }
}

/// Read one generated `depth`-deep expression with `reader` and print the
/// verdict, or die trying: see the module documentation.
fn nest(reader: Option<&str>, depth: Option<&str>) -> Result<(), Box<dyn Error>> {
    let readers = selection(reader)?;
    let &[reader] = readers.as_slice() else {
        let refusal = Err("nest reads with one reader: name syn or tree-sitter".into());
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
            .ok_or("a reader that ran no parse has no slowest one")?;
        let corpus_us = total
            .checked_div(per_round)
            .ok_or("a run of zero rounds measured nothing")?;
        writeln!(
            out,
            "reader={} accepted={} refused={} parses={parses} p50_us={p50} p99_us={p99} max_us={worst} corpus_ms_per_round={}",
            reader.name(),
            tally.accepted(),
            tally.verdicts.len().saturating_sub(tally.accepted()),
            corpus_us.div_euclid(1_000),
        )?;
    }

    let (Some(by_syn), Some(by_tree)) =
        (tallies.get(&Reader::Syn), tallies.get(&Reader::TreeSitter))
    else {
        return Ok(());
    };
    let mut listings: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut both_accept = 0_usize;
    for (path, syn_verdict) in &by_syn.verdicts {
        let Some(tree_verdict) = by_tree.verdicts.get(path) else {
            continue;
        };
        match (syn_verdict.as_ref(), tree_verdict.as_ref()) {
            (Ok(&()), Ok(&())) => both_accept = both_accept.saturating_add(1),
            (Ok(&()), Err(why)) => listings
                .entry("only_syn")
                .or_default()
                .push(format!("{path}\ttree-sitter {why}")),
            (Err(why), Ok(&())) => listings
                .entry("only_tree_sitter")
                .or_default()
                .push(format!("{path}\tsyn {why}")),
            (Err(syn_why), Err(tree_why)) => listings
                .entry("both_refuse")
                .or_default()
                .push(format!("{path}\tsyn {syn_why}\ttree-sitter {tree_why}")),
        }
    }
    let count = |name: &str| listings.get(name).map_or(0, Vec::len);
    writeln!(
        out,
        "agreement both_accept={both_accept} only_syn={} only_tree_sitter={} both_refuse={}",
        count("only_syn"),
        count("only_tree_sitter"),
        count("both_refuse"),
    )?;
    for (name, paths) in &listings {
        for entry in paths.iter().take(LISTED) {
            writeln!(out, "{name}\t{}", entry.replace('\n', " "))?;
        }
        if paths.len() > LISTED {
            writeln!(
                out,
                "{name}\t({} more not listed)",
                paths.len().saturating_sub(LISTED)
            )?;
        }
    }
    for path in unreadable.iter().take(LISTED) {
        writeln!(out, "unreadable\t{path}")?;
    }
    Ok(())
}
