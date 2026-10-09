//! The lexicon: every word of `script!`, one row each (SL-1, #380).
//!
//! The parser dispatches on this table, and the documentation is rendered from
//! it: the block table in the crate documentation and the word table in the
//! README are both checked against [`doc_table`] and [`readme_table`] by the
//! tests below, so a word cannot be added, renamed or re-described in one
//! place and not the other.
//!
//! A word is reachable only through its row. [`Kind`] is constructed nowhere
//! but in [`LEXICON`], so a kind with no row is a variant the compiler reports
//! as never constructed, which the workspace's `-D warnings` refuses; and
//! `emit` dispatches on the kind of the row a line's first token names, so a
//! line whose word has no row is plain Rust and never reaches an emitter.
//!
//! The particles inside a form (`in`, `up to`, `times`, `waiting`, `at most`,
//! `with`) are part of that word's grammar, read by its emitter; they are not
//! words, and a line cannot start with one.

use lgwks_deps::proc_macro2::TokenTree;

use crate::lines::{Line, ident};

/// What a word does, which is what `emit` dispatches on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// `flow name(inputs) -> Output:`
    Flow,
    /// `each x in xs:`
    Each,
    /// `within 2s:`
    Within,
    /// `retry up to 3 times:`
    Retry,
    /// `together:`
    Together,
    /// `step name:`
    Step,
    /// `for x in xs:`
    For,
    /// `if cond:`
    If,
    /// `else:` / `else if cond:`
    Else,
    /// `let x = <block>:`
    Let,
    /// `run other(args)`
    Run,
    /// `give back value`
    GiveBack,
    /// `fail with reason`
    Fail,
}

impl Kind {
    /// The token a line starts with to say this word.
    pub(crate) const fn spelling(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Each => "each",
            Self::Within => "within",
            Self::Retry => "retry",
            Self::Together => "together",
            Self::Step => "step",
            Self::For => "for",
            Self::If => "if",
            Self::Else => "else",
            Self::Let => "let",
            Self::Run => "run",
            Self::GiveBack => "give",
            Self::Fail => "fail",
        }
    }
}

/// Where a word may stand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Position {
    /// Opens a flow at the top level of a script.
    Flow,
    /// Opens an indented block: its line ends in `:`.
    Block,
    /// Starts a line that opens no block.
    Line,
    /// Anywhere inside a line of Rust.
    Inline,
}

impl Position {
    /// The name the README table gives this position.
    const fn name(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Block => "block",
            Self::Line => "line",
            Self::Inline => "inline",
        }
    }
}

/// One written form of a word and what it means.
pub(crate) struct Form {
    /// The form as an author writes it.
    pub(crate) written: &'static str,
    /// What it does, in one clause.
    pub(crate) means: &'static str,
}

/// One word of the language: the six things SL-1 names, plus where it stands.
pub(crate) struct Word {
    /// What the word does; its spelling is [`Kind::spelling`].
    pub(crate) kind: Kind,
    /// Where it may stand.
    pub(crate) position: Position,
    /// The `lgwks_bot::script` primitive the word expands to a call of, as
    /// Markdown the README renders as written: code in backticks, and prose,
    /// such as a word that expands to plain Rust, as prose.
    pub(crate) primitive: &'static str,
    /// Its written forms. A word documented inside another word's form (`else`
    /// in `if`'s) has none of its own.
    pub(crate) forms: &'static [Form],
    /// What the word itself refuses, beyond the refusals every passthrough
    /// line carries (`refuse.rs`).
    pub(crate) refuses: &'static [&'static str],
    /// What holds wherever the word is used (SL-4).
    pub(crate) guarantees: &'static [&'static str],
    /// A whole script using the word, which the tests expand.
    pub(crate) example: &'static str,
}

/// `if` and `else` are one construct, so they share one example.
const IF_ELSE_EXAMPLE: &str = "flow route(code: u16) -> u8:\n    if code < 400:\n        give back 0\n    else if code < 500:\n        give back 1\n    else:\n        give back 2\n";

