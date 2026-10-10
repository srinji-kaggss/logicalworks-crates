//! Per-grammar parse throughput, peak RSS, and the cost of the checked walk (#277).
//!
//! One run answers, for every grammar this build compiles: how many MB/s the
//! parser reaches on that grammar's own source and on three adversarial shapes
//! at the crate's byte ceiling, what share of a checked parse is the validation
//! walk rather than the parser, and what one parse costs resident memory.
//!
//! ```sh
//! cargo run --release -p lgwks_ast --features full --example parse_budget
//! ```
//!
//! Four shapes per grammar, because they fail differently:
//!
//! - `representative` — the grammar's own valid source tiled to the byte
//!   ceiling. This is the throughput number a tool sees on real code.
//! - `nested` — a delimiter pair (or an indentation run, where the grammar has
//!   no delimiter) nested as deeply as the byte ceiling allows. This is the
//!   adversarial input for [`MAX_AST_DEPTH`](lgwks_ast::MAX_AST_DEPTH).
//! - `longline` — the representative source with its newlines removed: one line
//!   of megabytes.
//! - `unbalanced` — the nested openers with no closers, which forces recovery at
//!   the deepest level.
//!
//! `nested` and `unbalanced` also report the **before/after** of the depth
//! ceiling: the walk the crate ran before this issue (a node cap only, which is
//! what the public `inspect_ast` still is) against the checked parse's walk
//! (node and depth caps), on one tree, in one process. The `checked_over_parse`
//! column is the ratio the checked parse costs to the bare `tree-sitter` parse
//! on the same source; the `walk_share` column is the pre-issue walk's share of
//! that pair.
//!
//! The language table below is not the crate's `FIXTURES` table, which is
//! `#[cfg(test)]` and unreachable from an example. It is deliberately separate:
//! this one describes *shapes to measure*, and a shape is allowed not to be
//! valid source. The `outcome` column reports which it turned out to be, so a
//! shape that does not parse is visible rather than folded into a number.
//!
//! Concurrency is a second mode, because a per-parse bound does not bound a
//! fleet of them. `--tier` repeats:
//!
//! ```sh
//! cargo run --release -p lgwks_ast --features full --example parse_budget -- \
//!     --grammar rust --tier 100 --tier 1000 --tier 10000 --tier 100000 \
//!     --tier-bytes 65536 --threads 8
//! ```
//!
//! Fan-out is bounded by construction rather than by a queue: the level's index
//! range is split into one contiguous slice per worker at admission, each worker
//! owns its slice end to end, and `thread::scope` joins all of them. There is no
//! shared queue to overflow and no lock to hold.
//!
//! Peak RSS is measured by `scripts/measure-ast-budget.sh`, one process per
//! grammar, because one process running 28 grammars can only report one number.
//! This example prints whatever `VmHWM` the host makes readable and says
//! `None` when it cannot, which is every macOS build: the peak there comes from
//! `/usr/bin/time -l` around the process.

use std::collections::BTreeMap;
use std::io::Write;

use std::time::Instant;

use lgwks_ast::{AstGrep, Language, MAX_AST_NODES, MAX_SOURCE_BYTES, inspect_ast, try_parse};

#[path = "support/measure.rs"]
mod measure;
use measure::spread;

/// What one grammar is measured on.
///
/// `valid` is source the grammar accepts, used to tile the representative shape.
/// `nest_open` and `nest_close` are the nesting fragments: a delimiter pair
/// where the grammar has one, an indentation run where it nests by indentation.
struct Shape {
    /// The language's stable name, the key this row is selected by.
    name: &'static str,
    /// Source the grammar accepts without a recovery node.
    valid: &'static str,
    /// One level of nesting.
    nest_open: &'static str,
    /// One closing level; empty where the grammar closes by dedent, or by
    /// nothing at all.
    nest_close: &'static str,
}

/// Expand one `name, valid, nest_open, nest_close` row per grammar.
///
/// The same shape as the crate's own `define_languages!`: a row cannot be added
/// by halves, and the table is written as data rather than as twenty-eight
/// copies of the same four field names.
macro_rules! shapes {
    ($(($name:literal, $valid:literal, $open:literal, $close:literal),)+) => {
        &[$(Shape {
            name: $name,
            valid: $valid,
            nest_open: $open,
            nest_close: $close,
        }),+]
    };
}

