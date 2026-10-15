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
- `lgwks_deps`'s `main.rs` was over the size law on `main` (2,100 lines), so touching
  it for the `scan` stub meant splitting it, by moving code verbatim into
  `check_report`, `debug_cmd`, `request_cmd` and `freshness_cmd` (836 lines now).
  Its `lib.rs` is already split on `main` and is only trimmed of `pub mod scan` and
  `pub use syn`.

## Fidelity

Verdict for verdict, against the reader this replaces:

- `sim_scan_detectors` (the independent model) moved with the detectors and passes
  on every seed: 1,024 mixed files in eight bands plus the per-rule families.
- The 52 unit verdicts moved unchanged (`scan_verdicts`); eight more pin the shapes
  tree-sitter spells differently (`scan_cases`).
- Differential, the old `lgwks-deps scan` built from `main` against the new
  `lgwks-ast scan`, one run per checkout, outputs sorted because the two walk in a
  different order: this repository (696 files, zero findings from both) and five
  third-party checkouts (bevy, helix, wezterm, gosub-engine, rust-sdk: 3,164 files).
  Where both read a file they agree on **every finding**: 2,147 identical findings,
  all five rules among them. The old binary reports 2,173 and the new 2,152.
- The 26 the new one lacks are all in 6 files that tree-sitter-rust 0.24 refuses
  (bevy `mesh.rs`, `render_context.rs`, `scene_viewer/main.rs`; wezterm
  `keyevent.rs`; gosub `property_definitions.rs`, `syntax_matcher.rs`). In all, it
  refuses 34 of the 3,164 files: 33 on syntax (29 of them bevy's) and one on the
  byte ceiling (a 5.9 MB generated table). `syn` refused one, helix
  `tests/indent/rust.rs`.
- The 5 findings only the new one reports (helix `xtask/src/main.rs`) are the
  stop-at-the-first-refusal fix: the old binary ended its walk at that one refused
  file, and `xtask` sorts after it. A refused file is now counted, named, and the
  run continues (`a_refused_file_is_counted_and_the_files_after_it_are_still_read`).
- None of the refused files is first-party: the gate over this repository reads 696
  files with none refused. A third-party grammar gap fails the gate, by design.

## Measurement

Taken 2026-10-15 on `main` at `eb6d4f4d9` plus this change: release builds, a Mac
shared with other sessions (load average 1.7 to 4.3), one reader per process so a
peak RSS (`/usr/bin/time -l`) is that reader's own.

The gate itself, the old binary against the new, wall time and peak RSS:

| corpus | old `lgwks-deps scan` | new `lgwks-ast scan` |
|---|---|---|
| this repository, 696 files (3 runs each) | 0.60 to 0.68 s, 35 MB | 1.63 to 1.65 s, 11 MB |
| bevy, 1,900 files | 1.46 s, 59 MB | 3.07 s, 13 MB |

**The port is 2.1x to 2.7x slower in wall time** and holds 3.2x to 4.5x less memory,
which no longer grows with the corpus. The cause is the detectors, which walk the
tree several times with per-node allocation: the scan costs 3x the checked parse
alone (below). An index pass or fewer walks would recover most of it and is not
done here. The gate lane takes 1.6 s, far inside its budget.

`rust_readers`, 5 rounds over every file, per-file latency:

| corpus, reader | accepted | p50 / p99 / max | one round | peak RSS |
|---|---|---|---|---|
| this repository (696 files, 12.2 MB), `try_parse` | 696 | 510 us / 4.2 ms / 9.0 ms | 474 ms | 10.0 MB |
| this repository, `scan` | 696 | 1.6 ms / 14.1 ms / 26.3 ms | 1,517 ms | 10.5 MB |
| bevy (1,900 files, 23.6 MB), `try_parse` | 1,871 | 232 us / 4.2 ms / 7.4 ms | 941 ms | 11.6 MB |
| bevy, `scan` | 1,871 | 704 us / 13.3 ms / 36.6 ms | 2,967 ms | 12.9 MB |

ADR 0001 measured the `syn` reader at 1,080 MB over a 107 MB corpus (the vendored
tree it used is gone from `main`, so that row is cited, not repeated).

Hostile nesting, one process per depth (`rust_readers nest`, all exit 0):

| depth | 10,000 | 100,000 | 1,000,000 |
|---|---|---|---|
| `syn` (ADR 0001) | abort, exit 134 | abort | abort |
| `try_parse` | `AstTooDeep` in 3.2 ms | 32 ms | 328 ms |
| `scan` | `AstTooDeep` in 3.2 ms | 31 ms | 330 ms |

`scan_bounds` holds these rows, and holds that a tree at the depth bound is
scanned on a 512 KiB stack in four nesting shapes.
