//! `script!`: orchestration written the way it is said.
//!
//! Re-exported as `lgwks_bot::script!`; use it from there. This crate writes
//! the Rust. The language is read by `lgwks_ast::script::parse`, the one
//! parser every tool that reads a script also calls, and the semantics live in
//! `lgwks_bot::script`, where every block below expands to one call.
//!
//! ```text
//! lgwks_bot::script! {
//!     /// Fetch every page for a tenant and report their sizes.
//!     pub flow crawl(site: &Site, paths: Vec<String>) -> Vec<usize>:
//!         let pages = each path in paths:
//!             retry up to 3 times, waiting 200ms:
//!                 within 5s:
//!                     site.fetch(scope.key(), &path).await.or_retry()?
//!         give back pages.iter().map(|page| page.len()).collect()
//! }
//! ```
//!
//! # The blocks
//!
//! | Written | Means |
//! |---|---|
//! | `[pub] flow name(inputs) [-> Output]:` | an `async fn` taking the tenant [`Scope`] first and returning `Result<Output, FlowError>` |
//! | `each x in xs:` | every item, as many at once as the machine sustains, results in input order, first failure stops the rest |
//! | `each x in xs, at most (limit) at once:` | the same, under a limit that comes from somewhere named (an upstream's quota) |
//! | `within 2s:` | the block, or `TimedOut` when the deadline passes |
//! | `retry up to 3 times[, waiting 100ms]:` | the block again while it fails transiently, same key each attempt, within the run's retry budget |
//! | `together:` | each line underneath concurrently; `let x = ..` lines bind their result |
//! | `step name:` | a named scope: its own key and error location |
//! | `for x in xs:` | every item in turn, each in its own scope |
//! | `if cond:` / `else if cond:` / `else:` | as written |
//! | `let x = <block>:` | the block's last line becomes `x` |
//! | `run other(args)` | call another flow in this scope, await it, propagate its failure |
//! | `observe domain::id of target` | poll the source the host's registry declares under `domain::id`, built from `target`, as a step of its own; bind it with a type, `let n: u16 = ..` |
//! | `act domain::id on target with value` | run the action the host's registry declares under `domain::id`, built from `target`, on `value`, as a step of its own |
//! | `give back value` | return from the flow |
//! | `fail with reason` / `fail transiently with reason` | stop with a permanent / retryable failure |
//! | `graph:` with `node name[ needs a, b]:` under it | every node once, each as soon as the nodes it needs have succeeded, longest remaining chain first, as many at once as the machine sustains; the value is the run's `GraphReport` |
//! | `graph, at most (limit) at once:` | the same, under a limit that comes from somewhere named |
//! | `node name[ needs a, b]:` | one node of the enclosing `graph`: its body runs once, after `a` and `b` have succeeded |
//! | `saga name:` with `compensate step:` after the steps it undoes | a named scope whose finished steps are undone, newest first, when a later line fails; its value is the body's |
//! | `compensate step:` | the undo of `step`, registered once that step has finished, run only if the saga later fails |
//! | `whenever x in batches, buffering N, drop oldest:` (or `drop newest`, `refuse`) | each event of a live feed in arrival order, each in its own scope; `batches` is awaited for the next `Option<Vec<_>>` and `None` ends the feed; at most `N` events wait, and past that the policy decides; the value is the run's `PumpReport` |
//! | `justify with (rules):` | the block in a scope that answers to `rules`: every step, node and retry attempt inside asks them first, and is slowed, parked or stopped as they decide |
//!
//! Every step keys by tenant, run and path (`Scope::key`): one run's attempts
//! and restarts share a key, two runs of one flow never do, and work that is
//! one act across runs names itself with `Scope::business_key`.
//!
//! Any other line is Rust, passed through as written. Inside every flow
//! `scope` is the current [`Scope`], and `.or_fail()` / `.or_retry()` turn a
//! foreign error into a flow failure.
//!
//! # What it refuses
//!
//! Each of these is a compile error naming the replacement: a concurrency
//! bound typed as a number (the runtime sizes fan-out to the host); attempts
//! outside `1..=1000`; zero durations; `unwrap`, `expect`, `unwrap_err` and
//! `expect_err`, as a method or by path (`Option::unwrap(x)`); `panic!`,
//! every `assert*!` and `debug_assert*!`, and the rest of the panicking
//! macros; indexing and slicing (`xs[i]`); `loop`, `while`, `spawn`,
//! `unbounded_channel`, `block_on`, `thread::sleep`, `process::exit`,
//! `process::abort`, `mem::forget`, `unsafe`; a `use` inside a flow that would
//! rename any of those; absolute paths from one machine anywhere in a string;
//! `give back` from inside a block whose value is its own last line; a flow
//! that promises an output but ends without one; a `graph` whose needs repeat,
//! name no node or form a cycle; a `node` outside a `graph`; a `compensate`
//! outside a `saga` or naming no step before it; a `whenever` with no buffer
//! or no overflow policy; and a `justify` with no rules.
//!
//! A macro sees tokens, not resolved names, so a call imported *outside* the
//! script under another spelling is invisible to those refusals. Every
//! generated flow therefore also carries `#[forbid(..)]` on `unsafe_code` and
//! on the clippy lints for the same defects (`unwrap_used`, `expect_used`,
//! `panic`, `indexing_slicing`, `exit`, `mem_forget`, `panic_in_result_fn`,
//! ...), which the consumer's own compiler and `cargo clippy` enforce by path.
//! It is placed after the author's attributes, so an `#[allow]` written above
//! a flow cannot lower it.
//!
//! Every script also emits `ARCHITECTURE`: the flows it declares and the tree
//! of blocks inside each, compiled from the same tokens.
//!
//! [`Scope`]: https://docs.rs/lgwks_bot/latest/lgwks_bot/script/struct.Scope.html

mod emit;
mod error;

use error::Error;
use lgwks_deps::proc_macro2::TokenStream;

/// Expand an indented orchestration script into `async fn`s over
/// `lgwks_bot::script` and an `ARCHITECTURE` map. See the crate
/// documentation for the language.
#[proc_macro]
pub fn script(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let expanded = lgwks_ast::script::parse(TokenStream::from(input))
        .map_err(|refusal| Error::new(refusal.span(), refusal.message()))
        .and_then(|script| emit::script(&script));
    match expanded {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lexicon_docs;

#[cfg(test)]
mod sim_emit;
