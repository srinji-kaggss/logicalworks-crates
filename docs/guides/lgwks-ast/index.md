# lgwks_ast consumer guide

One AST type across several languages, with grammar selection by cargo feature
and bounded traversal. Current version: `1.0.0`, MSRV Rust 1.98.0, edition 2024.

```sh
cargo add lgwks_ast
```

Adding this crate does not ask you to change anything else in your manifest. It
stands alone: it does not depend on `lgwks_std`, `lgwks_bot`, or `lgwks_deps`,
and adding it to a project that already uses tokio and serde_json leaves those
alone. The dependency policy in
[`../../AGENTS.md`](../../AGENTS.md) governs contributions to this repository,
not your application.

## Grammar selection

One cargo feature per grammar, forwarding to `ast-grep-language`. The default
build enables seven:

```toml
[dependencies]
lgwks_ast = "1"    # rust, python, typescript, javascript, go, java, swift
```

Add the rest by name, or take everything:

```toml
[dependencies]
lgwks_ast = { version = "1", features = ["lang-c", "lang-cpp", "lang-scala", "lang-kotlin", "lang-tsx"] }
```

```toml
[dependencies]
lgwks_ast = { version = "1", features = ["full"] }
```

`full` enables all 28 grammars. A language whose feature is off does not exist in
`Language::ALL` and is never returned by `Language::of_path`, so a consumer never
compiles a grammar it cannot select. That also means an unselected language comes
back as `None` from a path lookup rather than as a language that then fails to
parse.

| Default on | Also available as its own feature |
|---|---|
| `lang-rust`, `lang-python`, `lang-typescript`, `lang-javascript`, `lang-go`, `lang-java`, `lang-swift` | `lang-tsx`, `lang-c`, `lang-cpp`, `lang-csharp`, `lang-scala`, `lang-kotlin`, `lang-bash`, `lang-css`, `lang-dart`, `lang-elixir`, `lang-haskell`, `lang-hcl`, `lang-html`, `lang-json`, `lang-lua`, `lang-md`, `lang-nix`, `lang-php`, `lang-ruby`, `lang-solidity`, `lang-yaml` |

### One rename to know about

`0.2.0` renamed `Language::C` to `Language::CLang`. The workspace lint contract
forbids single-character identifiers, and that lint is `forbid` rather than
`deny`, so it cannot be lowered from source by an `#[allow]` on the variant. The
variant itself had to change. `Language::name()` still reports `"c"`, which is
the stable identity findings match on, and the `lang-c` feature is unchanged. Only
Rust code that names the variant is affected.

## Identify, parse, inspect

The test `the_guide_walkthrough_refuses_what_it_should` runs this walkthrough. Grammar
selection is by feature and the lookup follows it: `Language::of_path("src/lib.rs")` is
`Language::Rust` and its `name()` is `"rust"`, while a path whose grammar this build did not
select (`main.c` without `lang-c`) comes back `None`. `try_parse("fn f() { let x = 1; }",
language)` is a clean parse, and `inspect_ast(&parsed.root(), None)` reports no syntax
issues, more than five nodes and a depth of at least three. A recoverable tree is refused,
not reported as clean: `try_parse("fn f( {", language)` answers
`ParseError::InvalidSyntax` carrying at least one diagnostic, none truncated. The byte bound
is checked before the parser sees the source: one byte past `MAX_SOURCE_BYTES` answers
`ParseError::SourceTooLarge` naming both numbers. The unchecked escape hatch exists and is
named as one: `parse("fn f( {", language)` returns a tree for which `has_syntax_issues` is
true.

`detect` is `Language::of_path` under a second name: filename extensions select
a grammar but do not validate syntax. `detect` does not read a `#!` line;
`Language::of_shebang` does, for extensionless scripts. `.vue` or `.svelte`
selects the JavaScript grammar rather than a full-container grammar. Content sniffing is separate and opt-in, because it costs one full
parse per distinct candidate grammar:
`try_detect_content(source, &[Language::Rust, Language::Python])` names the small
candidate set you expect. Repeated candidates are parsed once. The
`ContentDetection` result is `NoMatch`, `Unique(language)`, or `Ambiguous`;
parser-unavailable and node-budget refusals return `Err` because they leave
detection incomplete.

## More

[Bounds and refusals](bounds-and-refusals.md) names the three constants and the four
refusals. [Custom languages and typed diagnostics](custom-and-diagnostics.md) covers a
caller-registered grammar and the shared derive.
