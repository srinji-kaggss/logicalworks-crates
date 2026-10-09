//! What a script may not say, and what to say instead.
//!
//! Each refusal is a defect class from the estate's fix history or from the
//! published record of machine-written code, turned into a compile error that
//! names the replacement. The check runs over every token a script passes
//! through to Rust, including inside brackets, so it cannot be stepped around
//! by nesting.
//!
//! Tokens are all a macro sees: it cannot resolve a name. Calling a refused
//! method by path (`Option::unwrap(x)`) or renaming it with a `use` inside a
//! flow is therefore refused by spelling, and a name imported *outside* the
//! script under another spelling is out of reach here. That half is closed by
//! the `forbid` lint attributes `lgwks_macros` puts on every generated flow,
//! which the consumer's own compiler and clippy enforce by resolved path.

use lgwks_deps::proc_macro2::{Delimiter, TokenStream, TokenTree};

use super::lines::{is_ident, is_punct};
use super::{Refusal, Result};

/// Macro names that end a program instead of returning an error.
const PANICKING_MACROS: [&str; 10] = [
    "panic",
    "todo",
    "unimplemented",
    "unreachable",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
];

/// Methods that end the program when their value is the wrong variant,
/// refused whether called as `.name()` or by path as `Type::name(value)`.
const PANICKING_METHODS: [&str; 4] = ["unwrap", "expect", "unwrap_err", "expect_err"];

/// Names a `use` inside a flow may not bring in: each would let a refused call
/// be written under a spelling the refusals do not see.
const REFUSED_IMPORTS: [&str; 11] = [
    "thread",
    "sleep",
    "process",
    "exit",
    "abort",
    "forget",
    "spawn",
    "block_on",
    "unwrap",
    "expect",
    "unbounded_channel",
];

/// Rust keywords that may stand directly before a `[`, where the bracket opens
/// an array, slice pattern or type rather than indexing a value.
const KEYWORDS_BEFORE_BRACKET: [&str; 12] = [
    "let", "for", "in", "return", "break", "else", "match", "if", "as", "move", "mut", "ref",
];

/// Absolute path prefixes that only exist on the machine that wrote them.
const MACHINE_PATHS: [&str; 9] = [
    "/Users/",
    "/home/",
    "/tmp/",
    "/var/",
    "/etc/",
    "/opt/",
    "/private/",
    "~/",
    "C:\\",
];

