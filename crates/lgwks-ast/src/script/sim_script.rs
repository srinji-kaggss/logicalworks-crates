//! Seeded simulations of the `script!` parser (#391).
//!
//! One seed draws whole scripts and every family asserts a property of the
//! real `lines::split`, `lines::tree`, `refuse::check` and [`parse`] against a
//! model the family keeps itself:
//!
//! - the line tree is the one an independent indentation model predicts — line
//!   number, column, token count, block and children — with calls split across
//!   lines inside their brackets;
//! - a line moved off every block column is refused at that line;
//! - every script the grammar generator draws, which between them say every
//!   word of the lexicon, parses (the macro writes Rust for any tree, so this
//!   is the language accepting it);
//! - a refused construct planted at any depth is refused at its own token with
//!   the replacement in the message, and still refused nested inside brackets;
//! - a step's label does not move when an unrelated line is inserted above it;
//! - the same seed replays to the same trace and distinct seeds diverge.
//!
//! The stream is `lgwks_std::seeded::Seeded` (INV-STD-SEEDED-1), so a seed names
//! the same scripts on every target, and the trace folds through that same
//! stream rather than a second hash. Each `TokenStream::from_str` adds its text
//! to `proc_macro2`'s per-thread source map, so a family's memory is bounded by
//! its seed count times one script, a few kilobytes each.

use std::str::FromStr;

use lgwks_deps::proc_macro2::{TokenStream, TokenTree};
use lgwks_std::seeded::Seeded;

use super::lexicon::{Kind, LEXICON};
use super::lines::{self, Node};
use super::tree::{self, Block, BranchValue, Call, Code, Construct, Fragment, Statement};
use super::{Refusal, parse, refuse};

/// Columns per indentation level.
const INDENT: usize = 4;

/// Seeds in one band; a band is one `#[test]`.
const SEEDS_PER_BAND: u64 = 256;

/// The deepest block the script generator opens.
const MAX_DEPTH: usize = 4;

/// Where the tree families' seeds start.
const TREE_BASE: u64 = 0x0391_7ee5_0000_0000;

/// Where the script families' seeds start.
const SCRIPT_BASE: u64 = 0x0391_5c41_0000_0000;

/// The seeded stream, with draws that answer a refusal as the family's error.
struct Draw(Seeded);

impl Draw {
    /// The stream `seed` names.
    fn new(seed: u64) -> Self {
        Self(Seeded::from_seed(seed))
    }

    /// An index below `len`.
    fn index(&mut self, len: usize) -> Result<usize, String> {
        self.0
            .index(len)
            .map_err(|refusal| format!("a draw below {len} was refused: {refusal}"))
    }

    /// True one time in `out_of`.
    fn one_in(&mut self, out_of: u64) -> Result<bool, String> {
        let drawn = self
            .0
            .below(out_of)
            .map_err(|refusal| format!("a draw below {out_of} was refused: {refusal}"))?;
        Ok(drawn == 0)
    }

    /// A count in `low..=high`.
    fn between(&mut self, low: usize, high: usize) -> Result<usize, String> {
        let width = high.saturating_sub(low).saturating_add(1);
        Ok(low.saturating_add(self.index(width)?))
    }

    /// One item of `from`.
    fn pick<'item, T>(&mut self, from: &'item [T]) -> Result<&'item T, String> {
        let at = self.index(from.len())?;
        from.get(at)
            .ok_or_else(|| format!("index {at} was drawn past {} items", from.len()))
    }
}

/// What a family observed, folded through the seeded stream.
#[derive(Default)]
struct Trace(u64);

impl Trace {
    /// Fold one word.
    fn word(&mut self, value: u64) {
        self.0 = Seeded::from_seed(self.0.rotate_left(29) ^ value).next_u64();
    }

    /// Fold a count, byte by byte, so every bit of it reaches the trace on
    /// every target width.
    fn count(&mut self, value: usize) {
        for byte in value.to_le_bytes() {
            self.word(u64::from(byte));
        }
    }

    /// Fold a text and its length.
    fn text(&mut self, text: &str) {
        self.count(text.len());
        for byte in text.bytes() {
            self.word(u64::from(byte));
        }
    }
}

/// Tokenize `source` the way a macro receives it.
fn stream(source: &str) -> Result<TokenStream, String> {
    TokenStream::from_str(source).map_err(|error| format!("does not tokenize: {error}\n{source}"))
}

// ── The indentation model ─────────────────────────────────────────────────

/// A statement of the tree model and the statements indented beneath it.
struct Shape {
    /// The statement as written, without a block `:`; it may span lines
    /// inside its brackets.
    text: String,
    /// What it opens; a header when non-empty.
    children: Vec<Shape>,
}

