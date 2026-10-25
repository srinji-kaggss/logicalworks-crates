//! Whether a closure body observes the error binding its parameter introduced.
//!
//! The evidence for "the error was used" is a **value read of that binding**,
//! resolved lexically: an identifier in expression position naming it while no
//! enclosing scope has rebound the name. Three things are deliberately not
//! evidence:
//!
//! - A *declaration*. `let _error = Vec::new();` introduces a second binding,
//!   and reading that one observes a different value entirely.
//! - A *spelling*. A field name, a method name, a path segment, or text inside a
//!   macro never touches the parameter's value.
//! - A *discard*. `drop(error)` and a bare `error;` read the binding and
//!   preserve nothing, which is the loss the ERROR-SWALLOW detector exists to
//!   name.
//!
//! Constructs the walk cannot resolve fail toward silence rather than toward a
//! false accusation: a macro whose tokens name the binding is credited, and a
//! pattern whose shape is not modelled is not treated as a shadow.

use tree_sitter::Node;

use super::syntax::Fielded;
use super::syntax::{self, Path, Tok};

/// Whether `body` reads the binding `name` introduced by a closure parameter.
pub(super) fn observes(source: &str, body: Node<'_>, name: &str) -> bool {
    let mut reads = Reads {
        source,
        name,
        shadowed: false,
        observed: false,
    };
    reads.visit(body);
    reads.observed
}

/// Walk state: the text, the name, and the two facts the walk keeps.
struct Reads<'a> {
    /// The text the tree was parsed from.
    source: &'a str,
    /// Identifier the closure parameter introduced.
    name: &'a str,
    /// True once an enclosing scope has rebound `name`, so an identifier with
    /// that spelling reads the other binding rather than this one.
    shadowed: bool,
    /// Set once a preserving read of the binding is seen.
    observed: bool,
}

