# lgwks_ast grammar selection

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
