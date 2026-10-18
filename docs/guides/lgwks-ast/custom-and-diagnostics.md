# lgwks_ast custom languages and typed diagnostics

## Custom languages

`ast-grep` ships a fixed built-in set. Its documented extension point for
anything else is a caller-registered parser.

Register a grammar with `CustomLang::new("sql", grammar)` and `with_extensions(&["sql"])`,
then parse with `try_parse_with(source, &sql, sql.name())`. The grammar crate is not a
dependency of this repository, and the `TSLanguage` value has to come from whichever grammar
crate you registered, so no example here compiles it; the unit tests
`a_registered_grammar_parses_through_the_bounded_api`,
`a_registered_grammar_keeps_the_recovery_refusal` and
`a_registered_grammar_refuses_oversized_source` run the same calls on an in-crate grammar.

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

- [Grammars](grammars.md) for the full language-and-extension table, and
  [script and custom languages](script-and-custom.md) for the custom-language guidance.
- [lgwks_deps](../lgwks-deps/index.md) if you want the admission gate on your own
  workspace.
