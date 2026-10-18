# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (15/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 15 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_ast — the parse had no time bound, and the walk was quadratic (#277) (continued)

- **The validation walk is linear now, and was not.** `inspect_ast` and
  `diagnostic::diagnostics` reached each child through
  `ast_grep_core::Node::child(i)` by index, which is `O(i)` on a node whose
  visible children are not its structural children — exactly what a
  recovery-heavy parse produces. Measured on Rust source of unbalanced
  delimiters: 16 385 nodes wide and 2 deep spent **1.39 s in the walk against
  1.06 ms in the parser**, and doubling the width quadrupled the walk, inside
  `try_parse`. At the crate's 2 MiB ceiling that is hours of CPU for one file.
  Both walks now drive a tree-sitter cursor, whose only retained state is its
  own ancestor stack: **40 ns per node on the same source, at every width from
  2 KiB to 2 MiB**. No `tree-sitter` edge is authored and no `tree-sitter` type
  is named; the cursor is reached through the node the walk already holds.
- **`MAX_AST_DEPTH` (512), with `ParseError::AstTooDeep`.** A source of `(((…` is
  a few bytes per nesting level, so the byte and node ceilings admit a tree
  hundreds of thousands of levels deep. A checked parse now refuses that, and
  the public `inspect_ast` walk stays uncapped in depth on purpose: a caller
  inspecting a malformed tree on purpose learns how deep it is rather than
  reading back the ceiling.
- **A deadline that stops tree-sitter mid-parse: NOT DONE.** It needs
  `Parser::parse_with_options` and its progress callback, which
  `ast-grep-core` 0.45 does not expose (`parse_lang` builds the `Parser`
  internally). Naming `tree-sitter` directly is the only route, and #277
  reserves that for the Director's word. Nothing here fakes it with a thread
  that cannot be stopped.
- **Measured, per grammar, in `bench/README.md` and
  `bench/ast-budget.tsv`** (224 rows): throughput p50/p99 and peak RSS at the
  2 MiB ceiling on representative and adversarial input, the share of a checked
  parse spent in the validation walk, and p99 plus peak RSS for a bounded
  fan-out at 100, 1 000, 10 000 and 100 000 concurrent parses. Produced by
  `examples/parse_budget.rs` under `scripts/measure-ast-budget.sh`. Three
  findings from it:
  - **The documented memory ceiling is 723 MiB resident** for one parse of a
    2 MiB file with the Ruby grammar, against a 2.1 MiB process floor; the
    cheapest grammar at the same size is `solidity` at 100 MiB, so a caller
    cannot size a parser from the input alone.
  - **The parser, not the walk, is the unbounded work on hostile input.** The
    validation walk is 10–15 ms on the three worst adversarial rows and the parse
    is 25–97 seconds — 256 KiB of nested braces takes the Dart grammar
    **97.5 seconds**, and at the 2 MiB ceiling it did not finish in 120 s. This
    is precisely what the missing deadline would bound and cannot be bounded from
    inside this crate.
  - **Peak RSS is flat in the concurrency level** — 195 to 198 MiB for `rust`
    across 100 to 100 000 concurrent 64 KiB parses — because the per-parse bound
    is what makes a fleet of them bounded.
- **The markdown grammar no longer aborts the process.** It did:
  `tree-sitter-markdown` 0.5.3's external scanner serializes its open block
  containers into a fixed 1 024-byte buffer and *asserts* when they do not fit,
  and an assertion in a C parser is `abort()`, so `"- "` repeated 255 times
  (510 bytes) ended the process with `SIGABRT` rather than returning anything —
  reachable from a hostile PR that adds a nested list to a README. The grammar
  arrives compiled through `ast-grep-language`, so the crate cannot patch the
  scanner; it refuses the source before the scanner sees it.
  `MAX_MARKDOWN_CONTAINERS_PER_LINE` (64) and the new
  `ParseError::ContainerNestingTooDeep` apply
  `lgwks_ast::markdown_containers` on the markdown path only, in one `O(bytes)`
  pass with `O(1)` state. The bound is measured, not guessed: seventeen
  container shapes were bisected from a child process, and **every shape that
  aborts does so at 255 open containers** — 255 repetitions of `- `, 128 of
  `> - `, 85 of `>>> `, and the same 255 for indentation-nested lists, fenced
  and indented code inside quotes. Three shapes never abort, because markdown
  does not nest blockquotes, ordered lists or tab runs by indentation. The
  count over-estimates where indentation and markers both carry depth, so 64
  cannot be 255: a margin of about 4x, on the safe side. `tests/it/hostile.rs`
  proves it from child processes — every shape at bound-1, bound and bound+1,
  every shape at its own measured abort depth, and 1 000 seeded mixes, 1 065
  children, zero `SIGABRT`. An exhaustive sweep of all seventeen shapes through
  every depth from 1 to 512 (8 704 pairs) also exits cleanly.
  Residual, stated rather than hidden: the guard is on the **checked** parse.
  `parse` and `parse_with` return a `Parsed` rather than a `Result`, so a refusal
  has nowhere to go there, and their documentation now says so.
- Two test modules: `tests/it/hostile.rs` (four adversarial generators per
  compiled grammar, every answer typed or a tree inside the bounds) and
  `tests/it/sim_parse_bounds.rs` (96 seeds per test over four generated shapes,
  same seed same trace hash, the shape-to-arm map pinned). Both run under a
  nextest `slow-timeout` with `terminate-after`, because they exist to catch a
  walk that stops making progress.

Order changes with the traversal and is additive to the API: nodes arrive in
source order rather than reverse-sibling order, so the node cap's `limit + 1`
witness is the earliest node rather than the last. Every published guarantee
survives it — `AstMetrics` folds order-independently, retained diagnostics are
the earliest under `MAX_SYNTAX_DIAGNOSTICS` and are sorted before being
returned — and `diagnostics` sorts its own output. `try_parse` keeps its
signature.

### Tests: the entropy replay simulation no longer folds drawn bytes (#276)

- `sim_random_error`'s replay trace folded whether each draw came back
  entirely equal to the sentinel byte. A one-byte draw from a working source
  does that one time in 256, so the same seed produced two different traces
  about 3% of runs, and `untouched_draws == 0` failed about 1.6% of runs (PR
  #302, run 37355259369). The trace now folds lengths only, a draw is
  classified untouched only from eight bytes up (`2^-64` by chance), and
  `the_trace_folds_no_drawn_byte` pins both without entropy. 300 runs of the
  two tests: 0 failures.

### lgwks_std — the blocking pool's ceiling and its shutdown (#264)

The two items #286 and #289 left open on the bounded blocking pool. Both are
additive: no signature changed and no existing behaviour did.
