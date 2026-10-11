//! The generator and the independent model `sim_scan_detectors` checks the
//! zero-gate detectors against (#408): blocks of source, each carrying what
//! the rules' contracts say it must report, laid out into whole files.

#![cfg(feature = "scan")]

use std::error::Error;
use std::ops::Range;

use lgwks_ast::scan::scan_source;

use crate::sim::{Rng, Trace, receipt};

/// What a scenario reports when a precondition did not hold.
pub(crate) type TestResult = Result<(), Box<dyn Error>>;

/// A fallible step of the generator.
pub(crate) type Drawn<T> = Result<T, Box<dyn Error>>;

/// One finding as the scanner reports it: line, rule and evidence text.
pub(crate) type Finding = (usize, &'static str, String);

/// Seeds every per-rule family sweeps.
pub(crate) const SEEDS: u64 = 256;

/// Seeds the mixed family sweeps across its bands.
pub(crate) const MIXED_SEEDS: u64 = 1_024;

/// How many bands the mixed family is cut into, one declared test each.
pub(crate) const BANDS: u64 = 8;

/// The odd multiplier that spreads consecutive indices across the seed space.
pub(crate) const SPREAD: u64 = 0x9e37_79b9_7f4a_7c15;

/// How many distinct files the concurrent family scans at once.
pub(crate) const TENANTS: u64 = 64;

/// How many times each tenant's thread scans its own file.
pub(crate) const ROUNDS: u32 = 8;

/// The rule names, verbatim from the scanner's contract.
pub(crate) const ALLOW: &str = "ALLOW-SILENCE";
/// See [`ALLOW`].
pub(crate) const CHAIN: &str = "long-try-chain";
/// See [`ALLOW`].
pub(crate) const DOC: &str = "tautological-doc";

/// The most `?` operators one statement may hold before it is a finding.
pub(crate) const MAX_TRY_CHAIN: u32 = 3;

/// Clippy lint paths an `allow` may name, a group and a nested path among them.
pub(crate) const CLIPPY_LINTS: [&str; 6] = [
    "clippy::unwrap_used",
    "clippy::too_many_lines",
    "clippy::needless_pass_by_value",
    "clippy::all",
    "clippy::pedantic",
    "clippy::module_name_repetitions",
];

/// Lints an `allow` may name that are not clippy's: rustc's own and another
/// tool's, which the rule leaves alone by contract.
pub(crate) const OTHER_LINTS: [&str; 5] = [
    "dead_code",
    "unused_variables",
    "non_snake_case",
    "rustdoc::broken_intra_doc_links",
    "missing_docs",
];

/// Lint levels that raise a lint rather than silence it.
pub(crate) const LOUD_LEVELS: [&str; 3] = ["deny", "warn", "forbid"];

/// Attributes that exempt a function from the try-chain rule: `test` as the
/// last path segment, or any name starting `test_`.
pub(crate) const EXEMPT_ATTRIBUTES: [&str; 4] = [
    "#[test]",
    "#[tokio::test]",
    "#[test_case(1)]",
    "#[test_fixture]",
];

/// Words a generated function name is made of.
pub(crate) const NAME_WORDS: [&str; 10] = [
    "load", "user", "record", "parse", "config", "build", "render", "frame", "merge", "fetch",
];

/// Words that share no stem with any name word, in either direction.
pub(crate) const OTHER_WORDS: [&str; 10] = [
    "queue", "budget", "socket", "window", "tenant", "quorum", "ledger", "cursor", "harbor",
    "violet",
];

/// Stop words the tautology rule removes before it counts.
pub(crate) const STOP_WORDS: [&str; 7] = ["the", "a", "of", "to", "is", "with", "for"];

/// Lines that carry no finding: a comment is no statement and no attribute,
/// but each still takes a line, which is what a line number has to survive.
pub(crate) const FILLER_LINES: [&str; 4] = ["", "// filler", "/* filler */", "const KEEP: u8 = 1;"];

/// What the model says one line must report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expect {
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
pub(crate) struct Block {
    /// The lines, without terminators.
    pub(crate) lines: Vec<String>,
    /// `(offset, expectation)` pairs, the offset indexing `lines`.
    pub(crate) expected: Vec<(usize, Expect)>,
}

impl Block {
    /// Appends a line and returns its offset.
    pub(crate) fn line(&mut self, text: impl Into<String>) -> usize {
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
pub(crate) struct File {
    /// The source text.
    pub(crate) source: String,
    /// The 1-based line each block starts on, indexed by block.
    pub(crate) starts: Vec<usize>,
    /// The model's findings.
    pub(crate) expected: Vec<Finding>,
}

/// The finding an expectation becomes once its block starts at line `base`.
pub(crate) fn render(base: usize, offset: usize, wanted: &Expect) -> Finding {
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
pub(crate) fn assemble(blocks: &[Block], order: &[usize], gaps: &[u32]) -> Drawn<File> {
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
pub(crate) fn scanned(source: &str, path: &str) -> Drawn<Vec<Finding>> {
    Ok(scan_source(source, path)?
        .iter()
        .map(|hit| (hit.line(), hit.rule(), hit.snippet().to_owned()))
        .collect())
}

/// Scans a generated file and refuses any disagreement with the model, naming
/// the seed and printing the file, so a failure is its own reproduction.
pub(crate) fn agree(seed: u64, file: &File) -> Drawn<Vec<Finding>> {
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
pub(crate) fn shuffle<T>(rng: &mut Rng, items: &mut [T]) -> Drawn<()> {
    for upper in (1..items.len()).rev() {
        let pick = usize::try_from(rng.below(u32::try_from(upper.saturating_add(1))?))?;
        items.swap(upper, pick);
    }
    Ok(())
}

// ── ALLOW-SILENCE blocks ────────────────────────────────────────────────────

/// Where an `allow` is written.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Placement {
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
pub(crate) const PLACEMENTS: [Placement; 4] = [
    Placement::Item,
    Placement::Statement,
    Placement::Method,
    Placement::TestFunction,
];

/// Which lints an attribute's list is drawn from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Lints {
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
pub(crate) fn allow_block(
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
pub(crate) enum Shape {
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
pub(crate) const SHAPES: [Shape; 7] = [
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
pub(crate) struct Scope {
    /// Offset of its declaration in the block.
    pub(crate) fn_offset: usize,
    /// Whether it is exempt from the rule.
    pub(crate) exempt: bool,
}

/// A call to `head` whose arguments hold `count` `?` operators.
pub(crate) fn call(head: &str, count: u32) -> String {
    let arguments = (0..count)
        .map(|step| format!("step{step}()?"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{head}({arguments})")
}

/// Records the finding a statement of `count` operators at `at` makes in
/// `scope`, if it makes one.
pub(crate) fn judge(block: &mut Block, scope: Scope, at: usize, count: u32) {
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
pub(crate) fn inner(rng: &mut Rng, block: &mut Block, scope: Scope, most: u32) {
    for _ in 0..rng.between(1, 2) {
        let count = rng.below(most.saturating_add(1));
        let at = block.line(format!("        {};", call("consume", count)));
        judge(block, scope, at, count);
    }
}

/// One statement of `shape` in a body judged in `scope`.
pub(crate) fn statement(
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
pub(crate) fn trait_block(rng: &mut Rng, index: u32, shapes: &[Shape], most: u32) -> Drawn<Block> {
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
pub(crate) fn chain_block(
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
pub(crate) enum Seen {
    /// `pub`.
    Public,
    /// `pub(crate)`.
    Crate,
    /// No visibility keyword.
    Private,
}

impl Seen {
    /// The keyword this visibility writes before `fn`.
    const fn keyword(self) -> &'static str {
        match self {
            Self::Public => "pub ",
            Self::Crate => "pub(crate) ",
            Self::Private => "",
        }
    }
}

/// Visibilities for a mixed draw, weighted toward the one the rule judges.
pub(crate) const SEEN: [Seen; 5] = [
    Seen::Public,
    Seen::Public,
    Seen::Public,
    Seen::Crate,
    Seen::Private,
];

/// What a documented function looks like.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DocSpec {
    /// Who can see it.
    pub(crate) seen: Seen,
    /// Whether it is a method in an inherent `impl`.
    pub(crate) method: bool,
    /// Whether it carries `#[test]`.
    pub(crate) test: bool,
    /// The fewest content tokens its doc carries.
    pub(crate) fewest: u32,
    /// The most content tokens its doc carries.
    pub(crate) most: u32,
    /// Whether every content token is a stem of the name.
    pub(crate) paraphrase: bool,
    /// The most stop words mixed in.
    pub(crate) stops: u32,
}

/// A documented function shaped by `spec`, and the finding the model expects
/// for it.
pub(crate) fn doc_block(rng: &mut Rng, index: u32, spec: DocSpec) -> Drawn<Block> {
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
pub(crate) fn filler_block(rng: &mut Rng) -> Drawn<Block> {
    let mut block = Block::default();
    for _ in 0..rng.between(1, 3) {
        block.line(*rng.pick_named("filler lines", &FILLER_LINES)?);
    }
    Ok(block)
}

// ── Files and sweeps ────────────────────────────────────────────────────────

/// A block of any kind, every knob drawn.
pub(crate) fn mixed_block(rng: &mut Rng, index: u32) -> Drawn<Block> {
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
pub(crate) fn mixed_blocks(rng: &mut Rng) -> Drawn<Vec<Block>> {
    (0..rng.between(4, 12))
        .map(|index| mixed_block(rng, index))
        .collect()
}

/// `blocks` in their own order, with zero to two comment lines before each.
pub(crate) fn laid_out(rng: &mut Rng, blocks: &[Block]) -> Drawn<File> {
    let order: Vec<usize> = (0..blocks.len()).collect();
    let gaps: Vec<u32> = order.iter().map(|_| rng.below(3)).collect();
    assemble(blocks, &order, &gaps)
}

/// The seed of the mixed family's `index`-th scenario.
pub(crate) const fn mixed_seed(index: u64) -> u64 {
    0x5ca7_0000_0000_0000 ^ index.wrapping_mul(SPREAD)
}

/// One seed's mixed file.
pub(crate) fn mixed_file(seed: u64) -> Drawn<File> {
    let mut rng = Rng::new(seed);
    let blocks = mixed_blocks(&mut rng)?;
    laid_out(&mut rng, &blocks)
}

/// One mixed seed folded into a trace hash: the file and every finding.
pub(crate) fn mixed_receipt(seed: u64) -> Drawn<u64> {
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
pub(crate) fn band_seeds(band: u64) -> Drawn<Range<u64>> {
    let per = MIXED_SEEDS
        .checked_div(BANDS)
        .ok_or("the family is cut into at least one band")?;
    let first = band.saturating_mul(per);
    Ok(first..first.saturating_add(per))
}

/// How many findings of `rule` a list holds.
pub(crate) fn tally(found: &[Finding], rule: &str) -> usize {
    found.iter().filter(|finding| finding.1 == rule).count()
}

/// One band: every file agrees with the model and replays to one trace, and
/// the band reaches every rule.
pub(crate) fn mixed_band(band: u64) -> TestResult {
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

/// Sweeps [`SEEDS`] seeds from `base`: each draws its blocks with `make`, lays
/// them out with drawn gaps, and must agree with the model.
pub(crate) fn sweep(
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
pub(crate) fn several(
    rng: &mut Rng,
    mut one: impl FnMut(&mut Rng, u32) -> Drawn<Block>,
) -> Drawn<Vec<Block>> {
    (0..rng.between(1, 4))
        .map(|index| one(rng, index))
        .collect()
}

/// Every finding of `rule` across a sweep.
pub(crate) fn swept_tally(swept: &[(File, Vec<Finding>)], rule: &str) -> usize {
    swept.iter().map(|swept| tally(&swept.1, rule)).sum()
}

/// Whether some swept line holds exactly `count` `?` operators, so a family
/// can show it reached the boundary it claims to test.
pub(crate) fn some_line_holds(swept: &[(File, Vec<Finding>)], count: usize) -> bool {
    swept.iter().any(|swept| {
        let file = &swept.0;
        file.source
            .lines()
            .any(|line| line.matches('?').count() == count)
    })
}

/// Whether some swept line holds more than three `?` operators.
pub(crate) fn some_line_exceeds(swept: &[(File, Vec<Finding>)]) -> bool {
    (4..=12).any(|count| some_line_holds(swept, count))
}
