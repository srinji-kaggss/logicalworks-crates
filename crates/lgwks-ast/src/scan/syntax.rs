//! What the detectors read out of a tree-sitter node: lines, text, the
//! attributes written above an item, paths, and the tokens of a macro or an
//! attribute.
//!
//! Every function here answers from the tree and the source it was parsed from,
//! and nothing else. A node that is not the shape asked for answers `None` or
//! `false`: the detectors decide what that means, and none of them reads a
//! guessed value in its place.

use std::cell::RefCell;
use std::num::NonZeroU16;

use tree_sitter::Node;

thread_local! {
    /// The grammar's id for each field name read on this thread, so a lookup is
    /// a few short string compares and not a scan of every field the grammar has.
    /// Only the Rust grammar is read here, so a name has one id.
    static FIELD_IDS: RefCell<Vec<(&'static str, Option<NonZeroU16>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Reading a node's child by the field name the grammar gives it.
pub(super) trait Fielded<'t> {
    /// The child in `name`, resolved through the per-thread id cache.
    fn by_field(&self, name: &'static str) -> Option<Node<'t>>;
}

impl<'t> Fielded<'t> for Node<'t> {
    fn by_field(&self, name: &'static str) -> Option<Node<'t>> {
        let id = FIELD_IDS.with(|cache| {
            let Ok(mut known) = cache.try_borrow_mut() else {
                return self.language().field_id_for_name(name);
            };
            if let Some(&(_, found)) = known.iter().find(|&&(spelled, _)| spelled == name) {
                return found;
            }
            let found = self.language().field_id_for_name(name);
            known.push((name, found));
            found
        })?;
        self.child_by_field_id(id.get())
    }
}

/// The 1-based line a node starts on.
pub(super) fn line(node: Node<'_>) -> usize {
    node.start_position().row.saturating_add(1)
}

/// The source text a node spans, and the empty string for a range that is not
/// on character boundaries (a node of a parse of this very text never is).
pub(super) fn text<'s>(source: &'s str, node: Node<'_>) -> &'s str {
    let Some(found) = source.get(node.start_byte()..node.end_byte()) else {
        return "";
    };
    found
}

/// Every child of a node, anonymous tokens and comments included, in source
/// order.
pub(super) fn kids(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// The named children of a node that are not comments, in source order.
pub(super) fn named_kids(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !child.is_extra())
        .collect()
}

/// Whether `inner` is the node `outer` names in `field`.
pub(super) fn is_field(outer: Node<'_>, field: &'static str, inner: Node<'_>) -> bool {
    outer
        .by_field(field)
        .is_some_and(|named| named.id() == inner.id())
}

/// Whether a function item is a method or associated function of a trait or an
/// impl, as opposed to a free function (at file level, in a module, or in a
/// block).
pub(super) fn is_in_trait(node: Node<'_>) -> bool {
    is_declared_in(node, "trait_item")
}

/// Whether a function item is a method or associated function of an `impl`.
pub(super) fn is_in_impl(node: Node<'_>) -> bool {
    is_declared_in(node, "impl_item")
}

/// Whether a node sits in the body (a declaration list) of an item of `kind`.
fn is_declared_in(node: Node<'_>, kind: &str) -> bool {
    node.parent()
        .filter(|parent| parent.kind() == "declaration_list")
        .and_then(|list| list.parent())
        .is_some_and(|owner| owner.kind() == kind)
}

/// The attributes written above an item, as the `attribute_item` siblings
/// directly before it, in source order. Comments between them are skipped; any
/// other sibling ends the run, so an attribute never reaches past the item
/// before it.
pub(super) fn attributes(node: Node<'_>) -> Vec<Node<'_>> {
    let mut found = Vec::new();
    let mut cursor = node.prev_sibling();
    while let Some(sibling) = cursor {
        match sibling.kind() {
            "attribute_item" => found.push(sibling),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        cursor = sibling.prev_sibling();
    }
    found.reverse();
    found
}

/// A path as written: its segments, and whether it starts with `::`.
///
/// Generic arguments are not part of it, which is how the detectors have always
/// read a path: `Vec::<u8>::new` is `Vec`, `new`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Path {
    /// Whether the path starts with `::`.
    pub(super) leading: bool,
    /// The identifiers, in order.
    pub(super) segments: Vec<String>,
}

impl Path {
    /// The path a node spells, or `None` when the node is not a path or is a
    /// qualified one (`<T as Trait>::item`), which names a type and not a place
    /// this file can see.
    pub(super) fn of(source: &str, node: Node<'_>) -> Option<Self> {
        let mut path = Self::default();
        path.collect(source, node).then_some(path)
    }

    /// Append the segments of `node`, and say whether it was a plain path.
    fn collect(&mut self, source: &str, node: Node<'_>) -> bool {
        match node.kind() {
            "identifier" | "type_identifier" | "self" | "super" | "crate" | "primitive_type" => {
                self.segments.push(text(source, node).to_owned());
                true
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                match node.by_field("path") {
                    Some(before) => {
                        if !self.collect(source, before) {
                            return false;
                        }
                    }
                    None => self.leading = true,
                }
                match node.by_field("name") {
                    Some(name) => {
                        self.segments.push(text(source, name).to_owned());
                        true
                    }
                    None => false,
                }
            }
            "generic_type" => node
                .by_field("type")
                .is_some_and(|inner| self.collect(source, inner)),
            "generic_function" => node
                .by_field("function")
                .is_some_and(|inner| self.collect(source, inner)),
            _ => false,
        }
    }

    /// The single identifier of a path that is only that: no `::`, no
    /// qualifier.
    pub(super) fn single(&self) -> Option<&str> {
        if self.leading {
            return None;
        }
        match (self.segments.first(), self.segments.get(1)) {
            (Some(only), None) => Some(only.as_str()),
            _ => None,
        }
    }

    /// The last identifier.
    pub(super) fn last(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }

    /// The identifiers joined with `::`, without a leading one.
    pub(super) fn render(&self) -> String {
        self.segments.join("::")
    }
}

/// An attribute as written: its path, and the token tree after it when it has
/// one.
pub(super) struct Attr<'t> {
    /// The attribute's path (`test`, `tokio::test`, `allow`).
    pub(super) path: Path,
    /// The `= value` expression of `#[name = value]`.
    pub(super) value: Option<Node<'t>>,
    /// The token tree of `#[name(..)]`.
    pub(super) arguments: Option<Node<'t>>,
}

/// The attribute inside an `attribute_item` or `inner_attribute_item`, or
/// `None` when its path is not a plain one.
pub(super) fn attribute<'t>(source: &str, item: Node<'t>) -> Option<Attr<'t>> {
    let inside = named_kids(item)
        .into_iter()
        .find(|child| child.kind() == "attribute")?;
    let head = named_kids(inside).into_iter().next()?;
    Some(Attr {
        path: Path::of(source, head)?,
        value: inside.by_field("value"),
        arguments: inside.by_field("arguments"),
    })
}

