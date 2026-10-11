//! Whether a closure body reads the error binding its parameter introduced.
//!
//! The evidence that an error was used is a value read of that binding,
//! resolved lexically: a path naming it while no enclosing scope has rebound
//! the name. Three things are not evidence. A declaration (`let _error = ..`)
//! is a second binding. A spelling (a field, a method, a path segment, plain
//! text in a macro) touches no value. A discard (`drop(error)`, `error;`,
//! `let _ = error;`) reads the binding and keeps nothing, which is the loss
//! the rule names. Shapes the walk cannot resolve fall toward silence, never
//! toward a false accusation.

use super::{
    ExprPath, Node, Owner, block_statements, call_arguments, first_named, is_comment, owner_of,
    statement_expression,
};

/// True when `body` reads the binding `name`.
pub(super) fn observes(body: &Node<'_>, name: &str) -> bool {
    let mut reads = Reads {
        name,
        shadowed: false,
        observed: false,
    };
    reads.visit(body);
    reads.observed
}

/// The name a pattern binds when it binds exactly one identifier at its top:
/// `x`, `mut x`, `ref x`, `x @ ..`, `syn`'s `Pat::Ident`.
pub(super) fn bound_name(pattern: &Node<'_>) -> Option<String> {
    match &*pattern.kind() {
        "identifier" => Some(pattern.text().into_owned()),
        "mut_pattern" | "ref_pattern" => {
            let inner = pattern
                .children()
                .filter(|child| child.is_named() && child.kind() != "mutable_specifier")
                .last()?;
            bound_name(&inner)
        }
        "captured_pattern" => first_named(pattern)
            .filter(|binder| binder.kind() == "identifier")
            .map(|binder| binder.text().into_owned()),
        _ => None,
    }
}

/// Kinds a value read can never sit under: types, attributes, declarations
/// and the signature parts of an item.
fn is_opaque(kind: &str) -> bool {
    kind.ends_with("_type")
        || matches!(
            kind,
            "type_identifier"
                | "scoped_type_identifier"
                | "removed_trait_bound"
                | "type_arguments"
                | "type_parameters"
                | "where_clause"
                | "trait_bounds"
                | "parameters"
                | "closure_parameters"
                | "visibility_modifier"
                | "function_modifiers"
                | "mutable_specifier"
                | "attribute_item"
                | "inner_attribute_item"
                | "use_declaration"
                | "extern_crate_declaration"
                | "macro_definition"
                | "lifetime"
                | "label"
                | "line_comment"
                | "block_comment"
        )
}

/// The walk's state.
struct Reads<'name> {
    /// The binding the closure parameter introduced.
    name: &'name str,
    /// True while an enclosing scope has rebound the name.
    shadowed: bool,
    /// Set once a preserving read is seen.
    observed: bool,
}

