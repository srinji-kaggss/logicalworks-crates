//! Attribute arguments read as `syn::Meta`.
//!
//! The grammar leaves `#[cfg(..)]` and `#[allow(..)]` arguments as a flat
//! token tree, so the two detectors that read inside an attribute parse it
//! here, with `syn`'s meta grammar: a path, then a delimited list, an
//! `= value`, or nothing. A list that does not parse is refused whole, as
//! `syn` refused it, and every caller treats that as "unreadable": a `cfg` it
//! cannot read is not a test marker, and an `allow` it cannot read names no
//! lint.

use super::{Node, is_comment};

/// One meta item.
pub(super) enum Meta<'tree> {
    /// A bare path: `test`, `clippy::all`.
    Path(Vec<String>),
    /// A path and a delimited group: `all(unix, test)`.
    List {
        /// The path's segments.
        path: Vec<String>,
        /// The group, kept as tokens until a caller asks for its items.
        group: Node<'tree>,
    },
    /// A path and a value: `feature = "x"`.
    NameValue {
        /// The path's segments.
        path: Vec<String>,
        /// The value as a cfg atom renders it.
        value: String,
    },
}

/// The items of a comma-separated list with an optional trailing comma, or
/// `None` when any item does not parse.
pub(super) fn parse_list<'tree>(group: &Node<'tree>) -> Option<Vec<Meta<'tree>>> {
    let tokens = group_tokens(group);
    let mut cursor = Cursor {
        tokens: &tokens,
        at: 0,
    };
    let mut items = Vec::new();
    while !cursor.done() {
        items.push(cursor.meta()?);
        if cursor.done() {
            break;
        }
        cursor.eat(",")?;
    }
    Some(items)
}

/// The one meta item a group holds, with nothing after it, `syn`'s
/// `parse_args::<Meta>`; `None` for anything else, a trailing comma included.
pub(super) fn parse_single<'tree>(group: &Node<'tree>) -> Option<Meta<'tree>> {
    let tokens = group_tokens(group);
    let mut cursor = Cursor {
        tokens: &tokens,
        at: 0,
    };
    let meta = cursor.meta()?;
    cursor.done().then_some(meta)
}

/// A group's tokens without its delimiters or comments.
///
/// Every delimiter that is a direct child of a token tree is one of the
/// group's own two: a nested group is its own `token_tree` node.
fn group_tokens<'tree>(group: &Node<'tree>) -> Vec<Node<'tree>> {
    group
        .children()
        .filter(|token| {
            !is_comment(token) && !matches!(&*token.kind(), "(" | ")" | "[" | "]" | "{" | "}")
        })
        .collect()
}

/// A read position over one group's tokens.
struct Cursor<'slice, 'tree> {
    /// The group's tokens.
    tokens: &'slice [Node<'tree>],
    /// The next unread token.
    at: usize,
}

impl<'tree> Cursor<'_, 'tree> {
    /// True when every token has been read.
    fn done(&self) -> bool {
        self.at >= self.tokens.len()
    }

    /// The next token, unread.
    fn peek(&self) -> Option<&Node<'tree>> {
        self.tokens.get(self.at)
    }

    /// True when the next token is the punctuation `text`.
    fn at_punct(&self, text: &str) -> bool {
        self.peek()
            .is_some_and(|token| !token.is_named() && token.kind() == text)
    }

    /// Consumes the punctuation `text`, or answers `None` and consumes nothing.
    fn eat(&mut self, text: &str) -> Option<()> {
        if !self.at_punct(text) {
            return None;
        }
        self.at = self.at.saturating_add(1);
        Some(())
    }

    /// One meta item: a path, then a group, an `=` and its value, or nothing.
    fn meta(&mut self) -> Option<Meta<'tree>> {
        let path = self.path()?;
        if let Some(group) = self.peek().filter(|token| token.kind() == "token_tree") {
            let group = group.clone();
            self.at = self.at.saturating_add(1);
            return Some(Meta::List { path, group });
        }
        if self.eat("=").is_some() {
            let value = self.value()?;
            return Some(Meta::NameValue { path, value });
        }
        Some(Meta::Path(path))
    }

    /// `syn`'s meta path: an optional `::`, then identifiers (keywords
    /// included) joined by `::`, with no generic arguments. The leading `::`
    /// is not kept: `syn` keyed a path by its segments alone.
    fn path(&mut self) -> Option<Vec<String>> {
        if self.at_punct("::") {
            self.at = self.at.saturating_add(1);
        }
        let mut segments = vec![self.segment()?];
        while self.eat("::").is_some() {
            segments.push(self.segment()?);
        }
        Some(segments)
    }

    /// One path segment: any identifier-shaped token, as `Ident::parse_any`
    /// accepts it.
    fn segment(&mut self) -> Option<String> {
        let token = self.peek()?;
        let text = token.text();
        let named_ident = matches!(
            &*token.kind(),
            "identifier"
                | "self"
                | "super"
                | "crate"
                | "primitive_type"
                | "mutable_specifier"
                | "boolean_literal"
        );
        let keyword = !token.is_named()
            && text
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
            && text
                .chars()
                .next()
                .is_some_and(|first| first == '_' || first.is_ascii_alphabetic());
        if !named_ident && !keyword {
            return None;
        }
        let segment = text.into_owned();
        self.at = self.at.saturating_add(1);
        Some(segment)
    }

    /// The value after `=`: every token up to the next top-level comma.
    ///
    /// A single literal renders as `syn` rendered a literal atom; anything
    /// longer renders as `..`, the one key `syn` gave every non-literal value.
    ///
    /// DIVERGENCE: `syn` parsed the value as an expression and refused one that
    /// does not parse; this reads its tokens and refuses only an empty one. A
    /// string renders by its source text where `syn` rendered its value, so
    /// `"a"` and `"\x61"` are two atoms here and one there. Neither changes a
    /// verdict on a `cfg` rustc accepts, whose values are plain strings.
    fn value(&mut self) -> Option<String> {
        let start = self.at;
        while !self.done() && !self.at_punct(",") {
            self.at = self.at.saturating_add(1);
        }
        match *self.tokens.get(start..self.at)? {
            [] => None,
            [ref only] => Some(render_literal(only)),
            _ => Some("..".to_owned()),
        }
    }
}

/// A literal token as a cfg atom key carries it; any other token is `..`.
fn render_literal(token: &Node<'_>) -> String {
    let text = token.text();
    match &*token.kind() {
        "string_literal" | "raw_string_literal" if text.starts_with(['b', 'c']) => {
            "b\"..\"".to_owned()
        }
        "string_literal" | "raw_string_literal" => {
            let inner = match (text.find('"'), text.rfind('"')) {
                (Some(open), Some(close)) => text.get(open.saturating_add(1)..close),
                _ => None,
            };
            match inner {
                Some(content) => format!("\"{content}\""),
                None => text.into_owned(),
            }
        }
        "char_literal" if text.starts_with('b') => "b'..'".to_owned(),
        "char_literal" | "integer_literal" | "float_literal" | "boolean_literal" => {
            text.into_owned()
        }
        _ => "..".to_owned(),
    }
}
