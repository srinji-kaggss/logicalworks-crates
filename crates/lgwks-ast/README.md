# lgwks_ast — the estate's one multi-language AST parser

Code tools across the estate need the same three things: decide a source
file's language, select that language's tree-sitter grammar, and walk the
resulting syntax tree safely. `keel-core`'s safety detectors and
`code-world-model`'s graph extractor each built their own language enum,
extension table, grammar mapping, and parse loop. That is one concept
implemented twice; this crate owns it once.

## Usage

```rust
use lgwks_ast::{Language, try_parse, inspect_ast};

// Identify by extension, or by trial parse when there is none.
let language = Language::of_path("src/lib.rs").expect("rust grammar is compiled in");

// Checked parse: oversized bytes, recovery nodes, and over-budget trees are
// refused before any consumer sees the tree.
let parsed = try_parse("fn f() {}", language).expect("valid rust parses");
let metrics = inspect_ast(&parsed.root(), None);
assert!(metrics.nodes > 1);
```

Consumers do not depend on `ast-grep` directly; the grammar types
(`AstGrep`, `Node`, `StrDoc`, `SupportLang`) are re-exported here as
`Parsed`, `AstNode`, and friends.

## Grammar selection

One cargo feature per grammar forwards to `ast-grep-language`. The default
enables the seven languages the safety detectors parse; every other grammar
ast-grep-language ships is its own opt-in feature, and `full` enables all 28:

```toml
[dependencies]
lgwks_ast = { version = "0.1", features = ["lang-c", "lang-cpp", "lang-scala", "lang-kotlin", "lang-tsx"] }
# or everything: features = ["full"]
```

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
`ast-grep-core` directly.

A grammar the estate needs belongs in `ast-grep-language` upstream, following
its [add-a-language guide](https://ast-grep.github.io/contributing/add-lang.html):
it must be popular (TIOBE / GitHub Octoverse), use a maintained grammar
published on crates.io, and stay inside the budget that keeps ast-grep's zipped
binary under 10 MB. Until it ships there, register it here with `CustomLang`;
when it does, delete the registration and select the built-in. This crate never
forks upstream's `SupportLang` tables — that is what keeps every addition
liftable into a pull request.

## Bounds

- `MAX_SOURCE_BYTES` — 2 MiB per checked parse.
- `MAX_AST_NODES` — 2,000,000 nodes; the boundary walk is capped and stops
  within one node of the limit, so refusal does not itself walk an unbounded
  tree.
- `try_parse` refuses an `ERROR`/`MISSING` recovery node: a recoverable tree
  is not proof of valid syntax. `parse` is the unchecked escape hatch for
  diagnostics and tests that inspect malformed trees on purpose.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
