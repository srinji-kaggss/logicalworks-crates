//! The three syntax-only rules: ALLOW-SILENCE, long-try-chain and
//! tautological-doc.

use super::meta::{self, Meta};
use super::{
    Hit, Node, Owner, Side, attribute_is, attribute_of, block_statements, is_doc_comment,
    is_test_attr, item_attributes, line_of, owner_of,
};

// ── ALLOW-SILENCE ───────────────────────────────────────────────────────────
// An `#[allow(clippy::*)]` silences a lint that has a mechanical fix. rustc's
// own lints and other tools' are not this rule's; `#[cfg_attr]` is
// conditional and out of scope.

/// One finding per clippy path an `allow` names, wherever the attribute is
/// written: an item, a statement, a field, a match arm, a parameter.
pub(super) fn allow_silence(root: &Node<'_>, hits: &mut Vec<Hit>) {
    for item in root.dfs() {
        if !matches!(&*item.kind(), "attribute_item" | "inner_attribute_item") {
            continue;
        }
        let Some(attribute) = attribute_of(&item) else {
            continue;
        };
        for lint in clippy_lints_allowed_by(&attribute) {
            hits.push(Hit {
                rule: "ALLOW-SILENCE",
                line: line_of(&item),
                snippet: format!("#[allow({lint})]"),
            });
        }
    }
}

/// Every clippy lint an attribute silences, in source order. An attribute that
/// is not `allow`, has no list, or whose list does not parse names none: the
/// rule does not report a lint it did not read.
fn clippy_lints_allowed_by(attribute: &Node<'_>) -> Vec<String> {
    if !attribute_is(attribute, "allow") || attribute.field("value").is_some() {
        return Vec::new();
    }
    let Some(lints) = attribute
        .field("arguments")
        .and_then(|group| meta::parse_list(&group))
    else {
        return Vec::new();
    };
    lints
        .into_iter()
        .filter_map(|lint| match lint {
            Meta::Path(path) if path.first().is_some_and(|first| first == "clippy") => {
                Some(path.join("::"))
            }
            Meta::Path(_) | Meta::List { .. } | Meta::NameValue { .. } => None,
        })
        .collect()
}

// ── long-try-chain ──────────────────────────────────────────────────────────
// More than three `?` in one statement other than a `let`, outside test
// functions.

/// Above this many `?` in one statement the chain is a finding; exactly this
/// many passes. The threshold is contractual.
const MAX_TRY_CHAIN: usize = 3;

/// Where the walk is: the innermost function's name line, and whether that
/// function is exempt.
#[derive(Clone, Copy)]
struct Scope {
    /// The line of the innermost enclosing function's name; `None` outside
    /// every function.
    line: Option<usize>,
    /// The innermost function is marked `test` or `test_*`.
    skipped: bool,
}

/// Every long-try-chain finding under `root`.
pub(super) fn long_try_chain(root: &Node<'_>, hits: &mut Vec<Hit>) {
    chain_walk(
        root,
        Scope {
            line: None,
            skipped: false,
        },
        hits,
    );
}

/// Visit `node` under `scope`, judging every block statement once.
///
/// Every function, trait default methods included, opens a scope. An exempt
/// one leaves the enclosing line in place and marks the walk skipped until it
/// ends; a production function nested in an exempt one is judged under its
/// own line.
fn chain_walk(node: &Node<'_>, scope: Scope, hits: &mut Vec<Hit>) {
    match &*node.kind() {
        "function_item" => {
            let inner = if is_test_attr(&item_attributes(node)) {
                Scope {
                    line: scope.line,
                    skipped: true,
                }
            } else {
                Scope {
                    line: node.field("name").as_ref().map(line_of),
                    skipped: false,
                }
            };
            for child in node.children() {
                chain_walk(&child, inner, hits);
            }
        }
        "block" => {
            for statement in block_statements(node) {
                inspect(&statement, scope, hits);
                chain_walk(&statement, scope, hits);
            }
        }
        _ => {
            for child in node.children() {
                chain_walk(&child, scope, hits);
            }
        }
    }
}

/// Report one statement whose own expression holds more than
/// [`MAX_TRY_CHAIN`] `?`, naming the enclosing function's line.
///
/// DIVERGENCE: an item statement written under an outer attribute is located
/// at the item's keyword, where `syn`'s span began at the `#`.
fn inspect(statement: &Node<'_>, scope: Scope, hits: &mut Vec<Hit>) {
    let fn_line = match scope.line {
        Some(line) if !scope.skipped => line,
        _ => return,
    };
    // A `let` breaks the chain by contract: its initializer is the named
    // intermediate value the finding asks for.
    if statement.kind() == "let_declaration" {
        return;
    }
    let count = count_tries(statement);
    if count > MAX_TRY_CHAIN {
        hits.push(Hit {
            rule: "long-try-chain",
            line: line_of(statement),
            snippet: format!(
                "fn near line {fn_line} — statement contains {count} `?` operators without an intermediate `let`. Break the chain with named intermediate values."
            ),
        });
    }
}