/// Refuse every banned construct in `tokens`, descending into groups.
///
/// # Errors
///
/// The first banned construct, spanned at the offending token, with the
/// replacement in the message.
pub(crate) fn check(tokens: &[TokenTree]) -> Result<()> {
    if let Some(import) = refused_import(tokens) {
        let refusal = Err(Refusal::new(
            import.span(),
            format!(
                "`use` of `{import}` inside a flow renames a call the script refuses; call what the \
             flow needs by its full path, or put it behind a function outside the script"
            ),
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "check: returning an error to the caller");
        return refusal;
    }
    for (index, token) in tokens.iter().enumerate() {
        let back = |distance: usize| index.checked_sub(distance).and_then(|at| tokens.get(at));
        let next = index.checked_add(1).and_then(|at| tokens.get(at));
        if let Some(message) = refusal(token, back(1), back(3), next) {
            let refusal = Err(Refusal::new(token.span(), message));
            tracing::debug!(error = ?refusal.as_ref().err(), "check: returning an error to the caller");
            return refusal;
        }
        if let TokenTree::Group(ref group) = *token {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            check(&inner)?;
        }
    }
    Ok(())
}

/// Refuse a whole stream; used where the tokens are not already a slice.
///
/// # Errors
///
/// As [`check`].
pub(crate) fn check_stream(stream: &TokenStream) -> Result<()> {
    let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
    check(&tokens)
}

/// The refusal for one token in its neighbourhood, if it is banned.
/// `previous` is the token just before; `path_head` is three tokens back,
/// which is the `thread` of `thread::sleep`.
fn refusal(
    token: &TokenTree,
    previous: Option<&TokenTree>,
    path_head: Option<&TokenTree>,
    next: Option<&TokenTree>,
) -> Option<String> {
    let after_dot = previous.is_some_and(|prior| is_punct(prior, '.'));
    let bang_follows = next.is_some_and(|following| is_punct(following, '!'));
    let after_path = previous.is_some_and(|prior| is_punct(prior, ':'));
    match *token {
        TokenTree::Ident(ref ident) => {
            let word = ident.to_string();
            if (after_dot || after_path) && PANICKING_METHODS.contains(&word.as_str()) {
                return Some(format!(
                    "`.{word}()` ends the program on failure; a flow returns its failure instead: \
                     use `?`, `.or_fail()?` (permanent) or `.or_retry()?` (worth repeating)"
                ));
            }
            if bang_follows && PANICKING_MACROS.contains(&word.as_str()) {
                return Some(format!(
                    "`{word}!` ends the program; a flow says why it stopped with \
                     `fail with \"reason\"` and its caller decides what happens next"
                ));
            }
            refusal_for_word(&word, after_path, path_head)
        }
        TokenTree::Literal(ref literal) => {
            let text = literal.to_string();
            // Anywhere in the literal, not only at its start: a format string
            // such as `"{}/home/me"` carries a machine path just as surely. The
            // literal is searched whole, delimiters and all: every prefix below
            // opens with `/`, `~` or `C`, none of which is a delimiter of any
            // literal spelling, so no amount of stripping quotes, `r` markers or
            // `#` hashes could stand between a prefix and the path it starts.
            MACHINE_PATHS
                .iter()
                .any(|prefix| text.contains(prefix))
                .then(|| {
                    "an absolute path from one machine does not exist on the next; \
                     take the path as a flow parameter or resolve it from configuration"
                        .to_owned()
                })
        }
        TokenTree::Group(ref group) if group.delimiter() == Delimiter::Bracket => indexes(previous)
            .then(|| {
                "indexing ends the program when the position is out of range; use \
                 `.get(i).or_fail()?` (or `.get(a..b)`) so a missing item is a flow failure"
                    .to_owned()
            }),
        TokenTree::Group(_) | TokenTree::Punct(_) => None,
    }
}

/// Whether a `[` group after `previous` indexes a value: it follows a name, a
/// call's `)` or another index's `]`, and not a keyword that opens an array or
/// slice pattern. `vec![..]`, `#[..]`, `: [u8; 4]` and `&[..]` follow a
/// punctuation mark and so are never read as indexing.
fn indexes(previous: Option<&TokenTree>) -> bool {
    let Some(token) = previous else {
        return false;
    };
    match *token {
        TokenTree::Ident(ref name) => !KEYWORDS_BEFORE_BRACKET.contains(&name.to_string().as_str()),
        TokenTree::Group(ref group) => matches!(
            group.delimiter(),
            Delimiter::Parenthesis | Delimiter::Bracket
        ),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    }
}

/// The first refused name a `use` line brings in, if `tokens` is one.
fn refused_import(tokens: &[TokenTree]) -> Option<TokenTree> {
    /// Search `tokens` and every group inside them.
    fn find(tokens: impl IntoIterator<Item = TokenTree>) -> Option<TokenTree> {
        tokens.into_iter().find_map(|token| match token {
            TokenTree::Ident(ref name) if REFUSED_IMPORTS.contains(&name.to_string().as_str()) => {
                Some(TokenTree::Ident(name.clone()))
            }
            TokenTree::Group(ref group) => find(group.stream()),
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => None,
        })
    }
    if !tokens.first().is_some_and(|first| is_ident(first, "use")) {
        return None;
    }
    find(tokens.iter().skip(1).cloned())
}

/// Refusals decided by the identifier alone, or by the path it ends.
fn refusal_for_word(word: &str, after_path: bool, path_head: Option<&TokenTree>) -> Option<String> {
    let message = match word {
        "loop" | "while" => {
            "a loop with no bound can run forever; use `retry up to N times:` to repeat \
             until success, `for x in xs:` or `each x in xs, at most N at once:` to visit items"
        }
        "spawn" => {
            "a spawned task has no owner in this flow and outlives its failure; \
             use `each x in xs, at most N at once:` or `together:`, which own their work"
        }
        "unbounded_channel" | "unbounded" => {
            "an unbounded channel grows until memory runs out; give it a capacity"
        }
        "block_on" => {
            "`block_on` inside a flow stalls the thread its siblings run on; `run` the \
             flow or `.await` the future"
        }
        "unsafe" => "a script never needs `unsafe`; put it behind a safe function in Rust",
        "sleep" if after_path && path_head.is_some_and(|path| is_ident(path, "thread")) => {
            "`thread::sleep` stalls every sibling on this thread; use `within D:` for a \
             deadline or `retry .., waiting D:` for a pause between attempts"
        }
        "exit" | "abort"
            if after_path && path_head.is_some_and(|path| is_ident(path, "process")) =>
        {
            "ending the process from a flow skips every caller's cleanup and leaves no \
             record of why; `fail with \"reason\"` and let the caller decide"
        }
        "forget" if after_path && path_head.is_some_and(|path| is_ident(path, "mem")) => {
            "`mem::forget` leaks what the value owns (a permit, a process group, a lock); \
             let it drop, or hand it to the owner that must outlive the flow"
        }
        _ => return None,
    };
    Some(message.to_owned())
}
