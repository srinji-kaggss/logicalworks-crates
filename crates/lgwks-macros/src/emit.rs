//! From the script tree to Rust: every word becomes one call into
//! `lgwks_bot::script`.
//!
//! The emitted code is meant to be read. A flow expands to an `async fn` whose
//! body is a sequence of `each(..)`, `within(..)`, `retry(..)` and
//! `try_join!(..)` calls with ordinary Rust between them, so `cargo expand`
//! shows the orchestration the script declared and nothing it did not.
//!
//! Nothing here reads the language. [`lgwks_ast::script::parse`] has already
//! read every word, decided every refusal and chosen every step label (labels
//! are structural and never carry a line number; see that module), so this
//! module only writes Rust for a tree it is handed. The one thing it can still
//! refuse is a word its version does not generate: a tree from a newer
//! `lgwks_ast` than this crate was built against.

use lgwks_ast::script::{
    Block, Bound, BranchValue, Call, Code, Construct, Duration, Each, Flow, FlowShape, For,
    Fragment, IfChain, Kind, Retry, Script, Site, Statement, Step, StepShape, Together, Within,
};
use lgwks_deps::proc_macro2::{Group, Ident, Literal, Span, TokenStream, TokenTree};
use lgwks_deps::quote::{ToTokens, quote};
use lgwks_deps::syn::{Error, Result};

/// The runtime every expansion calls into.
fn runtime() -> TokenStream {
    quote!(::lgwks_bot::script)
}

/// The refusal for a word this crate does not generate: the tree came from a
/// newer `lgwks_ast` than the one this crate was released with.
fn newer_word(what: &str) -> Error {
    Error::new(
        Span::call_site(),
        format!(
            "this script uses a {what} that the resolved `lgwks_ast` reads but this \
             `lgwks_macros` does not generate; use the `lgwks_macros` released with that \
             `lgwks_ast`"
        ),
    )
}

/// `Result<T, FlowError>` spelled out, so the expansion names nothing the
/// caller must import.
///
/// A value that ends in `?` is bound first: `Ok(x?)` is what a reader (and
/// `clippy::needless_question_mark`) flags, yet the `?` must stay, because it
/// is what converts a verb's `BotError` or an `io::Error` into a flow failure.
fn ok(value: &TokenStream) -> TokenStream {
    let script = runtime();
    let ends_in_question = value.clone().into_iter().last().is_some_and(
        |token| matches!(token, TokenTree::Punct(ref punct) if punct.as_char() == '?'),
    );
    if ends_in_question {
        quote!({
            let flow_value = #value;
            ::core::result::Result::Ok::<_, #script::FlowError>(flow_value)
        })
    } else {
        quote!(::core::result::Result::Ok::<_, #script::FlowError>(#value))
    }
}

/// The lints every generated flow carries at `forbid`, so the consumer's
/// compiler (`unsafe_code`) and clippy (the rest) refuse by resolved path what
/// the parser's token check can only refuse by spelling. `forbid` rather than
/// `deny` because an `#[allow]` inside the flow cannot lower it.
fn lint_forbids() -> TokenStream {
    quote! {
        #[forbid(
            unsafe_code,
            clippy::unwrap_used,
            clippy::expect_used,
            clippy::panic,
            clippy::todo,
            clippy::unimplemented,
            clippy::unreachable,
            clippy::indexing_slicing,
            clippy::exit,
            clippy::mem_forget,
            clippy::panic_in_result_fn
        )]
    }
}

/// A block's emitted body: its statements and its value.
struct Body {
    /// Every statement but the value, each ending its own statement.
    statements: TokenStream,
    /// The value, and whether it is a flow `Result` rather than the value.
    tail: Option<(TokenStream, bool)>,
    /// Whether the last line leaves the block, so no value follows it.
    diverges: bool,
}

