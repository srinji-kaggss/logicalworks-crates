//! Detector ALLOW-SILENCE: an `#[allow(clippy::*)]` annotation silences a lint
//! with a mechanical fix. rustc's own lints are untouched by construction;
//! `#[cfg_attr]` is out of scope (conditional).
//!
//! Every attribute in the file is visited, not only item-level ones, so an
//! allow on a `let`, a statement, a field or a match arm is caught by the same
//! walk.

use tree_sitter::Node;

use super::cfg::{self, Meta};
use super::{Hit, each_node, syntax};

/// Runs the ALLOW-SILENCE detector over a whole file.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    each_node(root, |node| {
        if !matches!(node.kind(), "attribute_item" | "inner_attribute_item") {
            return;
        }
        for lint in clippy_lints_allowed_by(source, node) {
            hits.push(Hit::new(
                "ALLOW-SILENCE",
                syntax::line(node),
                format!("#[allow({lint})]"),
            ));
        }
    });
}

/// Names every clippy lint an attribute silences, in source order.
///
/// An attribute that is not `allow`, that has no argument list (`#[allow]`), or
/// whose arguments are not a well-formed lint list answers empty: the detector
/// refuses to guess at token soup rather than reporting a lint name it did not
/// read.
fn clippy_lints_allowed_by(source: &str, item: Node<'_>) -> Vec<String> {
    let Some(attr) = syntax::attribute(source, item) else {
        return Vec::new();
    };
    if attr.path.single() != Some("allow") {
        return Vec::new();
    }
    let Some(arguments) = attr.arguments else {
        return Vec::new();
    };
    let Some(lints) = cfg::parse_list(&syntax::tokens(source, arguments)) else {
        return Vec::new();
    };
    lints.into_iter().filter_map(clippy_lint_name).collect()
}

/// Renders one entry of an `allow` list as a lint name, and `None` when the
/// entry is not a clippy lint.
///
/// The entry must be a bare path whose first segment is `clippy`, so
/// `clippy::too_many_arguments` is reported and a rustc lint such as `dead_code`
/// is not. A nested `clippy::all` group is reported by its path text, which is
/// what the gate's remediation message quotes.
fn clippy_lint_name(lint: Meta) -> Option<String> {
    let Meta::Path(path) = lint else {
        return None;
    };
    (path.segments.first().map(String::as_str) == Some("clippy")).then(|| path.render())
}
