# lgwks_ast: the `script!` reader and custom languages

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
`features = ["tool"]` adds `Script::to_json`, in the shape the runtime's
`Architecture::to_json` writes, and the `lgwks-ast` binary:

```text
lgwks-ast script map   [--json] [PATH...]   # every script's map, as its ARCHITECTURE renders
lgwks-ast script check [--json] [PATH...]   # only refusals, at cargo build's line:column
```

A directory is walked for `.rs` files (skipping `target`, `vendor`,
`node_modules`, `third_party`, `.venv`, `.git`, `.jj`); no `PATH` means `.`.
Exit 0 when nothing is refused, 2 when a script is refused, a file is not Rust
tokens or a path cannot be read. The repository's `script-check` gate lane runs
`check` over the whole tree. The tool lives here rather than in `lgwks-deps`
because `script` reaches `proc-macro2` through `lgwks_deps`, and `lgwks_deps`
depending back on this crate is a cycle Cargo refuses.

## Custom languages

ast-grep ships a fixed built-in set; its documented extension point for
anything else is a caller-registered parser. Register one with `CustomLang` and
parse it through `try_parse_with`, under the same byte bound, node bound, and
recovery refusal as a built-in.

Name the language, hand `CustomLang::new` a `tree-sitter` grammar, give it its
file extensions with `with_extensions`, and parse with
`try_parse_with(source, &custom, custom.name())`. The grammar comes from a crate
**the caller** depends on, which this crate deliberately does not, so no test here
can name one. What is exercised is the same calls on an in-crate grammar: the unit
tests `a_registered_grammar_parses_through_the_bounded_api`,
`a_registered_grammar_keeps_the_recovery_refusal`,
`a_registered_grammar_refuses_oversized_source` and
`registered_grammars_resolve_paths_by_extensions` show `try_parse_with` accepting a
`CustomLang` under the same byte, node and recovery bounds as a built-in grammar.

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