/// Token shapes the splitter treats differently: a path (`::`, which is never a
/// block colon), groups, an operator, literals, and a call whose arguments run
/// onto the next line, which is still one logical line.
const PIECES: [&str; 9] = [
    "a",
    "call(x)",
    "x::y",
    "k = 1",
    "&v",
    "1u8",
    "\"s\"",
    "pair(a,\nb)",
    "{ x }",
];

/// Draw a forest up to four levels deep and three wide at each level.
fn shapes(draw: &mut Draw, depth: usize) -> Result<Vec<Shape>, String> {
    let width = draw.between(1, 3)?;
    let mut drawn = Vec::with_capacity(width);
    for _ in 0..width {
        let pieces = draw.between(1, 3)?;
        let mut words = Vec::with_capacity(pieces);
        for _ in 0..pieces {
            words.push(*draw.pick(&PIECES)?);
        }
        let children = if depth < 3 && draw.one_in(3)? {
            shapes(draw, depth.saturating_add(1))?
        } else {
            Vec::new()
        };
        drawn.push(Shape {
            text: words.join(" "),
            children,
        });
    }
    Ok(drawn)
}

/// One logical line, as the model places it and as the tree reads it back.
#[derive(PartialEq, Eq, Debug)]
struct Row {
    /// Its first source line, counted from one.
    line: usize,
    /// The column of its first token.
    column: usize,
    /// Its tokens, a block `:` excluded.
    tokens: usize,
    /// Whether it opens a block.
    opens: bool,
    /// How many lines its block holds directly.
    children: usize,
}

/// One logical line of a laid-out forest and where it was written.
struct Placed {
    /// What the tree must read back for it.
    row: Row,
    /// The physical line it starts on, counted from zero.
    physical: usize,
    /// Whether it is the first line of its block, which sets the block's column
    /// rather than having to match it.
    first: bool,
}

/// A forest written out as script text.
#[derive(Default)]
struct Layout {
    /// The physical lines, indentation included.
    physical: Vec<String>,
    /// Every logical line, depth first.
    placed: Vec<Placed>,
}

/// Write `shapes` at `depth` into `out`. The model's token count comes from the
/// tokenizer alone, so neither `split` nor `tree` takes part in it.
fn layout(shapes: &[Shape], depth: usize, out: &mut Layout) -> Result<(), String> {
    let column = depth.saturating_mul(INDENT);
    let continuation = column.saturating_add(INDENT.saturating_mul(2));
    for (position, shape) in shapes.iter().enumerate() {
        let start = out.physical.len();
        let last = shape.text.matches('\n').count();
        let colon = if shape.children.is_empty() { "" } else { ":" };
        for (index, piece) in shape.text.split('\n').enumerate() {
            let pad = if index == 0 { column } else { continuation };
            let end = if index == last { colon } else { "" };
            out.physical
                .push(format!("{}{piece}{end}", " ".repeat(pad)));
        }
        out.placed.push(Placed {
            row: Row {
                line: start.saturating_add(1),
                column,
                tokens: stream(&shape.text)?.into_iter().count(),
                opens: !shape.children.is_empty(),
                children: shape.children.len(),
            },
            physical: start,
            first: position == 0,
        });
        layout(&shape.children, depth.saturating_add(1), out)?;
    }
    Ok(())
}

/// Every node the tree built, depth first, in the model's terms.
fn rows(nodes: &[Node], out: &mut Vec<Row>) {
    for node in nodes {
        out.push(Row {
            line: node.line.number,
            column: node.line.column,
            tokens: node.line.tokens.len(),
            opens: node.line.opens_block,
            children: node.children.len(),
        });
        rows(&node.children, out);
    }
}

/// Draw the forest `seed` names and lay it out.
fn laid_out(seed: u64) -> Result<(Draw, Layout), String> {
    let mut draw = Draw::new(seed);
    let mut laid = Layout::default();
    layout(&shapes(&mut draw, 0)?, 0, &mut laid)?;
    Ok((draw, laid))
}