impl Reads<'_> {
    /// Visit a node and everything under it, under the shadowing rules of the
    /// construct it is.
    fn visit(&mut self, node: Node<'_>) {
        if self.observed {
            return;
        }
        match node.kind() {
            "identifier" => {
                if !self.shadowed && self.is_binding(node) && is_value_position(node) {
                    self.observed = true;
                }
            }
            "macro_invocation" => {
                if !self.shadowed && self.macro_reads_binding(node) {
                    self.observed = true;
                }
            }
            "block" => self.visit_block(node),
            "expression_statement" => {
                if !self.is_discard(node) {
                    self.visit_children(node);
                }
            }
            "let_declaration" => {
                if !self.is_discarding_let(node) {
                    self.visit_children(node);
                }
            }
            "call_expression" => {
                if !self.is_drop_of_binding(node) {
                    self.visit_children(node);
                }
            }
            "closure_expression" => {
                let rebinds = node.by_field("parameters").is_some_and(|parameters| {
                    syntax::named_kids(parameters)
                        .into_iter()
                        .any(|pattern| self.binds(pattern))
                });
                if let Some(body) = node.by_field("body") {
                    self.scoped(rebinds, |reads| reads.visit(body));
                }
            }
            "for_expression" => {
                if let Some(iterated) = node.by_field("value") {
                    self.visit(iterated);
                }
                let rebinds = self.field_binds(node, "pattern");
                if let Some(body) = node.by_field("body") {
                    self.scoped(rebinds, |reads| reads.visit(body));
                }
            }
            "if_expression" => self.visit_branch(node, true),
            "while_expression" => self.visit_branch(node, false),
            "match_arm" => self.visit_arm(node),
            "function_item" => self.visit_function(node),
            _ => self.visit_children(node),
        }
    }

    /// Visit every named child.
    fn visit_children(&mut self, node: Node<'_>) {
        for child in syntax::named_kids(node) {
            self.visit(child);
        }
    }

    /// Visit a nested scope, hiding the binding while `rebinds` is set.
    fn scoped(&mut self, rebinds: bool, body: impl FnOnce(&mut Self)) {
        let outer = self.shadowed;
        if rebinds {
            self.shadowed = true;
        }
        body(self);
        self.shadowed = outer;
    }

    /// Whether an identifier node spells the binding.
    fn is_binding(&self, node: Node<'_>) -> bool {
        syntax::text(self.source, node) == self.name
    }

    /// Whether a pattern introduces the binding.
    fn binds(&self, pattern: Node<'_>) -> bool {
        pattern_binds(self.source, pattern, self.name)
    }

    /// Whether the pattern in `field` of `node` introduces the binding.
    fn field_binds(&self, node: Node<'_>, field: &'static str) -> bool {
        node.by_field(field)
            .is_some_and(|pattern| self.binds(pattern))
    }

    /// A `let` binds from the statement after it onward, so its initializer is
    /// visited under the state in force before the binding exists, and every
    /// later statement of the block under the shadowed state.
    fn visit_block(&mut self, block: Node<'_>) {
        let outer = self.shadowed;
        for statement in syntax::named_kids(block) {
            if statement.kind() == "let_declaration" && self.field_binds(statement, "pattern") {
                for field in ["value", "alternative"] {
                    if let Some(init) = statement.by_field(field) {
                        self.visit(init);
                    }
                }
                self.shadowed = true;
                continue;
            }
            self.visit(statement);
        }
        self.shadowed = outer;
    }

    /// An `if` or `while`: the condition is visited unshadowed, and a bare
    /// `let PAT = ..` condition binds in the branch it guards, not before it.
    /// A `let` inside a `&&` chain is not that shape and rebinds nothing here.
    fn visit_branch(&mut self, node: Node<'_>, has_else: bool) {
        let Some(condition) = node.by_field("condition") else {
            return;
        };
        self.visit(condition);
        let rebinds = condition.kind() == "let_condition" && self.field_binds(condition, "pattern");
        let guarded = node.by_field(if has_else { "consequence" } else { "body" });
        let alternative = node.by_field("alternative").filter(|_| has_else);
        self.scoped(rebinds, |reads| {
            if let Some(block) = guarded {
                reads.visit(block);
            }
            if let Some(other) = alternative {
                reads.visit(other);
            }
        });
    }

    /// A match arm's pattern binds in its guard and its body.
    fn visit_arm(&mut self, arm: Node<'_>) {
        let pattern = arm.by_field("pattern");
        let rebinds = pattern
            .and_then(|found| syntax::named_kids(found).into_iter().next())
            .is_some_and(|first| self.binds(first));
        let guard = pattern.and_then(|found| found.by_field("condition"));
        let value = arm.by_field("value");
        self.scoped(rebinds, |reads| {
            if let Some(found) = guard {
                reads.visit(found);
            }
            if let Some(found) = value {
                reads.visit(found);
            }
        });
    }

    /// A nested function's parameter binds in its own body. A method of an
    /// `impl` or a trait is read as any other node is.
    fn visit_function(&mut self, function: Node<'_>) {
        if syntax::is_in_impl(function) || syntax::is_in_trait(function) {
            self.visit_children(function);
            return;
        }
        let rebinds = function.by_field("parameters").is_some_and(|parameters| {
            syntax::named_kids(parameters)
                .into_iter()
                .any(|parameter| self.binds(parameter))
        });
        if let Some(body) = function.by_field("body") {
            self.scoped(rebinds, |reads| reads.visit(body));
        }
    }

    /// True for a statement whose whole effect is to drop the value: a bare
    /// `error;`.
    fn is_discard(&self, statement: Node<'_>) -> bool {
        if self.shadowed || !syntax::kids(statement).iter().any(|kid| kid.kind() == ";") {
            return false;
        }
        match syntax::named_kids(statement).as_slice() {
            &[only] => only.kind() == "identifier" && self.is_binding(only),
            _ => false,
        }
    }

    /// True for `let _ = error;`, with no written type.
    fn is_discarding_let(&self, statement: Node<'_>) -> bool {
        if self.shadowed || statement.by_field("type").is_some() {
            return false;
        }
        let wildcard = statement
            .by_field("pattern")
            .is_some_and(|pattern| pattern.kind() == "_");
        wildcard
            && statement
                .by_field("value")
                .is_some_and(|init| init.kind() == "identifier" && self.is_binding(init))
    }

    /// True for `drop(<binding>)`, including a qualified `std::mem::drop`. The
    /// read is real, but the cause is not made observable by it, so it does not
    /// clear the finding.
    fn is_drop_of_binding(&self, call: Node<'_>) -> bool {
        if self.shadowed {
            return false;
        }
        let dropped = call
            .by_field("function")
            .and_then(|function| Path::of(self.source, function))
            .is_some_and(|path| path.last() == Some("drop"));
        if !dropped {
            return false;
        }
        let Some(arguments) = call.by_field("arguments") else {
            return false;
        };
        let values: Vec<Node<'_>> = syntax::named_kids(arguments)
            .into_iter()
            .filter(|argument| argument.kind() != "attribute_item")
            .collect();
        match values.as_slice() {
            &[only] => only.kind() == "identifier" && self.is_binding(only),
            _ => false,
        }
    }

    /// Credit a macro's arguments if they read the binding.
    fn macro_reads_binding(&self, invocation: Node<'_>) -> bool {
        syntax::macro_tokens(invocation)
            .is_some_and(|tree| tokens_read(&syntax::tokens(self.source, tree), self.name))
    }
}

