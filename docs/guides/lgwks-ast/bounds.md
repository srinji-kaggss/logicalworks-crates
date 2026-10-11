# lgwks_ast: bounds

What each ceiling bounds, in which unit, and what it does not promise. Moved
from the crate README under the size law (#408).

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