/// The tree `split` and `tree` build is the one the model laid out.
fn tree_family(first: u64, count: u64) -> Result<u64, String> {
    let mut trace = Trace::default();
    for offset in 0..count {
        let seed = TREE_BASE.wrapping_add(first).wrapping_add(offset);
        let (_, laid) = laid_out(seed)?;
        let source = laid.physical.join("\n");
        let nodes = lines::tree(lines::split(stream(&source)?))
            .map_err(|error| format!("seed {seed:#x}: refused ({error}):\n{source}"))?;
        let mut read = Vec::new();
        rows(&nodes, &mut read);
        if !read.iter().eq(laid.placed.iter().map(|placed| &placed.row)) {
            let model: Vec<&Row> = laid.placed.iter().map(|placed| &placed.row).collect();
            let refusal = format!(
                "seed {seed:#x}: the model laid out {model:?} and the tree read {read:?}:\n{source}"
            );
            tracing::debug!(error = %refusal, "tree_family: returning an error to the caller");
            return Err(refusal);
        }
        for row in &read {
            trace.count(row.line);
            trace.count(row.column);
            trace.count(row.tokens);
            trace.count(row.children);
            trace.word(u64::from(row.opens));
        }
    }
    Ok(trace.0)
}

/// A line moved two columns off its block's column is refused, and the
/// refusal names that line.
fn misalignment_family(first: u64, count: u64) -> Result<u64, String> {
    let mut trace = Trace::default();
    for offset in 0..count {
        let seed = TREE_BASE.wrapping_add(first).wrapping_add(offset);
        let (mut draw, laid) = laid_out(seed)?;
        let movable: Vec<&Placed> = laid.placed.iter().filter(|placed| !placed.first).collect();
        if movable.is_empty() {
            trace.word(0);
            continue;
        }
        let moved = *draw.pick(&movable)?;
        let deeper = moved.row.column < INDENT || draw.one_in(2)?;
        let mut physical = laid.physical.clone();
        let line = physical
            .get_mut(moved.physical)
            .ok_or_else(|| format!("seed {seed:#x}: no physical line {}", moved.physical))?;
        let shifted = if deeper {
            format!("  {line}")
        } else {
            line.strip_prefix("  ")
                .ok_or_else(|| format!("seed {seed:#x}: `{line}` has no two columns to lose"))?
                .to_owned()
        };
        *line = shifted;
        let source = physical.join("\n");
        match lines::tree(lines::split(stream(&source)?)) {
            Ok(_) => {
                let refusal = format!(
                    "seed {seed:#x}: line {} moved off its column and was accepted:\n{source}",
                    moved.row.line
                );
                tracing::debug!(error = %refusal, "misalignment_family: returning an error to the caller");
                return Err(refusal);
            }
            Err(error) => {
                let named = error.span().start().line;
                if named != moved.row.line {
                    let refusal = format!(
                        "seed {seed:#x}: line {} moved, the refusal names line {named} \
                         ({error}):\n{source}",
                        moved.row.line
                    );
                    tracing::debug!(error = %refusal, "misalignment_family: returning an error to the caller");
                    return Err(refusal);
                }
                trace.count(named);
                trace.word(u64::from(deeper));
            }
        }
    }
    Ok(trace.0)
}

// ── The grammar generator ─────────────────────────────────────────────────

/// One logical line of a generated script.
struct Src {
    /// Its indentation level.
    depth: usize,
    /// The line, its block `:` included; continuation lines follow `\n`.
    text: String,
    /// The word whose block it opens, if it is a header.
    opens: Option<Kind>,
}

/// A script drawn from the grammar, and every word it said.
struct Script {
    /// The stream that drew it, which the families keep drawing from.
    draw: Draw,
    /// Its logical lines, in order.
    lines: Vec<Src>,
    /// The last name handed out.
    names: u32,
    /// Every word the script says.
    said: Vec<Kind>,
}

impl Script {
    /// The script `seed` names: one or two flows.
    fn drawn(seed: u64) -> Result<Self, String> {
        let mut script = Self {
            draw: Draw::new(seed),
            lines: Vec::new(),
            names: 0,
            said: Vec::new(),
        };
        let flows = script.draw.between(1, 2)?;
        for _ in 0..flows {
            script.flow()?;
        }
        Ok(script)
    }

    /// A number no other name in this script carries.
    fn name(&mut self) -> u32 {
        self.names = self.names.saturating_add(1);
        self.names
    }

    /// Record that the script says `kind`.
    fn say(&mut self, kind: Kind) {
        if !self.said.contains(&kind) {
            self.said.push(kind);
        }
    }

    /// Append one line.
    fn line(&mut self, depth: usize, text: String, opens: Option<Kind>) {
        if let Some(kind) = opens {
            self.say(kind);
        }
        self.lines.push(Src { depth, text, opens });
    }