/// Every word, in the order the documentation lists them.
pub(crate) const LEXICON: [Word; 13] = [
    Word {
        kind: Kind::Flow,
        position: Position::Flow,
        primitive: "`Scope::enter`",
        forms: &[Form {
            written: "`[pub] flow name(inputs) [-> Output]:`",
            means: "an `async fn` taking the tenant [`Scope`] first and returning `Result<Output, FlowError>`",
        }],
        refuses: &["a promised output the last line never produces"],
        guarantees: &[
            "every step keyed by tenant and path",
            "the panicking lints forbidden by resolved path",
        ],
        example: "flow count(items: Vec<u8>) -> usize:\n    give back items.len()\n",
    },
    Word {
        kind: Kind::Each,
        position: Position::Block,
        primitive: "`each`",
        forms: &[
            Form {
                written: "`each x in xs:`",
                means: "every item, as many at once as the machine sustains, results in input order, first failure stops the rest",
            },
            Form {
                written: "`each x in xs, at most (limit) at once:`",
                means: "the same, under a limit that comes from somewhere named (an upstream's quota)",
            },
        ],
        refuses: &[
            "a concurrency bound typed as a number",
            "a bound outside `1..=65536`",
        ],
        guarantees: &[
            "bounded fan-out",
            "results in input order",
            "first failure cancels the rest",
        ],
        example: "flow sizes(pages: Vec<String>) -> Vec<usize>:\n    let sizes = each page in pages:\n        page.len()\n    give back sizes\n",
    },
    Word {
        kind: Kind::Within,
        position: Position::Block,
        primitive: "`within`",
        forms: &[Form {
            written: "`within 2s:`",
            means: "the block, or `TimedOut` when the deadline passes",
        }],
        refuses: &["a zero duration", "a duration with no unit"],
        guarantees: &["a deadline"],
        example: "flow ping(host: &Host) -> u16:\n    let code = within 2s:\n        host.ping().await.or_retry()?\n    give back code\n",
    },
    Word {
        kind: Kind::Retry,
        position: Position::Block,
        primitive: "`retry`",
        forms: &[Form {
            written: "`retry up to 3 times[, waiting 100ms]:`",
            means: "the block again while it fails transiently, same key each attempt, within the run's retry budget",
        }],
        refuses: &["attempts outside `1..=1000`", "a zero wait"],
        guarantees: &[
            "one idempotency key across attempts",
            "attempts within the run's retry budget",
        ],
        example: "flow fetch(site: &Site) -> Page:\n    let page = retry up to 3 times, waiting 200ms:\n        site.get(scope.key()).await.or_retry()?\n    give back page\n",
    },
    Word {
        kind: Kind::Together,
        position: Position::Block,
        primitive: "`try_join!`",
        forms: &[Form {
            written: "`together:`",
            means: "each line underneath concurrently; `let x = ..` lines bind their result",
        }],
        refuses: &[
            "anything before its `:`",
            "an indent under a line that opens no block",
        ],
        guarantees: &[
            "every branch owned by this task",
            "first failure cancels the rest",
        ],
        example: "flow both(left: &Api, right: &Api):\n    together:\n        let up = run probe(left)\n        let down = run probe(right)\n",
    },
    Word {
        kind: Kind::Step,
        position: Position::Block,
        primitive: "`Scope::enter`",
        forms: &[Form {
            written: "`step name:`",
            means: "a named scope: its own key and error location",
        }],
        refuses: &["a name that is not one word"],
        guarantees: &["its own step key", "failures located at the step"],
        example: "flow deploy(target: &Host):\n    step upload:\n        target.upload().await.or_fail()?\n",
    },
    Word {
        kind: Kind::For,
        position: Position::Block,
        primitive: "`Scope::item`",
        forms: &[Form {
            written: "`for x in xs:`",
            means: "every item in turn, each in its own scope",
        }],
        refuses: &["a missing pattern or missing items"],
        guarantees: &["one scope per item", "items in order, one at a time"],
        example: "flow notify(users: Vec<User>):\n    for user in users:\n        run email(user)\n",
    },
    Word {
        kind: Kind::If,
        position: Position::Block,
        primitive: "none: Rust `if`",
        forms: &[Form {
            written: "`if cond:` / `else if cond:` / `else:`",
            means: "as written",
        }],
        refuses: &["a missing condition"],
        guarantees: &["a value only when an `else:` ends the chain"],
        example: IF_ELSE_EXAMPLE,
    },
    Word {
        kind: Kind::Else,
        position: Position::Block,
        primitive: "none: Rust `else`",
        forms: &[],
        refuses: &[
            "an `else` not after an `if`",
            "an `else:` that is not the last branch",
        ],
        guarantees: &[],
        example: IF_ELSE_EXAMPLE,
    },
    Word {
        kind: Kind::Let,
        position: Position::Block,
        primitive: "the block's own",
        forms: &[Form {
            written: "`let x = <block>:`",
            means: "the block's last line becomes `x`",
        }],
        refuses: &["a `let` with no `=` or nothing after it"],
        guarantees: &["the block's failure propagates before `x` is bound"],
        example: "flow total(xs: Vec<u8>) -> u8:\n    let sum = within 1s:\n        xs.iter().sum()\n    give back sum\n",
    },
    Word {
        kind: Kind::Run,
        position: Position::Inline,
        primitive: "the callee flow",
        forms: &[Form {
            written: "`run other(args)`",
            means: "call another flow in this scope, await it, propagate its failure",
        }],
        refuses: &[],
        guarantees: &["the callee runs in this tenant's scope"],
        example: "flow outer(site: &Site) -> usize:\n    let pages = run crawl(site)\n    give back pages\n",
    },
    Word {
        kind: Kind::GiveBack,
        position: Position::Line,
        primitive: "none: `return Ok`",
        forms: &[Form {
            written: "`give back value`",
            means: "return from the flow",
        }],
        refuses: &["`give back` inside a block that has its own value"],
        guarantees: &["leaves the flow, never a nested block"],
        example: "flow one() -> u8:\n    give back 1\n",
    },
    Word {
        kind: Kind::Fail,
        position: Position::Line,
        primitive: "`FlowError::failed` / `FlowError::transient`",
        forms: &[Form {
            written: "`fail with reason` / `fail transiently with reason`",
            means: "stop with a permanent / retryable failure",
        }],
        refuses: &["a reason missing after `with`"],
        guarantees: &["a typed failure the caller can classify"],
        example: "flow check(ready: bool):\n    if !ready:\n        fail with \"not ready\"\n",
    },
];