impl Reads<'_> {
    /// Visit `node` with the binding hidden while `rebinds` holds.
    fn scoped(&mut self, rebinds: bool, node: Option<Node<'_>>) {
        let Some(node) = node else {
            return;
        };
        let outer = self.shadowed;
        self.shadowed = outer || rebinds;
        self.visit(&node);
        self.shadowed = outer;
    }

    /// True when `node` is this binding as a value and nothing else.
    fn is_binding(&self, node: &Node<'_>) -> bool {
        node.kind() == "identifier" && node.text() == self.name
    }

    /// Visit one node in expression or statement position.
    fn visit(&mut self, node: &Node<'_>) {
        if self.observed {
            return;
        }
        match &*node.kind() {
            "identifier" => {
                if !self.shadowed && self.is_binding(node) {
                    self.observed = true;
                }
            }
            // A longer path, or a path with generic arguments, names something
            // other than the binding; a method turbofish still has a receiver.
            "scoped_identifier" => {}
            "generic_function" => {
                if let Some(callee) = node
                    .field("function")
                    .filter(|callee| callee.kind() == "field_expression")
                {
                    self.visit(&callee);
                }
            }
            "field_expression" => self.scoped(false, node.field("value")),
            "macro_invocation" => {
                if !self.shadowed
                    && node
                        .children()
                        .find(|child| child.kind() == "token_tree")
                        .is_some_and(|tokens| macro_reads(&tokens, self.name))
                {
                    self.observed = true;
                }
            }
            "block" => self.visit_block(node),
            "call_expression" => {
                if !self.is_drop_of_binding(node) {
                    self.descend(node);
                }
            }
            "closure_expression" => {
                let rebinds = node.field("parameters").is_some_and(|parameters| {
                    parameters
                        .children()
                        .any(|parameter| self.parameter_binds(&parameter))
                });
                self.scoped(rebinds, node.field("body"));
            }
            "for_expression" => {
                self.scoped(false, node.field("value"));
                let rebinds = node
                    .field("pattern")
                    .is_some_and(|pattern| pattern_binds(&pattern, self.name));
                self.scoped(rebinds, node.field("body"));
            }
            "if_expression" | "while_expression" => self.visit_conditional(node),
            "let_condition" | "let_declaration" => {
                self.scoped(false, node.field("value"));
                self.scoped(false, node.field("alternative"));
            }
            "match_arm" => self.visit_arm(node),
            "function_item" => self.visit_function(node),
            kind if is_opaque(kind) => {}
            _ => self.descend(node),
        }
    }

    /// Visit every child that can hold a value read: not a type, not a
    /// pattern, not the name an item declares.
    fn descend(&mut self, node: &Node<'_>) {
        let skipped: Vec<usize> = ["name", "pattern", "type", "return_type", "alias", "trait"]
            .iter()
            .filter_map(|field| node.field(field))
            .map(|child| child.node_id())
            .collect();
        for child in node.children() {
            if child.is_named() && !skipped.contains(&child.node_id()) {
                self.visit(&child);
            }
        }
    }

    /// A `let` binds from the statement after it, so its value and `else`
    /// block are read under the state before it and the rest under the
    /// shadow.
    fn visit_block(&mut self, block: &Node<'_>) {
        let outer = self.shadowed;
        for statement in block_statements(block) {
            let rebinding = statement.kind() == "let_declaration"
                && statement
                    .field("pattern")
                    .is_some_and(|pattern| pattern_binds(&pattern, self.name));
            if rebinding {
                self.scoped(false, statement.field("value"));
                self.scoped(false, statement.field("alternative"));
                self.shadowed = true;
            } else if !self.is_discard(&statement) {
                self.visit(&statement);
            }
        }
        self.shadowed = outer;
    }

    /// `if let` and `while let` bind in the body they guard, and in the
    /// `else` too, as `syn`'s visitor scoped it; a let chain does not rebind.
    fn visit_conditional(&mut self, node: &Node<'_>) {
        let condition = node.field("condition");
        let rebinds = condition.as_ref().is_some_and(|condition| {
            condition.kind() == "let_condition"
                && condition
                    .field("pattern")
                    .is_some_and(|pattern| pattern_binds(&pattern, self.name))
        });
        self.scoped(false, condition);
        let outer = self.shadowed;
        self.shadowed = outer || rebinds;
        for field in ["consequence", "body", "alternative"] {
            if let Some(branch) = node.field(field) {
                self.visit(&branch);
            }
        }
        self.shadowed = outer;
    }

    /// A match arm's pattern binds in its guard and its value.
    fn visit_arm(&mut self, arm: &Node<'_>) {
        let pattern = arm.field("pattern");
        let guard = pattern
            .as_ref()
            .and_then(|pattern| pattern.field("condition"));
        let guard_id = guard.as_ref().map(|guard| guard.node_id());
        // The arm's pattern is the match pattern's first child that is not
        // its guard; a `_` pattern is anonymous and binds nothing.
        let rebinds = pattern.as_ref().is_some_and(|pattern| {
            pattern.children().any(|bound| {
                bound.is_named()
                    && !is_comment(&bound)
                    && Some(bound.node_id()) != guard_id
                    && pattern_binds(&bound, self.name)
            })
        });
        let outer = self.shadowed;
        self.shadowed = outer || rebinds;
        if let Some(guard) = guard {
            self.visit(&guard);
        }
        if let Some(value) = arm.field("value") {
            self.visit(&value);
        }
        self.shadowed = outer;
    }

    /// A nested free function's parameters bind in its body. An `impl` or
    /// trait method was walked by `syn` without that scope, and is here.
    fn visit_function(&mut self, function: &Node<'_>) {
        let rebinds = owner_of(function) == Owner::Free
            && function.field("parameters").is_some_and(|parameters| {
                parameters
                    .children()
                    .filter(|parameter| parameter.kind() == "parameter")
                    .any(|parameter| self.parameter_binds(&parameter))
            });
        self.scoped(rebinds, function.field("body"));
    }

    /// True when one closure or function parameter binds the name.
    fn parameter_binds(&self, parameter: &Node<'_>) -> bool {
        if parameter.kind() == "parameter" {
            return parameter
                .field("pattern")
                .is_some_and(|pattern| pattern_binds(&pattern, self.name));
        }
        parameter.is_named() && !is_comment(parameter) && pattern_binds(parameter, self.name)
    }

    /// True for a statement whose whole effect is to drop the value: `error;`
    /// or `let _ = error;`.
    fn is_discard(&self, statement: &Node<'_>) -> bool {
        if self.shadowed {
            return false;
        }
        match &*statement.kind() {
            "expression_statement" => {
                statement_expression(statement).is_some_and(|inner| self.is_binding(&inner))
            }
            "let_declaration" => {
                statement
                    .field("pattern")
                    .is_some_and(|pattern| pattern.kind() == "_")
                    && statement.field("type").is_none()
                    && statement
                        .field("value")
                        .is_some_and(|value| self.is_binding(&value))
            }
            _ => false,
        }
    }

    /// True for `drop(<binding>)` through any path ending in `drop`: the read
    /// is real, and it keeps nothing.
    fn is_drop_of_binding(&self, call: &Node<'_>) -> bool {
        if self.shadowed {
            return false;
        }
        let names_drop = call
            .field("function")
            .and_then(|callee| ExprPath::read(&callee))
            .is_some_and(|path| path.last() == Some("drop"));
        if !names_drop {
            return false;
        }
        let Some(arguments) = call.field("arguments") else {
            return false;
        };
        matches!(*call_arguments(&arguments).as_slice(), [ref only] if self.is_binding(only))
    }
}