    /// A flow that ends by giving back a value.
    fn flow(&mut self) -> Result<(), String> {
        let name = self.name();
        self.line(
            0,
            format!("flow walk{name}(items: Vec<u8>, site: &Site) -> usize:"),
            Some(Kind::Flow),
        );
        self.statements(1)?;
        self.say(Kind::GiveBack);
        self.line(1, "give back items.len()".to_owned(), None);
        Ok(())
    }

    /// One to three statements at `depth`.
    fn statements(&mut self, depth: usize) -> Result<(), String> {
        let count = self.draw.between(1, 3)?;
        for _ in 0..count {
            let choice = if depth >= MAX_DEPTH {
                self.draw.index(2)?
            } else {
                self.draw.index(8)?
            };
            let written = match choice {
                0 => self.plain(depth),
                1 => self.run(depth),
                2 | 3 => self.value(depth),
                4 => self.nested(depth, "step part", Kind::Step),
                5 => self.nested(depth, "for item in items", Kind::For),
                6 => self.choice(depth),
                _ => self.together(depth),
            };
            written?;
        }
        Ok(())
    }

    /// A line of plain Rust, sometimes a call whose arguments run onto the
    /// next lines.
    fn plain(&mut self, depth: usize) -> Result<(), String> {
        let name = self.name();
        let text = match self.draw.index(3)? {
            0 => format!("let a{name} = site.get({name})"),
            1 => format!("let a{name} = site.pair(\n{name},\nitems.len())"),
            _ => format!("site.note({name}).await.or_retry()?"),
        };
        self.line(depth, text, None);
        Ok(())
    }

    /// A call of another flow.
    fn run(&mut self, depth: usize) -> Result<(), String> {
        let name = self.name();
        self.say(Kind::Run);
        let text = if self.draw.one_in(2)? {
            format!("let r{name} = run probe(site)")
        } else {
            "run probe(site)".to_owned()
        };
        self.line(depth, text, None);
        Ok(())
    }

    /// A value block bound with `let`.
    fn value(&mut self, depth: usize) -> Result<(), String> {
        let name = self.name();
        let (header, kind) = *self.draw.pick(&[
            ("each item in items", Kind::Each),
            (
                "each item in items, at most (site.quota()) at once",
                Kind::Each,
            ),
            ("within 2s", Kind::Within),
            ("retry up to 3 times", Kind::Retry),
            ("retry up to 3 times, waiting 100ms", Kind::Retry),
        ])?;
        self.say(Kind::Let);
        self.line(depth, format!("let v{name} = {header}:"), Some(kind));
        let inner = depth.saturating_add(1);
        if self.draw.one_in(2)? {
            self.plain(inner)?;
        }
        self.line(inner, format!("site.size({name})"), None);
        Ok(())
    }

    /// A block of statements under `header`, named when it is a step.
    fn nested(&mut self, depth: usize, header: &str, kind: Kind) -> Result<(), String> {
        let text = if kind == Kind::Step {
            format!("{header}{}:", self.name())
        } else {
            format!("{header}:")
        };
        self.line(depth, text, Some(kind));
        self.statements(depth.saturating_add(1))
    }

    /// An `if` chain, with or without `else if` and `else`.
    fn choice(&mut self, depth: usize) -> Result<(), String> {
        let name = self.name();
        let inner = depth.saturating_add(1);
        self.line(depth, format!("if items.len() > {name}:"), Some(Kind::If));
        self.branch(inner)?;
        if self.draw.one_in(2)? {
            self.line(
                depth,
                format!("else if items.len() == {name}:"),
                Some(Kind::Else),
            );
            self.branch(inner)?;
        }
        if self.draw.one_in(2)? {
            self.line(depth, "else:".to_owned(), Some(Kind::Else));
            self.branch(inner)?;
        }
        Ok(())
    }

    /// The body of one branch: a failure, or statements.
    fn branch(&mut self, depth: usize) -> Result<(), String> {
        if self.draw.one_in(3)? {
            self.say(Kind::Fail);
            let text = if self.draw.one_in(2)? {
                "fail with \"too many\""
            } else {
                "fail transiently with \"busy\""
            };
            self.line(depth, text.to_owned(), None);
            Ok(())
        } else {
            self.statements(depth)
        }
    }

    /// Two or three branches run together.
    fn together(&mut self, depth: usize) -> Result<(), String> {
        self.line(depth, "together:".to_owned(), Some(Kind::Together));
        let inner = depth.saturating_add(1);
        let branches = self.draw.between(2, 3)?;
        for _ in 0..branches {
            let name = self.name();
            let text = if self.draw.one_in(2)? {
                self.say(Kind::Run);
                format!("let t{name} = run probe(site)")
            } else {
                format!("let t{name} = site.ping({name}).await.or_retry()?")
            };
            self.line(inner, text, None);
        }
        Ok(())
    }

