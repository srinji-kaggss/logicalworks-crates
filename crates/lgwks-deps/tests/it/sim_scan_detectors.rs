//! Deterministic simulation of the zero-gate source detectors.
//!
//! `scan_source` is the gate every estate repository runs over its own code, so
//! a false positive blocks a correct commit and a false negative passes the
//! defect the gate exists to catch. Its unit tests pin one example per rule;
//! this family generates whole files. One seed draws a run of blocks — an
//! `allow` list, a function of `?` statements, a documented function, or
//! filler — and each block carries what an independent model, written from the
//! rules' contracts rather than from the scanner, says the three syntax-only
//! detectors must report for it:
//!
//! - ALLOW-SILENCE for every clippy path an `allow` names, in order, wherever
//!   the attribute is written, test functions included;
//! - long-try-chain for a statement other than a `let` whose own expression
//!   holds more than three `?`, judged in the innermost enclosing function and
//!   never in one marked `test` or `test_*`;
//! - tautological-doc for a `pub` function, not a test, whose doc carries one
//!   to six content tokens of which at least half share a stem with its name.
//!
//! The scan must agree finding for finding: line, rule and evidence text. The
//! generator never writes the two error detectors' triggers (`return Err`, a
//! dropped `Result`), so a finding outside the model is a false positive and a
//! model finding the scan missed is a false negative.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `mixed_band_00..07` | 1,024 mixed files over eight bands agree with the model, replay to one trace, and reach every rule |
//! | `the_mixed_bands_cover_every_seed_exactly_once` | the bands partition the seed space |
//! | `allow_*`, `an_allow_*`, `a_lint_*`, `deny_*` | ALLOW-SILENCE: clippy paths only, `allow` only, every placement |
//! | `a_statement_*` .. `a_production_function_*` | long-try-chain: the threshold, `let`, closures, conditions, exemptions, nesting |
//! | `a_short_paraphrase_*` .. `a_test_function_is_never_judged` | tautological-doc: visibility, the six-token ceiling, stop words, methods, tests |
//! | `clean_files_*` .. `the_same_seed_*` | no false positives, permutation, line shifts, order, tenants, malformed input, disk, replay |

#![cfg(feature = "scan")]

use std::collections::BTreeSet;
use std::error::Error;
use std::ops::Range;

use lgwks_deps::scan::{ScanError, scan_path, scan_source};
use lgwks_std::{hex, random};

use crate::sim::{Rng, Trace, receipt};

/// What a scenario reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// A fallible step of the generator.
type Drawn<T> = Result<T, Box<dyn Error>>;