/// One row per grammar `Language::ALL` declares under `--features full`.
const SHAPES: &[Shape] = shapes! {
    ("bash", "echo hello\n", "{", "}"),
    ("c", "int main(void) { return 0; }\n", "{", "}"),
    ("cpp", "int main() { return 0; }\n", "{", "}"),
    ("csharp", "class A { }\n", "{", "}"),
    ("css", "a { color: red; }\n", "{", "}"),
    ("dart", "void main() {}\n", "{", "}"),
    ("elixir", "defmodule A do\n  def f, do: 1\nend\n", "defmodule A do\n", "end\n"),
    ("go", "package main\n\nfunc main() {}\n", "{", "}"),
    ("haskell", "main = putStrLn \"hi\"\n", "(", ")"),
    ("hcl", "resource \"a\" \"b\" {\n}\n", "{", "}"),
    ("html", "<!DOCTYPE html>\n<html><body><p>hi</p></body></html>\n", "<div>", "</div>"),
    ("java", "class A {}\n", "{", "}"),
    ("javascript", "const a = 1;\n", "(", ")"),
    ("json", "{\"a\": 1}\n", "[", "]"),
    ("kotlin", "fun main() {}\n", "{", "}"),
    ("lua", "local a = 1\n", "(", ")"),
    ("markdown", "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n", "- ", ""),
    ("nix", "{ pkgs }: pkgs.hello\n", "{", "}"),
    ("php", "<?php echo \"hi\";\n", "(", ")"),
    ("python", "def f():\n    return 1\n", "(", ")"),
    ("ruby", "def f\n  1\nend\n", "(", ")"),
    ("rust", "fn main() {}\n", "(", ")"),
    ("scala", "object A { def f = 1 }\n", "{", "}"),
    ("solidity", "contract A {}\n", "{", "}"),
    ("swift", "func f() {}\n", "{", "}"),
    ("tsx", "const A = () => 1;\n", "(", ")"),
    ("typescript", "const a: number = 1;\n", "(", ")"),
    ("yaml", "a: 1\n", "  ", ""),
};

/// The four source shapes one grammar is measured on.
const KINDS: [&str; 4] = ["representative", "nested", "longline", "unbalanced"];

/// The default byte ceiling, which is the crate's own.
const DEFAULT_BYTES: usize = MAX_SOURCE_BYTES;

/// The default samples per measurement.
///
/// Three, not one: a single timing of a parse is a sample of the scheduler and
/// the page cache as much as of the grammar, and a published MB/s with no
/// spread behind it is a number no reader can interpret. Three is the least
/// that makes a p99 distinguishable from a p50 at this length; a run that wants
/// a distribution asks for `--rounds`.
const DEFAULT_ROUNDS: usize = 3;

/// The table header `run` writes.
const HEADER: &str = concat!(
    "grammar\tshape\tbytes\t",
    "parse_p50_ns\tparse_p99_ns\t",
    "walk_p50_ns\twalk_p99_ns\t",
    "checked_p50_ns\tchecked_p99_ns\t",
    "parse_mb_s_milli_p50\tparse_mb_s_milli_p99\tchecked_mb_s_milli_p50\tchecked_mb_s_milli_p99\t",
    "walk_share_milli\tchecked_over_parse_milli\t",
    "nodes\tdepth\toutcome"
);

/// The tier header `run` writes.
const TIER_HEADER: &str = concat!(
    "tier\tlevel\tbytes\tworkers\tp50_ns\tp99_ns\tmax_ns\twall_ns\t",
    "throughput_mb_s_milli\trefused\tgrammar"
);

/// `numerator / denominator`, or `None` when there is no rate to report.
///
/// `integer_division` is forbidden workspace-wide, so a division here is the
/// checked one — and its `None` *is* the answer rather than a failure to paper
/// over: a zero denominator is a shape the crate refused before the parser ran,
/// which has no elapsed time behind it. The row prints `-` for such a column and
/// names the refusal in `outcome`, so an absent rate is never read as a rate of
/// zero.
fn ratio(numerator: u128, denominator: u128) -> Option<u128> {
    numerator.checked_div(denominator)
}

/// One report column: the measurement, or `-` for the one this row does not have.
fn column(measurement: Option<u128>) -> String {
    match measurement {
        Some(measurement) => measurement.to_string(),
        None => String::from("-"),
    }
}

/// Megabytes per second, in thousandths, from a byte count and a duration.
///
/// Integer arithmetic throughout: `clippy::float_cmp` and
/// `clippy::integer_division` are both `forbid` here, and a measurement printed
/// as a float is a measurement a reader has to trust. A megabyte is 10^6 bytes
/// and a nanosecond is 10^-9 of a second, so `bytes * 10^6 / nanos` is MB/s
/// scaled by a thousand.
fn megabytes_per_second_milli(bytes: u128, nanos: u128) -> Option<u128> {
    ratio(bytes.saturating_mul(1_000_000), nanos)
}

