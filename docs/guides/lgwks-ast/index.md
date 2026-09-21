# lgwks_ast consumer guide

One AST type across several languages, with grammar selection by cargo feature
and bounded traversal. Current version: `0.2.2`, MSRV Rust 1.98.0, edition 2024.

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
lgwks_ast = "0.2.2"    # rust, python, typescript, javascript, go, java, swift
```

Add the rest by name, or take everything:

```toml
[dependencies]
lgwks_ast = { version = "0.2.2", features = ["lang-c", "lang-cpp", "lang-scala", "lang-kotlin", "lang-tsx"] }
```

```toml
[dependencies]
lgwks_ast = { version = "0.2.2", features = ["full"] }
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

```rust
use lgwks_ast::{Language, ParseError, inspect_ast, try_parse};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Grammar selection is by feature, and the lookup follows it.
    let language = Language::of_path("src/lib.rs").expect("the rust feature is on");
    assert_eq!(language, Language::Rust);
    assert_eq!(language.name(), "rust");

    // A grammar this build did not select is not recognized at all.
    assert_eq!(Language::of_path("main.c"), None);

    // A clean parse, and a real metric.
    let parsed = try_parse("fn f() { let x = 1; }", language)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert_eq!(metrics.has_syntax_issues, false);
    assert!(metrics.nodes > 5, "got {} nodes", metrics.nodes);
    assert!(metrics.max_depth >= 3, "got depth {}", metrics.max_depth);

    // A recoverable tree is refused, not reported as clean.
    match try_parse("fn f( {", language) {
        Err(ParseError::InvalidSyntax { language }) => assert_eq!(language, "rust"),
        other => return Err(format!("expected InvalidSyntax, got {:?}", other.err()).into()),
    }

    // The byte bound is checked before the parser sees the source.
    let oversized = "a".repeat(lgwks_ast::MAX_SOURCE_BYTES + 1);
    match try_parse(&oversized, language) {
        Err(ParseError::SourceTooLarge { actual, limit }) => {
            assert_eq!(limit, lgwks_ast::MAX_SOURCE_BYTES);
            assert_eq!(actual, lgwks_ast::MAX_SOURCE_BYTES + 1);
        }
        other => return Err(format!("expected SourceTooLarge, got {:?}", other.err()).into()),
    }

    // The unchecked escape hatch exists, and is named as one.
    let recovered = lgwks_ast::parse("fn f( {", language);
    assert!(lgwks_ast::has_syntax_issues(&recovered.root()));

    Ok(())
}
```

`detect` is `Language::of_path` under a second name. Content sniffing is
separate and opt-in, because it costs one full parse per candidate grammar:
`try_detect_content(source, &[Language::Rust, Language::Python])` names the small
candidate set you expect.

## The bounds

Three constants, in different units, bounding different work.

| Constant | Value | Bounds |
|---|---|---|
| `MAX_SOURCE_BYTES` | 2 MiB | the bytes handed to tree-sitter |
| `MAX_AST_NODES` | 2,000,000 | the validation walk, and every downstream walk |
| `MAX_DETECT_BYTES` | 64 KiB | one content-detection probe |

The two parse bounds are not interchangeable, and the crate says so plainly
(`crates/lgwks-ast/src/lib.rs:54`):

> [`MAX_SOURCE_BYTES`] bounds the bytes handed to the parser and is what keeps
> parse work linear in input; [`MAX_AST_NODES`] is measured on the tree *after*
> tree-sitter has built it, so it bounds the validation walk and every downstream
> walk, not the parser's own allocation. Neither is a hard memory ceiling.

Read that first sentence twice if you were planning to rely on `MAX_AST_NODES` as
a parser memory guard. It is a *post-parse* bound. A 2 MiB source that expands
into a huge tree is parsed before the node count is checked; what the constant
stops is an unbounded walk over the result.

`MAX_DETECT_BYTES` is small on purpose, because detection trial-parses each
candidate in full.

## Refusals

`try_parse` returns `Result<Parsed, ParseError>`, and `ParseError` is
`#[non_exhaustive]` with four variants:

| Variant | Trigger |
|---|---|
| `SourceTooLarge { actual, limit }` | input past `MAX_SOURCE_BYTES` |
| `ParserUnavailable { language, detail }` | the grammar produced no tree |
| `InvalidSyntax { language }` | the tree carries an `ERROR` or `MISSING` node |
| `AstTooLarge { language, observed, limit }` | the tree passed the node bound |

None of them may be reported as clean. That is the point of the type: a
tree-sitter tree with recovery nodes is not proof of valid syntax, so `try_parse`
refuses it rather than handing you a tree that mostly parsed.

`parse` is the unchecked counterpart, for diagnostics and tests that inspect
malformed trees on purpose. Use it when a partial tree is what you want, and not
as a faster `try_parse`.

## Custom languages

`ast-grep` ships a fixed built-in set. Its documented extension point for
anything else is a caller-registered parser:

```rust,ignore
use lgwks_ast::{CustomLang, try_parse_with};

let sql = CustomLang::new("sql", tree_sitter_sequel::LANGUAGE.into())
    .with_extensions(&["sql"]);
let parsed = try_parse_with("SELECT 1", &sql, sql.name()).expect("valid SQL");
```

That block is not compiled here. The grammar crate it names is not a dependency
of this repository, and the `TSLanguage` value has to come from whichever grammar
crate you registered.

`TSLanguage` is re-exported, so you add only the grammar crate and never
`ast-grep-core` directly. A custom grammar goes through the same byte bound, node
bound, and recovery refusal as a built-in.

For this repository, a grammar crate is a third-party edge like any other and
must be registered in `contract/APPROVED.toml` before `lgwks-deps check` will
pass. That requirement is this repo's, not yours.

## Typed diagnostics

`lgwks_ast` owns the shared diagnostic derive so analysers report failures through
one `Display`/`source` implementation rather than each declaring `thiserror`. The
derive expands to absolute `::thiserror::__private<N>::...` paths resolved in the
consuming crate, so name this crate as `thiserror` to make the expansion resolve:

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

## Where to go next

- `crates/lgwks-ast/README.md` for the full language-and-extension table and the
  custom-language guidance.
- [lgwks_deps](../lgwks-deps/index.md) if you want the admission gate on your own
  workspace.
