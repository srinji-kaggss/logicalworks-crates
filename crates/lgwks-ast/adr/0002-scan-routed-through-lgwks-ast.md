# ADR 0002: the scan gate reads Rust through `lgwks_ast`

Status: **accepted**, 2026-10-12. Issue: #408. Supersedes the "Not yet routed"
consequence of `0001-one-rust-reader.md`, which is not edited.

## What changed

- The five zero-gate detectors (`ERROR-SWALLOW`, `unlogged-err-return`,
  `ALLOW-SILENCE`, `long-try-chain`, `tautological-doc`) moved from
  `lgwks_deps::scan` (`syn::visit`) to `lgwks_ast::scan` (tree-sitter nodes), behind
  the default-off `scan` feature, with the `lgwks-ast scan [--json] [PATH...]`
  subcommand. `lgwks_deps` could not host it: `lgwks_ast`'s `script` feature
  depends on `lgwks_deps`.
- The `scan` gate lane and its CI step run `lgwks-ast scan` (same feature set as
  `script-check`, so the pair builds once).
- `lgwks-deps scan` is gone and says where it went. `lgwks_deps` has no default
  feature and no `syn` edge.
- The `syn` approval (`scan.rust_parser`) is retired from `contract/APPROVED.toml`.
  `lgwks_macros` needed `syn::Error` only to report a refusal, so it now writes
  its own `compile_error!` (`crates/lgwks-macros/src/error.rs`). **No consumer of
  `syn` remains in the workspace's authored edges.** `proc-macro2` stays, for
  the `script!` token streams, and its approval is renamed `tokens.span_locations`.
- A file the checked parse refuses is `ScanError::Refused` (byte ceiling, node
  budget, depth bound `AstTooDeep`, deadline) or `Unparseable` (syntax, with the
  line of the first damage). It is counted, named, and the run continues.
- `lgwks_deps`'s `lib.rs` and `main.rs` were over the size law and are split by
  moving code verbatim (`edge`, `license`, `tier`, `refusal`, `tests`,
  `check_report`, `debug_cmd`, `request_cmd`, `freshness_cmd`).

## Fidelity

Verdict for verdict, against the reader this replaces:

- `sim_scan_detectors` (the independent model) moved with the detectors and passes
  on every seed: 38 tests, 1,024 mixed files in eight bands plus the per-rule
  families.
- The 52 unit verdicts moved unchanged (`scan_verdicts`); eight more pin the shapes
  tree-sitter spells differently (`scan_cases`), each checked against the old
  binary on the same file.
- Differential, old `lgwks-deps scan` against new `lgwks-ast scan`, file by file:
  the 7,688 tracked `.rs` files (vendored crates included) and every third file of
  the crates.io registry cache (15,832 files). Where both accept a file they agree
  on **every finding** (6,661 and 13,951 findings; all five rules exercised: 8,595
  `ALLOW-SILENCE`, 7,907 `unlogged-err-return`, 2,257 `tautological-doc`, 1,590
  `ERROR-SWALLOW`, 268 `long-try-chain` over both corpora). One divergence was found
  and fixed: a `return` under an attribute starts at the attribute.
- Where they differ it is the grammar: 57 vendored files (as in ADR 0001) and
  2,108 registry files that tree-sitter-rust 0.24 refuses (for example a macro
  named after a primitive type, `char!(..)`), against 2 that only `syn` refused.
  None is first-party; the gate over this repository reports 518 files, zero findings.

## Measurement

`rust_readers` over the 7,685 readable tracked files (107,737,432 bytes, 5 rounds,
release build, one reader per process, machine loaded by other sessions):

| | `try_parse` alone | `scan` (parse and detectors) |
|---|---|---|
| per-file p50 / p99 / max | 245 us / 14.0 ms / 138 ms | 681 us / 30.1 ms / 297 ms |
| one round over the corpus | 7.3 s | 18.3 s |
| peak RSS | 53.5 MB | 53.9 MB |

ADR 0001 measured the `syn` reader at 1,080 MB over the same corpus: the scan now
holds 54 MB, 20 times less, and RSS no longer grows with what is scanned. On this
repository's 518 files the gate takes 1.6 s and 16 MB, against 0.38 s and 44 MB
before: **the port is about 4x slower in wall time**, because the detectors walk
the tree several times with per-node allocation; an index pass or fewer walks
would recover most of it and is not done here.

Hostile nesting, one process per depth (`rust_readers nest`):

| depth | 10,000 | 100,000 | 1,000,000 |
|---|---|---|---|
| `syn` (ADR 0001) | abort, exit 134 | abort | abort |
| `try_parse` | `AstTooDeep` in 5.6 ms | 35 ms | 368 ms |
| `scan` | `AstTooDeep` in 3.3 ms | 35 ms | 355 ms |

`scan_bounds` holds these rows, and holds that a tree at the depth bound is
scanned on a 512 KiB stack in four nesting shapes.