/// A byte count in the width every column's arithmetic uses.
///
/// A `usize` is at most sixty-four bits on every target Rust supports, so this
/// cannot fail there; the refusal names the case rather than measuring a source
/// of no bytes over a nanosecond count.
fn width_of(bytes: usize) -> Result<u128, String> {
    u128::try_from(bytes)
        .map_err(|error| format!("{bytes} bytes do not fit the report's arithmetic: {error}"))
}

/// `part / whole`, in thousandths, for a share of a total.
fn share_milli(part: u128, whole: u128) -> Option<u128> {
    ratio(part.saturating_mul(1_000), whole)
}

/// This grammar's row in the shape table.
fn shape_of(language: Language) -> Result<&'static Shape, String> {
    SHAPES
        .iter()
        .find(|row| row.name == language.name())
        .ok_or_else(|| {
            format!(
                "no shape row for the compiled grammar `{}`",
                language.name()
            )
        })
}

/// How many whole nesting levels of `shape` fit in `bytes`.
///
/// Both halves of a level are charged, so a closed level is never counted as
/// half a level and the source stays inside the byte budget it was asked for. A
/// shape with no nesting fragment is a table defect rather than zero levels, so
/// it is refused where it is read.
fn levels_for(shape: &Shape, bytes: usize) -> Result<usize, String> {
    let level = shape.nest_open.len().saturating_add(shape.nest_close.len());
    if level == 0 {
        let refusal = Err(format!(
            "the shape for `{}` has no nesting fragment, so it nests nothing",
            shape.name
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "levels_for: returning an error to the caller");
        return refusal;
    }
    // `integer_division` is forbidden, so this is the checked division. The
    // divisor is a nesting fragment's length and cannot be zero — the empty
    // fragment is refused above — so the refusal is the invariant restated.
    bytes.checked_div(level).ok_or(format!(
        "{level} levels do not divide {bytes} bytes; the fragment is empty"
    ))
}

/// `fragment` repeated to at most `bytes`, in whole copies and never past it.
///
/// Whole copies only: cutting the last one in half is how a "representative"
/// measurement ends up measuring a broken file, and the truncated tail's
/// recovery node would be charged to the grammar's throughput.
fn tile(fragment: &str, bytes: usize) -> String {
    let mut source = String::with_capacity(bytes);
    while source.len().saturating_add(fragment.len()) <= bytes {
        source.push_str(fragment);
    }
    source
}

/// `levels` copies of `fragment`.
fn repeat(fragment: &str, levels: usize) -> String {
    let mut source = String::with_capacity(fragment.len().saturating_mul(levels));
    for _ in 0..levels {
        source.push_str(fragment);
    }
    source
}

/// The nesting shape: as many openers as `bytes` allows, then the closers.
///
/// The openers run first so the source is a run of nested opens, which is where
/// a GLR parser does its super-linear work; the closers follow rather than
/// interleave, because interleaving halves the depth for the same byte budget.
fn nested(shape: &Shape, bytes: usize) -> Result<String, String> {
    let levels = levels_for(shape, bytes)?;
    let mut source = repeat(shape.nest_open, levels);
    source.push_str(&repeat(shape.nest_close, levels));
    Ok(source)
}

/// The named form of a checked parse's answer, so a refusal is legible in the
/// table rather than counted as a failure.
fn outcome_of(answer: Result<lgwks_ast::Parsed, lgwks_ast::ParseError>) -> String {
    match answer {
        Ok(_tree) => "accepted".to_owned(),
        Err(lgwks_ast::ParseError::SourceTooLarge { .. }) => "source-too-large".to_owned(),
        Err(lgwks_ast::ParseError::ParserUnavailable { .. }) => "parser-unavailable".to_owned(),
        Err(lgwks_ast::ParseError::InvalidSyntax { .. }) => "invalid-syntax".to_owned(),
        Err(lgwks_ast::ParseError::AstTooLarge { .. }) => "ast-too-large".to_owned(),
        Err(lgwks_ast::ParseError::AstTooDeep { .. }) => "ast-too-deep".to_owned(),
        Err(lgwks_ast::ParseError::ContainerNestingTooDeep { .. }) => {
            "container-nesting-too-deep".to_owned()
        }
        Err(lgwks_ast::ParseError::TimedOut { .. }) => "timed-out".to_owned(),
        Err(other) => format!("unclassified:{other}"),
    }
}

/// The source named `kind`, or a typed refusal naming an unknown one.
fn source_for(
    kind: &str,
    shape: &Shape,
    representative: &str,
    bytes: usize,
) -> Result<String, String> {
    match kind {
        "representative" => Ok(representative.to_owned()),
        "nested" => nested(shape, bytes),
        "longline" => Ok(representative.replace('\n', "")),
        "unbalanced" => Ok(repeat(shape.nest_open, levels_for(shape, bytes)?)),
        other => Err(format!("unknown shape `{other}`")),
    }
}

/// One measured shape of one grammar.
struct Measured {
    /// The grammar's stable name.
    grammar: &'static str,
    /// Which of [`KINDS`] this row measured.
    kind: String,
    /// Bytes handed to the parser.
    ///
    /// A `u128` because every rate this table prints divides by a nanosecond
    /// count, which is a `u128`; converting once here keeps the arithmetic below
    /// single-width instead of widening a `usize` at every rate.
    bytes: u128,
    /// Nanoseconds for the bare parse, p50 and p99 over `rounds`.
    parse_p50: u128,
    /// 99th percentile of the bare parse.
    parse_p99: u128,
    /// Nanoseconds for the pre-issue walk alone — a node cap, no depth cap.
    walk_p50: u128,
    /// 99th percentile of that walk.
    walk_p99: u128,
    /// Nanoseconds for the whole checked parse, p50 and p99.
    checked_p50: u128,
    /// 99th percentile of the checked parse.
    checked_p99: u128,
    /// What the checked parse answered, as the variant's own name.
    outcome: String,
    /// Nodes the walk visited.
    nodes: usize,
    /// Deepest branch the walk reached.
    depth: usize,
}

impl Measured {
    /// The bare parse's median rate, MB/s scaled by a thousand.
    fn parse_mb_s_milli(&self) -> Option<u128> {
        megabytes_per_second_milli(self.bytes, self.parse_p50)
    }

    /// The checked parse's median rate, MB/s scaled by a thousand.
    fn checked_mb_s_milli(&self) -> Option<u128> {
        megabytes_per_second_milli(self.bytes, self.checked_p50)
    }

    /// The pre-issue walk's share of parse-plus-walk, in thousandths.
    ///
    /// Not `walk / checked`: the checked parse's walk carries the depth cap the
    /// pre-issue walk does not, so the two walks are not the same work and a
    /// ratio between them would price a bound rather than measure one.
    fn walk_share_milli(&self) -> Option<u128> {
        share_milli(self.walk_p50, self.parse_p50.saturating_add(self.walk_p50))
    }

    /// What the checked parse costs per unit of bare parse, in thousandths.
    fn checked_over_parse_milli(&self) -> Option<u128> {
        ratio(self.checked_p50.saturating_mul(1_000), self.parse_p50)
    }
}

/// The bare parse ast-grep performs, which is `tree_sitter::Parser::parse` and
/// nothing else, timed on its own so the walk's share is a difference of two
/// measurements rather than an estimate.
fn measure(
    language: Language,
    shape: &'static Shape,
    kind: &str,
    representative: &str,
    bytes: usize,
    rounds: usize,
) -> Result<Measured, String> {
    let source = source_for(kind, shape, representative, bytes)?;
    let mut parse_samples = Vec::with_capacity(rounds);
    let mut checked_samples = Vec::with_capacity(rounds);

    // The checked parse first, because it is the crate's own boundary and it is
    // the only one of the three that may refuse. The raw `AstGrep::try_new` probe
    // below deliberately bypasses every bound -- that is what makes it a
    // measurement of the parser rather than of the crate -- so it has to be the
    // one that runs last, and only when the checked parse admitted the source.
    // For markdown at a container depth past `MAX_MARKDOWN_CONTAINERS_PER_LINE`
    // that ordering is the difference between a row and a dead process: the raw
    // probe reaches a scanner whose serialization buffer overflows, and that is
    // an `abort()`, not a parse that returns.
    let mut outcome = None;
    let mut refused_before_parser = false;
    for _ in 0..rounds {
        let started = Instant::now();
        let checked = try_parse(&source, language);
        checked_samples.push(started.elapsed().as_nanos());
        let arm = outcome_of(checked);
        refused_before_parser = arm == "container-nesting-too-deep";
        outcome.get_or_insert(arm);
    }
    let mut walk_samples = Vec::new();
    if refused_before_parser {
        // No raw probe and no walk: the crate refused the source before the
        // grammar saw it, so there is no parser cost and no tree to walk. The
        // row's `outcome` says so, and the zeros are the absence of a
        // measurement rather than a measurement of zero.
        let outcome = outcome
            .ok_or_else(|| format!("{kind}: no round of `{}` was measured", language.name()))?;
        let bytes = width_of(source.len())?;
        let (checked_p50, checked_p99) = spread(&mut checked_samples)?;
        return Ok(Measured {
            grammar: language.name(),
            kind: kind.to_owned(),
            bytes,
            parse_p50: 0,
            parse_p99: 0,
            walk_p50: 0,
            walk_p99: 0,
            checked_p50,
            checked_p99,
            outcome,
            nodes: 0,
            depth: 0,
        });
    }

    // One tree, walked `rounds` times: the pre-issue walk is the public
    // `inspect_ast` under a node cap and no depth cap, which is exactly what
    // the crate ran before `MAX_AST_DEPTH` existed. Re-parsing per round would
    // charge the walk for the parser's variance.
    let tree = AstGrep::try_new(&source, language.support_lang())
        .map_err(|detail| format!("{} {kind}: {detail}", language.name()))?;
    walk_samples.reserve(rounds);
    let mut observed = inspect_ast(&tree.root(), Some(MAX_AST_NODES));
    for _ in 0..rounds {
        let started = Instant::now();
        observed = inspect_ast(&tree.root(), Some(MAX_AST_NODES));
        walk_samples.push(started.elapsed().as_nanos());
    }

    for _ in 0..rounds {
        let started = Instant::now();
        AstGrep::try_new(&source, language.support_lang())
            .map_err(|detail| format!("{} {kind}: {detail}", language.name()))?;
        parse_samples.push(started.elapsed().as_nanos());
    }
    drop(tree);

    let outcome =
        outcome.ok_or_else(|| format!("{kind}: no round of `{}` was measured", language.name()))?;
    let bytes = width_of(source.len())?;
    let (parse_p50, parse_p99) = spread(&mut parse_samples)?;
    let (walk_p50, walk_p99) = spread(&mut walk_samples)?;
    let (checked_p50, checked_p99) = spread(&mut checked_samples)?;
    Ok(Measured {
        grammar: language.name(),
        kind: kind.to_owned(),
        bytes,
        parse_p50,
        parse_p99,
        walk_p50,
        walk_p99,
        checked_p50,
        checked_p99,
        outcome,
        nodes: observed.nodes,
        depth: observed.max_depth,
    })
}

/// The line one measured row prints.
fn measured_line(row: &Measured) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        row.grammar,
        row.kind,
        row.bytes,
        row.parse_p50,
        row.parse_p99,
        row.walk_p50,
        row.walk_p99,
        row.checked_p50,
        row.checked_p99,
        column(row.parse_mb_s_milli()),
        column(megabytes_per_second_milli(row.bytes, row.parse_p99)),
        column(row.checked_mb_s_milli()),
        column(megabytes_per_second_milli(row.bytes, row.checked_p99)),
        column(row.walk_share_milli()),
        column(row.checked_over_parse_milli()),
        row.nodes,
        row.depth,
        row.outcome,
    )
}

