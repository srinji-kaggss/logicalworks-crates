//! The language, checked on source text without a compiler round trip.
//!
//! `proc_macro2` parses source text outside a real macro expansion and, with
//! span locations on, keeps each token's line and column. That lets every
//! refusal be asserted by its exact message: a `compile_fail` doctest passes
//! on any compile error at all, and so cannot tell a refusal from a typo.

use std::str::FromStr;

use lgwks_deps::proc_macro2::TokenStream;

use crate::emit;

/// Expand `source` as the macro would, as text or as the refusal message.
///
/// The crate's one expansion fixture: every test module that asserts on an
/// expansion reads it through here. It is the macro's own path, `parse` then
/// `emit`, so a refusal asserted here is the one `cargo build` reports.
pub(crate) fn expand(source: &str) -> Result<String, String> {
    let stream = TokenStream::from_str(source).map_err(|error| error.to_string())?;
    let script = lgwks_ast::script::parse(stream).map_err(|refusal| refusal.to_string())?;
    emit::script(&script)
        .map(|tokens| tokens.to_string())
        .map_err(|error| error.to_string())
}

/// Every refusal the language makes, each with the fragment of its message
/// that names the replacement.
///
/// A word's guarantee may cite a case here by its name (`lexicon::Evidence`),
/// which is why the table is visible to the crate.
pub(crate) const REFUSALS: [(&str, &str, &str); 34] = [
    (
        "unwrap by path",
        "flow f(x: Option<u8>):\n    let n = Option::unwrap(x)\n",
        "unwrap()` ends the program",
    ),
    (
        "expect by turbofish path",
        "flow f(r: Result<u8, String>):\n    let n = Result::<u8, String>::expect(r, \"n\")\n",
        "expect()` ends the program",
    ),
    (
        "unwrap_err",
        "flow f(r: Result<u8, String>):\n    let e = r.unwrap_err()\n",
        "unwrap_err()` ends the program",
    ),
    (
        "expect_err",
        "flow f(r: Result<u8, String>):\n    let e = r.expect_err(\"e\")\n",
        "expect_err()` ends the program",
    ),
    (
        "assert_eq",
        "flow f(a: u8, b: u8):\n    assert_eq!(a, b)\n",
        "`assert_eq!` ends the program",
    ),
    (
        "debug_assert",
        "flow f(a: bool):\n    debug_assert!(a)\n",
        "`debug_assert!` ends the program",
    ),
    (
        "process exit",
        "flow f():\n    std::process::exit(1)\n",
        "ending the process from a flow",
    ),
    (
        "process abort",
        "flow f():\n    std::process::abort()\n",
        "ending the process from a flow",
    ),
    (
        "mem forget",
        "flow f(guard: String):\n    std::mem::forget(guard)\n",
        "`mem::forget` leaks",
    ),
    (
        "indexing",
        "flow f(xs: Vec<u8>) -> u8:\n    xs[3]\n",
        "indexing ends the program",
    ),
    (
        "slicing a call's result",
        "flow f(text: String) -> usize:\n    text.as_bytes()[1..].len()\n",
        "indexing ends the program",
    ),
    (
        "import that renames a refused call",
        "flow f():\n    use std::thread::{sleep as pause};\n    pause(d)\n",
        "`use` of `thread`",
    ),
    (
        "machine path inside a format string",
        "flow f(name: &str):\n    let p = format!(\"{}/home/me/{}\", root, name)\n",
        "absolute path from one machine",
    ),
    (
        "typed concurrency",
        "flow f(xs: Vec<u8>):\n    each x in xs, at most 16 at once:\n        x\n",
        "typed concurrency number",
    ),
    (
        "zero fan-out",
        "flow f(xs: Vec<u8>):\n    each x in xs, at most 0 at once:\n        x\n",
        "typed concurrency number",
    ),
    (
        "fan-out past the ceiling",
        "flow f(xs: Vec<u8>):\n    each x in xs, at most 70000 at once:\n        x\n",
        "typed concurrency number",
    ),
    (
        "zero attempts",
        "flow f():\n    retry up to 0 times:\n        1\n",
        "outside 1..=1000",
    ),
    (
        "zero deadline",
        "flow f():\n    within 0s:\n        1\n",
        "zero duration",
    ),
    (
        "duration without a unit",
        "flow f():\n    within 5:\n        1\n",
        "number with a unit",
    ),
    (
        "unwrap",
        // Spaced as written by hand: the refusal reads tokens, not text.
        "flow f(text: String):\n    let n: u8 = text.parse(). unwrap ()\n",
        "unwrap()` ends the program",
    ),
    (
        "expect",
        "flow f(text: String):\n    let n: u8 = text.parse().expect(\"n\")\n",
        "`.expect()` ends the program",
    ),
    (
        "panic",
        "flow f():\n    panic!(\"no\")\n",
        "`panic!` ends the program",
    ),
    (
        "unbounded loop",
        "flow f():\n    loop { }\n",
        "a loop with no bound",
    ),
    (
        "while",
        "flow f():\n    while true { }\n",
        "a loop with no bound",
    ),
    (
        "spawn",
        "flow f():\n    tokio::spawn(async {})\n",
        "a spawned task has no owner",
    ),
    (
        "thread sleep",
        "flow f():\n    std::thread::sleep(d)\n",
        "`thread::sleep` stalls",
    ),
    (
        "machine path",
        "flow f():\n    let p = \"/Users/me/data.csv\"\n",
        "absolute path from one machine",
    ),
    (
        "give back from a nested block",
        "flow f() -> u8:\n    within 1s:\n        give back 1\n",
        "`give back` returns from the flow",
    ),
    (
        "if with no else as the value",
        "flow f(ready: bool) -> u8:\n    if ready:\n        1\n",
        "promises an output",
    ),
    (
        "promised output never produced",
        "flow f() -> u8:\n    let x = 1\n",
        "promises an output",
    ),
    (
        "stray indent",
        "flow f():\n    let x = 1\n        let y = 2\n",
        "unexpected indent",
    ),
    (
        "header with no block",
        "flow f():\n    within 1s:\nflow g():\n    1\n",
        "nothing is indented beneath it",
    ),
    (
        "unknown block",
        "flow f():\n    whenever x:\n        1\n",
        "unknown block",
    ),
    (
        "else without if",
        "flow f():\n    else:\n        1\n",
        "`else:` must follow",
    ),
];

