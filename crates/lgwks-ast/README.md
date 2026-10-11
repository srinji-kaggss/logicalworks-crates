# lgwks_ast

A multi-language AST front end for tools that read source code.

A code tool needs the same three things: identify a source file's language by
extension, by its `#!` line, or by explicitly trial-parsing a small candidate
set, select that language's tree-sitter grammar, and walk the resulting syntax
tree safely. This crate provides those paths under explicit bounds. It does not
parse full Vue/Svelte containers; those extensions select the JavaScript
grammar as a filename heuristic.

## Usage

```rust
use lgwks_ast::{Language, inspect_ast, try_parse};

fn main() -> Result<(), lgwks_ast::ParseError> {

    // Identify by extension — the cheap, always-on path.
    let language = Language::of_path("src/lib.rs");
    assert_eq!(language, Some(Language::Rust), "`.rs` resolves without parsing");

    // Content sniffing is opt-in: one full parse per distinct candidate, and a
    // result of NoMatch, Unique(language) or Ambiguous.
    // let detected = try_detect_content(source, &[Language::Rust, Language::Python])?;

    // Checked parse: oversized bytes, recovery nodes, and over-budget trees are
    // refused before any consumer sees the tree.
    let parsed = try_parse("fn f() {}", Language::Rust)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(metrics.nodes > 1, "a `fn` item is more than one node");
    Ok(())
}
```

That block is a doctest. `try_parse` returns `Result<Parsed, ParseError>`, so
the `?` is required. Consumers never depend on `ast-grep` directly: its grammar
types are re-exported here as `Parsed`, `AstNode`, and friends.

## Typed diagnostics

`ParseError` is built with this crate's diagnostic derive, and an analyser
derives its own diagnostics from the same stack:

```rust
extern crate lgwks_ast as thiserror;

#[derive(thiserror::Error, Debug)]
enum Diagnostic {
    #[error("parse refused: {0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

`#[derive(Error)]` expands to absolute `::thiserror::__private<N>::…` paths
resolved in the consuming crate, so the `extern crate … as thiserror;` line is
what makes the expansion resolve; the crate root re-exports the module it
needs.

## The `script!` language

`features = ["script"]` adds `lgwks_ast::script::parse`, the one reader of the
`script!` orchestration language: tokens (a macro's input, or
`TokenStream::from_str` over a file) in, a typed `Script` out, or a `Refusal`
located at its token that names what to write instead. `lgwks_macros` compiles
scripts through it, so a tool that maps, checks or dry-compiles a script gets
exactly the tree and the refusals `cargo build` does. The feature selects no
grammar; it builds `proc-macro2` through the `lgwks_deps` storefront and
nothing else.

`script::read_source` finds every `script!` in one Rust file the way the
compiler hands them to the macro (by token, at any nesting, under any path
prefix; never inside a string or comment) and reads each through `parse`.

## The `lgwks-ast` binary

`features = ["tool"]` adds `Script::to_json`, in the shape the runtime's
`Architecture::to_json` writes, and the `lgwks-ast` binary:

```text
lgwks-ast scan         [--json] [PATH...]   # the zero-gate source detectors, one verdict
lgwks-ast script map   [--json] [PATH...]   # every script's map, as its ARCHITECTURE renders
lgwks-ast script check [--json] [PATH...]   # only refusals, at cargo build's line:column
```

A directory is walked for `.rs` files (skipping `target`, `vendor`,
`node_modules`, `third_party`, `.venv`, `.git`, `.jj`); no `PATH` means `.`.
Exit 0 when nothing is found or refused, 2 when something is, or when a file is
not Rust or a path cannot be read: every file is read before the verdict, so an
unreadable one is named and refuses the run rather than being skipped.

`scan` (the `scan` feature, which `tool` enables) runs the detectors the
repository's `scan` lane holds at zero findings: a silenced error, an error
returned unlogged, a lint allowance, an overlong `?` chain, and a doc comment
that only restates its item's name. Text mode prints
`path:line: [rule] snippet`; `--json` prints `files`, `findings` and
`unreadable`. It replaced `lgwks-deps scan` (#408), so the detectors read this
crate's bounded grammar rather than a second Rust parser. The tool lives here
rather than in `lgwks-deps` because `script` reaches `proc-macro2` through
`lgwks_deps`, and `lgwks_deps` depending back on this crate is a cycle Cargo
refuses.

## Grammars, custom languages and bounds

The default build enables seven grammars (Rust, Python, TypeScript, JavaScript,
Go, Java, Swift); every other grammar is its own `lang-*` feature, and `full`
enables all 28. The full table, the derive-only build, custom languages and
the `0.2.0` rename are in
[`docs/guides/lgwks-ast/grammars.md`](https://github.com/srinji-kaggss/logicalworks-crates/blob/main/docs/guides/lgwks-ast/grammars.md);
every size, node and depth bound is in
[`docs/guides/lgwks-ast/bounds.md`](https://github.com/srinji-kaggss/logicalworks-crates/blob/main/docs/guides/lgwks-ast/bounds.md).

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated. `lgwks_ast` depends on
no other crate from this repository; the
[repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
lists the others and indexes the design documents.