/// Peak resident set size of this process in bytes, where the host publishes it
/// without a new edge.
///
/// Linux publishes `VmHWM` in `/proc/self/status`. macOS publishes it only
/// through `getrusage`, which needs an FFI leaf this crate may not author, so
/// the answer there is `None` and the peak comes from `/usr/bin/time -l` around
/// the process instead (`scripts/measure-ast-budget.sh`).
///
/// `VmHWM` is published in kibibytes (`VmHWM:   12345 kB`), so the figure is
/// scaled to the bytes the column is named for; a line in any other unit is not
/// a reading this function knows how to scale, and answers `None`.
fn peak_rss_bytes() -> Option<u128> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let mut fields = line.strip_prefix("VmHWM:")?.split_whitespace();
        let kibibytes = fields.next()?.parse::<u128>().ok()?;
        match fields.next() {
            Some("kB") => kibibytes.checked_mul(1024),
            _ => None,
        }
    })
}

/// One worker's share of a concurrency tier.
struct TierWorker {
    /// How many parses this worker was given.
    assigned: u64,
    /// Per-parse nanoseconds, in the order the worker took them.
    samples: Vec<u128>,
    /// How many of the answers were typed refusals.
    refused: u64,
}

/// What one concurrency tier reports.
struct TierRow {
    /// Parses admitted.
    level: u64,
    /// Bytes per parse.
    ///
    /// A `u128` for the same reason as [`Measured::bytes`]: the throughput
    /// column divides bytes by a nanosecond count, and widening here keeps the
    /// arithmetic below single-width.
    bytes: u128,
    /// Workers admitted.
    workers: usize,
    /// Median parse latency.
    p50: u128,
    /// 99th-percentile parse latency.
    p99: u128,
    /// The slowest parse observed.
    max: u128,
    /// Parses answered with a typed refusal rather than a tree.
    refused: u64,
    /// Whole-run wall time.
    wall_ns: u128,
    /// The grammar this tier measured.
    grammar: &'static str,
}

