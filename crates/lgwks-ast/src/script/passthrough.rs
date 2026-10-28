//! The lines a flow passes through as plain Rust (SL-3, #389).
//!
//! A line is passthrough when no word reads it: a plain Rust statement, a
//! `let` that opens no block, or a `together:` branch's value, with no `run`,
//! `observe` or `act` anywhere inside. Those are the lines the word-admission
//! census counts (`docs/script-word-admission.md`), because a shape that
//! keeps recurring among them is a word the language is missing (SL-7).
//!
//! Read from the parsed tree rather than recorded while parsing, so the
//! census and the code generator see one reading: a line is passthrough
//! exactly when the expansion emits it as written.

use lgwks_deps::proc_macro2::{Group, TokenStream, TokenTree};

use super::lines::text;
use super::tree::{Block, BranchValue, Code, Construct, Fragment, Passthrough, Statement};

/// Every passthrough line in `block` and the blocks inside it, in source
/// order.
pub(crate) fn of(block: &Block) -> Vec<Passthrough> {
    let mut found = Vec::new();
    block_lines(block, &mut found);
    found
}

/// The passthrough lines of one block, appended to `out`.
fn block_lines(block: &Block, out: &mut Vec<Passthrough>) {
    for statement in block.statements() {
        match *statement {
            Statement::Rust(ref code) | Statement::Let(ref code) => plain(code, out),
            Statement::Bind {
                block: ref construct,
                ..
            }
            | Statement::Construct(ref construct) => construct_lines(construct, out),
            Statement::Together(ref together) => {
                for branch in together.branches() {
                    match *branch.value() {
                        BranchValue::Construct(ref inner) => construct_lines(inner, out),
                        BranchValue::Rust(ref code) => bound(branch.pattern(), code, out),
                        BranchValue::Run(_) => {}
                    }
                }
            }
            Statement::For(ref each) => block_lines(each.body(), out),
            Statement::If(ref chain) => {
                for branch in chain.branches() {
                    block_lines(branch.body(), out);
                }
            }
            Statement::Compensate(ref undo) => block_lines(undo.undo(), out),
            Statement::Run(_) | Statement::GiveBack(_) | Statement::Fail { .. } => {}
        }
    }
}

/// The passthrough lines inside a block word's body. A `whenever`'s batch
/// expression is the word's own, so only its body is read.
fn construct_lines(construct: &Construct, out: &mut Vec<Passthrough>) {
    let body = match *construct {
        Construct::Each(ref each) => each.body(),
        Construct::Within(ref within) => within.body(),
        Construct::Retry(ref again) => again.body(),
        Construct::Step(ref step) | Construct::Saga(ref step) => step.body(),
        Construct::Whenever(ref feed) => feed.body(),
        Construct::Justify(ref block) => block.governed(),
        Construct::Graph(ref declared) => {
            for node in declared.nodes() {
                block_lines(node.body(), out);
            }
            return;
        }
    };
    block_lines(body, out);
}

/// `code` as a passthrough line, when nothing in it is a word's call.
fn plain(code: &Code, out: &mut Vec<Passthrough>) {
    let Some(tokens) = written(code) else {
        return;
    };
    if let Some(first) = tokens.first() {
        out.push(Passthrough {
            line: first.span().start().line,
            text: text(&tokens),
        });
    }
}

/// A `together:` branch's value as a passthrough line, written back with the
/// `let <pattern> =` the branch binds it to, so the census reads the line as
/// the author wrote it rather than its right-hand side alone.
fn bound(pattern: Option<&TokenStream>, code: &Code, out: &mut Vec<Passthrough>) {
    let Some(pattern) = pattern else {
        plain(code, out);
        return;
    };
    let Some(value) = written(code) else {
        return;
    };
    let binding: Vec<TokenTree> = pattern.clone().into_iter().collect();
    let Some(first) = binding.first().or_else(|| value.first()) else {
        return;
    };
    out.push(Passthrough {
        line: first.span().start().line,
        text: format!("let {} = {}", text(&binding), text(&value)),
    });
}

/// The tokens of `code` as written, or `None` when a `run`, `observe` or `act`
/// sits anywhere inside it, at any bracket depth.
fn written(code: &Code) -> Option<Vec<TokenTree>> {
    let mut tokens = Vec::with_capacity(code.fragments().len());
    for fragment in code.fragments() {
        match *fragment {
            Fragment::Token(ref token) => tokens.push(token.clone()),
            Fragment::Group {
                delimiter,
                span,
                ref inner,
            } => {
                let mut group = Group::new(delimiter, written(inner)?.into_iter().collect());
                group.set_span(span);
                tokens.push(TokenTree::Group(group));
            }
            Fragment::Run(_) | Fragment::Domain(_) => return None,
        }
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::super::read_source;

    /// The passthrough lines of the one flow in `source`, as `(line, text)`.
    fn listed(source: &str) -> Result<Vec<(usize, String)>, String> {
        let invocations = read_source(source).map_err(|refusal| refusal.to_string())?;
        let mut lines = Vec::new();
        for invocation in &invocations {
            let script = invocation.read().map_err(|refusal| refusal.to_string())?;
            for flow in script.flows() {
                for passed in flow.shape().passthrough() {
                    lines.push((passed.line(), passed.text().to_owned()));
                }
            }
        }
        Ok(lines)
    }

    #[test]
    fn a_word_anywhere_in_a_line_keeps_it_off_the_list_at_any_depth() -> Result<(), String> {
        let source = "script! {
    flow sweep(items: Vec<u8>, site: &Site) -> usize:
        let seen = site.get(items.len())
        let nested = site.pair((run probe(site)), 1)
        let polled: u16 = observe probe::status of site.key(1)
        site.note(seen).await?
        if seen > 2:
            site.mark([seen, 1])
        together:
            let left = site.ping(1)
            let right = run probe(site)
        give back items.len()
}";
        let expected = vec![
            (3, "let seen = site.get(items.len())".to_owned()),
            (6, "site.note(seen).await?".to_owned()),
            (8, "site.mark([seen, 1])".to_owned()),
            (10, "let left = site.ping(1)".to_owned()),
        ];
        let read = listed(source)?;
        if read == expected {
            Ok(())
        } else {
            Err(format!("expected {expected:?}, listed {read:?}"))
        }
    }
}