/// The `?` operators in one statement's own expression.
///
/// Closures are not entered: their `?` is their own control flow. Blocks are
/// not entered either: each statement inside one is judged on its own, so
/// counting it here would charge a loop with every `?` in its body. What
/// stays counted is the statement's own expression, a `for` iterator, a
/// `while` or `if` condition, a `match` scrutinee and an arm without braces.
fn count_tries(node: &Node<'_>) -> usize {
    match &*node.kind() {
        "closure_expression" | "block" => 0,
        kind => {
            let own = usize::from(kind == "try_expression");
            node.children().fold(own, |count, child| {
                count.saturating_add(count_tries(&child))
            })
        }
    }
}

// ── tautological-doc ────────────────────────────────────────────────────────
// A `pub` function whose docstring paraphrases its name: half or more of at
// most six content tokens share a stem with the name's tokens.

/// Words that carry no behavior, removed before counting.
const DOC_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "at", "be", "by", "do", "for", "from", "if", "in", "is", "it", "of", "on",
    "or", "s", "that", "the", "this", "to", "was", "will", "with",
];

/// Every tautological-doc finding under `root`: free functions and `impl`
/// methods; a trait method is not judged.
pub(super) fn tautological_doc(root: &Node<'_>, hits: &mut Vec<Hit>) {
    for function in root.dfs() {
        if function.kind() != "function_item" || owner_of(&function) == Owner::Trait {
            continue;
        }
        if let Some(hit) = judge(&function) {
            hits.push(hit);
        }
    }
}

/// The finding for one function, if its doc paraphrases its name.
///
/// Nothing is reported unless the doc has at least three bytes and one to six
/// content tokens: a long docstring reuses a name token somewhere, so the
/// ceiling keeps the rule off real documentation.
fn judge(function: &Node<'_>) -> Option<Hit> {
    let name = function.field("name")?;
    let attributes = item_attributes(function);
    // `pub` alone: `pub(crate)` costs a crate-internal reader little.
    let public = function
        .children()
        .any(|child| child.kind() == "visibility_modifier" && child.text() == "pub");
    if !public || is_test_attr(&attributes) {
        return None;
    }
    let doc = doc_text(&attributes);
    let doc = doc.trim();
    if doc.len() < 3 {
        return None;
    }
    let tokens: Vec<String> = split_words(doc)
        .into_iter()
        .filter(|word| !DOC_STOP_WORDS.contains(&word.as_str()))
        .collect();
    if tokens.is_empty() || tokens.len() > 6 {
        return None;
    }
    let name_text = name.text();
    let name_tokens: Vec<String> = name_text
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(str::to_lowercase)
        .collect();
    let shared = tokens
        .iter()
        .filter(|token| {
            name_tokens
                .iter()
                .any(|stem| token.starts_with(stem.as_str()) || stem.starts_with(token.as_str()))
        })
        .count();
    // At least half: `shared / total >= 0.5`, in integers.
    if shared.saturating_mul(2) < tokens.len() {
        return None;
    }
    Some(Hit {
        rule: "tautological-doc",
        line: line_of(&name),
        snippet: format!(
            "pub fn {name_text} — docstring is a paraphrase of the function name ({shared} of {} tokens share stems). Replace it with substantive behavioral information.",
            tokens.len()
        ),
    })
}

/// Lowercase alphanumeric words, punctuation discarded, so `` `create` `` and
/// `create,` are one token.
fn split_words(doc: &str) -> Vec<String> {
    doc.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Every doc line of an item joined by spaces, in source order: `///`,
/// `//!`, `/** */`, `/*! */` and `#[doc = "..."]` with a string literal. A
/// `#[doc = concat!(..)]` carries no text the rule can read and is dropped.
fn doc_text(attributes: &[Node<'_>]) -> String {
    attributes
        .iter()
        .filter_map(doc_piece)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One attribute's doc text, as `syn` gave a `#[doc]` its value.
fn doc_piece(item: &Node<'_>) -> Option<String> {
    if is_doc_comment(item, Side::Outer) || is_doc_comment(item, Side::Inner) {
        let text = match item.field("doc") {
            Some(content) => content.text().into_owned(),
            None => String::new(),
        };
        return Some(text.trim_end_matches(['\n', '\r']).to_owned());
    }
    let attribute = attribute_of(item)?;
    if !attribute_is(&attribute, "doc") {
        return None;
    }
    let value = attribute
        .field("value")
        .filter(|value| matches!(&*value.kind(), "string_literal" | "raw_string_literal"))?;
    let text = value.text();
    let open = text.find('"')?;
    let close = text.rfind('"')?;
    text.get(open.saturating_add(1)..close).map(str::to_owned)
}