/// Run `level` checked parses of `source` across at most `threads` workers.
///
/// The index range is split at admission, so a worker owns its slice outright:
/// no queue, no lock, and a worker's retained state is its own slice. Every
/// worker is joined by the scope, so no handle outlives the run.
fn measure_tier(
    language: Language,
    representative: &str,
    level: u64,
    bytes: usize,
    threads: usize,
) -> Result<TierRow, String> {
    let workers = threads.max(1);
    // The work count is at most sixty-four bits wide on every target, so this
    // conversion refuses rather than assuming a worker count it cannot hold.
    let workers_u64 = u64::try_from(workers)
        .map_err(|error| format!("{workers} workers do not fit the tier's arithmetic: {error}"))?;
    // `integer_division` is forbidden, so the split of the level across the
    // workers is the checked one. `workers` is at least one by construction, so
    // the refusal below is unreachable and states the invariant it rests on.
    let share = level
        .checked_div(workers_u64)
        .ok_or("a tier with no workers cannot be split")?;
    let tiled = tile(representative, bytes);
    let sized = non_empty_tier(&tiled, bytes);
    sized?;
    let source = std::sync::Arc::new(tiled);
    let mut reports: Vec<TierWorker> = Vec::with_capacity(workers);
    let started = Instant::now();
    let admitted = std::thread::scope(|scope| -> Result<(), String> {
        let mut handles = Vec::with_capacity(workers);
        for worker in 0..workers {
            let owned = std::sync::Arc::clone(&source);
            let at = u64::try_from(worker)
                .map_err(|error| format!("worker {worker} does not fit its own index: {error}"))?;
            let first = share.saturating_mul(at);
            let count = if worker.saturating_add(1) == workers {
                level.saturating_sub(first)
            } else {
                share
            };
            let handle = std::thread::Builder::new()
                .name(format!("ast-tier-{worker}"))
                .spawn_scoped(scope, move || -> Result<TierWorker, String> {
                    let room = usize::try_from(count).map_err(|error| {
                        format!("{count} parses do not fit this target's address space: {error}")
                    })?;
                    let mut samples = Vec::with_capacity(room);
                    let mut refused = 0_u64;
                    for _ in 0..count {
                        let at = Instant::now();
                        let answer = try_parse(&owned, language);
                        samples.push(at.elapsed().as_nanos());
                        if answer.is_err() {
                            refused = refused.saturating_add(1);
                        }
                    }
                    Ok(TierWorker {
                        assigned: count,
                        samples,
                        refused,
                    })
                })
                .map_err(|error| format!("a tier worker did not start: {error}"))?;
            handles.push(handle);
        }
        for handle in handles {
            match handle.join() {
                Ok(Ok(report)) => reports.push(report),
                // A worker's own refusal names the shortfall it hit rather than
                // reporting a tier row over fewer parses than it admitted.
                Ok(Err(shortfall)) => {
                    let refusal = Err(shortfall);
                    tracing::debug!(error = ?refusal.as_ref().err(), "tier: returning a worker's refusal to the caller");
                    return refusal;
                }
                // A panicking worker reports zero, and the count check below
                // turns that into a refusal naming the shortfall rather than a
                // tier that silently measured fewer parses than it admitted.
                Err(_) => reports.push(TierWorker {
                    assigned: 0,
                    samples: Vec::new(),
                    refused: 0,
                }),
            }
        }
        Ok(())
    });
    admitted?;
    let wall_ns = started.elapsed().as_nanos();

    let assigned = reports
        .iter()
        .fold(0_u64, |total, row| total.saturating_add(row.assigned));
    let complete = all_admitted(assigned, level);
    complete?;
    let refused = reports
        .iter()
        .fold(0_u64, |total, row| total.saturating_add(row.refused));
    let mut samples: Vec<u128> = reports.iter().flat_map(|row| row.samples.clone()).collect();
    let (p50, p99) = spread(&mut samples)?;
    // The slowest parse observed is read off the sorted list rather than
    // defaulted: a tier whose workers admitted nothing is already refused by
    // `all_admitted`, so the list is never empty here.
    let max = samples
        .last()
        .copied()
        .ok_or("every admitted parse reported no elapsed time")?;
    let bytes = width_of(source.len())?;
    Ok(TierRow {
        level: assigned,
        bytes,
        workers,
        p50,
        p99,
        max,
        refused,
        wall_ns,
        grammar: language.name(),
    })
}

