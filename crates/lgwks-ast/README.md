# lgwks_ast

A multi-language AST front end for tools that read source code.

A code tool needs the same three things: identify a source file's language by
extension, by its `#!` line, or by explicitly trial-parsing a small candidate
set, select that language's tree-sitter grammar, and walk the resulting syntax
tree safely. This crate provides those paths under explicit bounds. It does not
parse full Vue/Svelte containers; those extensions select the JavaScript
grammar as a filename heuristic.

It also provides the shared typed-diagnostic derive, so a parser and its
consumers report failures through one `Display`/`source` implementation rather
than each declaring `thiserror`.

## Usage

`Language::of_path("src/lib.rs")` resolves `.rs` to `Language::Rust` without parsing:
the cheap, always-on path. `try_parse(source, language)` is the checked parse: oversized
bytes, recovery nodes and over-budget trees are refused before any consumer sees the tree.
It returns `Result<Parsed, ParseError>`, so the `?` is required, not decorative, and
`inspect_ast(&parsed.root(), None)` reports the node count, the depth and whether the walk
completed.

Content sniffing is opt-in, because it costs one full parse per distinct candidate:
`try_detect_content(source, &[Language::Rust, Language::Python])?` distinguishes `NoMatch`,
`Unique(language)` and `Ambiguous`. Repeated candidates do not repeat parsing, and an
unavailable or over-budget candidate returns an error instead of being treated as a failed
syntax match.

The test `the_readme_usage_flow_identifies_parses_and_inspects` runs the identify, parse and
inspect flow against this crate's own feature set.

Consumers do not depend on `ast-grep` directly; the grammar types
(`AstGrep`, `Node`, `StrDoc`, `SupportLang`) are re-exported here as
`Parsed`, `AstNode`, and friends.

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

A crate that wants only the derive turns the parser off, and compiles no
grammar and no tree-sitter at all:

```toml
lgwks_ast = { version = "1", default-features = false }
```

Without default features the crate is the derive and the `diagnostic` types
(`Diagnostic`, `Span`, `Pos`, `Severity`). The parser is the `parser` feature,
which every `lang-*` feature enables; `default-features = false, features =
["parser"]` is the parser with no grammar compiled, for a caller that
registers its own through `CustomLang`.

## More

The grammar features and the extension table are in
[grammars](../../docs/guides/lgwks-ast/grammars.md); the `script!` reader and custom
languages are in [script and custom languages](../../docs/guides/lgwks-ast/script-and-custom.md);
every bound the checked parse applies is in [bounds](../../docs/guides/lgwks-ast/bounds.md).

## The other crates

Five crates ship from this repository — the four in the table below plus
`lgwks_macros`, a proc-macro crate carrying the syntax of `lgwks_bot::script!`
which exists only because Rust requires a proc macro to live in its own crate.
It is not a surface of its own; use it through `lgwks_bot`. They share a
release process, not a dependency graph: `lgwks_bot` and `lgwks_deps` depend on
`lgwks_std` (unconditionally, plus an optional `lgwks_macros` behind `bot`'s
`script` feature), and `lgwks_ast` stands alone.

| Crate | What it gives you |
|---|---|
| [`lgwks_std`](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: codecs, a blocking HTTP client, retry, default structured debugging, time, hashing, ids |
| [`lgwks_bot`](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs, capability-gated authority, change-triggered execution, supervised background work |
| [`lgwks_deps`](https://docs.rs/lgwks_deps) | The audited storefront for third-party stacks, plus `lgwks-deps check` and `lgwks-deps debug` to prove dependency and debugger wiring |

Logical Works Incorporated publishes `lgwks_ast` as one of five independently versioned crates in [logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates#readme): `lgwks_std`, `lgwks_bot`, `lgwks_ast`, `lgwks_macros` and `lgwks_deps`.
[Should I use logicalworks-crates?](https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md) says when to reach for each one and when to skip it.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
