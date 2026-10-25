//! Detector ALLOW-SILENCE: any attribute that lowers a lint's level so a
//! violation goes unreported.
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
//! Every attribute in the file is visited, not only item-level ones, so an
//! allow on a `let`, a statement, a field or a match arm is caught by the same
//! walk. A list this reader cannot model is not guessed at: it answers nothing
//! rather than reporting a lint name it did not read.

use tree_sitter::Node;

use super::cfg::{self, Meta};
use super::{Hit, each_node, syntax};

/// The lint levels that silence. `warn`, `deny` and `forbid` raise or keep a
/// lint visible and are not findings.
const SILENCING_LEVELS: [&str; 2] = ["allow", "expect"];

/// Runs the ALLOW-SILENCE detector over a whole file.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    each_node(root, |node| {
        if !matches!(node.kind(), "attribute_item" | "inner_attribute_item") {
            return;
        }
        for silenced in silenced_by(source, node) {
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
    let Some(arguments) = attr.arguments else {
        return Vec::new();
    };
    let Some(entries) = cfg::parse_list(&syntax::tokens(source, arguments)) else {
        return Vec::new();
    };
    if SILENCING_LEVELS.contains(&head) {
        return entries
            .iter()
            .filter_map(lint_name)
            .map(|lint| render(head, &lint, false))
            .collect();
    }
    if head == "cfg_attr" {
        // The first entry is the configuration predicate; every entry after it
        // is an attribute, applied when that predicate holds.
        let mut silenced = Vec::new();
        for entry in entries.iter().skip(1) {
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
    let Some(entries) = children.as_ref() else {
        return;
    };
    if SILENCING_LEVELS.contains(&head) {
        silenced.extend(
            entries
                .iter()
                .filter_map(lint_name)
                .map(|lint| render(head, &lint, true)),
        );
    } else if head == "cfg_attr" {
        for entry in entries.iter().skip(1) {
            collect_attributes(entry, silenced);
        }
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