/// Refuse a tier source that holds nothing.
///
/// `tile` takes whole copies only, so a fragment longer than the tier size tiles
/// to the empty string, and a tier of empty parses reports throughput nobody
/// measured.
fn non_empty_tier(source: &str, bytes: usize) -> Result<(), String> {
    if source.is_empty() {
        Err(format!(
            "no whole copy of the fragment fits in {bytes} bytes, so the tier would parse nothing"
        ))
    } else {
        Ok(())
    }
}

/// Refuse a tier whose workers admitted fewer parses than the level asked for.
///
/// A worker that panicked reports zero admitted, so a short count is the one
/// place a lost worker becomes visible; a tier row over fewer parses than its
/// level names would be a concurrency number nobody ran.
fn all_admitted(assigned: u64, level: u64) -> Result<(), String> {
    if assigned == level {
        Ok(())
    } else {
        Err(format!("{assigned} of {level} parses were admitted"))
    }
}

/// The line one tier row prints.
fn tier_line(row: &TierRow) -> String {
    // The tier's throughput is the bytes it parsed over the wall time: the level
    // is a `u64` and the bytes a `u128`, so the widening is total and needs no
    // stand-in for a conversion.
    let parsed = u128::from(row.level).saturating_mul(row.bytes);
    format!(
        "tier\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        row.level,
        row.bytes,
        row.workers,
        row.p50,
        row.p99,
        row.max,
        row.wall_ns,
        column(megabytes_per_second_milli(parsed, row.wall_ns)),
        row.refused,
        row.grammar,
    )
}