/// The row `token` names, when it is a word.
///
/// The identifier is rendered once and compared with each spelling, rather
/// than asked of each row, so a lookup costs one rendering however long the
/// table grows.
pub(crate) fn word(token: &TokenTree) -> Option<&'static Word> {
    let name = ident(token)?.to_string();
    LEXICON.iter().find(|row| row.kind.spelling() == name)
}

/// Whether `token` is the word `kind`.
pub(crate) fn is(token: &TokenTree, kind: Kind) -> bool {
    word(token).is_some_and(|row| row.kind == kind)
}

/// The word a line starts with, if it starts with one.
pub(crate) fn kind_of(line: &Line) -> Option<Kind> {
    line.tokens.first().and_then(word).map(|row| row.kind)
}

/// The block words, as the refusal for an unknown block lists them.
pub(crate) fn block_words() -> String {
    let words: Vec<String> = LEXICON
        .iter()
        .filter(|row| row.position == Position::Block)
        .map(|row| format!("`{}`", row.kind.spelling()))
        .collect();
    words.join(", ")
}

/// The crate documentation's block table, as `//!` lines.
pub(crate) fn doc_table() -> String {
    let rows = LEXICON
        .iter()
        .flat_map(|row| row.forms)
        .map(|form| format!("//! | {} | {} |\n", form.written, form.means));
    ["//! | Written | Means |\n".to_owned(), "//! |---|---|\n".to_owned()]
        .into_iter()
        .chain(rows)
        .collect()
}

/// The README's word table: every row, with the columns the crate
/// documentation leaves to the README.
pub(crate) fn readme_table() -> String {
    let rows = LEXICON.iter().map(|row| {
        let written: Vec<&str> = row.forms.iter().map(|form| form.written).collect();
        let means: Vec<String> = row.forms.iter().map(|form| plain(form.means)).collect();
        let written = if written.is_empty() {
            format!("`{}` (see `if`)", row.kind.spelling())
        } else {
            written.join("<br>")
        };
        format!(
            "| {written} | {} | {} | {} | {} | {} |\n",
            row.position.name(),
            row.primitive,
            cell(&means.join("<br>")),
            cell(&row.refuses.join("; ")),
            cell(&row.guarantees.join("; ")),
        )
    });
    [
        "| Written | Where | Calls | Means | Refuses | Guarantees |\n".to_owned(),
        "|---|---|---|---|---|---|\n".to_owned(),
    ]
    .into_iter()
    .chain(rows)
    .collect()
}

/// An intra-doc link rendered as the code it names, for a page rustdoc does
/// not resolve.
fn plain(text: &str) -> String {
    text.replace("[`", "`").replace("`]", "`")
}

/// An empty cell written as a dash, so a reader sees "nothing" rather than a
/// gap.
fn cell(text: &str) -> &str {
    if text.is_empty() { "—" } else { text }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{LEXICON, doc_table, readme_table};
    use crate::tests::expand;

    /// The crate documentation's block table is the one the lexicon renders.
    #[test]
    fn the_crate_doc_table_is_rendered_from_the_lexicon() {
        let table = doc_table();
        assert!(
            include_str!("lib.rs").contains(&table),
            "src/lib.rs must carry the lexicon's block table verbatim:\n{table}"
        );
    }

    /// The README's word table is the one the lexicon renders.
    #[test]
    fn the_readme_table_is_rendered_from_the_lexicon() {
        let table = readme_table();
        assert!(
            include_str!("../README.md").contains(&table),
            "README.md must carry the lexicon's word table verbatim:\n{table}"
        );
    }

    /// One row per spelling: two rows for one token would make dispatch
    /// depend on table order.
    #[test]
    fn every_spelling_has_exactly_one_row() {
        let mut seen = HashSet::new();
        for row in &LEXICON {
            assert!(
                seen.insert(row.kind.spelling()),
                "`{}` has two rows",
                row.kind.spelling()
            );
        }
    }

    /// Every row's example is a script the macro expands, and it uses the
    /// word it illustrates.
    #[test]
    fn every_example_expands_and_uses_its_word() -> Result<(), String> {
        for row in &LEXICON {
            let spelling = row.kind.spelling();
            let uses_word = row
                .example
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .any(|token| token == spelling);
            assert!(uses_word, "the example for `{spelling}` never says it");
            expand(row.example)
                .map_err(|refusal| format!("the example for `{spelling}` is refused: {refusal}"))?;
        }
        Ok(())
    }
}