#[test]
fn every_refusal_names_its_replacement() -> Result<(), String> {
    for (case, source, expected) in REFUSALS {
        let Err(message) = expand(source) else {
            return Err(format!("{case}: expanded instead of refusing"));
        };
        assert!(
            message.contains(expected),
            "{case}: expected {expected:?}, got {message:?}"
        );
    }
    Ok(())
}

/// The near neighbours of every refusal, which must still expand: a refusal
/// that also catches these would push authors back to writing the flow by
/// hand, which is the opposite of what the language is for.
#[test]
fn the_near_misses_of_each_refusal_still_expand() -> Result<(), String> {
    let source = "flow f(xs: Vec<u8>, pair: [u8; 2], r: Result<u8, String>) -> u8:\n\
                  \x20   let [a, b] = pair\n\
                  \x20   let table: [u8; 2] = [a, b]\n\
                  \x20   let built = vec![1, 2]\n\
                  \x20   let first = xs.get(3).copied().unwrap_or(0)\n\
                  \x20   let fallback = r.unwrap_or_default()\n\
                  \x20   let tail = xs.get(1..).map_or(0, <[u8]>::len)\n\
                  \x20   let kept = std::mem::take(&mut vec![0u8])\n\
                  \x20   for x in [1, 2]:\n\
                  \x20       let seen = x\n\
                  \x20   give back first + fallback + table.len() as u8 + built.len() as u8 + tail as u8 + kept.len() as u8\n";
    let expanded = expand(source)?;
    assert!(
        expanded.contains("forbid"),
        "every flow carries the forbidden lints: {expanded}"
    );
    Ok(())
}

#[test]
fn every_flow_forbids_the_lints_the_tokens_cannot_resolve() -> Result<(), String> {
    let expanded = expand("pub flow f():\n    let x = 1\n")?;
    for lint in [
        "unsafe_code",
        "clippy :: unwrap_used",
        "clippy :: expect_used",
        "clippy :: panic",
        "clippy :: indexing_slicing",
        "clippy :: exit",
        "clippy :: mem_forget",
        "clippy :: panic_in_result_fn",
    ] {
        assert!(expanded.contains(lint), "missing {lint:?} in {expanded}");
    }
    let at_forbid = expanded.find("forbid").ok_or("no forbid attribute")?;
    let at_fn = expanded.find("async fn").ok_or("no flow")?;
    assert!(
        at_forbid < at_fn,
        "the attribute sits on the flow, not after it"
    );
    Ok(())
}

