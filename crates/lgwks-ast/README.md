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

```rust
use lgwks_ast::{Language, inspect_ast, try_parse};

fn main() -> Result<(), lgwks_ast::ParseError> {

    // Identify by extension — the cheap, always-on path.
    let language = Language::of_path("src/lib.rs");
    assert_eq!(language, Some(Language::Rust), "`.rs` resolves without parsing");

    // Content sniffing is opt-in, because it costs one full parse per distinct
    // candidate. Repeated candidates do not repeat parsing, and an unavailable or
    // over-budget candidate returns an error instead of being treated as a failed
    // syntax match. Its result distinguishes NoMatch, Unique(language), and
    // Ambiguous:
    // let detected = try_detect_content(source, &[Language::Rust, Language::Python])?;

    // Checked parse: oversized bytes, recovery nodes, and over-budget trees are
    // refused before any consumer sees the tree.
    let parsed = try_parse("fn f() {}", Language::Rust)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(metrics.nodes > 1, "a `fn` item is more than one node");
    Ok(())
}
```

Every line of that block is compiled and run as a doctest, against this crate's
own feature set. `try_parse` returns `Result<Parsed, ParseError>` — the `?` is
required, not decorative; binding it directly and calling `.root()` does not
compile.

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

## Grammar selection

One cargo feature per grammar forwards to `ast-grep-language`. The default
enables seven languages; every other grammar ast-grep-language ships is its own
opt-in feature, and `full` enables all 28:

```toml
[dependencies]
lgwks_ast = { version = "1", features = ["lang-c", "lang-cpp", "lang-scala", "lang-kotlin", "lang-tsx"] }
# or everything: features = ["full"]
```

**0.2.0 renames one variant.** `Language::C` is now `Language::CLang`. The
workspace lint contract forbids single-character identifiers, and that lint is
`forbid` rather than `deny`, so it cannot be lowered from source by an
`#[allow]` on the variant. The variant itself had to change. `Language::name()`
still reports `"c"`, which is the stable identity findings match on, and the
`lang-c` *feature* is unchanged, so only Rust code that names the variant is
affected.

| Feature | Language | Extensions | Default |
|---------|----------|------------|---------|
| `lang-rust` | Rust | `rs` | yes |
| `lang-python` | Python | `py`, `pyi` | yes |
| `lang-typescript` | TypeScript | `ts`, `mts`, `cts` | yes |
| `lang-javascript` | JavaScript | `js`, `jsx`, `mjs`, `cjs`, `vue`, `svelte` | yes |
| `lang-go` | Go | `go` | yes |
| `lang-java` | Java | `java` | yes |
| `lang-swift` | Swift | `swift` | yes |
| `lang-tsx` | TSX (parsed by the TypeScript grammar; shares it with `lang-typescript`) | `tsx` | no |
| `lang-c` | C | `c`, `h` | no |
| `lang-cpp` | C++ | `cpp`, `hpp`, `cc`, `cxx`, `hh`, `hxx` | no |
| `lang-csharp` | C# | `cs` | no |
| `lang-scala` | Scala | `scala`, `sc` | no |
| `lang-kotlin` | Kotlin | `kt`, `kts` | no |
| `lang-bash` | Bash / POSIX shell | `sh`, `bash` | no |
| `lang-css` | CSS | `css` | no |
| `lang-dart` | Dart | `dart` | no |
| `lang-elixir` | Elixir | `ex`, `exs` | no |
| `lang-haskell` | Haskell | `hs` | no |
| `lang-hcl` | HCL / Terraform | `hcl`, `tf` | no |
| `lang-html` | HTML | `html`, `htm` | no |
| `lang-json` | JSON | `json` | no |
| `lang-lua` | Lua | `lua` | no |
| `lang-md` | Markdown | `md`, `markdown` | no |
| `lang-nix` | Nix | `nix` | no |
| `lang-php` | PHP | `php` | no |
| `lang-ruby` | Ruby | `rb` | no |
| `lang-solidity` | Solidity | `sol` | no |
| `lang-yaml` | YAML | `yaml`, `yml` | no |
| `full` | all of the above | — | no |

A language whose feature is off does not exist in `Language::ALL` and is never
returned by `Language::of_path`, so a consumer never compiles a grammar it
cannot select.

Extension lookup is a filename heuristic, not a syntax verdict. In particular,
`.vue` and `.svelte` select the JavaScript grammar; they do not enable dedicated
container grammars. An extensionless script is identified by its `#!` line
through `Language::of_shebang`, which `detect` does not consult.

`try_detect_content` returns `ContentDetection::NoMatch`, `Unique(language)`, or
`Ambiguous`; non-syntax parser failures return `Err` because they leave a
required candidate uninspected.

## The `script!` language

`features = ["script"]` adds `lgwks_ast::script::parse`, the one reader of the
`script!` orchestration language: tokens (a macro's input, or
`TokenStream::from_str` over a file) in, a typed `Script` out, or a `Refusal`
located at its token that names what to write instead. `lgwks_macros` compiles
scripts through it, so a tool that maps, checks or dry-compiles a script gets
exactly the tree and the refusals `cargo build` does. The feature selects no
grammar; it builds `proc-macro2` through the `lgwks_deps` storefront and
nothing else.

