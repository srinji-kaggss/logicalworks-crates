//! Detector ALLOW-SILENCE: any attribute that lowers a lint's level so a
//! violation goes unreported, and any source the scan cannot read that could
//! hold one.
//!
//! The Rust reference gives one syntax to five levels — `allow`, `expect`,
//! `warn`, `deny` and `forbid` — and two of them silence: `allow` suppresses the
//! emission, and `expect` suppresses it too while recording the expectation.
//! Every way of naming a lint is read: a rustc lint (`dead_code`), a tool lint
//! (`clippy::unwrap_used`, `rustdoc::broken_intra_doc_links`), and a group
//! (`warnings`, `unused`). A `reason = ".."` argument states why the lint was
//! silenced; it does not make the silence anything else, so it is read past
//! rather than accepted as the whole story (#699).
//!
//! `cfg_attr(predicate, allow(..))` is the same act under a condition, so it is
//! read the same way, through the meta list the attribute arguments already
//! parse into, including a `cfg_attr` nested in another.
//!
//! A raw identifier is the identifier it prefixes, so `#[r#allow(dead_code)]` is
//! `#[allow(dead_code)]` to rustc and to this reader. An attribute written inside
//! a macro — a `macro_rules!` body, or the arguments of an invocation — is inert
//! text until the macro expands, and expansion leaves it on the item, so it is
//! read from the token tree the same way (#758).
//!
//! Every attribute in the file is visited, not only item-level ones, so an
//! allow on a `let`, a statement, a field or a match arm is caught by the same
//! walk. A silencing attribute whose list this reader cannot model (a macro
//! metavariable in it) is reported with its lints as `..`: a list that cannot be
//! read is a silence that cannot be shown to be empty.
//!
//! The walker reads `.rs` files, so `include!("lint.inc")` and
//! `#[path = "lint.inc"] mod m;` pull in source that no scan ever sees. Each is
//! reported, because the file can hold anything and nothing would say so. An
//! `include!` whose argument is not one string literal (`concat!(env!(..), ..)`)
//! names generated code, which is not authored and is not a file in the tree.
//! What a macro does with a bare `meta` fragment (`#[$m]`) is not an attribute
//! until expansion, so it is not read.

use std::path::Path;

use tree_sitter::Node;

use super::cfg::{self, Meta};
use super::syntax::Tok;
use super::{Hit, each_node, syntax};

/// The lint levels that silence. `warn`, `deny` and `forbid` raise or keep a
/// lint visible and are not findings.
const SILENCING_LEVELS: [&str; 2] = ["allow", "expect"];

/// Runs the ALLOW-SILENCE detector over a whole file.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    each_node(root, |node| {
        let evidence = match node.kind() {
            "attribute_item" | "inner_attribute_item" => silenced_by(source, node),
            "#" => macro_attribute(source, node),
            "macro_invocation" => unread_include(source, node),
            _ => return,
        };
        for silenced in evidence {
            hits.push(Hit::new("ALLOW-SILENCE", syntax::line(node), silenced));
        }
    });
}

/// The evidence for every lint this attribute silences, in source order.
fn silenced_by(source: &str, item: Node<'_>) -> Vec<String> {
    let Some(attr) = syntax::attribute(source, item) else {
        return Vec::new();
    };
    let Some(head) = attr.path.single() else {
        return Vec::new();
    };
    if head == "path" {
        let target = attr
            .value
            .and_then(|value| syntax::string_value(source, value));
        return match target {
            Some(target) => unread(&format!("#[path = {target:?}]"), &target),
            None => Vec::new(),
        };
    }
    let Some(arguments) = attr.arguments else {
        return Vec::new();
    };
    silenced_by_head(head, &syntax::tokens(source, arguments))
}

/// The evidence for the lints an attribute named `head` with these `arguments`
/// silences, however the attribute was found.
fn silenced_by_head(head: &str, arguments: &[Tok]) -> Vec<String> {
    let entries = cfg::parse_list(arguments);
    if SILENCING_LEVELS.contains(&head) {
        return evidence(head, entries.as_deref(), false);
    }
    if head == "cfg_attr" {
        // The first entry is the configuration predicate; every entry after it
        // is an attribute, applied when that predicate holds.
        let mut silenced = Vec::new();
        for entry in entries.iter().flatten().skip(1) {
            collect_attributes(entry, &mut silenced);
        }
        return silenced;
    }
    Vec::new()
}