#[test]
fn a_valid_script_expands_to_calls_into_the_runtime_and_a_map() -> Result<(), String> {
    let source = "pub flow crawl(paths: Vec<String>) -> Vec<usize>:\n\
                  \x20   let sizes = each path in paths:\n\
                  \x20       retry up to 3 times, waiting 200ms:\n\
                  \x20           within 1.5s:\n\
                  \x20               path.len()\n\
                  \x20   give back sizes\n";
    let expanded = expand(source)?;
    for fragment in [
        ":: lgwks_bot :: script :: each",
        ":: lgwks_bot :: script :: retry",
        ":: lgwks_bot :: script :: within",
        "from_nanos (1500000000)",
        "from_nanos (200000000)",
        "\"each:path\"",
        "pub const ARCHITECTURE",
        "StepKind :: Each",
    ] {
        assert!(
            expanded.contains(fragment),
            "missing {fragment:?} in {expanded}"
        );
    }
    Ok(())
}

#[test]
fn step_labels_carry_structure_never_line_numbers() -> Result<(), String> {
    let near =
        expand("flow f(xs: Vec<u8>):\n    each x in xs, at most (width) at once:\n        x\n")?;
    let far = expand(
        "\n\n\n\nflow f(xs: Vec<u8>):\n    each x in xs, at most (width) at once:\n        x\n",
    )?;
    let label = |text: &str| {
        text.split("\"each:")
            .nth(1)
            .map(|rest| rest.split('"').next().map(str::to_owned))
    };
    assert_eq!(
        label(&near),
        label(&far),
        "moving a flow down the file must not change its keys"
    );
    Ok(())
}

#[test]
fn sibling_steps_of_one_kind_get_distinct_labels() -> Result<(), String> {
    let expanded = expand(
        "flow f():\n    retry up to 2 times:\n        1\n    retry up to 2 times:\n        2\n",
    )?;
    assert!(
        expanded.contains("\"retry\""),
        "the first sibling keeps the plain label"
    );
    assert!(
        expanded.contains("\"retry~2\""),
        "the second is numbered, so their keys differ"
    );
    Ok(())
}

/// An expansion carrying a `run` call is still Rust.
///
/// The re-parse is the whole assertion: `script!` emits an `async fn` and an
/// `ARCHITECTURE` const, so text that parses back is an expansion a compiler
/// can read. Splicing a call into the middle of a line is the one thing that
/// can break that, and it is invisible to an assertion that only looks for a
/// callee's name — a call whose tail arithmetic was off by one still
/// contained the callee.
#[test]
fn an_expansion_with_a_run_call_is_still_rust() -> Result<(), String> {
    // Every shape a `run` call reaches: the whole line, a `let` binding, a
    // binding inside a `for` body, and a `::` callee with tokens after the
    // call. Each is a different route through `rewrite`, so a tail that is
    // off by one in one of them shows here.
    let source = "flow f(log: &RefCell<Vec<u8>>, rows: Vec<u8>) -> u8:\n\
                  \x20   together:\n\
                  \x20       let joined = run branch(log, \"a\")\n\
                  \x20   let first = run branch(log, \"a\")\n\
                  \x20   for value in [1, 2]:\n\
                  \x20       let seen = run outer::inner(value)\n\
                  \x20   let scaled = run outer::fetch(rows).pow(2)\n\
                  \x20   give back first + seen + scaled\n";
    let expanded = expand(source)?;
    assert!(
        expanded.contains("branch"),
        "the callee is in the expansion: {expanded}"
    );
    assert!(
        expanded.contains("outer :: inner"),
        "a `::` path is in the expansion: {expanded}"
    );
    // The tokens after a call on the same line belong to the line, not to
    // the call: a callee that consumed one token too many takes the rest of
    // the expression with it, and the expansion is still valid Rust
    // afterwards — which is why the re-parse alone does not catch it.
    assert!(
        expanded.contains("pow"),
        "the tokens after a `run` call survive: {expanded}"
    );
    // A `let` inside `together:` binds the joined result, so the pattern the
    // split produced is the name alone and never the name with its `=`.
    assert!(
        expanded.contains("let (joined ,) ="),
        "a `let` binding binds its name: {expanded}"
    );
    TokenStream::from_str(&expanded)
        .map(|_| ())
        .map_err(|error| format!("the expansion is not Rust ({error}): {expanded}"))
}

/// The unknown-block refusal lists the block words as the macro always has:
/// moving the parser into `lgwks_ast` (#383) rendered the list from the
/// lexicon, and the rendering must still read `if`/`else` as one choice and
/// show `let` in the form that opens a block.
#[test]
fn an_unknown_block_lists_every_block_word_in_one_sentence() -> Result<(), String> {
    let refusal = match expand("flow f():\n    whenever x:\n        1\n") {
        Ok(expanded) => return Err(format!("`whenever:` is no block: {expanded}")),
        Err(refusal) => refusal,
    };
    assert_eq!(
        refusal,
        "unknown block; a line ending in `:` is one of `each`, `within`, `retry`, \
         `together`, `step`, `for`, `if`/`else`, or `let x = <block>:`",
    );
    Ok(())
}