/// One token of a token tree, as `proc_macro2` would present it: identifiers
/// and keywords alike, punctuation, literals, and nested groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Tok {
    /// An identifier or a keyword.
    Ident(String),
    /// Punctuation, as the grammar tokenises it (`::`, `=`, `,`).
    Punct(String),
    /// A literal, with its quotes.
    Literal(String),
    /// A delimited group, without its delimiters.
    Group(Vec<Tok>),
    /// Anything else (a metavariable, a repetition).
    Other,
}

impl Tok {
    /// The identifier this token is.
    pub(super) fn ident(&self) -> Option<&str> {
        match *self {
            Self::Ident(ref name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// The punctuation this token is.
    pub(super) fn punct(&self) -> Option<&str> {
        match *self {
            Self::Punct(ref text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// The tokens of the group this token is.
    pub(super) fn group(&self) -> Option<&[Self]> {
        match *self {
            Self::Group(ref inner) => Some(inner.as_slice()),
            _ => None,
        }
    }

    /// The text of the literal this token is.
    pub(super) fn literal(&self) -> Option<&str> {
        match *self {
            Self::Literal(ref text) => Some(text.as_str()),
            _ => None,
        }
    }
}

/// The tokens inside a `token_tree` node, delimiters removed.
pub(super) fn tokens(source: &str, tree: Node<'_>) -> Vec<Tok> {
    let children: Vec<Node<'_>> = kids(tree)
        .into_iter()
        .filter(|child| !child.is_extra())
        .collect();
    let last = children.len().saturating_sub(1);
    let mut found = Vec::new();
    for (index, child) in children.into_iter().enumerate() {
        if index == 0 || index == last {
            continue;
        }
        found.push(token(source, child));
    }
    found
}

/// One child of a token tree as a token.
fn token(source: &str, node: Node<'_>) -> Tok {
    let spelled = text(source, node);
    match node.kind() {
        "token_tree" => Tok::Group(tokens(source, node)),
        "identifier" | "self" | "super" | "crate" | "primitive_type" | "mutable_specifier"
        | "boolean_literal" => Tok::Ident(spelled.to_owned()),
        "string_literal" | "raw_string_literal" | "char_literal" | "integer_literal"
        | "float_literal" => Tok::Literal(spelled.to_owned()),
        _ if !node.is_named() => {
            let word = spelled
                .chars()
                .next()
                .is_some_and(|first| first.is_alphabetic() || first == '_');
            if word {
                Tok::Ident(spelled.to_owned())
            } else {
                Tok::Punct(spelled.to_owned())
            }
        }
        _ => Tok::Other,
    }
}

/// The token tree of a macro invocation.
pub(super) fn macro_tokens(node: Node<'_>) -> Option<Node<'_>> {
    kids(node)
        .into_iter()
        .rev()
        .find(|child| child.kind() == "token_tree")
}

/// The value of a string literal node: its characters with the escapes read,
/// or `None` for a node that is not a string.
pub(super) fn string_value(source: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "raw_string_literal" => Some(
            named_kids(node)
                .into_iter()
                .map(|piece| text(source, piece))
                .collect(),
        ),
        "string_literal" => {
            let mut value = String::new();
            for piece in named_kids(node) {
                match piece.kind() {
                    "escape_sequence" => value.push_str(&unescape(text(source, piece))),
                    _ => value.push_str(text(source, piece)),
                }
            }
            Some(value)
        }
        _ => None,
    }
}

/// The character an escape sequence stands for. A sequence this does not model
/// (a `\u{..}` or `\x..` value) reads as a space: documentation splits words on
/// anything that is not alphanumeric, so the space is the faithful stand-in for
/// a character that is not one.
fn unescape(escape: &str) -> String {
    match escape {
        "\\n" => "\n".to_owned(),
        "\\t" => "\t".to_owned(),
        "\\r" => "\r".to_owned(),
        "\\0" => "\0".to_owned(),
        "\\\\" => "\\".to_owned(),
        "\\\"" => "\"".to_owned(),
        "\\'" => "'".to_owned(),
        _ => " ".to_owned(),
    }
}