/// Whether an identifier is in a position where it is a value: not part of a
/// longer path, not a pattern, not the name of a declaration, and not a path
/// with generic arguments.
fn is_value_position(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "scoped_identifier"
        | "scoped_type_identifier"
        | "scoped_use_list"
        | "use_list"
        | "use_as_clause"
        | "use_declaration"
        | "use_wildcard"
        | "attribute"
        | "attribute_item"
        | "inner_attribute_item"
        | "macro_invocation"
        | "token_tree"
        | "label"
        | "lifetime"
        | "closure_parameters"
        | "parameters"
        | "parameter"
        | "tuple_pattern"
        | "tuple_struct_pattern"
        | "struct_pattern"
        | "field_pattern"
        | "slice_pattern"
        | "ref_pattern"
        | "reference_pattern"
        | "mut_pattern"
        | "or_pattern"
        | "captured_pattern"
        | "range_pattern"
        | "generic_pattern"
        | "generic_function"
        | "visibility_modifier"
        | "function_item"
        | "function_signature_item"
        | "mod_item"
        | "extern_crate_declaration"
        | "macro_definition"
        | "const_parameter" => false,
        "let_declaration" | "let_condition" | "for_expression" => {
            !syntax::is_field(parent, "pattern", node)
        }
        "match_pattern" => syntax::is_field(parent, "condition", node),
        "const_item" | "static_item" | "enum_variant" => syntax::is_field(parent, "value", node),
        _ => true,
    }
}

/// True when a pattern introduces a binding named `name`.
///
/// Covers the shapes a closure body reaches for: an identifier, possibly through
/// a tuple, struct, slice, reference, or-pattern, or type ascription. A pattern
/// this walk does not model answers `false` and is therefore not treated as a
/// shadow, the same refusal to accuse that an unrecognised closure parameter
/// earns.
pub(super) fn pattern_binds(source: &str, pattern: Node<'_>, name: &str) -> bool {
    let inside = |node: Node<'_>| pattern_binds(source, node, name);
    match pattern.kind() {
        "identifier" => syntax::text(source, pattern) == name,
        "tuple_pattern" | "slice_pattern" | "or_pattern" | "ref_pattern" | "reference_pattern"
        | "mut_pattern" => syntax::named_kids(pattern).into_iter().any(inside),
        "tuple_struct_pattern" => syntax::named_kids(pattern)
            .into_iter()
            .filter(|kid| !syntax::is_field(pattern, "type", *kid))
            .any(inside),
        "struct_pattern" => syntax::named_kids(pattern)
            .into_iter()
            .filter(|kid| kid.kind() == "field_pattern")
            .any(|field| match field.by_field("pattern") {
                Some(written) => inside(written),
                None => field
                    .by_field("name")
                    .is_some_and(|short| syntax::text(source, short) == name),
            }),
        "captured_pattern" => syntax::named_kids(pattern)
            .into_iter()
            .next()
            .is_some_and(|first| first.kind() == "identifier" && inside(first)),
        "parameter" => pattern.by_field("pattern").is_some_and(inside),
        _ => false,
    }
}

/// True when a macro's tokens read `name` as a value.
///
/// Macro arguments are token trees rather than syntax, so only two spellings
/// count: an identifier token in value position, and an inline format capture
/// inside a string literal. Two things are excluded on purpose. The name reached
/// through `.` or `::` is a field, method, or path segment rather than the
/// binding. And ordinary text inside a string literal is not a read at all:
/// `concat!("_error")` names the binding while touching nothing.
///
/// The scan reads a group's tokens as flat text, so a `let` that rebinds the name
/// inside a macro's block argument is not tracked. That over-credits a read,
/// which is the direction this detector has always failed in: silence rather
/// than a false accusation.
fn tokens_read(tokens: &[Tok], name: &str) -> bool {
    let mut previous: Option<&Tok> = None;
    for token in tokens {
        let found = match *token {
            Tok::Ident(ref ident) => ident == name && !is_access_prefix(previous),
            Tok::Literal(ref literal) => {
                literal.contains(&format!("{{{name}}}")) || literal.contains(&format!("{{{name}:"))
            }
            Tok::Group { ref inner, .. } => tokens_read(inner, name),
            Tok::Punct(_) | Tok::Other => false,
        };
        if found {
            return true;
        }
        previous = Some(token);
    }
    false
}

/// True when the token before an identifier binds it into a field, method, or
/// path rather than naming the value.
fn is_access_prefix(previous: Option<&Tok>) -> bool {
    previous
        .and_then(Tok::punct)
        .is_some_and(|text| text.ends_with('.') || text.ends_with(':'))
}