## Custom languages

ast-grep ships a fixed built-in set; its documented extension point for
anything else is a caller-registered parser. Register one with `CustomLang` and
parse it through `try_parse_with`, under the same byte bound, node bound, and
recovery refusal as a built-in:

```rust ignore
use lgwks_ast::{CustomLang, try_parse_with};
use some_sql_grammar::LANGUAGE; // the caller's own grammar crate

let sql = CustomLang::new("sql", LANGUAGE.into()).with_extensions(&["sql"]);
let parsed = try_parse_with("SELECT 1", &sql, sql.name())?;
```

This block is `ignore`d, and that is the honest encoding of what it shows:
`LANGUAGE` comes from a grammar crate **the caller** depends on, which this
crate deliberately does not, so no doctest here could compile it. What the
block does prove is compiled — [`try_parse_with`] accepts a `CustomLang` under
the same byte, node and recovery bounds as a built-in grammar, and the
surrounding `#[test]`s exercise it with an in-crate grammar.

`TSLanguage` is re-exported, so a consumer adds only the grammar crate, never
`ast-grep-core` directly. A grammar crate is a third-party edge like any
other: register it in `contract/APPROVED.toml` (owner, capability, reason)
before depending on it; `lgwks-deps check` refuses it otherwise.

A grammar that belongs upstream should be contributed to `ast-grep-language`
itself, following its
[add-a-language guide](https://ast-grep.github.io/contributing/add-lang.html):
it must be popular (TIOBE / GitHub Octoverse), use a maintained grammar
published on crates.io, and stay inside the budget that keeps ast-grep's zipped
binary under 10 MB. Until it ships there, register it here with `CustomLang`;
when it does, delete the registration and select the built-in. This crate does
not fork upstream's `SupportLang` tables, so every addition remains submittable
upstream as a pull request.

## Bounds

- `MAX_SOURCE_BYTES`: 2 MiB per checked parse. This bounds the bytes handed
  to tree-sitter. It is a byte bound, not a complexity guarantee: see the note
  on walk cost below.
- `MAX_AST_NODES`: 2,000,000 nodes; the boundary walk is capped and stops
  within one node of the limit, so refusal does not itself walk an unbounded
  tree. It is measured on the tree *after* tree-sitter builds it, so it bounds
  the validation walk, not the parser's own allocation. On dense sources this
  ceiling is the one that binds first — a 2 MiB file of `x;\n` reaches it at
  roughly 2.2 bytes per node — so the node limit, not the byte limit, is what
  refuses that input.
- `MAX_DETECT_BYTES`: 64 KiB per content-detection probe. `try_detect_content`
  tries each distinct caller-named candidate in full, so the probe is bounded
  well below `MAX_SOURCE_BYTES`; `detect` never parses at all.
- `MAX_SHEBANG_BYTES`: 256 bytes — the `#!` line is the only part of a file
  `of_shebang` reads, and a script whose first line is longer than that is
  treated as unidentified rather than scanned further.
- `try_parse` refuses an `ERROR`/`MISSING` recovery node: a recoverable tree
  is not proof of valid syntax. `parse` is the unchecked escape hatch for
  diagnostics and tests that inspect malformed trees on purpose.
- Checked syntax refusals carry at most `MAX_SYNTAX_DIAGNOSTICS` recovery-node
  diagnostics. Each identifies `ERROR` versus `MISSING` and a half-open byte
  range into the original UTF-8 source; no source text is copied into the
  diagnostic.
- A tree is not only accepted or refused. `diagnostics` and `to_diagnostic`
  turn a refused or suspect parse into a `Diagnostic` carrying a byte offset, a
  1-based line and column, and a byte span into the original source, so a caller
  reports *where* the source stopped making sense rather than only that it did.
  A refused parse points at its earliest recovery node; only whole-file
  refusals (size, parser, node budget) sit at the end of the file.
  Two limits of that span are worth stating, because they are what the span is:
  a `MISSING` node is zero-width by construction — it marks an insertion point,
  not text — so its span is empty and underlines nothing; and an `ERROR` node
  spans the whole region the grammar could not parse, which may include
  perfectly valid lines around the real fault. Use the line and column to locate
  the fault, and treat the span as a region rather than as the offending text.
  This crate does not render a caret underline: `Diagnostic::render` emits
  `path:line:column: severity: message`, so a caller that wants an underline
  draws one from the span.
- `inspect_ast` reports whether traversal completed, the applied node limit,
  and its stop reason. A partial walk's `has_syntax_issues == false` is not a
  clean-syntax result.
- The walk's cost is proportional to the nodes it visits *times* the fan-out it
  walks them at, not to input length. The boundary walk indexes children by
  position and `Node::child(nth)` is an index into the child list, so a source
  that is one wide root costs more per node than the same node count spread
  deep and narrow. The two ceilings above bound the work; they do not make it
  linear in input, and no benchmark in this repository establishes a complexity
  claim for either the parser or the walk.

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

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