impl Body {
    /// `{ statements; Ok(tail) }` for an async body that returns a flow result.
    fn into_ok_block(self) -> TokenStream {
        let Self {
            statements,
            tail,
            diverges,
        } = self;
        let value = match tail {
            _ if diverges => TokenStream::new(),
            Some((tokens, true)) => tokens,
            Some((tokens, false)) => ok(&tokens),
            None => ok(&quote!(())),
        };
        quote!({ #statements #value })
    }

    /// `{ statements; tail }` for a plain Rust block.
    fn into_block(self) -> TokenStream {
        let Self {
            statements, tail, ..
        } = self;
        let value = tail.map(|(tokens, fallible)| if fallible { quote!(#tokens?) } else { tokens });
        quote!({ #statements #value })
    }
}

/// Expand a whole script: every flow, then the architecture map.
///
/// # Errors
///
/// A word this crate's version does not generate.
pub(crate) fn script(script: &Script) -> Result<TokenStream> {
    let mut output = TokenStream::new();
    let mut flows = Vec::new();
    for flow in script.flows() {
        output.extend(flow_code(flow)?);
        flows.push(flow_shape(flow.shape()));
    }
    let runtime = runtime();
    output.extend(quote! {
        /// The orchestration this `script!` block declares, compiled from the
        /// same tokens as its flows. See `lgwks_bot::script::Architecture`.
        pub const ARCHITECTURE: #runtime::Architecture = #runtime::Architecture::new(&[#(#flows),*]);
    });
    Ok(output)
}

/// One flow as an `async fn`.
fn flow_code(flow: &Flow) -> Result<TokenStream> {
    let body_block = body(flow.body())?.into_ok_block();
    let script = runtime();
    let name = flow.name();
    let name_text = name.to_string();
    let inputs = flow.inputs();
    let inputs = if inputs.is_empty() {
        quote!(scope: &#script::Scope)
    } else {
        quote!(scope: &#script::Scope, #inputs)
    };
    let unit = quote!(());
    // A flow that promises no output type returns `()`, which is the declared
    // output of a flow whose signature has no `->`. Matched rather than
    // substituted for a missing type: the two arms are different signatures.
    let output = match flow.output() {
        Some(ty) => ty,
        None => &unit,
    };
    let attributes = flow.attributes();
    let has_doc = attributes.clone().into_iter().any(|token| {
        matches!(token, TokenTree::Group(ref group) if group.stream().into_iter().next().is_some_and(|first| matches!(first, TokenTree::Ident(ref word) if word == "doc")))
    });
    let doc = if has_doc {
        TokenStream::new()
    } else {
        let text = format!(" Flow `{name_text}`, written with `script!`.");
        quote!(#[doc = #text])
    };
    // The helper traits are imported only where a flow calls them, so an
    // expansion never carries an unused import into a crate that denies one.
    let helpers = if mentions(&body_block, &["or_fail", "or_retry"]) {
        quote!(use #script::{OptionExt as _, ResultExt as _};)
    } else {
        TokenStream::new()
    };
    // After the author's own attributes, so an `#[allow(..)]` written above the
    // flow cannot lower what the flow forbids.
    let forbids = lint_forbids();
    let visibility = flow.visibility();
    let header = flow.shape();
    let site = site_static(
        Kind::Flow,
        header.line(),
        &format!("flow {}", header.signature()),
    );
    Ok(quote! {
        #attributes
        #forbids
        #doc
        #visibility async fn #name(#inputs) -> ::core::result::Result<#output, #script::FlowError> {
            #helpers
            let scope = &scope.enter_at(#name_text, #site)?;
            let outcome: ::core::result::Result<#output, #script::FlowError> = async #body_block.await;
            scope.settle(&outcome);
            outcome.map_err(|error| error.located(scope))
        }
    })
}

/// A block's statements, with its last expression set apart as its value when
/// the block wants one.
fn body(block: &Block) -> Result<Body> {
    let mut statements = TokenStream::new();
    let mut tail = None;
    let value_at = block
        .tail()
        .map(|_| block.statements().len().saturating_sub(1));
    for (index, line) in block.statements().iter().enumerate() {
        let tokens = statement(line)?;
        if Some(index) == value_at {
            tail = Some((tokens, line.is_fallible()));
        } else if line.is_fallible() {
            statements.extend(quote!(#tokens?;));
        } else if line.is_expression() {
            statements.extend(quote!(#tokens;));
        } else {
            statements.extend(tokens);
        }
    }
    Ok(Body {
        statements,
        tail,
        diverges: block.diverges(),
    })
}

/// One line, or one `if` chain.
fn statement(line: &Statement) -> Result<TokenStream> {
    let script = runtime();
    match *line {
        Statement::Rust(ref rust) => code(rust),
        Statement::Let(ref rust) => {
            let rust = code(rust)?;
            Ok(quote!(#rust;))
        }
        Statement::Run(ref run) => call(run),
        Statement::Bind {
            ref pattern,
            ref block,
        } => {
            let value = construct(block)?;
            Ok(quote!(let #pattern = #value?;))
        }
        Statement::Construct(ref block) => construct(block),
        Statement::Together(ref branches) => together(branches),
        Statement::For(ref each) => for_loop(each),
        Statement::If(ref chain) => if_chain(chain),
        Statement::GiveBack(ref value) => {
            let value = code(value)?;
            Ok(quote!(return ::core::result::Result::Ok(#value);))
        }
        Statement::Fail {
            transient,
            ref reason,
        } => {
            let constructor = if transient {
                quote!(transient)
            } else {
                quote!(failed)
            };
            let reason = code(reason)?;
            Ok(
                quote!(return ::core::result::Result::Err(#script::FlowError::#constructor(#reason));),
            )
        }
        _ => Err(newer_word("statement")),
    }
}

/// A block that yields a value: `each`, `within`, `retry`, `step`.
fn construct(block: &Construct) -> Result<TokenStream> {
    match *block {
        Construct::Each(ref fan) => each(fan),
        Construct::Within(ref deadline) => within(deadline),
        Construct::Retry(ref attempts) => retry(attempts),
        Construct::Step(ref named) => step(named),
        _ => Err(newer_word("block word")),
    }
}

/// `each <pattern> in <items>:`.
fn each(fan: &Each) -> Result<TokenStream> {
    let script = runtime();
    let limit = match fan.bound() {
        None => quote!(::core::option::Option::None),
        Some(written) => {
            let limit = bound(written)?;
            quote!(::core::option::Option::Some(#script::at_most(#limit)?))
        }
    };
    let label = fan.label();
    let items = code(fan.items())?;
    let pattern = fan.pattern();
    let inner = body(fan.body())?.into_ok_block();
    let site = site_of(fan.site());
    Ok(quote! {
        #script::each_at(scope, #label, #site, #limit, #items, async |scope: #script::Scope, #pattern| {
            let scope = &scope;
            let outcome: ::core::result::Result<_, #script::FlowError> = async #inner.await;
            scope.settle(&outcome);
            outcome
        }).await
    })
}

/// `within <duration>:`.
fn within(deadline: &Within) -> Result<TokenStream> {
    let script = runtime();
    let label = deadline.label();
    let limit = duration(deadline.limit())?;
    let inner = body(deadline.body())?.into_ok_block();
    let site = site_of(deadline.site());
    Ok(quote! {
        #script::within_at(scope, #label, #site, #limit, async #inner).await
    })
}

/// `retry up to <N> times[, waiting <duration>]:`.
fn retry(attempts: &Retry) -> Result<TokenStream> {
    let script = runtime();
    let label = attempts.label();
    let count = bound(attempts.attempts())?;
    let waiting = match attempts.waiting() {
        None => quote!(::core::time::Duration::from_millis(100)),
        Some(pause) => duration(pause)?,
    };
    let inner = body(attempts.body())?.into_ok_block();
    let site = site_of(attempts.site());
    Ok(quote! {
        #script::retry_at(scope, #label, #site, #script::attempts(#count)?, #waiting, async |scope: #script::Scope, attempt: u32| {
            let scope = &scope;
            let _ = attempt;
            #inner
        }).await
    })
}

/// `step <name>:`.
fn step(named: &Step) -> Result<TokenStream> {
    let script = runtime();
    let label = named.label();
    let inner = body(named.body())?.into_ok_block();
    let site = site_of(named.site());
    Ok(quote! {
        {
            let scope = &scope.enter_at(#label, #site)?;
            let outcome: ::core::result::Result<_, #script::FlowError> = async #inner.await;
            scope.settle(&outcome);
            outcome.map_err(|error| error.located(scope))
        }
    })
}

/// `together:` and its branches, run concurrently on this task.
fn together(branches: &Together) -> Result<TokenStream> {
    let mut patterns = Vec::new();
    let mut futures = Vec::new();
    for branch in branches.branches() {
        patterns.push(branch.pattern().map_or_else(|| quote!(_), Clone::clone));
        let value = match *branch.value() {
            BranchValue::Construct(ref block) => construct(block)?,
            BranchValue::Run(ref run) => call(run)?,
            BranchValue::Rust(ref rust) => ok(&code(rust)?),
            _ => {
                let refusal = newer_word("`together:` branch");
                lgwks_std::trace::debug!(error = %refusal, "together: returning an error to the caller");
                return Err(refusal);
            }
        };
        futures.push(quote!(async { #value }));
    }
    Ok(quote!(let (#(#patterns,)*) = ::lgwks_bot::try_join!(#(#futures),*)?;))
}

/// `for <pattern> in <items>:`, sequential, one scope per iteration.
fn for_loop(each: &For) -> Result<TokenStream> {
    let label = each.label();
    let items = code(each.items())?;
    let pattern = each.pattern();
    let script = runtime();
    let site = site_of(each.site());
    let iteration = body(each.body())?;
    // An iteration that reaches its end settles as done. One whose last line
    // leaves the flow has no end to reach, and a settle after it would be code
    // the compiler reports as unreachable; it stays unsettled, and the step
    // that the exit settles says why.
    let settled = if iteration.diverges {
        TokenStream::new()
    } else {
        quote!(scope.settle::<()>(&::core::result::Result::<(), #script::FlowError>::Ok(()));)
    };
    let inner = iteration.into_block();
    Ok(quote! {
        for (lgwks_script_index, #pattern) in ::core::iter::IntoIterator::into_iter(#items).enumerate() {
            let scope = &scope.item_at(#label, lgwks_script_index, #site)?;
            #inner
            #settled
        }
    })
}

/// `if cond:` with its `else if cond:` and `else:` branches.
fn if_chain(chain: &IfChain) -> Result<TokenStream> {
    let mut tokens = TokenStream::new();
    for (position, branch) in chain.branches().iter().enumerate() {
        let inner = body(branch.body())?.into_block();
        let keyword = match (position, branch.condition()) {
            (_, None) => quote!(else),
            (0, Some(condition)) => {
                let condition = code(condition)?;
                quote!(if #condition)
            }
            (_, Some(condition)) => {
                let condition = code(condition)?;
                quote!(else if #condition)
            }
        };
        tokens.extend(quote!(#keyword #inner));
    }
    Ok(tokens)
}

/// A line of Rust with each `run` call written as an awaited call whose
/// failure propagates.
fn code(rust: &Code) -> Result<TokenStream> {
    let mut tokens = TokenStream::new();
    for fragment in rust.fragments() {
        match *fragment {
            Fragment::Token(ref token) => tokens.extend([token.clone()]),
            Fragment::Group {
                delimiter,
                span,
                ref inner,
            } => {
                let mut rebuilt = Group::new(delimiter, code(inner)?);
                rebuilt.set_span(span);
                tokens.extend([TokenTree::Group(rebuilt)]);
            }
            Fragment::Run(ref run) => {
                let run = call(run)?;
                tokens.extend(quote!((#run?)));
            }
            _ => {
                let refusal = newer_word("fragment of Rust");
                lgwks_std::trace::debug!(error = %refusal, "code: returning an error to the caller");
                return Err(refusal);
            }
        }
    }
    Ok(tokens)
}

/// A `run` call, awaited, as a flow `Result`: the call itself is the scope's
/// child when its label is the callee's own name, and enters a scope of its
/// numbered label otherwise, so two calls in one scope never share a key.
fn call(run: &Call) -> Result<TokenStream> {
    let path = run.path();
    let arguments = if run.arguments().is_empty() {
        TokenStream::new()
    } else {
        let arguments = code(run.arguments())?;
        quote!(, #arguments)
    };
    if run.label() == run.callee() {
        // The callee enters its own name, from its own header line.
        return Ok(quote!(#path(scope #arguments).await));
    }
    let label = run.label();
    let site = site_of(run.site());
    Ok(quote!({
        let called = &scope.enter_at(#label, #site)?;
        let outcome = #path(called #arguments).await;
        called.settle(&outcome);
        outcome
    }))
}

/// A `&'static Site` for `site`, as a block holding the one `static` it names.
fn site_of(site: &Site) -> TokenStream {
    site_static(site.kind(), site.line(), site.text())
}

/// A `&'static Site` for the line `text`, the word `kind`, on source line
/// `line`.
///
/// A `static` rather than a value: the trail keeps a pointer per step, so the
/// line's text is written into the binary once and never copied at run time.
/// The variant is named by the word's `Debug` rendering, as [`step_shape`]
/// names it.
fn site_static(kind: Kind, line: usize, text: &str) -> TokenStream {
    let script = runtime();
    let kind = Ident::new(&format!("{kind:?}"), Span::call_site());
    let line = line_literal(line);
    quote!({
        static SITE: #script::Site = #script::Site::new(#script::StepKind::#kind, #line, #text);
        &SITE
    })
}

/// A bound as the runtime's argument.
fn bound(written: &Bound) -> Result<TokenStream> {
    match *written {
        Bound::Count(count) => Ok(Literal::u64_unsuffixed(count).into_token_stream()),
        Bound::Expr(ref expr) => Ok(expr.clone()),
        _ => Err(newer_word("bound")),
    }
}

/// A duration as a `core::time::Duration` expression.
fn duration(written: &Duration) -> Result<TokenStream> {
    match *written {
        Duration::Nanos(nanos) => {
            let nanos = Literal::u64_unsuffixed(nanos);
            Ok(quote!(::core::time::Duration::from_nanos(#nanos)))
        }
        Duration::Expr(ref expr) => Ok(expr.clone()),
        _ => Err(newer_word("duration")),
    }
}

/// The runtime map's line, a `u32` literal saturating at `u32::MAX`.
///
/// The tree keeps the source's own line; the runtime's map stores a `u32`. A
/// source with more lines than that cannot be compiled by any host, and a
/// saturated line keeps the entry on the last line there is rather than
/// wrapping onto some other line of the file.
fn line_literal(line: usize) -> Literal {
    match u32::try_from(line) {
        Ok(number) => Literal::u32_unsuffixed(number),
        Err(_more_lines_than_a_u32_holds) => Literal::u32_unsuffixed(u32::MAX),
    }
}

/// A flow's `FlowShape::new(..)` entry in the map.
fn flow_shape(shape: &FlowShape) -> TokenStream {
    let script = runtime();
    let name = shape.name();
    let signature = shape.signature();
    let line = line_literal(shape.line());
    let steps = shape.steps().iter().map(step_shape);
    quote! {
        #script::FlowShape::new(#name, #signature, #line, &[#(#steps),*])
    }
}

/// A step's `StepShape::new(..)` entry in the map.
///
/// The variant is named by the word's `Debug` rendering: `lgwks_bot`'s
/// `StepKind` has one variant per map word, spelled as the lexicon's `Kind`
/// variant is, so the derive is the one spelling both crates share.
fn step_shape(shape: &StepShape) -> TokenStream {
    let script = runtime();
    let kind = Ident::new(&format!("{:?}", shape.kind()), Span::call_site());
    let subject = shape.subject();
    let detail = shape.detail();
    let line = line_literal(shape.line());
    let children = shape.children().iter().map(step_shape);
    quote! {
        #script::StepShape::new(#script::StepKind::#kind, #subject, #detail, #line, &[#(#children),*])
    }
}

/// Whether any identifier in `stream`, at any depth, is one of `words`.
fn mentions(stream: &TokenStream, words: &[&str]) -> bool {
    stream.clone().into_iter().any(|token| match token {
        TokenTree::Ident(ref word) => words.iter().any(|wanted| word == wanted),
        TokenTree::Group(ref inner) => mentions(&inner.stream(), words),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}