/// One finding as the scanner reports it: line, rule and evidence text.
type Finding = (usize, &'static str, String);

/// Seeds every per-rule family sweeps.
const SEEDS: u64 = 256;

/// Seeds the mixed family sweeps across its bands.
const MIXED_SEEDS: u64 = 1_024;

/// How many bands the mixed family is cut into, one declared test each.
const BANDS: u64 = 8;

/// The odd multiplier that spreads consecutive indices across the seed space.
const SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// How many distinct files the concurrent family scans at once.
const TENANTS: u64 = 64;

/// How many times each tenant's thread scans its own file.
const ROUNDS: u32 = 8;

/// The rule names, verbatim from the scanner's contract.
const ALLOW: &str = "ALLOW-SILENCE";
/// See [`ALLOW`].
const CHAIN: &str = "long-try-chain";
/// See [`ALLOW`].
const DOC: &str = "tautological-doc";

/// The most `?` operators one statement may hold before it is a finding.
const MAX_TRY_CHAIN: u32 = 3;

/// Clippy lint paths an `allow` may name, a group and a nested path among them.
const CLIPPY_LINTS: [&str; 6] = [
    "clippy::unwrap_used",
    "clippy::too_many_lines",
    "clippy::needless_pass_by_value",
    "clippy::all",
    "clippy::pedantic",
    "clippy::module_name_repetitions",
];

/// Lints an `allow` may name that are not clippy's: rustc's own and another
/// tool's, which the rule leaves alone by contract.
const OTHER_LINTS: [&str; 5] = [
    "dead_code",
    "unused_variables",
    "non_snake_case",
    "rustdoc::broken_intra_doc_links",
    "missing_docs",
];

/// Lint levels that raise a lint rather than silence it.
const LOUD_LEVELS: [&str; 3] = ["deny", "warn", "forbid"];

/// Attributes that exempt a function from the try-chain rule: `test` as the
/// last path segment, or any name starting `test_`.
const EXEMPT_ATTRIBUTES: [&str; 4] = [
    "#[test]",
    "#[tokio::test]",
    "#[test_case(1)]",
    "#[test_fixture]",
];

/// Words a generated function name is made of.
const NAME_WORDS: [&str; 10] = [
    "load", "user", "record", "parse", "config", "build", "render", "frame", "merge", "fetch",
];

/// Words that share no stem with any name word, in either direction.
const OTHER_WORDS: [&str; 10] = [
    "queue", "budget", "socket", "window", "tenant", "quorum", "ledger", "cursor", "harbor",
    "violet",
];

/// Stop words the tautology rule removes before it counts.
const STOP_WORDS: [&str; 7] = ["the", "a", "of", "to", "is", "with", "for"];

/// Lines that carry no finding: `syn` discards every comment, but each still
/// takes a line, which is what a line number has to survive.
const FILLER_LINES: [&str; 4] = ["", "// filler", "/* filler */", "const KEEP: u8 = 1;"];

/// What the model says one line must report.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    /// An `allow` naming this clippy path.
    Allow(String),
    /// A statement of `count` `?` operators in the function declared at
    /// `fn_offset` of the same block.
    Chain {
        /// Offset of the innermost enclosing function's declaration.
        fn_offset: usize,
        /// The `?` operators the statement's own expression holds.
        count: u32,
    },
    /// A `pub` function whose doc has `shared` of `total` content tokens on the
    /// stems of its name.
    Doc {
        /// The function's name.
        name: String,
        /// Content tokens sharing a stem with the name.
        shared: u32,
        /// Content tokens in all.
        total: u32,
    },
}

/// A run of source lines and the findings the model expects in it.
#[derive(Debug, Clone, Default)]
struct Block {
    /// The lines, without terminators.
    lines: Vec<String>,
    /// `(offset, expectation)` pairs, the offset indexing `lines`.
    expected: Vec<(usize, Expect)>,
}

impl Block {
    /// Appends a line and returns its offset.
    fn line(&mut self, text: impl Into<String>) -> usize {
        let offset = self.lines.len();
        self.lines.push(text.into());
        offset
    }

    /// Records that the line at `offset` must report `wanted`.
    fn want(&mut self, offset: usize, wanted: Expect) {
        self.expected.push((offset, wanted));
    }
}

/// A laid-out file: its text, where each block starts, and the findings the
/// model expects in the scanner's order (by line, then rule, stable).
#[derive(Debug)]
struct File {
    /// The source text.
    source: String,
    /// The 1-based line each block starts on, indexed by block.
    starts: Vec<usize>,
    /// The model's findings.
    expected: Vec<Finding>,
}

/// The finding an expectation becomes once its block starts at line `base`.
fn render(base: usize, offset: usize, wanted: &Expect) -> Finding {
    let line = base.saturating_add(offset);
    match *wanted {
        Expect::Allow(ref lint) => (line, ALLOW, format!("#[allow({lint})]")),
        Expect::Chain { fn_offset, count } => (
            line,
            CHAIN,
            format!(
                "fn near line {} — statement contains {count} `?` operators without an intermediate `let`. Break the chain with named intermediate values.",
                base.saturating_add(fn_offset)
            ),
        ),
        Expect::Doc {
            ref name,
            shared,
            total,
        } => (
            line,
            DOC,
            format!(
                "pub fn {name} — docstring is a paraphrase of the function name ({shared} of {total} tokens share stems). Replace it with substantive behavioral information."
            ),
        ),
    }
}