/// What the argument vector asked for.
struct Options {
    /// One grammar's stable name, or `None` for every compiled grammar.
    grammar: Option<String>,
    /// One shape name, or `None` for all of them.
    kind: Option<String>,
    /// Bytes per representative and long-line source.
    bytes: usize,
    /// Bytes per adversarial source: `nested` and `unbalanced` only.
    shape_bytes: usize,
    /// Samples per measurement.
    rounds: usize,
    /// Concurrency tiers to run; empty skips the tier mode.
    tiers: Vec<u64>,
    /// Bytes per parse inside a tier.
    tier_bytes: usize,
    /// Workers a tier may admit.
    threads: usize,
    /// Whether to print the grammar names and stop.
    list: bool,
}

/// Parse the argument vector, or report why it is wrong.
fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        grammar: None,
        kind: None,
        bytes: DEFAULT_BYTES,
        shape_bytes: DEFAULT_BYTES,
        rounds: DEFAULT_ROUNDS,
        tiers: Vec::new(),
        tier_bytes: 64 * 1024,
        threads: 8,
        list: false,
    };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let applied = apply_flag(&mut options, flag, &mut rest);
        applied?;
    }
    validated(options)
}

/// Apply one flag, taking its value from `rest` when it has one.
///
/// Every arm answers a `Result` rather than returning early, so the one caller
/// propagates exactly one refusal per flag and a flag with no value, an
/// unparseable value and an unknown flag are three distinct messages.
fn apply_flag<'arg>(
    options: &mut Options,
    flag: &str,
    rest: &mut impl Iterator<Item = &'arg String>,
) -> Result<(), String> {
    let mut value = || rest.next().ok_or(format!("{flag} needs a value"));
    match flag {
        "--grammar" => value().map(|raw| options.grammar = Some(raw.clone())),
        "--shape" => value().map(|raw| options.kind = Some(raw.clone())),
        "--bytes" => value()
            .and_then(|raw| number(flag, raw))
            .map(|bytes| options.bytes = bytes),
        "--shape-bytes" => value()
            .and_then(|raw| number(flag, raw))
            .map(|bytes| options.shape_bytes = bytes),
        "--rounds" => value()
            .and_then(|raw| number::<usize>(flag, raw))
            .map(|rounds| options.rounds = rounds.max(1)),
        "--tier" => value()
            .and_then(|raw| number(flag, raw))
            .map(|level| options.tiers.push(level)),
        "--tier-bytes" => value()
            .and_then(|raw| number(flag, raw))
            .map(|bytes| options.tier_bytes = bytes),
        "--threads" => value()
            .and_then(|raw| number::<usize>(flag, raw))
            .map(|threads| options.threads = threads.max(1)),
        "--list-grammars" => {
            options.list = true;
            Ok(())
        }
        other => Err(format!("unknown argument `{other}`")),
    }
}

/// Parse one flag's numeric value, naming the flag in the refusal.
fn number<T>(flag: &str, raw: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse().map_err(|error| format!("{flag}: {error}"))
}

/// Refuse a byte budget of zero, and a concurrency level of zero.
///
/// A zero-byte source measures nothing and a tier of zero parses admits no
/// worker, so both would print a row of throughput over no input; the tier row's
/// percentiles are then percentiles of nothing.
fn validated(options: Options) -> Result<Options, String> {
    if options.bytes == 0 {
        Err("--bytes must be at least one".to_owned())
    } else if options.shape_bytes == 0 {
        Err("--shape-bytes must be at least one".to_owned())
    } else if options.tiers.contains(&0) {
        Err("--tier must be at least one".to_owned())
    } else {
        Ok(options)
    }
}

