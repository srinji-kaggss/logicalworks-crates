//! The generator and the model of the scan simulation: the blocks a seed draws
//! (an `allow` list, a function of `?` statements, a documented function, or
//! filler), the findings an independent model says each must report, and the
//! layout of blocks into a file.

use std::error::Error;
use std::ops::Range;

use lgwks_ast::scan::scan_source;

use crate::sim::{Rng, Trace, receipt};

use super::allow::{include_block, spelled_block, unreadable_block};

/// What a scenario reports when a precondition did not hold.
pub(super) type TestResult = Result<(), Box<dyn Error>>;

/// A fallible step of the generator.
pub(super) type Drawn<T> = Result<T, Box<dyn Error>>;

/// One finding as the scanner reports it: line, rule and evidence text.
pub(super) type Finding = (usize, &'static str, String);

/// Seeds every per-rule family sweeps.
pub(super) const SEEDS: u64 = 256;

/// Seeds the mixed family sweeps across its bands.
pub(super) const MIXED_SEEDS: u64 = 1_024;

/// How many bands the mixed family is cut into, one declared test each.
pub(super) const BANDS: u64 = 8;

/// The odd multiplier that spreads consecutive indices across the seed space.
pub(super) const SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// How many distinct files the concurrent family scans at once.
pub(super) const TENANTS: u64 = 64;

/// How many times each tenant's thread scans its own file.
pub(super) const ROUNDS: u32 = 8;

/// The rule names, verbatim from the scanner's contract.
pub(super) const ALLOW: &str = "ALLOW-SILENCE";
/// See [`ALLOW`].
pub(super) const CHAIN: &str = "long-try-chain";
/// See [`ALLOW`].
pub(super) const DOC: &str = "tautological-doc";

/// The most `?` operators one statement may hold before it is a finding.
pub(super) const MAX_TRY_CHAIN: u32 = 3;

/// Clippy lint paths an `allow` may name, a group and a nested path among them.
pub(super) const CLIPPY_LINTS: [&str; 6] = [
    "clippy::unwrap_used",
    "clippy::too_many_lines",
    "clippy::needless_pass_by_value",
    "clippy::all",
    "clippy::pedantic",
    "clippy::module_name_repetitions",
];

/// Lints an `allow` may name that are not clippy's: rustc's own and another
/// tool's, which silence exactly as a clippy lint does (#699).
pub(super) const OTHER_LINTS: [&str; 5] = [
    "dead_code",
    "unused_variables",
    "non_snake_case",
    "rustdoc::broken_intra_doc_links",
    "missing_docs",
];

/// Lint levels that raise a lint rather than silence it.
pub(super) const LOUD_LEVELS: [&str; 3] = ["deny", "warn", "forbid"];

/// Lint levels that silence: `allow` suppresses the emission, and `expect`
/// suppresses it while recording the expectation.
pub(super) const SILENCING_LEVELS: [&str; 2] = ["allow", "expect"];

/// Attributes that exempt a function from the try-chain rule: `test` as the
/// last path segment, or any name starting `test_`.
pub(super) const EXEMPT_ATTRIBUTES: [&str; 4] = [
    "#[test]",
    "#[tokio::test]",
    "#[test_case(1)]",
    "#[test_fixture]",
];

/// Words a generated function name is made of.
pub(super) const NAME_WORDS: [&str; 10] = [
    "load", "user", "record", "parse", "config", "build", "render", "frame", "merge", "fetch",
];

/// Words that share no stem with any name word, in either direction.
pub(super) const OTHER_WORDS: [&str; 10] = [
    "queue", "budget", "socket", "window", "tenant", "quorum", "ledger", "cursor", "harbor",
    "violet",
];

/// Stop words the tautology rule removes before it counts.
pub(super) const STOP_WORDS: [&str; 7] = ["the", "a", "of", "to", "is", "with", "for"];

/// Lines that carry no finding: `syn` discards every comment, but each still
/// takes a line, which is what a line number has to survive.
pub(super) const FILLER_LINES: [&str; 4] = ["", "// filler", "/* filler */", "const KEEP: u8 = 1;"];

/// What the model says one line must report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Expect {
    /// A silencing attribute at `level` naming this lint, written directly on
    /// the item or inside a `cfg_attr`.
    Allow {
        /// The level that silenced: `allow` or `expect`.
        level: &'static str,
        /// The lint the attribute names.
        lint: String,
        /// Whether the attribute is written inside a `cfg_attr`.
        nested: bool,
    },
    /// A silence the scan can only name by its own words: a list it cannot read
    /// (`#[allow(..)]`), or a file it never reads (`include!("x.inc")`).
    Said(String),
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
pub(super) struct Block {
    /// The lines, without terminators.
    pub(super) lines: Vec<String>,
    /// `(offset, expectation)` pairs, the offset indexing `lines`.
    pub(super) expected: Vec<(usize, Expect)>,
}

impl Block {
    /// Appends a line and returns its offset.
    pub(super) fn line(&mut self, text: impl Into<String>) -> usize {
        let offset = self.lines.len();
        self.lines.push(text.into());
        offset
    }

    /// Records that the line at `offset` must report `wanted`.
    pub(super) fn want(&mut self, offset: usize, wanted: Expect) {
        self.expected.push((offset, wanted));
    }
}

/// A laid-out file: its text, where each block starts, and the findings the
/// model expects in the scanner's order (by line, then rule, stable).
#[derive(Debug)]
pub(super) struct File {
    /// The source text.
    pub(super) source: String,
    /// The 1-based line each block starts on, indexed by block.
    pub(super) starts: Vec<usize>,
    /// The model's findings.
    pub(super) expected: Vec<Finding>,
}

/// The finding an expectation becomes once its block starts at line `base`.
pub(super) fn render(base: usize, offset: usize, wanted: &Expect) -> Finding {
    let line = base.saturating_add(offset);
    match *wanted {
        Expect::Allow {
            level,
            ref lint,
            nested,
        } => (
            line,
            ALLOW,
            if nested {
                format!("#[cfg_attr(.., {level}({lint}))]")
            } else {
                format!("#[{level}({lint})]")
            },
        ),
        Expect::Said(ref text) => (line, ALLOW, text.clone()),
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
pub(super) fn assemble(blocks: &[Block], order: &[usize], gaps: &[u32]) -> Drawn<File> {
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
pub(super) fn scanned(source: &str, path: &str) -> Drawn<Vec<Finding>> {
    Ok(scan_source(source, path)?
        .iter()
        .map(|hit| (hit.line(), hit.rule(), hit.snippet().to_owned()))
        .collect())
}

/// Scans a generated file and refuses any disagreement with the model, naming
/// the seed and printing the file, so a failure is its own reproduction.
pub(super) fn agree(seed: u64, file: &File) -> Drawn<Vec<Finding>> {
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
pub(super) fn shuffle<T>(rng: &mut Rng, items: &mut [T]) -> Drawn<()> {
    for upper in (1..items.len()).rev() {
        let pick = usize::try_from(rng.below(u32::try_from(upper.saturating_add(1))?))?;
        items.swap(upper, pick);
    }
    Ok(())
}

// ── ALLOW-SILENCE blocks ────────────────────────────────────────────────────

/// Where an `allow` is written.
#[derive(Debug, Clone, Copy)]
pub(super) enum Placement {
    /// On a free function.
    Item,
    /// On a `let` statement inside a function.
    Statement,
    /// On a method in an inherent `impl`.
    Method,
    /// On a function that also carries `#[test]`.
    TestFunction,
    /// In the body of a `macro_rules!`, which puts it on the item it expands to.
    MacroBody,
    /// In the arguments of a macro invocation that hands the item back.
    MacroArgument,
}

/// Every placement, for a draw.
pub(super) const PLACEMENTS: [Placement; 6] = [
    Placement::Item,
    Placement::Statement,
    Placement::Method,
    Placement::TestFunction,
    Placement::MacroBody,
    Placement::MacroArgument,
];

/// How an attribute's identifiers are written. A raw identifier is the plain
/// identifier it prefixes, so rustc reads every spelling as the same attribute
/// naming the same lint (Rust reference, "Raw identifiers"; #758).
#[derive(Debug, Clone, Copy)]
pub(super) enum Spelling {
    /// `allow`, `dead_code`.
    Plain,
    /// `r#allow`, and `r#cfg_attr` around it: the attribute's own names.
    RawHead,
    /// `r#dead_code`: the lints a single identifier names.
    RawLints,
    /// Both of the above.
    RawBoth,
}

/// Every spelling, for a draw.
pub(super) const SPELLINGS: [Spelling; 4] = [
    Spelling::Plain,
    Spelling::RawHead,
    Spelling::RawLints,
    Spelling::RawBoth,
];

/// Which lints an attribute's list is drawn from.
#[derive(Debug, Clone, Copy)]
pub(super) enum Lints {
    /// Clippy's and others', a coin per entry.
    Mixed,
    /// Clippy's alone.
    ClippyOnly,
    /// Never clippy's.
    OtherOnly,
}

// ── long-try-chain blocks ───────────────────────────────────────────────────

/// The statement shapes a function body is drawn from.
#[derive(Debug, Clone, Copy)]
pub(super) enum Shape {
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
pub(super) const SHAPES: [Shape; 7] = [
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
pub(super) struct Scope {
    /// Offset of its declaration in the block.
    pub(super) fn_offset: usize,
    /// Whether it is exempt from the rule.
    pub(super) exempt: bool,
}

/// A call to `head` whose arguments hold `count` `?` operators.
pub(super) fn call(head: &str, count: u32) -> String {
    let arguments = (0..count)
        .map(|step| format!("step{step}()?"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{head}({arguments})")
}

/// Records the finding a statement of `count` operators at `at` makes in
/// `scope`, if it makes one.
pub(super) fn judge(block: &mut Block, scope: Scope, at: usize, count: u32) {
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
pub(super) fn inner(rng: &mut Rng, block: &mut Block, scope: Scope, most: u32) {
    for _ in 0..rng.between(1, 2) {
        let count = rng.below(most.saturating_add(1));
        let at = block.line(format!("        {};", call("consume", count)));
        judge(block, scope, at, count);
    }
}

/// One statement of `shape` in a body judged in `scope`.
pub(super) fn statement(
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
pub(super) fn trait_block(rng: &mut Rng, index: u32, shapes: &[Shape], most: u32) -> Drawn<Block> {
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
pub(super) fn chain_block(
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

// ── tautological-doc blocks ─────────────────────────────────────────────────

/// Who can see a documented function.
#[derive(Debug, Clone, Copy)]
pub(super) enum Seen {
    /// `pub`.
    Public,
    /// `pub(crate)`.
    Crate,
    /// No visibility keyword.
    Private,
}

impl Seen {
    /// The keyword this visibility writes before `fn`.
    pub(super) const fn keyword(self) -> &'static str {
        match self {
            Self::Public => "pub ",
            Self::Crate => "pub(crate) ",
            Self::Private => "",
        }
    }
}

/// Visibilities for a mixed draw, weighted toward the one the rule judges.
pub(super) const SEEN: [Seen; 5] = [
    Seen::Public,
    Seen::Public,
    Seen::Public,
    Seen::Crate,
    Seen::Private,
];

/// What a documented function looks like.
#[derive(Debug, Clone, Copy)]
pub(super) struct DocSpec {
    /// Who can see it.
    pub(super) seen: Seen,
    /// Whether it is a method in an inherent `impl`.
    pub(super) method: bool,
    /// Whether it carries `#[test]`.
    pub(super) test: bool,
    /// The fewest content tokens its doc carries.
    pub(super) fewest: u32,
    /// The most content tokens its doc carries.
    pub(super) most: u32,
    /// Whether every content token is a stem of the name.
    pub(super) paraphrase: bool,
    /// The most stop words mixed in.
    pub(super) stops: u32,
}

/// A documented function shaped by `spec`, and the finding the model expects
/// for it.
pub(super) fn doc_block(rng: &mut Rng, index: u32, spec: DocSpec) -> Drawn<Block> {
    let first = *rng.pick_named("name words", &NAME_WORDS)?;
    let second = *rng.pick_named("name words", &NAME_WORDS)?;
    let name = format!("{first}_{second}_{index}");
    let total = rng.between(spec.fewest, spec.most);
    let shared = if spec.paraphrase {
        total
    } else {
        rng.below(total.saturating_add(1))
    };
    let mut words: Vec<String> = Vec::new();
    for word in 0..total {
        if word < shared {
            let stem = if rng.coin() { first } else { second };
            words.push(if rng.coin() {
                format!("{stem}s")
            } else {
                stem.to_owned()
            });
        } else {
            words.push((*rng.pick_named("other words", &OTHER_WORDS)?).to_owned());
        }
    }
    for _ in 0..rng.below(spec.stops.saturating_add(1)) {
        words.push((*rng.pick_named("stop words", &STOP_WORDS)?).to_owned());
    }
    shuffle(rng, &mut words)?;
    let mut block = Block::default();
    let indent = if spec.method {
        block.line(format!("impl DocHost{index} {{"));
        "    "
    } else {
        ""
    };
    if spec.test {
        block.line(format!("{indent}#[test]"));
    }
    if !words.is_empty() {
        let split = if rng.coin() {
            words.len().div_ceil(2)
        } else {
            words.len()
        };
        let (head, tail) = words
            .split_at_checked(split)
            .ok_or("a split inside the doc")?;
        block.line(format!("{indent}/// {}.", head.join(" ")));
        if !tail.is_empty() {
            block.line(format!("{indent}/// {}.", tail.join(" ")));
        }
    }
    let receiver = if spec.method { "&self" } else { "" };
    let at = block.line(format!(
        "{indent}{}fn {name}({receiver}) {{}}",
        spec.seen.keyword()
    ));
    if spec.method {
        block.line("}");
    }
    if matches!(spec.seen, Seen::Public)
        && !spec.test
        && (1..=6).contains(&total)
        && shared.saturating_mul(2) >= total
    {
        block.want(
            at,
            Expect::Doc {
                name,
                shared,
                total,
            },
        );
    }
    Ok(block)
}

/// One to three lines that carry no finding.
pub(super) fn filler_block(rng: &mut Rng) -> Drawn<Block> {
    let mut block = Block::default();
    for _ in 0..rng.between(1, 3) {
        block.line(*rng.pick_named("filler lines", &FILLER_LINES)?);
    }
    Ok(block)
}

// ── Files and sweeps ────────────────────────────────────────────────────────

/// A block of any kind, every knob drawn.
pub(super) fn mixed_block(rng: &mut Rng, index: u32) -> Drawn<Block> {
    match rng.below(6) {
        0 => {
            let placement = *rng.pick_named("placements", &PLACEMENTS)?;
            let level = if rng.chance(150) {
                *rng.pick_named("loud levels", &LOUD_LEVELS)?
            } else if rng.coin() {
                "allow"
            } else {
                "expect"
            };
            let nested = rng.coin();
            let spelling = *rng.pick_named("spellings", &SPELLINGS)?;
            spelled_block(rng, index, placement, level, Lints::Mixed, nested, spelling)
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
        3 => filler_block(rng),
        4 => include_block(rng, index),
        _ => unreadable_block(rng, index),
    }
}

/// Four to twelve mixed blocks.
pub(super) fn mixed_blocks(rng: &mut Rng) -> Drawn<Vec<Block>> {
    (0..rng.between(4, 12))
        .map(|index| mixed_block(rng, index))
        .collect()
}

/// `blocks` in their own order, with zero to two comment lines before each.
pub(super) fn laid_out(rng: &mut Rng, blocks: &[Block]) -> Drawn<File> {
    let order: Vec<usize> = (0..blocks.len()).collect();
    let gaps: Vec<u32> = order.iter().map(|_| rng.below(3)).collect();
    assemble(blocks, &order, &gaps)
}

/// The seed of the mixed family's `index`-th scenario.
pub(super) const fn mixed_seed(index: u64) -> u64 {
    0x5ca7_0000_0000_0000 ^ index.wrapping_mul(SPREAD)
}

/// One seed's mixed file.
pub(super) fn mixed_file(seed: u64) -> Drawn<File> {
    let mut rng = Rng::new(seed);
    let blocks = mixed_blocks(&mut rng)?;
    laid_out(&mut rng, &blocks)
}

/// One mixed seed folded into a trace hash: the file and every finding.
pub(super) fn mixed_receipt(seed: u64) -> Drawn<u64> {
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
pub(super) fn band_seeds(band: u64) -> Drawn<Range<u64>> {
    let per = MIXED_SEEDS
        .checked_div(BANDS)
        .ok_or("the family is cut into at least one band")?;
    let first = band.saturating_mul(per);
    Ok(first..first.saturating_add(per))
}

/// How many findings of `rule` a list holds.
pub(super) fn tally(found: &[Finding], rule: &str) -> usize {
    found.iter().filter(|finding| finding.1 == rule).count()
}

/// Sweeps [`SEEDS`] seeds from `base`: each draws its blocks with `make`, lays
/// them out with drawn gaps, and must agree with the model.
pub(super) fn sweep(
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
pub(super) fn several(
    rng: &mut Rng,
    mut one: impl FnMut(&mut Rng, u32) -> Drawn<Block>,
) -> Drawn<Vec<Block>> {
    (0..rng.between(1, 4))
        .map(|index| one(rng, index))
        .collect()
}

/// Every finding of `rule` across a sweep.
pub(super) fn swept_tally(swept: &[(File, Vec<Finding>)], rule: &str) -> usize {
    swept.iter().map(|swept| tally(&swept.1, rule)).sum()
}

/// Whether some swept line holds exactly `count` `?` operators, so a family
/// can show it reached the boundary it claims to test.
pub(super) fn some_line_holds(swept: &[(File, Vec<Finding>)], count: usize) -> bool {
    swept.iter().any(|swept| {
        let file = &swept.0;
        file.source
            .lines()
            .any(|line| line.matches('?').count() == count)
    })
}

/// Whether some swept line holds more than three `?` operators.
pub(super) fn some_line_exceeds(swept: &[(File, Vec<Finding>)]) -> bool {
    (4..=12).any(|count| some_line_holds(swept, count))
}

/// The doc spec every tautology family starts from: a public free function
/// whose doc carries one to six content tokens.
pub(super) const PUBLIC_DOC: DocSpec = DocSpec {
    seen: Seen::Public,
    method: false,
    test: false,
    fewest: 1,
    most: 6,
    paraphrase: false,
    stops: 2,
};
