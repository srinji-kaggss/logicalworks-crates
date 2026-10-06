//! Property tests over the line splitter and the indentation tree (#273).
//!
//! A generated tree of statements is rendered as indented script text, read
//! back through `split` and `tree`, and must come back as the same tree: the
//! same statements, token for token, under the same headers. A generated tree
//! with one line moved to a column no block owns must be refused, and the
//! refusal must name that line. A failing tree shrinks to the smallest one.

#[path = "../../lgwks-std/tests/support/prop.rs"]
mod prop;

use std::str::FromStr;

use lgwks_deps::proc_macro2::TokenStream;
use proptest::collection::vec;
use proptest::prelude::{Strategy, any};
use proptest::test_runner::TestCaseError;

use crate::lines::{self, Line, Node};
use prop::{Outcome, check, setup};

/// The seed every run starts from. Changing it is a new corpus, not a retry.
const SEED: u64 = 0x2730_c0de_c5ee_d005;

/// Columns per indentation level.
const INDENT: usize = 4;

/// One statement and the statements indented beneath it.
#[derive(Debug, Clone)]
struct Shape {
    /// The statement as written, without a block `:`.
    statement: String,
    /// What it opens; a header when non-empty.
    children: Vec<Shape>,
}

/// Statements built from token shapes the splitter treats differently: paths
/// (`::`, which must not read as a block colon), groups, operators, literals.
fn statements() -> impl Strategy<Value = String> {
    let word = proptest::sample::select(&["a", "b", "call(x)", "x::y", "k = 1", "&v", "1u8"][..]);
    vec(word, 1..4).prop_map(|words| words.join(" "))
}

/// Trees up to three levels deep and eight nodes wide.
fn shapes() -> impl Strategy<Value = Vec<Shape>> {
    let leaf = statements().prop_map(|statement| Shape {
        statement,
        children: Vec::new(),
    });
    let tree = leaf.prop_recursive(3, 24, 4, |inner| {
        (statements(), vec(inner, 1..4)).prop_map(|(statement, children)| Shape {
            statement,
            children,
        })
    });
    vec(tree, 1..4)
}

/// One rendered line: its text, its column, and whether it is the first line
/// of its block (a first line sets its block's column, so moving it moves the
/// block rather than misaligning a line).
struct Rendered {
    /// The full line, indentation included.
    text: String,
    /// Its column.
    column: usize,
    /// Whether it opens its block's column.
    first_in_block: bool,
}

/// Render `shapes` at `depth`, appending to `out`.
fn render(shapes: &[Shape], depth: usize, out: &mut Vec<Rendered>) {
    for (position, shape) in shapes.iter().enumerate() {
        let column = depth.saturating_mul(INDENT);
        let colon = if shape.children.is_empty() { "" } else { ":" };
        out.push(Rendered {
            text: format!("{}{}{colon}", " ".repeat(column), shape.statement),
            column,
            first_in_block: position == 0,
        });
        render(&shape.children, depth.saturating_add(1), out);
    }
}

/// Read script text the way the macro does.
fn read(source: &str) -> Result<Vec<Node>, TestCaseError> {
    let stream = setup(TokenStream::from_str(source))?;
    lines::tree(lines::split(stream))
        .map_err(|error| TestCaseError::fail(format!("refused: {error}\n{source}")))
}

/// The tree as (statement text, children), comparable with a [`Shape`].
fn project(nodes: &[Node]) -> Vec<(String, Vec<(String, usize)>)> {
    nodes
        .iter()
        .map(|node| {
            (
                lines::text(&node.line.tokens),
                node.children
                    .iter()
                    .map(|child| (lines::text(&child.line.tokens), child.children.len()))
                    .collect(),
            )
        })
        .collect()
}

/// The same projection, of the generated shapes.
fn expected(shapes: &[Shape]) -> Vec<(String, Vec<(String, usize)>)> {
    shapes
        .iter()
        .map(|shape| {
            (
                shape.statement.clone(),
                shape
                    .children
                    .iter()
                    .map(|child| (child.statement.clone(), child.children.len()))
                    .collect(),
            )
        })
        .collect()
}

/// Every node, depth first, as (text, opens a block, child count).
fn flatten(nodes: &[Node], out: &mut Vec<(String, bool, usize)>) {
    for node in nodes {
        out.push((
            lines::text(&node.line.tokens),
            node.line.opens_block,
            node.children.len(),
        ));
        flatten(&node.children, out);
    }
}

/// Every shape, depth first, in the same form as [`flatten`].
fn flatten_shapes(shapes: &[Shape], out: &mut Vec<(String, bool, usize)>) {
    for shape in shapes {
        out.push((
            shape.statement.clone(),
            !shape.children.is_empty(),
            shape.children.len(),
        ));
        flatten_shapes(&shape.children, out);
    }
}

