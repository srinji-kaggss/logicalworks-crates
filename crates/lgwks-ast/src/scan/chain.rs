//! Detector long-try-chain: more than three `?` operators in one non-`let`
//! statement, outside test functions.
//!
//! A statement is a child of a block: an expression statement, a tail
//! expression, an item. The innermost enclosing function decides whether it is
//! judged and which line the finding names.

use tree_sitter::Node;

use super::cfg;
use super::syntax::Fielded;
use super::{Hit, syntax};

/// Above this many `?` operators in one non-`let` statement, the chain is a
/// finding. Three is the reference threshold and is contractual: a chain of
/// exactly this length passes, one longer fails.
const MAX_TRY_CHAIN: usize = 3;

/// Runs the long-try-chain detector over a whole file.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    let mut walker = TryChainWalker {
        source,
        hits,
        current_fn_line: None,
        current_fn_skipped: false,
    };
    walker.walk(root);
}

/// Walk state: the accumulated findings plus the innermost function being
/// visited and whether it is exempt.
struct TryChainWalker<'a> {
    /// The text the tree was parsed from.
    source: &'a str,
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
    /// Line of the innermost enclosing function's name, or `None` outside any
    /// function. The snippet names this line so the reader can find the chain.
    current_fn_line: Option<usize>,
    /// True when the innermost function was entered through an exempt
    /// attribute, in which case none of its statements are inspected.
    current_fn_skipped: bool,
}

impl TryChainWalker<'_> {
    /// Visit a node and everything under it.
    fn walk(&mut self, node: Node<'_>) {
        match node.kind() {
            "function_item" => {
                let previous = self.enter_function(node);
                self.walk_children(node);
                self.exit_function(previous);
            }
            "block" => {
                self.inspect_block(node);
                self.walk_children(node);
            }
            _ => self.walk_children(node),
        }
    }

    /// Visit every child of a node.
    fn walk_children(&mut self, node: Node<'_>) {
        for child in syntax::kids(node) {
            self.walk(child);
        }
    }

    /// Marks the start of a function body and returns the state it displaced,
    /// for [`Self::exit_function`] to restore.
    ///
    /// The innermost function decides. An exempt one marks the walk skipped until
    /// it ends, so none of its statements is inspected even when it is nested in
    /// a production function; a production function nested in an exempt one is
    /// inspected under its own line. The displaced line stays the `Option` it is
    /// (`None` means there was no enclosing function), so a top-level statement
    /// is not attributed to a function that does not exist.
    fn enter_function(&mut self, function: Node<'_>) -> (Option<usize>, bool) {
        let previous = (self.current_fn_line, self.current_fn_skipped);
        if cfg::is_test_or_helper(self.source, function) {
            self.current_fn_skipped = true;
        } else {
            let named = match function.by_field("name") {
                Some(name) => name,
                None => function,
            };
            self.current_fn_line = Some(syntax::line(named));
            self.current_fn_skipped = false;
        }
        previous
    }

    /// Restores the state [`Self::enter_function`] displaced.
    const fn exit_function(&mut self, previous: (Option<usize>, bool)) {
        (self.current_fn_line, self.current_fn_skipped) = previous;
    }

    /// Reports a finding for every statement of a block that carries more than
    /// [`MAX_TRY_CHAIN`] `?` operators.
    ///
    /// Does nothing when no inspectable function encloses the block (code
    /// outside any function, or inside an exempt one), because the snippet names
    /// the enclosing function's line and there is none to name.
    fn inspect_block(&mut self, block: Node<'_>) {
        let fn_line = match self.current_fn_line {
            Some(line) if !self.current_fn_skipped => line,
            _ => return,
        };
        for statement in syntax::named_kids(block) {
            if matches!(
                statement.kind(),
                "attribute_item" | "inner_attribute_item" | "label" | "empty_statement"
            ) {
                continue;
            }
            let try_count = count_tries_in_statement(statement);
            if try_count > MAX_TRY_CHAIN {
                // A statement starts where its first attribute does.
                let start = match syntax::attributes(statement).first() {
                    Some(attribute) => syntax::line(*attribute),
                    None => syntax::line(statement),
                };
                self.hits.push(Hit::new(
                    "long-try-chain",
                    start,
                    format!(
                        "fn near line {fn_line} — statement contains {try_count} `?` operators without an intermediate `let`. Break the chain with named intermediate values."
                    ),
                ));
            }
        }
    }
}

/// Counts the `?` operators in a statement's own expression.
///
/// A `let` statement answers zero by construction: the detector's contract is
/// that binding an intermediate with `let` breaks the chain, so an initializer
/// full of `?` is the recommended shape rather than a finding. Any other
/// statement is counted over its own expression, which covers `for` / `while` /
/// `match` statements whose headers can carry a `?`.
fn count_tries_in_statement(statement: Node<'_>) -> usize {
    match statement.kind() {
        "let_declaration" => 0,
        "expression_statement" => syntax::named_kids(statement)
            .into_iter()
            .map(count_tries)
            .fold(0, usize::saturating_add),
        _ => count_tries(statement),
    }
}

/// Counts the `?` operators under a node.
///
/// Closure bodies are not descended into: a `?` inside a closure belongs to the
/// closure's own control flow, not to the statement being counted. Nested blocks
/// are not descended into either: each statement inside one is inspected on its
/// own, so counting it here as well would charge a loop with every `?` in its
/// body, including those under a `let`, which this detector's contract says
/// breaks the chain. What stays counted is the statement's own expression: a
/// `for` iterator, a `while` or `if` condition, a `match` scrutinee and an arm
/// written without braces.
fn count_tries(node: Node<'_>) -> usize {
    match node.kind() {
        "closure_expression" | "block" => 0,
        "try_expression" => syntax::named_kids(node)
            .into_iter()
            .map(count_tries)
            .fold(1, usize::saturating_add),
        _ => syntax::named_kids(node)
            .into_iter()
            .map(count_tries)
            .fold(0, usize::saturating_add),
    }
}
