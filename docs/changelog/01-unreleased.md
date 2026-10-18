# Unreleased

Release: Unreleased. Page 1 of 1. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — a run reads back as its script (#386, SL-6)

- **Added** `script::Site` (word, source line, line as written), `Outcome`
  (`Succeeded`, `Retried`, `TimedOut`, `Cancelled`, `Failed`), `TrailEntry`
  and `ScriptTrail`. Every step a `script!` line enters (a flow, `step`,
  `each` and each of its bodies, `within`, `retry`, each `for` iteration, a
  numbered `run`) records its `Site` in the run's trail and settles there with
  its outcome; `retry` settles with the attempt that succeeded.
- **Added** `task::Report::trail` and `Report::script_trail`, which renders a
  finished run as the script's lines in the order they ran, each with its
  source line, nesting, item number and outcome (`tests/it/script_trail.rs`).
- **Added** `script::StepKind::Flow`, the word a flow header's `Site` names; a
  `StepShape` never carries it. `StepKind` is `#[non_exhaustive]`, so this is
  additive.
- A `within` written in `script!` is now an entry of the trail, so
  `Report::steps` lists it; a `within` called by hand records nothing, as
  before. The host settles its task step, so the trail's first entry carries
  the run's outcome.
- Step keys and paths are unchanged: the site rides beside the key, never in
  it (`a_script_step_keys_as_its_path_entered_by_hand`). The trail is the same
  bounded ring (INV-BOT-20).
- `script!` calls the doc-hidden `Scope::enter_at`, `Scope::item_at`,
  `Scope::settle`, `each_at`, `within_at` and `retry_at`; `enter`, `item`,
  `each` and `retry` keep their signatures.
- **Changed** `within` and `within_on` from `async fn` to `fn` returning
  `impl Future` (`+ use<'s, T, Fut>`), the same output: they hand back the
  race's own future, so a nest of deadlines pays one poll frame per level
  rather than one per wrapper, and the borrow of `step` ends at the call. A
  caller that awaits them is unchanged
  (`sim_clock_wiring::a_deep_nest_of_clock_governed_steps_is_stack_bounded`).
- Cost, measured with `examples/measure_overhead.rs` (2,000 runs of 1,024
  script lines, release, the same machine): one script line costs the runtime
  p50 134 ns / p99 153 ns on main 86163649 and p50 143 ns / p99 170 ns with
  the trail, which also records each `within` line main did not record.

### lgwks_ast — every construct knows its line (#386)

- **Added** `script::Site` and a `site()` on `Each`, `Within`, `Retry`, `Step`,
  `For` and `Call`: the word, line and text the construct's map entry already
  carried, so the macro can emit them beside the structural label.

### lgwks_ast — the script tool, a second caller of the one parser (#384)

- **Added** `script::read_source` and `script::Invocation`: every `script!` in
  a Rust file, found by token and read through `script::parse`, each with its
  line and column. `Script` implements `Display` as the runtime's
  `Architecture` renders the `ARCHITECTURE` the macro emits for it.
- **Added** the `tool` feature (default off): `Script::to_json`, and the
  `lgwks-ast` binary, `lgwks-ast script map|check [--json] [PATH...]`. The
  tool's map equals the compiled `ARCHITECTURE` on every `script!` in the
  repository: the bot's tests, compared in process, and its two examples and
  the AI-authoring bench's reference solutions, held by `const` assertions a
  `cargo check` probe evaluates (`script_map_parity`). Its refusal is the
  compiler's, at
  the same line and column (`the_tool_reports_the_refusal_the_compiler_reports`).
- **Added** `script::write_map` and `script::MapStep`, the one rendering of an
  architecture map: `lgwks_bot::script::Architecture`'s `Display` now prints
  through it, so the tool and the runtime cannot render a map differently.

### lgwks_bot — the map renders through `lgwks_ast` (#384)

- The `script` feature now also enables the grammar-free `lgwks_ast/script`
  (already built by `lgwks_macros`) for `Architecture`'s rendering. The
  `lgwks_ast` edge is `default-features = false`; `inspect` turns the grammars
  on as before, so a `script` build still compiles no tree-sitter and still
  checks for `wasm32-wasip1`.
- **Added** `script::FlowShape::line`, the `flow` header's source line, as a
  `const fn` beside `name`, `signature` and `steps`.
- The `script-check` gate lane runs `lgwks-ast script check` over the
  repository.

### lgwks_ast — the typed-error derive without a parser (#367)

The grammar stack (`ast-grep-core`, `ast-grep-language`, `tree-sitter`,
`tracing`) is now the optional `parser` feature, which every `lang-*` feature
enables. The default set is unchanged and so is every path a consumer names.
`default-features = false` now yields only the typed-error derive and the
`diagnostic` types, with no tree-sitter compiled.

**Breaking, `default-features = false` only:** that configuration used to be
the parser with no grammar compiled; it is now `default-features = false,
features = ["parser"]`. `diagnostic::diagnostics` and
`diagnostic::recovery_count` walk a parsed tree and need `parser`.

### lgwks_ast — the `script!` parser (#383)

- **Added** the `script` feature (default off): `lgwks_ast::script::parse`
  reads the `script!` language into a typed tree (flows, steps, labels and the
  `ARCHITECTURE` map) with every refusal decided there, and exports the lexicon
  (`LEXICON`, `Kind`, `Word`). A tool now reads a script through the same
  function the macro compiles it with, from plain source text, refusals
  included (SL-2, #383). The feature reaches `proc-macro2` through the
  `lgwks_deps` storefront and selects no grammar.

### lgwks_macros

- **Changed** `script!` is a shim over `lgwks_ast::script::parse`: the macro
  only writes Rust for the parsed tree. Expansions and refusals are unchanged.
