# lgwks_ast bounds and refusals

Three constants, in different units, bounding different work.

| Constant | Value | Bounds |
|---|---|---|
| `MAX_SOURCE_BYTES` | 2 MiB | the bytes handed to tree-sitter |
| `MAX_AST_NODES` | 2,000,000 | the validation walk, and every downstream walk |
| `MAX_DETECT_BYTES` | 64 KiB | one content-detection probe |

The two parse bounds are not interchangeable, and the crate says so plainly
(`crates/lgwks-ast/src/parser.md:77`):

> [`MAX_SOURCE_BYTES`] bounds the bytes handed to the parser and is what keeps
> parse work linear in input; [`MAX_AST_NODES`] is measured on the tree *after*
> tree-sitter has built it, so it bounds the validation walk and every downstream
> walk, not the parser's own allocation. Neither is a hard memory ceiling.

Read that first sentence twice if you were planning to rely on `MAX_AST_NODES` as
a parser memory guard. It is a *post-parse* bound. A 2 MiB source that expands
into a huge tree is parsed before the node count is checked; what the constant
stops is an unbounded walk over the result.

`MAX_DETECT_BYTES` is small on purpose, because detection trial-parses each
distinct candidate in full.

## Refusals

`try_parse` returns `Result<Parsed, ParseError>`, and `ParseError` is
`#[non_exhaustive]` with four variants:

| Variant | Trigger |
|---|---|
| `SourceTooLarge { actual, limit }` | input past `MAX_SOURCE_BYTES` |
| `ParserUnavailable { language, detail }` | the grammar produced no tree |
| `InvalidSyntax { language, diagnostics, diagnostics_truncated }` | the tree carries bounded `ERROR`/`MISSING` diagnostics with half-open source-byte ranges |
| `AstTooLarge { language, observed, limit }` | the tree exceeded the node bound |

None of them may be reported as clean. That is the point of the type: a
tree-sitter tree with recovery nodes is not proof of valid syntax, so `try_parse`
refuses it rather than handing you a tree that mostly parsed.

`inspect_ast` reports `complete`, the applied `node_limit`, and the
`stop_reason`. A partial walk's `has_syntax_issues == false` does not certify a
clean tree. Checked syntax diagnostics retain at most
`MAX_SYNTAX_DIAGNOSTICS` recovery nodes and do not copy source text.

`parse` is the unchecked counterpart, for diagnostics and tests that inspect
malformed trees on purpose. Use it when a partial tree is what you want, and not
as a faster `try_parse`.