/// Report the measurement, or the refusal that stopped it.
fn run(options: &Options) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if options.list {
        for &language in Language::ALL {
            emit(&mut out, language.name())?;
        }
        return Ok(());
    }
    let kinds: Vec<String> = options.kind.as_deref().map_or_else(
        || KINDS.iter().map(|one| (*one).to_owned()).collect(),
        |one| vec![one.to_owned()],
    );
    let mut tally = Tally {
        rows: 0,
        grammars: 0,
        outcomes: BTreeMap::new(),
    };
    emit(
        &mut out,
        &format!("peak_rss_bytes\t{}", column(peak_rss_bytes())),
    )?;
    if !options.tiers.is_empty() {
        emit(&mut out, TIER_HEADER)?;
    }
    for &language in Language::ALL {
        let wanted = options.grammar.as_deref();
        if wanted.is_some_and(|one| one != language.name()) {
            continue;
        }
        let measured = measure_grammar(&mut out, options, language, &kinds, &mut tally);
        measured?;
    }
    finish(&mut out, options, &tally)
}

/// What a run has measured so far, across grammars.
struct Tally {
    /// Shape rows printed; the header goes before the first.
    rows: usize,
    /// Grammars selected and measured.
    grammars: usize,
    /// How many rows ended in each outcome.
    outcomes: BTreeMap<String, u64>,
}

/// Measure every selected shape and tier of one grammar.
fn measure_grammar(
    out: &mut impl Write,
    options: &Options,
    language: Language,
    kinds: &[String],
    tally: &mut Tally,
) -> Result<(), String> {
    tally.grammars = tally.grammars.saturating_add(1);
    let shape = shape_of(language)?;
    let representative = tile(shape.valid, options.bytes);
    for kind in kinds {
        let row = measure(
            language,
            shape,
            kind,
            &representative,
            options.bytes_for(kind),
            options.rounds,
        )?;
        if tally.rows == 0 {
            emit(out, HEADER)?;
        }
        emit(out, &measured_line(&row))?;
        tally.rows = tally.rows.saturating_add(1);
        let count = tally.outcomes.entry(row.outcome.clone()).or_insert(0);
        *count = count.saturating_add(1);
    }
    for &level in &options.tiers {
        // The grammar's own fragment, not the 2 MiB representative: tiling a
        // source longer than the tier size fits no whole copy, and the tiers once
        // measured parses of the empty string that way.
        let tier = measure_tier(
            language,
            shape.valid,
            level,
            options.tier_bytes,
            options.threads,
        )?;
        emit(out, &tier_line(&tier))?;
    }
    Ok(())
}

/// Print the outcome counts and the closing RSS, or refuse a run that measured
/// no grammar at all.
///
/// Counted grammars, not rows: an unknown `--grammar` next to a `--tier`
/// measured nothing at all, and a header with no data under it reads as a
/// result rather than as a name this build does not compile.
fn finish(out: &mut impl Write, options: &Options, tally: &Tally) -> Result<(), String> {
    if tally.grammars == 0 {
        Err(match options.grammar.as_deref() {
            Some(wanted) => format!("`{wanted}` is not a compiled grammar"),
            None => "no grammar compiled; run with --features full".to_owned(),
        })
    } else {
        for (outcome, count) in &tally.outcomes {
            emit(out, &format!("outcomes\t{outcome}\t{count}"))?;
        }
        emit(
            out,
            &format!("peak_rss_bytes_end\t{}", column(peak_rss_bytes())),
        )
    }
}

/// Write one report line, naming the write in the refusal.
fn emit(out: &mut impl Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}").map_err(|error| format!("writing the report: {error}"))
}

impl Options {
    /// The byte budget `kind` is measured at.
    ///
    /// The default is the crate's own ceiling for every shape, adversarial ones
    /// included: the issue asks what the crate costs on a 2 MiB hostile file, and
    /// an adversarial shape measured at 8 KiB would answer a different
    /// question. A caller who wants a cheaper sweep narrows it by name with
    /// `--shape-bytes`, and the table records the byte count it used on every
    /// row, so a smaller figure is never mistaken for the ceiling's.
    fn bytes_for(&self, kind: &str) -> usize {
        match kind {
            "nested" | "unbalanced" => self.shape_bytes,
            _ => self.bytes,
        }
    }
}

/// The measurement's own failure, reported rather than panicked on.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = parse_options(&args)?;
    run(&options).map_err(|error| -> Box<dyn std::error::Error> { error.into() })
}
