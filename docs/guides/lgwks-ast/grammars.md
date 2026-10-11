# lgwks_ast: grammars and custom languages

Which grammars a build compiles, how to compile none, and how to add one.

## A build with no grammar

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
block does prove is compiled — `try_parse_with` accepts a `CustomLang` under
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