/// Lays `blocks` out in `order`, with `gaps[i]` comment lines before the
/// `i`-th placed block.
fn assemble(blocks: &[Block], order: &[usize], gaps: &[u32]) -> Drawn<File> {
    let mut source = String::new();
    let mut starts = vec![0; blocks.len()];
    let mut expected = Vec::new();
    let mut next_line: usize = 1;
    for (position, &index) in order.iter().enumerate() {
        let gap = gaps
            .get(position)
            .copied()
            .ok_or("a gap per placed block")?;
        for _ in 0..gap {
            source.push_str("// gap\n");
            next_line = next_line.saturating_add(1);
        }
        let block = blocks.get(index).ok_or("the order names a block")?;
        let start = starts.get_mut(index).ok_or("a start per block")?;
        *start = next_line;
        for &(offset, ref wanted) in &block.expected {
            expected.push(render(next_line, offset, wanted));
        }
        for text in &block.lines {
            source.push_str(text);
            source.push('\n');
            next_line = next_line.saturating_add(1);
        }
    }
    expected.sort_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));
    Ok(File {
        source,
        starts,
        expected,
    })
}

/// Every finding the scanner reports for `source`, as [`Finding`]s.
fn scanned(source: &str, path: &str) -> Drawn<Vec<Finding>> {
    Ok(scan_source(source, path)?
        .iter()
        .map(|hit| (hit.line(), hit.rule(), hit.snippet().to_owned()))
        .collect())
}

/// Scans a generated file and refuses any disagreement with the model, naming
/// the seed and printing the file, so a failure is its own reproduction.
fn agree(seed: u64, file: &File) -> Drawn<Vec<Finding>> {
    let observed = scanned(&file.source, "sim.rs")?;
    let verdict: Drawn<()> = if observed == file.expected {
        Ok(())
    } else {
        Err(format!(
            "seed {seed:#x}: the scan reported {observed:#?}\nthe model expects {:#?}\nsource:\n{}",
            file.expected, file.source
        )
        .into())
    };
    verdict?;
    Ok(observed)
}

/// Shuffles `items` in place with the seed's stream (Fisher–Yates).
fn shuffle<T>(rng: &mut Rng, items: &mut [T]) -> Drawn<()> {
    for upper in (1..items.len()).rev() {
        let pick = usize::try_from(rng.below(u32::try_from(upper.saturating_add(1))?))?;
        items.swap(upper, pick);
    }
    Ok(())
}

// ── ALLOW-SILENCE blocks ────────────────────────────────────────────────────

/// Where an `allow` is written.
#[derive(Debug, Clone, Copy)]
enum Placement {
    /// On a free function.
    Item,
    /// On a `let` statement inside a function.
    Statement,
    /// On a method in an inherent `impl`.
    Method,
    /// On a function that also carries `#[test]`.
    TestFunction,
}

/// Every placement, for a draw.
const PLACEMENTS: [Placement; 4] = [
    Placement::Item,
    Placement::Statement,
    Placement::Method,
    Placement::TestFunction,
];

/// Which lints an attribute's list is drawn from.
#[derive(Debug, Clone, Copy)]
enum Lints {
    /// Clippy's and others', a coin per entry.
    Mixed,
    /// Clippy's alone.
    ClippyOnly,
    /// Never clippy's.
    OtherOnly,
}

/// A lint attribute at `level` naming one to four lints, written at
/// `placement`, and one expected finding per clippy path when `level` is
/// `allow`.
fn allow_block(
    rng: &mut Rng,
    index: u32,
    placement: Placement,
    level: &str,
    lints: Lints,
) -> Drawn<Block> {
    let mut names = Vec::new();
    let mut silenced = Vec::new();
    for _ in 0..rng.between(1, 4) {
        let clippy = match lints {
            Lints::Mixed => rng.coin(),
            Lints::ClippyOnly => true,
            Lints::OtherOnly => false,
        };
        let table: &[&str] = if clippy { &CLIPPY_LINTS } else { &OTHER_LINTS };
        let name = *rng.pick_named("lints", table)?;
        names.push(name);
        if clippy && level == "allow" {
            silenced.push(name);
        }
    }
    let attribute = format!("#[{level}({})]", names.join(", "));
    let mut block = Block::default();
    let at = match placement {
        Placement::Item => {
            let at = block.line(attribute);
            block.line(format!("fn allowed_{index}() {{}}"));
            at
        }
        Placement::Statement => {
            block.line(format!("fn allowed_{index}() {{"));
            let at = block.line(format!("    {attribute}"));
            block.line("    let held = 1_u8;");
            block.line("    drop(held);");
            block.line("}");
            at
        }
        Placement::Method => {
            block.line(format!("impl Allowed{index} {{"));
            let at = block.line(format!("    {attribute}"));
            block.line("    fn method(&self) {}");
            block.line("}");
            at
        }
        Placement::TestFunction => {
            block.line("#[test]");
            let at = block.line(attribute);
            block.line(format!("fn allowed_{index}() {{}}"));
            at
        }
    };
    for name in silenced {
        block.want(at, Expect::Allow(name.to_owned()));
    }
    Ok(block)
}

