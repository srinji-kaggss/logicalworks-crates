//! What a script may not say, and what to say instead.
//!
//! Each refusal is a defect class from the estate's fix history or from the
//! published record of machine-written code, turned into a compile error that
//! names the replacement. The check runs over every token a script passes
//! through to Rust, including inside brackets, so it cannot be stepped around
//! by nesting.

use lgwks_deps::proc_macro2::{TokenStream, TokenTree};
use lgwks_deps::syn::{Error, Result};

use crate::lines::{is_ident, is_punct};

/// Macro names that end a program instead of returning an error.
const PANICKING_MACROS: [&str; 5] = ["panic", "todo", "unimplemented", "unreachable", "assert"];

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
    lgwks_std::trace::warn!(
        operation = "check",
        "operation refused its request; the typed error carries the facts"
    );
    for (index, token) in tokens.iter().enumerate() {
        let back = |distance: usize| index.checked_sub(distance).and_then(|at| tokens.get(at));
        let next = index.checked_add(1).and_then(|at| tokens.get(at));
        if let Some(message) = refusal(token, back(1), back(3), next) {
            return Err(Error::new(token.span(), message));
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
            if after_dot && (word == "unwrap" || word == "expect") {
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
            let body = text.trim_start_matches('r').trim_start_matches('#');
            let body = body.strip_prefix('"').unwrap_or(body);
            MACHINE_PATHS
                .iter()
                .any(|prefix| body.starts_with(prefix))
                .then(|| {
                    "an absolute path from one machine does not exist on the next; \
                     take the path as a flow parameter or resolve it from configuration"
                        .to_owned()
                })
        }
        TokenTree::Group(_) | TokenTree::Punct(_) => None,
    }
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
        _ => return None,
    };
    Some(message.to_owned())
}