#[test]
fn a_rendered_tree_reads_back_as_the_same_tree() -> Outcome {
    prop::runner(SEED, 256, file!()).run(&shapes(), |shapes| {
        let mut rendered = Vec::new();
        render(&shapes, 0, &mut rendered);
        let source: Vec<String> = rendered.into_iter().map(|line| line.text).collect();
        let nodes = read(&source.join("\n"))?;
        check(project(&nodes) == expected(&shapes), || {
            format!("{:?} read back as {:?}", expected(&shapes), project(&nodes))
        })?;
        let (mut read_back, mut written) = (Vec::new(), Vec::new());
        flatten(&nodes, &mut read_back);
        flatten_shapes(&shapes, &mut written);
        check(read_back == written, || {
            format!("wrote {written:?}, read {read_back:?}")
        })
    })?;
    Ok(())
}

/// A tree builder under test, over already-split lines.
type Build = fn(Vec<Line>) -> lgwks_deps::syn::Result<Vec<Node>>;

/// One line moved two columns off its block's column must be refused, and
/// the refusal must name that line.
fn misalignment_property(
    build: Build,
    shapes: &[Shape],
    which: usize,
    deeper: bool,
) -> Result<(), TestCaseError> {
    let mut rendered = Vec::new();
    render(shapes, 0, &mut rendered);
    let movable: Vec<usize> = rendered
        .iter()
        .enumerate()
        .filter(|&(_, line)| !line.first_in_block && (deeper || line.column >= INDENT))
        .map(|(index, _)| index)
        .collect();
    let Some(&moved) = which
        .checked_rem(movable.len())
        .and_then(|at| movable.get(at))
    else {
        return Ok(());
    };
    let mut source: Vec<String> = Vec::with_capacity(rendered.len());
    for (index, line) in rendered.iter().enumerate() {
        let written = match (index == moved, deeper) {
            (true, true) => format!("  {}", line.text),
            // Moving a line shallower means dropping two columns of its indent,
            // which is the two leading spaces the renderer put there. A line
            // without them is a generator defect rather than a shape, and the
            // refusal says so instead of shortening the line to whatever prefix
            // it happens to carry.
            (true, false) => match line.text.strip_prefix("  ") {
                Some(shifted) => shifted.to_owned(),
                None => {
                    return Err(TestCaseError::fail(format!(
                        "line {} is `{}`, which carries no two-column indent to move it by",
                        moved.saturating_add(1),
                        line.text
                    )));
                }
            },
            (false, _) => line.text.clone(),
        };
        source.push(written);
    }
    let text = source.join("\n");
    let stream = setup(TokenStream::from_str(&text))?;
    match build(lines::split(stream)) {
        Ok(_) => Err(TestCaseError::fail(format!(
            "line {} moved off its column and was accepted:\n{text}",
            moved.saturating_add(1)
        ))),
        Err(error) => {
            let named = error.span().start().line;
            check(named == moved.saturating_add(1), || {
                format!(
                    "line {} was moved but the refusal names line {named} ({error}):\n{text}",
                    moved.saturating_add(1)
                )
            })
        }
    }
}

/// Misalignment cases: a tree, which movable line, and which direction.
fn misalignments() -> impl Strategy<Value = (Vec<Shape>, usize, bool)> {
    (shapes(), any::<usize>(), proptest::bool::ANY)
}

#[test]
fn a_line_off_every_block_column_is_refused_at_that_line() -> Outcome {
    prop::runner(SEED, 256, file!()).run(&misalignments(), |(shapes, which, deeper)| {
        misalignment_property(lines::tree, &shapes, which, deeper)
    })?;
    Ok(())
}

#[test]
fn the_property_catches_a_builder_that_snaps_lines_to_the_grid() -> Outcome {
    /// Rounds every column down to a multiple of the indent before building:
    /// a misaligned line is silently re-homed instead of refused.
    fn snapping(mut split: Vec<Line>) -> lgwks_deps::syn::Result<Vec<Node>> {
        for line in &mut split {
            // Rounding a column down to a multiple of the indent is arithmetic
            // on the column, and a divisor of zero is not a rounding at all, so
            // a zero `INDENT` is refused where it is read rather than answered
            // with a column of zero.
            let over = match line.column.checked_rem(INDENT) {
                Some(over) => over,
                None => {
                    let refusal = Err(lgwks_deps::syn::Error::new(
                        line.span,
                        format!("the indentation unit is {INDENT}, and zero is not an indent"),
                    ));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "snapping: returning an error to the caller");
                    return refusal;
                }
            };
            line.column = line.column.saturating_sub(over);
        }
        lines::tree(split)
    }
    let minimal = prop::shrunk(SEED, &misalignments(), |(shapes, which, deeper)| {
        misalignment_property(snapping, &shapes, which, deeper)
    })?;
    let mut rendered = Vec::new();
    render(&minimal.0, 0, &mut rendered);
    // `deeper` shrinks to false first, and a line moved shallower needs a
    // column of at least one indent: a header, its first child, and the
    // second child moved off the grid.
    assert_eq!(
        (rendered.len(), minimal.2),
        (3, false),
        "a header and two children, the second moved shallower: {minimal:?}"
    );
    Ok(())
}