// ── long-try-chain blocks ───────────────────────────────────────────────────

/// The statement shapes a function body is drawn from.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// `consume(step0()?, ..);`
    Call,
    /// `let bound = consume(..);`, which the contract says breaks any chain.
    Let,
    /// `if check(..) { .. }`: the condition counts, the block's statements are
    /// judged on their own.
    If,
    /// `run(|| { .. });`: the closure's statements are judged on their own.
    Closure,
    /// A production function nested in the body.
    NestedFn,
    /// A `#[test]` function nested in the body.
    NestedTest,
    /// A trait nested in the body, whose default method is judged under its
    /// own line.
    NestedTrait,
}

/// Every shape, for a draw.
const SHAPES: [Shape; 7] = [
    Shape::Call,
    Shape::Let,
    Shape::If,
    Shape::Closure,
    Shape::NestedFn,
    Shape::NestedTest,
    Shape::NestedTrait,
];

/// The innermost function a statement is judged in.
#[derive(Debug, Clone, Copy)]
struct Scope {
    /// Offset of its declaration in the block.
    fn_offset: usize,
    /// Whether it is exempt from the rule.
    exempt: bool,
}

/// A call to `head` whose arguments hold `count` `?` operators.
fn call(head: &str, count: u32) -> String {
    let arguments = (0..count)
        .map(|step| format!("step{step}()?"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{head}({arguments})")
}

/// Records the finding a statement of `count` operators at `at` makes in
/// `scope`, if it makes one.
fn judge(block: &mut Block, scope: Scope, at: usize, count: u32) {
    if !scope.exempt && count > MAX_TRY_CHAIN {
        block.want(
            at,
            Expect::Chain {
                fn_offset: scope.fn_offset,
                count,
            },
        );
    }
}

/// One or two plain calls at the nested indent, judged in `scope`.
fn inner(rng: &mut Rng, block: &mut Block, scope: Scope, most: u32) {
    for _ in 0..rng.between(1, 2) {
        let count = rng.below(most.saturating_add(1));
        let at = block.line(format!("        {};", call("consume", count)));
        judge(block, scope, at, count);
    }
}

/// One statement of `shape` in a body judged in `scope`.
fn statement(
    rng: &mut Rng,
    block: &mut Block,
    scope: Scope,
    shape: Shape,
    ordinal: u32,
    most: u32,
) {
    let count = rng.below(most.saturating_add(1));
    match shape {
        Shape::Call => {
            let at = block.line(format!("    {};", call("consume", count)));
            judge(block, scope, at, count);
        }
        Shape::Let => {
            block.line(format!("    let bound = {};", call("consume", count)));
        }
        Shape::If => {
            let at = block.line(format!("    if {} {{", call("check", count)));
            judge(block, scope, at, count);
            inner(rng, block, scope, most);
            block.line("    }");
        }
        Shape::Closure => {
            block.line("    run(|| {");
            inner(rng, block, scope, most);
            block.line("    });");
        }
        Shape::NestedFn | Shape::NestedTest => {
            let test = matches!(shape, Shape::NestedTest);
            if test {
                block.line("    #[test]");
            }
            let fn_offset = block.line(format!("    fn inner_{ordinal}() {{"));
            inner(
                rng,
                block,
                Scope {
                    fn_offset,
                    exempt: test,
                },
                most,
            );
            block.line("    }");
        }
        Shape::NestedTrait => {
            block.line(format!("    trait Inner{ordinal} {{"));
            let fn_offset = block.line(format!("    fn inner_{ordinal}() {{"));
            inner(
                rng,
                block,
                Scope {
                    fn_offset,
                    exempt: false,
                },
                most,
            );
            block.line("    }");
            block.line("    }");
        }
    }
}

/// A top-level trait whose one default method holds one to five statements
/// drawn from `shapes`. A default method's body is code like any other
/// function's, so it is judged under its own line.
fn trait_block(rng: &mut Rng, index: u32, shapes: &[Shape], most: u32) -> Drawn<Block> {
    let mut block = Block::default();
    block.line(format!("trait Chain{index} {{"));
    let fn_offset = block.line(format!("fn chain_{index}() {{"));
    for ordinal in 0..rng.between(1, 5) {
        let shape = *rng.pick_named("shapes", shapes)?;
        statement(
            rng,
            &mut block,
            Scope {
                fn_offset,
                exempt: false,
            },
            shape,
            ordinal,
            most,
        );
    }
    block.line("}");
    block.line("}");
    Ok(block)
}

/// A function of one to five statements drawn from `shapes`, each holding at
/// most `most` operators, under an exempting attribute when `exempt`.
fn chain_block(
    rng: &mut Rng,
    index: u32,
    exempt: bool,
    shapes: &[Shape],
    most: u32,
) -> Drawn<Block> {
    let mut block = Block::default();
    if exempt {
        block.line(*rng.pick_named("exempting attributes", &EXEMPT_ATTRIBUTES)?);
    }
    let fn_offset = block.line(format!("fn chain_{index}() {{"));
    for ordinal in 0..rng.between(1, 5) {
        let shape = *rng.pick_named("shapes", shapes)?;
        statement(
            rng,
            &mut block,
            Scope { fn_offset, exempt },
            shape,
            ordinal,
            most,
        );
    }
    block.line("}");
    Ok(block)
}

/// One to three lines that carry no finding.
fn filler_block(rng: &mut Rng) -> Drawn<Block> {
    let mut block = Block::default();
    for _ in 0..rng.between(1, 3) {
        block.line(*rng.pick_named("filler lines", &FILLER_LINES)?);
    }
    Ok(block)
}

// ── Files and sweeps ────────────────────────────────────────────────────────

/// A block of any kind, every knob drawn.
fn mixed_block(rng: &mut Rng, index: u32) -> Drawn<Block> {
    match rng.below(4) {
        0 => {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            let level = if rng.chance(150) {
                *rng.pick_named("loud levels", &LOUD_LEVELS)?
            } else {
                "allow"
            };
            allow_block(rng, index, placement, level, Lints::Mixed)
        }
        1 => {
            let exempt = rng.chance(200);
            chain_block(rng, index, exempt, &SHAPES, 7)
        }
        2 => {
            let spec = DocSpec {
                seen: *rng.pick_named("visibilities", &SEEN)?,
                method: rng.coin(),
                test: rng.chance(100),
                fewest: 0,
                most: 8,
                paraphrase: false,
                stops: 3,
            };
            doc_block(rng, index, spec)
        }
        _ => filler_block(rng),
    }
}

/// Four to twelve mixed blocks.
fn mixed_blocks(rng: &mut Rng) -> Drawn<Vec<Block>> {
    (0..rng.between(4, 12))
        .map(|index| mixed_block(rng, index))
        .collect()
}

/// `blocks` in their own order, with zero to two comment lines before each.
fn laid_out(rng: &mut Rng, blocks: &[Block]) -> Drawn<File> {
    let order: Vec<usize> = (0..blocks.len()).collect();
    let gaps: Vec<u32> = order.iter().map(|_| rng.below(3)).collect();
    assemble(blocks, &order, &gaps)
}

/// The seed of the mixed family's `index`-th scenario.
const fn mixed_seed(index: u64) -> u64 {
    0x5ca7_0000_0000_0000 ^ index.wrapping_mul(SPREAD)
}

/// One seed's mixed file.
fn mixed_file(seed: u64) -> Drawn<File> {
    let mut rng = Rng::new(seed);
    let blocks = mixed_blocks(&mut rng)?;
    laid_out(&mut rng, &blocks)
}

/// One mixed seed folded into a trace hash: the file and every finding.
fn mixed_receipt(seed: u64) -> Drawn<u64> {
    let file = mixed_file(seed)?;
    let found = agree(seed, &file)?;
    let mut trace = Trace::new();
    trace.record(&file.source);
    for &(line, rule, ref snippet) in &found {
        trace.record_number(rule, line);
        trace.record(snippet);
    }
    Ok(receipt(&trace)?)
}

/// The scenario indices band `band` of the mixed family sweeps.
fn band_seeds(band: u64) -> Drawn<Range<u64>> {
    let per = MIXED_SEEDS
        .checked_div(BANDS)
        .ok_or("the family is cut into at least one band")?;
    let first = band.saturating_mul(per);
    Ok(first..first.saturating_add(per))
}

/// How many findings of `rule` a list holds.
fn tally(found: &[Finding], rule: &str) -> usize {
    found.iter().filter(|finding| finding.1 == rule).count()
}

/// One band: every file agrees with the model and replays to one trace, and
/// the band reaches every rule.
fn mixed_band(band: u64) -> TestResult {
    let mut reached = [0_usize; 3];
    for index in band_seeds(band)? {
        let seed = mixed_seed(index);
        let found = agree(seed, &mixed_file(seed)?)?;
        for (slot, rule) in reached.iter_mut().zip([ALLOW, CHAIN, DOC]) {
            *slot = slot.saturating_add(tally(&found, rule));
        }
        assert_eq!(
            mixed_receipt(seed)?,
            mixed_receipt(seed)?,
            "seed {seed:#x} replays to one trace"
        );
    }
    assert!(
        reached.iter().all(|&count| count > 0),
        "band {band} reached every rule (allow, chain, doc): {reached:?}"
    );
    Ok(())
}

macro_rules! mixed_family {
    ($($name:ident => $band:expr);+ $(;)?) => {
        $(
            /// A seeded band of mixed files, each agreeing with the model.
            #[test]
            fn $name() -> TestResult {
                mixed_band($band)
            }
        )+
    };
}

mixed_family!(
    mixed_band_00 => 0; mixed_band_01 => 1; mixed_band_02 => 2; mixed_band_03 => 3;
    mixed_band_04 => 4; mixed_band_05 => 5; mixed_band_06 => 6; mixed_band_07 => 7;
);

/// The bands partition the seed space: no gap, no overlap.
#[test]
fn the_mixed_bands_cover_every_seed_exactly_once() -> TestResult {
    let mut covered = BTreeSet::new();
    for band in 0..BANDS {
        for index in band_seeds(band)? {
            assert!(covered.insert(index), "seed {index} is in two bands");
        }
    }
    assert_eq!(
        u64::try_from(covered.len())?,
        MIXED_SEEDS,
        "the bands cover every seed"
    );
    Ok(())
}

/// Sweeps [`SEEDS`] seeds from `base`: each draws its blocks with `make`, lays
/// them out with drawn gaps, and must agree with the model.
fn sweep(
    base: u64,
    mut make: impl FnMut(&mut Rng) -> Drawn<Vec<Block>>,
) -> Drawn<Vec<(File, Vec<Finding>)>> {
    let mut swept = Vec::new();
    for index in 0..SEEDS {
        let seed = base ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let blocks = make(&mut rng)?;
        let file = laid_out(&mut rng, &blocks)?;
        let found = agree(seed, &file)?;
        swept.push((file, found));
    }
    Ok(swept)
}

/// One to four blocks from `one`.
fn several(rng: &mut Rng, mut one: impl FnMut(&mut Rng, u32) -> Drawn<Block>) -> Drawn<Vec<Block>> {
    (0..rng.between(1, 4))
        .map(|index| one(rng, index))
        .collect()
}

/// Every finding of `rule` across a sweep.
fn swept_tally(swept: &[(File, Vec<Finding>)], rule: &str) -> usize {
    swept.iter().map(|swept| tally(&swept.1, rule)).sum()
}

/// Whether some swept line holds exactly `count` `?` operators, so a family
/// can show it reached the boundary it claims to test.
fn some_line_holds(swept: &[(File, Vec<Finding>)], count: usize) -> bool {
    swept.iter().any(|swept| {
        let file = &swept.0;
        file.source
            .lines()
            .any(|line| line.matches('?').count() == count)
    })
}

/// Whether some swept line holds more than three `?` operators.
fn some_line_exceeds(swept: &[(File, Vec<Finding>)]) -> bool {
    (4..=12).any(|count| some_line_holds(swept, count))
}

mod doc_blocks;
use doc_blocks::{DocSpec, PUBLIC_DOC, SEEN, Seen, doc_block};

mod allow_silence;
mod cross_cutting;
mod tautological_doc;
mod try_chain;