/// Every silencing level inside a `cfg_attr` entry, recursing through a nested
/// `cfg_attr`.
fn collect_attributes(meta: &Meta, silenced: &mut Vec<String>) {
    let Meta::List {
        ref path,
        ref children,
    } = *meta
    else {
        return;
    };
    let Some(head) = path.single() else {
        return;
    };
    if SILENCING_LEVELS.contains(&head) {
        silenced.extend(evidence(head, children.as_deref(), true));
    } else if head == "cfg_attr" {
        for entry in children.iter().flatten().skip(1) {
            collect_attributes(entry, silenced);
        }
    }
}

/// The evidence of a silencing attribute at `level`: one entry per lint it names,
/// or one entry with the lints shown as `..` when its list could not be read.
fn evidence(level: &str, entries: Option<&[Meta]>, nested: bool) -> Vec<String> {
    match entries {
        Some(list) => list
            .iter()
            .filter_map(lint_name)
            .map(|lint| render(level, &lint, nested))
            .collect(),
        None => vec![render(level, "..", nested)],
    }
}

/// The lint an entry of a list names, or `None` when the entry is not a lint.
///
/// `reason = ".."` is a `Meta::NameValue` and a nested list is a `Meta::List`;
/// neither names a lint. A leading `::` is not how a lint is written, so it is
/// not read as one.
fn lint_name(meta: &Meta) -> Option<String> {
    match *meta {
        Meta::Path(ref path) if !path.leading => Some(path.render()),
        _ => None,
    }
}

/// How one silenced lint is written, as the attribute that silenced it.
fn render(level: &str, lint: &str, nested: bool) -> String {
    if nested {
        format!("#[cfg_attr(.., {level}({lint}))]")
    } else {
        format!("#[{level}({lint})]")
    }
}

/// The next token of a token tree after `node`, comments skipped.
fn next_token(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.next_sibling();
    while let Some(sibling) = cursor {
        if !sibling.is_extra() {
            return Some(sibling);
        }
        cursor = sibling.next_sibling();
    }
    None
}

/// The evidence for an attribute written in a macro's token tree, where `hash` is
/// the `#` that opens it: `#` or `#!`, then a `[name(..)]` group.
///
/// A `#` that is not inside a token tree is part of an attribute item and is read
/// by [`silenced_by`]; a `#` followed by anything else is interpolation
/// (`quote!`'s `#name`), not an attribute.
fn macro_attribute(source: &str, hash: Node<'_>) -> Vec<String> {
    if !hash
        .parent()
        .is_some_and(|parent| parent.kind() == "token_tree")
    {
        return Vec::new();
    }
    let Some(mut group) = next_token(hash) else {
        return Vec::new();
    };
    if group.kind() == "!" {
        let Some(after) = next_token(group) else {
            return Vec::new();
        };
        group = after;
    }
    if group.kind() != "token_tree" || !syntax::text(source, group).starts_with('[') {
        return Vec::new();
    }
    let tokens = syntax::tokens(source, group);
    if tokens.len() != 2 {
        return Vec::new();
    }
    let name = tokens.first().and_then(Tok::ident);
    let arguments = tokens.get(1).filter(|tree| tree.is_call());
    match (name, arguments.and_then(Tok::group)) {
        (Some(name), Some(inner)) => silenced_by_head(syntax::unraw(name), inner),
        _ => Vec::new(),
    }
}

/// The evidence for an `include!` of one string literal that names a file the
/// scan does not read, or nothing for any other macro or any other argument.
fn unread_include(source: &str, node: Node<'_>) -> Vec<String> {
    let kids = syntax::named_kids(node);
    let Some(&name) = kids.first() else {
        return Vec::new();
    };
    let Some(path) = syntax::Path::of(source, name) else {
        return Vec::new();
    };
    let is_include = match *path.segments.as_slice() {
        [ref only] => only == "include",
        [ref root, ref last] => matches!(root.as_str(), "std" | "core") && last == "include",
        _ => false,
    };
    let Some(tree) = syntax::macro_tokens(node).filter(|_| is_include) else {
        return Vec::new();
    };
    let argument = syntax::named_kids(tree);
    let &[literal] = argument.as_slice() else {
        return Vec::new();
    };
    match syntax::string_value(source, literal) {
        Some(target) => unread(&format!("include!({target:?})"), &target),
        None => Vec::new(),
    }
}

/// The evidence that `call` names `target`, a file the scan never reads, or
/// nothing when `target` is a `.rs` file, which the walker does read.
fn unread(call: &str, target: &str) -> Vec<String> {
    let read = Path::new(target)
        .extension()
        .is_some_and(|extension| extension == "rs");
    if read {
        return Vec::new();
    }
    vec![format!(
        "{call} — names a file the scan never reads (it reads .rs files only)"
    )]
}
