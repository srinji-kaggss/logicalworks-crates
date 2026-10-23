# ADR 0002: the scan gate reads Rust through `lgwks_ast`

Status: **accepted**, 2026-10-12. Issue: #408. Supersedes the "Not yet routed"
consequence of `0001-one-rust-reader.md`, which is not edited.

Stakeholders: the Director; the gate's author; a contributor whose file the scan reads; a consumer reading a refusal
Concerns: a refused file must not end the walk or abort the run; the gate must hold one reader; a detector port must agree verdict for verdict with the reader it replaces
Requirements: R8, R13
Viewpoint: the detector viewpoint — one view per reader, each view the finding count, the wall time and the peak RSS over one corpus
Elements and relations: `lgwks_ast::scan`, its five detectors, the `lgwks-ast scan` command, the `scan` lane and `contract/APPROVED.toml`; relations: `lane runs command`, `command runs detectors`, `detector reads through parser`

## Context

ADR 0001 measured the two Rust readers and chose `lgwks_ast`'s, but its own
"not yet routed" consequence left the `scan` gate still reading with `syn`, with
both measured hazards open: memory that grows with the corpus, and an abort on
deeply nested input. `lgwks_deps` could not host the port: `lgwks_ast`'s `script`
feature depends on it, so the reverse edge is a cycle Cargo refuses. This record
is that routing.

## Decision

- The five zero-gate detectors (`ERROR-SWALLOW`, `unlogged-err-return`,
  `ALLOW-SILENCE`, `long-try-chain`, `tautological-doc`) moved from
  `lgwks_deps::scan` (`syn::visit`) to `lgwks_ast::scan` (tree-sitter nodes),
  behind the `scan` feature, with the `lgwks-ast scan [--json] [PATH...]`
  subcommand.
- The `scan` gate lane and its CI step run `lgwks-ast scan` (same feature set as
  `script-check`, so the pair builds once).
- `lgwks-deps scan` is gone and says where it went. `lgwks_deps` has no `syn`
  edge.
- The `syn` approval (`scan.rust_parser`) is retired from
  `contract/APPROVED.toml`. `lgwks_macros` needed `syn::Error` only to report a
  refusal, so it now writes its own `compile_error!`
  (`crates/lgwks-macros/src/error.rs`). **No consumer of `syn` remains in the
  workspace's authored edges.** `proc-macro2` stays, for the `script!` token
  streams, and its approval is renamed `tokens.span_locations`.
- A file the checked parse refuses is `ScanError::Refused` (byte ceiling, node
  budget, depth bound `AstTooDeep`, deadline) or `Unparseable` (syntax, with the
  line of the first damage). It is counted, named, and the run continues.
- `lgwks_deps`'s `main.rs` was over the size law on `main` (2,100 lines), so
  touching it for the `scan` stub meant splitting it, by moving code verbatim
  into `check_report`, `debug_cmd`, `request_cmd` and `freshness_cmd` (836 lines
  now).

## Fidelity

Verdict for verdict, against the reader this replaces:

- `sim_scan_detectors` (the independent model) moved with the detectors and passes
  on every seed: 1,024 mixed files in eight bands plus the per-rule families.
- The 52 unit verdicts moved unchanged (`scan_verdicts`); eight more pin the shapes
  tree-sitter spells differently (`scan_cases`).
- Differential, the old `lgwks-deps scan` built from `main` against the new
  `lgwks-ast scan`, one run per checkout, outputs sorted because the two walk in a
  different order: this repository (696 files, zero findings from both) and five
  third-party checkouts (bevy, helix, wezterm, gosub-engine, rust-sdk: 3,164
  files). Where both read a file they agree on **every finding**: 2,147 identical
  findings, all five rules among them. The old binary reports 2,173 and the new
  2,152.
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

The wall-time and peak-RSS tables, the per-file latency table and the hostile
nesting rows are in
[`measurements/0002-scan-route-measurement.md`](measurements/0002-scan-route-measurement.md).

## Alternatives rejected

- **Port the detectors into `lgwks_deps`.** A cycle: `lgwks_ast`'s `script`
  feature depends on `lgwks_deps`, which already carries the script token
  reader, so `lgwks_deps` cannot depend back on it.
- **Keep `scan` reading `syn` beside the routed reader.** That is the second
  reader ADR 0001 and SL-2 forbid, and the two already disagree on 57 files.
- **Abort the walk on the first refused file.** The measured cost is the five
  `xtask` findings and every file that sorts after a refusal; a refusal is now
  counted and named, and the run continues.

## Consequences

- The gate is 2.1x to 2.7x slower in wall time and holds 3.2x to 4.5x less
  memory, which no longer grows with the corpus; the lane still takes 1.6 s.
- `lgwks_deps` holds no Rust grammar, and `syn` is gone from every authored
  edge, so the workspace compiles one fewer proc-macro stack.
- A third-party grammar gap now fails the gate by design: 34 of 3,164 third-party
  files are refused, none of them first-party.
- The detectors' per-node allocation is the remaining cost; an index pass would
  recover most of it and is not done here.

## Correspondence

Routes the "not yet routed" consequence of
[ADR 0001](0001-one-rust-reader.md) and does not edit it. Serves
`INV-SCAN-ZERO` and `INV-AST-ONE-PARSER`.

## Verification

The `scan` lane (`lgwks-ast scan`, exit 2 on any finding or refused file);
`sim_scan_detectors` against its independent model; `scan_verdicts` and
`scan_cases` for the verdicts; `scan_bounds` for the hostile rows; and
`scan_evidence` for the command itself.