    /// Every header a statement may be inserted under as its first line.
    /// `together:` is left out: each line under it is a branch, not a
    /// statement.
    fn openers(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|&(_, src)| src.opens.is_some_and(|kind| kind != Kind::Together))
            .map(|(index, _)| index)
            .collect()
    }

    /// Insert `text` as the first line of the block header `at` opens, and
    /// return its indentation level.
    fn insert_under(&mut self, at: usize, text: String) -> Result<usize, String> {
        let depth = self
            .lines
            .get(at)
            .map(|header| header.depth.saturating_add(1))
            .ok_or_else(|| format!("no line {at} to insert under"))?;
        let tail = self.lines.split_off(at.saturating_add(1));
        self.lines.push(Src {
            depth,
            text,
            opens: None,
        });
        self.lines.extend(tail);
        Ok(depth)
    }

    /// The script as text, and the source line each logical line starts on.
    fn render(&self) -> (String, Vec<usize>) {
        let mut physical: Vec<String> = Vec::new();
        let mut starts = Vec::with_capacity(self.lines.len());
        for src in &self.lines {
            starts.push(physical.len().saturating_add(1));
            let column = src.depth.saturating_mul(INDENT);
            let continuation = column.saturating_add(INDENT.saturating_mul(2));
            for (index, piece) in src.text.split('\n').enumerate() {
                let pad = if index == 0 { column } else { continuation };
                physical.push(format!("{}{piece}", " ".repeat(pad)));
            }
        }
        (physical.join("\n"), starts)
    }
}

/// Read `source` as the macro does: the tree, or the refusal with its span.
fn read(source: &str) -> Result<Result<tree::Script, Refusal>, String> {
    Ok(parse(stream(source)?))
}

/// The tree of a script the family expects to be accepted.
fn accepted(seed: u64, source: &str) -> Result<tree::Script, String> {
    read(source)?.map_err(|refusal| format!("seed {seed:#x}: refused ({refusal}):\n{source}"))
}

/// Every generated script parses, and its map names every step it wrote.
fn expansion_family(first: u64, count: u64) -> Result<u64, String> {
    let mut trace = Trace::default();
    for offset in 0..count {
        let seed = SCRIPT_BASE.wrapping_add(first).wrapping_add(offset);
        let script = Script::drawn(seed)?;
        let (source, _) = script.render();
        let read = accepted(seed, &source)?;
        for flow in read.flows() {
            trace.text(flow.shape().signature());
            trace.count(flow.shape().steps().len());
        }
        for label in labels(&read) {
            trace.text(&label);
        }
        trace.count(script.lines.len());
    }
    Ok(trace.0)
}

// ── Refusals ──────────────────────────────────────────────────────────────

/// A construct the language refuses, as one line of a flow.
struct Plant {
    /// The line, without its indentation.
    line: &'static str,
    /// The text the refusal is spanned at: its first occurrence in `line`.
    at: &'static str,
    /// The part of the refusal that names what to write instead.
    says: &'static str,
}

/// How a planted value line starts; what follows it can be nested in brackets.
const LET: &str = "let planted = ";

/// Every defect class `refuse.rs` names, one line each.
static PLANTS: [Plant; 16] = [
    Plant {
        line: "let planted = items.first().unwrap()",
        at: "unwrap",
        says: "`.unwrap()` ends the program",
    },
    Plant {
        line: "let planted = Option::expect(items.first(), \"x\")",
        at: "expect",
        says: "`.expect()` ends the program",
    },
    Plant {
        line: "let planted = panic!(\"stop\")",
        at: "panic",
        says: "`panic!` ends the program",
    },
    Plant {
        line: "let planted = todo!()",
        at: "todo",
        says: "`todo!` ends the program",
    },
    Plant {
        line: "let planted = items[3]",
        at: "[",
        says: "indexing ends the program",
    },
    Plant {
        line: "let planted = site.load(\"/home/me/data\")",
        at: "\"/home",
        says: "an absolute path from one machine",
    },
    Plant {
        line: "let planted = loop { break 1 }",
        at: "loop",
        says: "a loop with no bound can run forever",
    },
    Plant {
        line: "let planted = while ready { }",
        at: "while",
        says: "a loop with no bound can run forever",
    },
    Plant {
        line: "let planted = std::thread::spawn(work)",
        at: "spawn",
        says: "a spawned task has no owner",
    },
    Plant {
        line: "let planted = std::thread::sleep(pause)",
        at: "sleep",
        says: "`thread::sleep` stalls every sibling",
    },
    Plant {
        line: "let planted = std::process::exit(1)",
        at: "exit",
        says: "ending the process from a flow",
    },
    Plant {
        line: "let planted = std::mem::forget(items)",
        at: "forget",
        says: "`mem::forget` leaks",
    },
    Plant {
        line: "let planted = unsafe { site.raw() }",
        at: "unsafe",
        says: "a script never needs `unsafe`",
    },
    Plant {
        line: "let planted = executor::block_on(work)",
        at: "block_on",
        says: "`block_on` inside a flow stalls",
    },
    Plant {
        line: "let planted = channel::unbounded_channel()",
        at: "unbounded_channel",
        says: "an unbounded channel grows",
    },
    Plant {
        line: "use std::thread::sleep",
        at: "thread",
        says: "`use` of `thread` inside a flow",
    },
];

