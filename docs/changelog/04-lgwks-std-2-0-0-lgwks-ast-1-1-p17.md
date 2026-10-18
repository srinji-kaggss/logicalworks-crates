# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (17/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 17 of 20. [Changelog index](../../CHANGELOG.md)

### The gate: the saturation tiers spawn the fake the way a default binding does (#272) (continued)

- The `sim_review_path` saturation tiers bound the fake `gh` through a `PATH`
  override and a bare program name. With that combination `std` cannot hand the
  child to `posix_spawn`: it searches the `PATH` itself and falls back to
  `fork` + `execvp`, so each of the 50,000 calls at the 10,000 tier forked a
  test process holding 64 runs in flight. The tiers now name the fake by its
  absolute path with no `PATH` override (`gh_spawned_directly`), which is the
  spawn a default `Gh` binding makes. Every other family keeps the `PATH`
  binding, so `PATH` resolution stays covered. The assertions are unchanged.
- Measured on an Apple M5 Pro, the 10,000 tier alone: 65.98 s wall and 466 s
  CPU (212.6 user, 253.8 system), peak RSS 71.7 MB, before; 46.35 s wall and
  380 s CPU (221.1 user, 158.5 system), peak RSS 65.6 MB, after, with the
  machine more loaded for the second run.
- The `gpui-windows` lane builds its fixture into the workspace `target` with
  `--target-dir target`. The fixture is its own workspace, so it built into a
  directory the CI cache never saved and recompiled the GPUI stack on every
  run (3 min 03 s, 134 crates, the run's critical path at 242 s). The CI cache
  step takes a new key so the first `main` run saves those artifacts.
- The `test` profile emits line tables instead of full debuginfo
  (`debug = "line-tables-only"`). Backtraces still name function, file and
  line. Each `Tests (lgwks-bot full)` shard spent 152 s of a 233 s job compiling
  and linking on a full dependency-cache hit, and the four shards are the run's
  critical path. CI sets the same value as `CARGO_PROFILE_TEST_DEBUG`, because
  Swatinem/rust-cache ignores `[profile]` when it hashes manifests: without
  it the restore stayed a full match on the old key and nothing was saved.

### lgwks_ast — `parse_budget`: a column with no measurement prints `-`

The measurement rig converted its own numbers with `unwrap_or` defaults, so a row
that never reached the parser printed a rate of zero. A rate over nothing is not
a rate.

- **`ratio` returns `Option<u128>`** and `integer_division` is forbidden
  workspace-wide, so the checked division's `None` *is* the answer: a zero
  denominator is a shape the crate refused before the parser ran. A rate column
  with no measurement prints `-`, and the row's `outcome` column names the
  refusal beside it. The refused markdown rows now read
  `… 0 0 0 0 172375 172375 - - 380194 380194 - - 0 0 container-nesting-too-deep`
  where they used to read four zeros that could be mistaken for measurements.
- **`Measured::bytes` and `TierRow::bytes` are `u128`**, so a rate divides without
  widening a `usize` at every column, and `tier_line`'s throughput is
  `u128::from(level) * bytes` rather than a narrowing conversion of the level.
- **`percentile` refuses an empty sample list**, which is what a row with no
  timed round is; `measure` and `measure_tier` refuse a run whose rounds measured
  nothing rather than reporting `unmeasured` in the outcome column.
- **`levels_for` refuses a shape with no nesting fragment** rather than measuring
  zero levels, and `--tier 0` is refused with the byte budgets, because a tier of
  zero parses admits no worker and prints a percentile of nothing.
- **A tier worker returns its own `Result`**, so a refusal inside a worker names
  the shortfall instead of reporting a row over fewer parses than its level.

Verified by running the shipped example: `--grammar rust --shape nested --tier 100
--threads 4` prints a tier row (p50 6.03 ms, p99 10.22 ms, max 10.79 ms, 100/100
admitted), `--grammar markdown --shape nested` prints the `-` columns above, and
`--tier 0`, `--bytes 0` and `--grammar cobol` each refuse by name.

### lgwks_macros — the nine-axis sweep: no stand-in for a value that is not there

Every `unwrap_or`/`unwrap_or_else`/`unwrap_or_default` in `lgwks_macros` read a
missing value as a plausible one. Each is now either the thing itself or a typed
refusal, and the fourteen spellings of "the tokens after this keyword" are one
shared reading.

- **`lines::after(tokens, count)`** is the one place that reads "the tokens after
  the word the caller already matched", so an `each` whose clause has no items
  cannot answer differently from a `for` whose clause has none.
- **`split_at_keyword`** and **`pattern_and_items`** are the one search for
  `<keyword> in <items>`, shared by `each`, `for` and `retry`; `pattern_and_items`
  owns the empty-pattern and empty-items refusals both forms reported separately.
- **`run_call` returns the tokens left after the call** instead of a count, so
  `rewrite` and `run_only` carry the tail rather than re-deriving an index, and
  `run_only`'s "the call is the whole line" test is `remaining.is_empty()`.
- **`Line::keyword` returns the `&Ident`** and `Line::starts_with` answers the
  question each emitter was really asking, which removes the empty keyword string
  three call sites defaulted to — and the per-line `String` allocation with it.
- **`Line::finish` returns `Option<Line>`**: a line with no tokens is not a line
  whose position and span are invented from `Span::call_site`.
- **`refuse` searches the literal whole.** Every machine-path prefix opens with
  `/`, `~` or `C`, none of which is a literal's own delimiter, so stripping `r`,
  `#` and the quotes could never stand between a prefix and the path it starts.
- **A duration literal with no unit, and a number with no decimal point, are two
  readings and not a fallback**: both arms are stated where the split is made.
- **`line_literal` saturates**, so a source longer than `u32::MAX` lines keeps the
  last line there is instead of wrapping onto another line's number.
- **Two split points were off by one**, and the crate's own tests could not see
  either: `split_let` bound `let x =` as the pattern of a binding, and `run_call`
  skipped the callee path twice when it reported the tokens left after the call.
  Both reached `lgwks_bot`'s `script!` users as un-compilable expansions, which is
  how they were found. `emit::words::tests::an_expansion_with_a_run_call_is_still_rust`
  pins them: it expands every shape a `run` call reaches — the whole line, a
  `let` binding, a binding inside `together:`, a `::` callee with tokens after the
  call — and asserts the expansion re-parses as Rust, that the tokens after a call
  survive, and that a `let` binds its name. Reintroducing either defect fails it.

Behaviour is unchanged. All 10 `lgwks_macros` tests pass, including the property
suite that renders trees, reads them back and asserts a line moved off its column
is refused at that line, and `lgwks_bot`'s 192 `script!` tests pass against the
rewritten emitter.
### lgwks_bot — `sim_observe_refresh`: a schedule cell the run did not draw is `None`

The observe/refresh simulation had 60 places where a failed width conversion was
folded onto a number, and four where a *missing* schedule cell became a real one.
The largest of the four was a tenant or pace window the schedule does not
declare: it arrived as a healthy plan, a fast window carrying the value `1`, a
healthy fallback tenant, or a domain id borrowed from another tenant's row — and
a `domain()` is precisely what a caller triaging two forced refreshes reads, so
that last one would have blamed the wrong source.