/// True when a pattern introduces a binding named `name`, through tuples,
/// structs, slices, references and or-patterns. A shape this walk does not
/// model answers `false` and so is not taken for a shadow.
pub(super) fn pattern_binds(pattern: &Node<'_>, name: &str) -> bool {
    match &*pattern.kind() {
        "identifier" => pattern.text() == name,
        "captured_pattern" | "mut_pattern" | "ref_pattern" => {
            bound_name(pattern).is_some_and(|bound| bound == name)
        }
        "tuple_pattern" | "slice_pattern" | "or_pattern" | "reference_pattern" => pattern
            .children()
            .any(|element| element.is_named() && pattern_binds(&element, name)),
        "tuple_struct_pattern" => {
            let constructor = pattern.field("type").map(|path| path.node_id());
            pattern.children().any(|element| {
                element.is_named()
                    && Some(element.node_id()) != constructor
                    && pattern_binds(&element, name)
            })
        }
        "struct_pattern" => pattern
            .children()
            .filter(|field| field.kind() == "field_pattern")
            .any(|field| match field.field("pattern") {
                Some(inner) => pattern_binds(&inner, name),
                None => field
                    .field("name")
                    .is_some_and(|short| short.text() == name),
            }),
        _ => false,
    }
}

/// True when a macro's tokens read `name` as a value.
///
/// Only two spellings count: an identifier token not reached through `.` or
/// `:` (a field, method or path segment), and an inline format capture,
/// `{name}` or `{name:..}`, inside a string literal. Plain text in a literal
/// names the binding and touches nothing. A `let` that rebinds the name inside
/// a macro's block is not tracked, which over-credits a read: silence, the
/// direction this rule fails in.
fn macro_reads(tokens: &Node<'_>, name: &str) -> bool {
    let mut previous: Option<Node<'_>> = None;
    for token in tokens.children() {
        if is_comment(&token)
            || (!token.is_named() && matches!(&*token.kind(), "(" | ")" | "[" | "]" | "{" | "}"))
        {
            continue;
        }
        if token_reads(&token, previous.as_ref(), name) {
            return true;
        }
        previous = Some(token);
    }
    false
}

/// True when one macro token reads `name`: the identifier itself where no
/// access punctuation precedes it, a nested group that reads it, or a string
/// literal that captures it.
fn token_reads(token: &Node<'_>, previous: Option<&Node<'_>>, name: &str) -> bool {
    match &*token.kind() {
        "identifier" => token.text() == name && !is_access_prefix(previous),
        "token_tree" => macro_reads(token, name),
        literal if literal.ends_with("string_literal") => {
            let text = token.text();
            text.contains(&format!("{{{name}}}")) || text.contains(&format!("{{{name}:"))
        }
        _ => false,
    }
}

/// True when the token before an identifier makes it a field, method or path
/// segment: punctuation ending in `.` or `:` (`.`, `..`, `:`, `::`).
fn is_access_prefix(previous: Option<&Node<'_>>) -> bool {
    previous.is_some_and(|token| !token.is_named() && token.text().ends_with(['.', ':']))
}