/// Brackets a refused construct can be nested in; none of them indexes.
const WRAPPERS: [(&str, &str); 4] = [("outer(", ")"), ("{ ", " }"), ("vec![", "]"), ("(", ")")];

/// The line to plant and the column, from its first character, its refusal
/// must be spanned at: `plant` as written, or nested one to three brackets
/// deep when `nest` is set.
fn planted_line(draw: &mut Draw, plant: &Plant, nest: bool) -> Result<(String, usize), String> {
    let missing = || format!("`{}` does not contain `{}`", plant.line, plant.at);
    let Some(rest) = plant.line.strip_prefix(LET).filter(|_| nest) else {
        return Ok((
            plant.line.to_owned(),
            plant.line.find(plant.at).ok_or_else(missing)?,
        ));
    };
    let depth = draw.between(1, 3)?;
    let mut opening = String::new();
    let mut closing = Vec::with_capacity(depth);
    for _ in 0..depth {
        let &(open, close) = draw.pick(&WRAPPERS)?;
        opening.push_str(open);
        closing.push(close);
    }
    closing.reverse();
    let column = LET
        .len()
        .saturating_add(opening.len())
        .saturating_add(rest.find(plant.at).ok_or_else(missing)?);
    Ok((format!("{LET}{opening}{rest}{}", closing.concat()), column))
}

/// A refused construct planted at a drawn depth is refused at its own token,
/// with the replacement in the message — through the whole expansion and
/// through `refuse::check` on the line alone. Returns the trace and which
/// plants were drawn.
fn planting(first: u64, count: u64, nest: bool) -> Result<(u64, Vec<usize>), String> {
    let mut trace = Trace::default();
    let mut drawn = Vec::new();
    for offset in 0..count {
        let seed = SCRIPT_BASE.wrapping_add(first).wrapping_add(offset);
        let mut script = Script::drawn(seed)?;
        let points = script.openers();
        let under = *script.draw.pick(&points)?;
        let which = script.draw.index(PLANTS.len())?;
        let plant = PLANTS
            .get(which)
            .ok_or_else(|| format!("no plant {which}"))?;
        if !drawn.contains(&which) {
            drawn.push(which);
        }
        let (line, offset_in_line) = planted_line(&mut script.draw, plant, nest)?;
        let depth = script.insert_under(under, line.clone())?;
        let (source, starts) = script.render();
        let line_number = *starts
            .get(under.saturating_add(1))
            .ok_or_else(|| format!("seed {seed:#x}: the planted line has no start"))?;
        let column = depth.saturating_mul(INDENT).saturating_add(offset_in_line);
        let Err(error) = read(&source)? else {
            let refusal = format!("seed {seed:#x}: `{line}` was accepted:\n{source}");
            tracing::debug!(error = %refusal, "planting: returning an error to the caller");
            return Err(refusal);
        };
        spanned_at(seed, &error, (line_number, column), plant.says, &source)?;
        let alone: Vec<TokenTree> = stream(&line)?.into_iter().collect();
        let Err(direct) = refuse::check(&alone) else {
            let refusal = format!("seed {seed:#x}: refuse::check accepted `{line}` alone");
            tracing::debug!(error = %refusal, "planting: returning an error to the caller");
            return Err(refusal);
        };
        spanned_at(seed, &direct, (1, offset_in_line), plant.says, &line)?;
        trace.count(line_number);
        trace.count(column);
        trace.text(&error.to_string());
    }
    Ok((trace.0, drawn))
}

