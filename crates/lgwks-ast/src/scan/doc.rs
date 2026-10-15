//! Detector tautological-doc: a public fn whose docstring paraphrases its name
//! (half or more of its at most six content tokens share stems with the name
//! tokens).

use tree_sitter::Node;

use super::cfg;
use super::syntax::Fielded;
use super::{Hit, each_node, syntax};

/// Words ignored when deciding whether a docstring paraphrases its function
/// name. Articles, prepositions, and copulas carry no behavior, so counting them
/// as shared stems would call `/// Returns this value` on `fn value`
/// tautological when it says nothing about what is returned.
const DOC_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "at", "be", "by", "do", "for", "from", "if", "in", "is", "it", "of", "on",
    "or", "s", "that", "the", "this", "to", "was", "will", "with",
];

/// The most content tokens a docstring may have and still be judged: a long
/// docstring inevitably reuses a name token somewhere.
const MAX_DOC_TOKENS: usize = 6;

/// Runs the tautological-doc detector over a whole file.
///
/// A trait's own functions are not judged: a trait item has no visibility of its
/// own, and its implementors are judged where they are written.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    each_node(root, |node| {
        if node.kind() == "function_item" && !syntax::is_in_trait(node) {
            check(source, node, hits);
        }
    });
}

/// Records a finding when a public function's docstring is a paraphrase of its
/// own name.
///
/// Nothing is reported unless the doc has at least three bytes of text, yields at
/// least one content token, and has at most six of them. The ratio is then
/// measured as "at least half the content tokens share a stem with the name".
fn check(source: &str, function: Node<'_>, hits: &mut Vec<Hit>) {
    let Some(name) = function.by_field("name") else {
        return;
    };
    if !is_public(source, function) || cfg::is_test_or_helper(source, function) {
        return;
    }
    let doc = doc_text(source, function);
    let doc = doc.trim();
    if doc.len() < 3 {
        return;
    }
    let tokens = tokenize_doc(doc);
    if tokens.is_empty() {
        return;
    }
    let name_text = syntax::text(source, name);
    let shared = shared_stem_count(&tokens, &tokenize_name(name_text));
    // `shared` never exceeds the token count, so "a ratio under one half" is
    // exactly "twice the shared count under the total": no float is needed.
    if shared.saturating_mul(2) < tokens.len() || tokens.len() > MAX_DOC_TOKENS {
        return;
    }
    hits.push(Hit::new(
        "tautological-doc",
        syntax::line(name),
        format!(
            "pub fn {name_text} — docstring is a paraphrase of the function name ({shared} of {} tokens share stems). Replace it with substantive behavioral information.",
            tokens.len()
        ),
    ));
}

/// True only for `pub` items. `pub(crate)` and `pub(super)` are not this
/// detector's concern: a docstring paraphrase costs a crate-internal reader
/// nothing like what it costs a published API's reader.
fn is_public(source: &str, item: Node<'_>) -> bool {
    syntax::kids(item)
        .into_iter()
        .find(|child| child.kind() == "visibility_modifier")
        .is_some_and(|visibility| syntax::text(source, visibility) == "pub")
}

/// Joins every outer doc line of an item into one string, in source order: the
/// `///` and `/** */` comments and the `#[doc = ".."]` attributes written above
/// it. An item with none yields an empty string, which [`check`] rejects by
/// length before any tokenizing happens.
fn doc_text(source: &str, item: Node<'_>) -> String {
    let mut pieces = Vec::new();
    let mut cursor = item.prev_sibling();
    while let Some(sibling) = cursor {
        match sibling.kind() {
            "line_comment" | "block_comment" => {
                if let Some(doc) = outer_doc(source, sibling) {
                    pieces.push(doc);
                }
            }
            "attribute_item" => {
                if let Some(value) = doc_attribute(source, sibling) {
                    pieces.push(value);
                }
            }
            _ => break,
        }
        cursor = sibling.prev_sibling();
    }
    pieces.reverse();
    pieces.join(" ")
}

/// The text of an outer doc comment (`///`, `/** */`), and `None` for any other
/// comment, an inner one (`//!`) included.
fn outer_doc(source: &str, comment: Node<'_>) -> Option<String> {
    comment.by_field("outer")?;
    Some(match comment.by_field("doc") {
        Some(doc) => syntax::text(source, doc).to_owned(),
        None => String::new(),
    })
}

/// One `#[doc = "..."]` attribute's text, and `None` for any other attribute
/// shape.
///
/// The value must be a string literal: `#[doc = concat!(..)]` is legal Rust and
/// answers `None`, which drops it from the concatenated doc rather than guessing
/// at its content.
fn doc_attribute(source: &str, item: Node<'_>) -> Option<String> {
    let attr = syntax::attribute(source, item)?;
    if attr.path.single() != Some("doc") {
        return None;
    }
    syntax::string_value(source, attr.value?)
}

/// Splits a function name on `_` into lowercase tokens.
///
/// Empty segments are dropped, so a leading or doubled underscore (`__init`)
/// yields `init` rather than empty tokens that could never match anything.
fn tokenize_name(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|segment| !segment.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Splits a docstring into its content tokens, with stop words removed.
///
/// Splitting on any non-alphanumeric character is what makes `` `create` `` and
/// `create,` yield the same token, so quoting a parameter in a docstring does not
/// hide it from the paraphrase check. An all-stop-word docstring yields an empty
/// vector, which [`check`] treats as no evidence rather than as a paraphrase.
fn tokenize_doc(doc: &str) -> Vec<String> {
    doc.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .filter(|word| !DOC_STOP_WORDS.contains(&word.as_str()))
        .collect()
}

/// Counts doc tokens that share a stem with some name token.
///
/// The relation is prefix-based in either direction, so `creates` matches
/// `create` and `user` matches `users`. It over-counts by design: the check only
/// fires at a 0.5 ratio over at most six tokens, so a generous match makes the
/// detector accuse more readily, which is the direction the reference
/// implementation chose.
fn shared_stem_count(doc_tokens: &[String], name_tokens: &[String]) -> usize {
    doc_tokens
        .iter()
        .filter(|doc_token| {
            name_tokens.iter().any(|name_token| {
                doc_token.starts_with(name_token.as_str())
                    || name_token.starts_with(doc_token.as_str())
            })
        })
        .count()
}
