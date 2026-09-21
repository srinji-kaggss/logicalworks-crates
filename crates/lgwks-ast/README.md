# lgwks_ast

A multi-language AST front end for tools that read source code.

A code tool needs the same three things: identify a source file's language,
select that language's tree-sitter grammar, and walk the resulting syntax tree
safely. This crate provides all three, once, under explicit bounds.

It also provides the shared typed-diagnostic derive, so a parser and its
consumers report failures through one `Display`/`source` implementation rather
than each declaring `thiserror`.

## Usage

```rust
use lgwks_ast::{Language, try_parse, inspect_ast};

// Identify by extension — the cheap, always-on path.
let language = Language::of_path("src/lib.rs").expect("rust grammar is compiled in");

// Content sniffing is opt-in, because it costs one full parse per candidate:
// let language = try_detect_content(source, &[Language::Rust, Language::Python])?;

// Checked parse: oversized bytes, recovery nodes, and over-budget trees are
// refused before any consumer sees the tree.
let parsed = try_parse("fn f() {}", language).expect("valid rust parses");
let metrics = inspect_ast(&parsed.root(), None);
assert!(metrics.nodes > 1);
```

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

## Grammar selection

One cargo feature per grammar forwards to `ast-grep-language`. The default
enables seven languages; every other grammar ast-grep-language ships is its own
opt-in feature, and `full` enables all 28:

```toml
[dependencies]
lgwks_ast = { version = "0.2.2", features = ["lang-c", "lang-cpp", "lang-scala", "lang-kotlin", "lang-tsx"] }
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
| `lang-tsx` | TSX | `tsx` | no |
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

## Custom languages

ast-grep ships a fixed built-in set; its documented extension point for
anything else is a caller-registered parser. Register one with `CustomLang` and
parse it through `try_parse_with`, under the same byte bound, node bound, and
recovery refusal as a built-in:

```rust
use lgwks_ast::{CustomLang, try_parse_with};

let sql = CustomLang::new("sql", tree_sitter_sequel::LANGUAGE.into())
    .with_extensions(&["sql"]);
let parsed = try_parse_with("SELECT 1", &sql, sql.name()).expect("valid SQL");
```

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
  to tree-sitter and keeps parse work linear in input.
- `MAX_AST_NODES`: 2,000,000 nodes; the boundary walk is capped and stops
  within one node of the limit, so refusal does not itself walk an unbounded
  tree. It is measured on the tree *after* tree-sitter builds it, so it bounds
  the validation walk, not the parser's own allocation.
- `MAX_DETECT_BYTES`: 64 KiB per content-detection probe. `try_detect_content`
  tries each caller-named candidate in full, so the probe is bounded well below
  `MAX_SOURCE_BYTES`; `detect` never parses at all.
- `try_parse` refuses an `ERROR`/`MISSING` recovery node: a recoverable tree
  is not proof of valid syntax. `parse` is the unchecked escape hatch for
  diagnostics and tests that inspect malformed trees on purpose.

## The other crates

Four crates ship from this repository. They share a release process, not a
dependency graph: `lgwks_bot` and `lgwks_deps` depend on `lgwks_std`, and
`lgwks_ast` stands alone.

| Crate | What it gives you |
|---|---|
| [`lgwks_std`](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: codecs, a blocking HTTP client, retry, structured logging, time, hashing, ids |
| [`lgwks_bot`](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs, capability-gated authority, change-triggered execution, supervised background work |
| [`lgwks_deps`](https://docs.rs/lgwks_deps) | The audited storefront for third-party stacks, plus `lgwks-deps check` to prove no unreviewed dependency entered a build |

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