/// The refusal is spanned at `(line, column)` and names the replacement.
fn spanned_at(
    seed: u64,
    error: &Refusal,
    (line, column): (usize, usize),
    says: &str,
    source: &str,
) -> Result<(), String> {
    let start = error.span().start();
    let message = error.to_string();
    if (start.line, start.column) != (line, column) || !message.contains(says) {
        let refusal = format!(
            "seed {seed:#x}: expected `{says}` at {line}:{column}, the refusal was `{message}` \
             at {}:{}:\n{source}",
            start.line, start.column
        );
        tracing::debug!(error = %refusal, "spanned_at: returning an error to the caller");
        return Err(refusal);
    }
    Ok(())
}

/// [`planting`] at the planted line's own depth.
fn plant_family(first: u64, count: u64) -> Result<u64, String> {
    planting(first, count, false).map(|(trace, _)| trace)
}

/// [`planting`] with the construct nested inside brackets.
fn nested_plant_family(first: u64, count: u64) -> Result<u64, String> {
    planting(first, count, true).map(|(trace, _)| trace)
}

// ── Step labels ───────────────────────────────────────────────────────────

/// Every label the tree gives a scope, depth first in source order: each
/// block's, and each `run` call's.
fn labels(script: &tree::Script) -> Vec<String> {
    let mut out = Vec::new();
    for flow in script.flows() {
        block_labels(flow.body(), &mut out);
    }
    out
}

/// The labels under `block`.
fn block_labels(block: &Block, out: &mut Vec<String>) {
    for statement in block.statements() {
        match *statement {
            Statement::Rust(ref code)
            | Statement::Let(ref code)
            | Statement::GiveBack(ref code) => {
                code_labels(code, out);
            }
            Statement::Fail { ref reason, .. } => code_labels(reason, out),
            Statement::Run(ref call) => call_labels(call, out),
            Statement::Bind { ref block, .. } | Statement::Construct(ref block) => {
                construct_labels(block, out);
            }
            Statement::Together(ref together) => {
                for branch in together.branches() {
                    match *branch.value() {
                        BranchValue::Construct(ref construct) => construct_labels(construct, out),
                        BranchValue::Run(ref call) => call_labels(call, out),
                        BranchValue::Rust(ref code) => code_labels(code, out),
                    }
                }
            }
            Statement::For(ref each) => {
                out.push(each.label().to_owned());
                code_labels(each.items(), out);
                block_labels(each.body(), out);
            }
            Statement::If(ref chain) => {
                for branch in chain.branches() {
                    if let Some(condition) = branch.condition() {
                        code_labels(condition, out);
                    }
                    block_labels(branch.body(), out);
                }
            }
        }
    }
}

/// The labels of one block construct and everything under it.
fn construct_labels(construct: &Construct, out: &mut Vec<String>) {
    match *construct {
        Construct::Each(ref each) => {
            out.push(each.label().to_owned());
            code_labels(each.items(), out);
            block_labels(each.body(), out);
        }
        Construct::Within(ref within) => {
            out.push(within.label().to_owned());
            block_labels(within.body(), out);
        }
        Construct::Retry(ref again) => {
            out.push(again.label().to_owned());
            block_labels(again.body(), out);
        }
        Construct::Step(ref step) => {
            out.push(step.label().to_owned());
            block_labels(step.body(), out);
        }
    }
}

/// The labels of the `run` calls in a line of Rust.
fn code_labels(code: &Code, out: &mut Vec<String>) {
    for fragment in code.fragments() {
        match *fragment {
            Fragment::Token(_) => {}
            Fragment::Group { ref inner, .. } => code_labels(inner, out),
            Fragment::Run(ref call) => call_labels(call, out),
        }
    }
}

/// A call's label, then the labels of the calls in its arguments.
fn call_labels(call: &Call, out: &mut Vec<String>) {
    out.push(call.label().to_owned());
    code_labels(call.arguments(), out);
}

/// A line inserted above a step changes none of the script's step labels.
fn label_family(first: u64, count: u64) -> Result<u64, String> {
    let mut trace = Trace::default();
    let mut seen = 0_usize;
    for offset in 0..count {
        let seed = SCRIPT_BASE.wrapping_add(first).wrapping_add(offset);
        let mut script = Script::drawn(seed)?;
        let (source, _) = script.render();
        let before = labels(&accepted(seed, &source)?);
        let points = script.openers();
        let under = *script.draw.pick(&points)?;
        let name = script.name();
        script.insert_under(under, format!("let unrelated{name} = {name}"))?;
        let (edited, _) = script.render();
        let after = labels(&accepted(seed, &edited)?);
        if before != after {
            let refusal = format!(
                "seed {seed:#x}: an unrelated line moved the labels {before:?} to \
                 {after:?}:\n{edited}"
            );
            tracing::debug!(error = %refusal, "label_family: returning an error to the caller");
            return Err(refusal);
        }
        seen = seen.saturating_add(before.len());
        for label in &before {
            trace.text(label);
        }
    }
    if seen == 0 {
        let refusal =
            "no script in the band entered a labelled scope, so nothing was tested".to_owned();
        tracing::debug!(error = %refusal, "label_family: returning an error to the caller");
        return Err(refusal);
    }
    Ok(trace.0)
}

