# ADR 0001 evidence: the two Rust readers, measured

The measurement section of [ADR 0001](../0001-one-rust-reader.md), kept whole
here so the record stays inside the repository's 120-line limit. The record's
decision, alternatives and consequences are unchanged; this file is the evidence
they rest on and is re-measured, not rewritten.

`crates/lgwks-ast/examples/rust_readers.rs` reads the same files with both
readers. The corpus is this repository's tracked `.rs` files at `c544ea874`:
7,682 files, 107,904,076 bytes, vendored crates included. Each file is parsed
five times by each reader, and every parse is one latency sample.
tree-sitter's answer is the checked boundary inspection uses
(`try_parse`: byte ceiling, deadline, node budget and depth bound), so its cost
includes the validation walk.

```sh
git ls-files '*.rs' > corpus.txt
cargo build --locked --release -p lgwks_ast --example rust_readers
target/release/examples/rust_readers both 5 < corpus.txt
/usr/bin/time -l target/release/examples/rust_readers syn 5 < corpus.txt
/usr/bin/time -l target/release/examples/rust_readers syn-released 5 < corpus.txt
/usr/bin/time -l target/release/examples/rust_readers tree-sitter 5 < corpus.txt
target/release/examples/rust_readers nest syn 10000          # one process per depth
```

Results, measured on Apple silicon (macOS, release build) with no other cargo
job running:

| | `syn` | tree-sitter (`lgwks_ast::try_parse`) |
|---|---|---|
| files accepted | 7,681 | 7,624 |
| per-file parse p50 / p99 / max | 117 µs / 4.9 ms / 34 ms | 204 µs / 8.8 ms / 58 ms |
| one round over the corpus | 3.1 s | 5.4 s |
| peak RSS, one reader per process | **1,080 MB** | **51 MB** |
| peak RSS, `syn` releasing spans after each parse | 311 MB | — |
| balanced nesting, depth 1,000 | accepted | refused: depth bound (512) |
| depth 10,000, 100,000 and 1,000,000 | **stack overflow: the process aborts (exit 134)** | refused: depth bound, in 3 ms, 34 ms and 345 ms |

Agreement over the 7,682 files:

- 7,624 files are accepted by both readers.
- 57 are accepted by `syn` only. tree-sitter refuses them with an `ERROR` or
  `MISSING` node: these are grammar gaps in tree-sitter-rust. **Every one of
  the 57 is under `vendor/`**, so none of them is a file this workspace writes
  or ships.
- 0 are accepted by tree-sitter only.
- 1 is refused by both (`vendor/syn-1.0.109/tests/test_item.rs`, which is
  deliberately invalid).
