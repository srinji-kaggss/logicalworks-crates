//! The refusal an expansion ends in: a message at a span, written as the
//! `compile_error!` the compiler reports at that span.
//!
//! The language is read by `lgwks_ast::script::parse`, which makes every refusal
//! and names where it is; this crate only has to say it to the compiler. That is
//! a message, a span and one macro call, so it needs no Rust grammar of its own.

use std::fmt;

use lgwks_deps::proc_macro2::{
    Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree,
};

/// A message the compiler is to report at a span.
#[derive(Debug)]
pub(crate) struct Error {
    /// Where the compiler points.
    span: Span,
    /// What it says.
    message: String,
}

/// An expansion's result: tokens, or the refusal to report.
pub(crate) type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// A refusal reported at `span`.
    pub(crate) fn new(span: Span, message: impl fmt::Display) -> Self {
        Self {
            span,
            message: message.to_string(),
        }
    }

    /// The `::core::compile_error!{ "message" }` that makes the compiler say it,
    /// every token spanned at the refusal.
    pub(crate) fn to_compile_error(&self) -> TokenStream {
        let mut message = Literal::string(&self.message);
        message.set_span(self.span);
        let mut braces = Group::new(
            Delimiter::Brace,
            TokenStream::from(TokenTree::Literal(message)),
        );
        braces.set_span(self.span);
        let path = [
            TokenTree::Punct(Punct::new(':', Spacing::Joint)),
            TokenTree::Punct(Punct::new(':', Spacing::Alone)),
            TokenTree::Ident(Ident::new("core", self.span)),
            TokenTree::Punct(Punct::new(':', Spacing::Joint)),
            TokenTree::Punct(Punct::new(':', Spacing::Alone)),
            TokenTree::Ident(Ident::new("compile_error", self.span)),
            TokenTree::Punct(Punct::new('!', Spacing::Alone)),
        ];
        path.into_iter()
            .map(|mut token| {
                token.set_span(self.span);
                token
            })
            .chain([TokenTree::Group(braces)])
            .collect()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