// ── The families as tests ─────────────────────────────────────────────────

/// One `#[test]` per band of a family, so a failing band names itself.
macro_rules! bands {
    ($family:ident: $($test:ident = $band:literal),+ $(,)?) => {
        $(
            #[test]
            fn $test() -> Result<(), String> {
                $family(SEEDS_PER_BAND.saturating_mul($band), SEEDS_PER_BAND).map(drop)
            }
        )+
    };
}

bands!(tree_family:
    sim_the_line_tree_matches_the_indentation_model_band_00 = 0,
    sim_the_line_tree_matches_the_indentation_model_band_01 = 1,
    sim_the_line_tree_matches_the_indentation_model_band_02 = 2,
    sim_the_line_tree_matches_the_indentation_model_band_03 = 3,
);

bands!(misalignment_family:
    sim_a_misaligned_line_is_refused_at_its_own_line_band_00 = 0,
    sim_a_misaligned_line_is_refused_at_its_own_line_band_01 = 1,
);

bands!(expansion_family:
    sim_every_generated_script_expands_band_00 = 0,
    sim_every_generated_script_expands_band_01 = 1,
);

bands!(plant_family:
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_00 = 0,
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_01 = 1,
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_02 = 2,
    sim_a_planted_refusal_is_spanned_at_its_own_token_band_03 = 3,
);

bands!(nested_plant_family:
    sim_a_refusal_nested_in_brackets_is_still_refused_band_00 = 0,
    sim_a_refusal_nested_in_brackets_is_still_refused_band_01 = 1,
);

bands!(label_family:
    sim_step_labels_survive_a_line_inserted_above_band_00 = 0,
    sim_step_labels_survive_a_line_inserted_above_band_01 = 1,
);

#[test]
fn sim_one_band_of_scripts_says_every_word() -> Result<(), String> {
    let mut said: Vec<Kind> = Vec::new();
    for offset in 0..SEEDS_PER_BAND {
        let script = Script::drawn(SCRIPT_BASE.wrapping_add(offset))?;
        for kind in script.said {
            if !said.contains(&kind) {
                said.push(kind);
            }
        }
    }
    let unsaid: Vec<&str> = LEXICON
        .iter()
        .filter(|row| !said.contains(&row.kind()))
        .map(|row| row.kind().spelling())
        .collect();
    if unsaid.is_empty() {
        Ok(())
    } else {
        Err(format!("no script in the band says {unsaid:?}"))
    }
}

#[test]
fn sim_one_band_plants_every_refusal() -> Result<(), String> {
    let (_, drawn) = planting(0, SEEDS_PER_BAND, false)?;
    let missing: Vec<&str> = PLANTS
        .iter()
        .enumerate()
        .filter(|&(index, _)| !drawn.contains(&index))
        .map(|(_, plant)| plant.line)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("the band never planted {missing:?}"))
    }
}

/// Every family, over `count` seeds from `first`, as one trace.
fn every_family(first: u64, count: u64) -> Result<Vec<u64>, String> {
    Ok(vec![
        tree_family(first, count)?,
        misalignment_family(first, count)?,
        expansion_family(first, count)?,
        plant_family(first, count)?,
        nested_plant_family(first, count)?,
        label_family(first, count)?,
    ])
}

#[test]
fn sim_the_same_seed_replays_to_the_same_trace() -> Result<(), String> {
    let once = every_family(0, 64)?;
    let again = every_family(0, 64)?;
    if once == again {
        Ok(())
    } else {
        Err(format!(
            "one seed range traced {once:x?} and then {again:x?}"
        ))
    }
}

#[test]
fn sim_distinct_seeds_diverge_in_every_family() -> Result<(), String> {
    let low = every_family(0, 64)?;
    let high = every_family(64, 64)?;
    let same: Vec<usize> = low
        .iter()
        .zip(&high)
        .enumerate()
        .filter(|&(_, (left, right))| left == right)
        .map(|(family, _)| family)
        .collect();
    if same.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "families {same:?} traced two seed ranges identically"
        ))
    }
}

#[test]
fn sim_four_thousand_seeds_hold_every_property() -> Result<(), String> {
    every_family(1_048_576, 4096).map(drop)
}
