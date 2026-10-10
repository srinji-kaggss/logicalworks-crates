# ADR 0001: one Rust reader, and it is `lgwks_ast`

Status: **accepted**, 2026-10-10, on the Director's direction of 2026-10-08
(SL-2, "a second reader of the same language is a defect even when it agrees
today"). Issue: #387. Follow-up that routes the loser: #408.

## Context

The workspace reads Rust source in two places, with two parsers:

- **`syn`**, in the `scan` gate (`crates/lgwks-deps/src/scan.rs`, INV-SCAN-ZERO).
  `syn::parse_file` reads each shipped file, and five `syn::visit` detectors
  walk the result.
- **tree-sitter**, through `lgwks_ast::try_parse`, in structural inspection
  (`crates/lgwks-bot/src/inspect.rs`, INV-BOT-21: "parses through `lgwks_ast`,
  never a second parser").

`lgwks_macros` names `syn` only for `syn::Error`, to report a refusal. It reads
no Rust source, so it is not a third reader.

Two readers can disagree about the same file. The spec
(`docs/script-language.spec.md`, "Where the one parser lives") says the split
is settled by measurement: measure fidelity and cost on our own sources, record
the decision, and route the losing reader through the winner.

## Measurement

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

## Decision

**tree-sitter, through `lgwks_ast`, is the workspace's one Rust reader.** The
`scan` gate is the losing caller and is routed through `lgwks_ast` (#408).

Why tree-sitter wins, though it is the slower and the less complete grammar:

1. **It is bounded, and `syn` is not.** `try_parse` refuses oversized input,
   excessive depth, an exhausted node budget and a passed deadline as typed
   errors (INV-AST-1, INV-AST-4, INV-AST-5). `syn` is recursive descent with no
   depth bound. A 10,000-deep expression overflows an 8 MiB stack, and the
   process dies before it can return a refusal. For a gate whose contract is
   "an unparseable file is a refusal, not a pass", a reader that can crash is
   disqualifying.
2. **It holds 21× less memory.** With `span-locations` (which the scan needs
   for line numbers), `proc-macro2` keeps every parsed source in a per-thread
   map, so `syn`'s footprint grows with every byte one process reads: 1,080 MB
   over this corpus. Releasing the map after each parse still peaks at 311 MB,
   6× tree-sitter's 51 MB.
3. **Its fidelity gap does not reach our code.** The 57 files only `syn`
   accepts are all vendored third-party sources. On the files this workspace
   ships, which are the ones the scan and inspection read, the two readers agree
   on every file.
4. **It is already the one parser.** `lgwks_ast` is the workspace's parser for
   every other language and for `script!` (SL-2, INV-AST-ONE-PARSER), so making
   it the Rust reader removes a reader rather than adding a rule.

What tree-sitter costs, stated plainly: `syn` is **1.7× faster** per file
(p50 117 µs against 204 µs; one corpus round 3.1 s against 5.4 s). It also
reads **57 vendored files** that tree-sitter cannot. Neither cost lands on a
file the workspace ships.

## Alternatives rejected

- **`syn` as the one reader.** It cannot parse the other languages inspection
  reads. It has no error recovery, so it cannot produce the earliest-recovery-node
  diagnostics INV-AST-2 requires. And it has neither of the two bounds above.
  Inspection would gain a crash path and lose its multi-language reach.
- **Keep both, with separate jurisdictions.** This is the state SL-2 names as
  a defect, and the two readers already disagree on 57 files.
- **Route by calling `lgwks_ast` from `lgwks_deps`.** Cargo refuses it:
  `lgwks_ast`'s `script` feature depends on `lgwks_deps`, so the reverse edge
  is a cycle. The detectors move into `lgwks_ast` instead, behind a
  default-off `scan` feature, as the script tool did (#384).

## Consequences

- **Not yet routed.** The scan is still read by `syn` until #408 lands. Until
  then, both measured hazards are open in the gate: memory that grows with
  everything one process scans, and an abort on deeply nested input.
- **What #408 must do.** It ports the five detectors to tree-sitter verdict
  for verdict, using `crates/lgwks-deps/tests/it/sim_scan_detectors.rs` as the
  model. It points the `scan` lane at the new command, and it retires the
  `scan.rust_parser` approval for `syn`.
- **Re-measure on every change.** Any change to either reader is re-measured
  with `rust_readers`, and this record is superseded rather than edited.
