`lgwks_ast` owns the single multi-language AST parser.

Code tools that consume this crate, a safety linter and a graph extractor
among them, need the same three things: decide a source file's language,
select that language's tree-sitter grammar, and walk the resulting syntax
tree safely. Before this crate each rebuilt its own language enum,
extension table, grammar mapping, and parse loop; that is one concept
implemented twice, so it lives here instead.

Enforced invariant **INV-AST-ONE-PARSER**: a consumer identifies, selects,
and parses through this crate and does not depend on `ast-grep` directly.

## The README is compiled

This module documentation is generated from `README.md`, so the usage
example above is built and run as a doctest on every `cargo test`, so
`cargo test -p lgwks_ast --doc` fails if the example stops compiling or
stops asserting what the text above claims — as it did while `try_parse`
was documented as if it were infallible.

## Code observability

Code observability here rests on two pieces: structural parsing, and the
shared typed-diagnostic derive in [`error`]. [`ParseError`] is built with
it, and downstream analysers derive `Display`/`source`/`#[from]` from the
same stack instead of each declaring `thiserror`. The crate root re-exports
the derive because `#[derive(Error)]` expands to absolute
`::thiserror::__private<N>::…` paths resolved in the *consuming* crate: a
consumer names this crate `thiserror` (`extern crate lgwks_ast as thiserror;`)
and derives normally, with no `thiserror` edge of its own.

## Grammar selection

One cargo feature per grammar forwards to `ast-grep-language`. The default
enables seven widely-used languages — Rust, Python, TypeScript, JavaScript,
Go, Java and Swift — and every remaining grammar ast-grep-language ships
(C#, CSS, Dart, Elixir, Haskell, HCL, HTML, JSON, Lua, Markdown, Nix, PHP,
Ruby, Solidity, YAML, and the rest) is its own opt-in `lang-*` feature, and
`full` enables all 28. A language whose feature is off is not in
[`Language::ALL`] and is never returned by [`Language::of_path`], so no
consumer pays to compile a grammar it cannot select.

Two notes on reading that table. `lang-tsx` is a *selection*, not a separate
grammar: ast-grep parses `.tsx` with the TypeScript parser, so it forwards to
the same crate as `lang-typescript` and pulls nothing further. And a `#!`
line resolves a language where a filename cannot — see
[`Language::of_shebang`], which is how an extensionless `bin/deploy` is
identified at all.

## Custom languages

ast-grep keeps its built-in set small on purpose; its documented extension
point for anything else is a caller-registered parser. [`CustomLang`] is
that registration: name the language, hand it a `tree-sitter` grammar, and
parse it through [`try_parse_with`] under the same bounds and recovery
refusal as a built-in. A grammar a consumer here needs should be contributed
to `ast-grep-language` upstream and the local registration deleted once it
ships. This crate never forks upstream's language tables.

## Bounded parsing

A recoverable tree-sitter tree is not proof of valid syntax: recovery emits
`ERROR` and `MISSING` nodes. [`try_parse`] therefore refuses before any
detector sees the tree: oversized bytes, a parser that cannot produce a
tree, a parse still running at its deadline, a tree past [`MAX_AST_NODES`],
or one carrying recovery nodes. The unchecked [`parse`] exists for
diagnostics and tests that inspect malformed trees on purpose.

The byte ceiling bounds what the parser is handed, not how long it works:
tree-sitter's GLR parser is super-linear in nesting on some grammars, and a
quarter of the byte ceiling of nested braces held the Dart parser for 97.5 s.
The checked parse therefore runs under [`DEFAULT_PARSE_DEADLINE`] (or the
caller's own, through [`try_parse_within`]): the parser asks at every
hundredth operation whether to continue, stops when the deadline has passed,
frees its thread, and the call answers [`ParseError::TimedOut`].

The bounds are not interchangeable. [`MAX_SOURCE_BYTES`] bounds the
bytes handed to the parser and is what keeps parse work linear in input;
[`MAX_AST_NODES`] and [`MAX_AST_DEPTH`] are measured on the tree *after*
tree-sitter has built it, so they bound the validation walk and every
downstream walk, not the parser's own allocation. The parser's allocation
is bounded by input, not by a counter in this crate: the worst case at the
byte ceiling is **the measured peak RSS in
[`bench/README.md`](https://github.com/srinji-kaggss/logicalworks-crates/blob/main/bench/README.md),
per grammar**, produced by the `parse_budget` example. Read that number for
what one parse of a full [`MAX_SOURCE_BYTES`] source costs resident memory;
the two tree bounds are what make the *walk* bounded, not the parser.

The walk itself is linear in tree size and holds state proportional to
depth. That is a property of *how* it walks rather than of any ceiling: it
drives a tree-sitter cursor, so no node is reached by its index among its
siblings. Indexing is `O(index)` on a recovery-heavy tree, where the visible
children are not the structural ones, and a walk that did it was quadratic
in fan-out — 85 microseconds per node on a 16 KiB source of unbalanced
delimiters, and hours for one file at the byte ceiling. See the walk's own
documentation for the measurement and the regression test that holds it.

Content sniffing is opt-in for the same reason: [`try_detect_content`]
trial-parses each distinct candidate grammar in full, so the caller names
a small candidate set and the probe source is held to [`MAX_DETECT_BYTES`]. The
extension-only [`detect`] never parses.
