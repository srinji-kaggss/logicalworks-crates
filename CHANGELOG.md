# Changelog

All notable changes to the four crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
semantic from 1.0.0: a major bump breaks, a minor adds, a patch fixes
(`docs/releasing.md` §3). Before 1.0.0 any minor could break, and those
breaks are listed explicitly under the crate.

## [Unreleased]

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

- **`TenantSchedule::tenant` and `PacePlan::at` return `Option`.** Every caller
  propagates, and the assertions that already compared a report against the
  schedule now fail on a cell that does not exist rather than passing against a
  stand-in.
- **`domain_of` names the unmapped pair.** The wrap into a row is a mask over a
  width the table declares (`1 << DOMAIN_ROW_BITS`), so the mask and the row
  cannot drift apart, and a pair outside the table is `test::unmapped`.
- The remaining conversions are each loop or draw drawn in the width its consumer
  already uses, so there is no conversion left to stand in for.

(9-axis sweep)

### lgwks_bot — the journal and inspection test fixtures are read, not re-spelled

Four test files each carried their own copy of an identity the shared fixture
already owns, and three carried a bounded wait whose "no deadline" arm turned
the bound off.

- **The run identity and the ladder are read, not re-spelled.**
  `effect_journal.rs` built its own `EffectKey` from its own copies of the run,
  action, environment, flow and digest constants and walked the ladder in its own
  loop; `effect_identity.rs` copied the same five constants. Both now read
  `tests/support/journal.rs`, and the admit-and-prepare walk takes its rungs from
  the shared `ladder`, so a second spelling cannot fence a different world under
  what looks like the same key (INV-BOT-58).
- **`authority.rs`'s doubles open through `tests/support/poll.rs`.** A scripted
  source now admits through the shared `admit_poll` helper and counts there, and
  a source that runs past its own script refuses with a typed error instead of
  answering a value the test never scripted.
- **`inspect_contract.rs` bounds its waits by elapsed time.** Three waits
  compared against `now + PATIENCE`, and the arm where that addition is
  unrepresentable returned `now` — which is an unbounded wait, the opposite of
  what the helper is for. The budget is now an elapsed comparison, which cannot
  fail to represent "longer than this".
- **`registry.rs` fails when there is no refusal to inspect.** A hostile
  identifier that *was* accepted rendered as the empty string and passed an
  assertion about newlines for free; it is now the failure the test means.
- `process_ownership.rs` refuses a host whose clock reads before 1970 rather
  than naming its scratch directory with a stand-in zero, which would have
  folded that host into the namespace of every epoch-aligned run; `rt_runtime_stack.rs`'s
  frame padding comes from a conversion that cannot fail; and
  `inspect_support/mod.rs`'s `allow(dead_code)` is gone, because every fixture in
  it is reachable from the one integration binary (#272).

### lgwks_bot — the measurement examples: no sentinel stands in for a reading

Every `unwrap_or` family call across the ten example harnesses was a fabricated
number, a fabricated identity, or a fabricated error, and each is now either the
reading the operation produced or a typed refusal. The published numbers change
in three places, all of them cases where the old output claimed a measurement
nobody made.

- **No example substitutes a default for a missing value.** A percentile over an
  empty sample is `null` (INV-BOT-142's spelling of *not measured*) rather than
  `0`, a run whose elapsed window is below the clock's own resolution reports no
  rate rather than a fabricated maximum, `fsyncs/records` is `none` when nothing
  was staged, and `peak_rss_kib` is `null` off Linux instead of `0` — a printed
  zero beside a memory field reads as a process that used none.
- **One percentile definition.** `inspect_scale` and `measure_overhead` carried
  private percentile functions beside the shared instrument at
  `examples/support/measure.rs` that the other three harnesses use; both now
  report through it, so every harness's p50/p95/p99 means the same thing.
  `measure_overhead`'s two JSON lines become that instrument's line format, and
  `inspect_scale`'s tier line carries the same `n=`/`max=` fields the others do.
- **One scratch directory.** `examples/support/scratch.rs` owns the random-named
  temp root four harnesses built by hand and removed only on the success path;
  a refusal half way through a sweep now leaves nothing behind for the next run
  to inherit (INV-BOT-116). One recovery from a poisoned `Mutex` lives at
  `tests/support/lock.rs` and is included by path from the tests and the
  examples, so no harness carries a second opinion about what a panic leaves.
- **An unknown mode is a refusal.** `inspect_scale` folded any unrecognised
  argument into the tier sweep, so a mistyped mode measured the wrong thing
  silently; it now names the two modes and refuses the rest. `review_pr_bench`
  refuses a tier of zero, whose percentiles would have been readings of an
  empty sample, and its usage and diagnostic lines go through locked handles
  whose write errors are handled.
- `script_tenants` and `compare_orchestration` spell their retry ceiling inside
  the loop instead of carrying an `Option` whose `None` arm stood in for a
  refusal nobody produced, and `examples/probes/invariant_probe.rs` propagates
  the two `unwrap()`s it used to carry so the audit record demonstrates one
  claim rather than two.
### lgwks_bot — the lexicon ranks a score it cannot order last, not as a tie (nine-axis sweep)

`score_all` ranked candidates with `partial_cmp(..).unwrap_or(Ordering::Equal)`.

- **An unorderable score now sorts last.** `partial_cmp` returns `None` for
  exactly one pair of `f64` values — a NaN on either side — and the blend above
  cannot produce one from two clamped unit scores and non-negative weights, so
  the arm is unreachable today. `total_cmp` alone would have ranked a NaN
  *first*, handing the win to the one candidate that measured nothing; treating
  it as a tie left an unorderable entry in the list `decide` then computed its
  lead over.
- **`INPUT_BOUND_CHARS`** is the input bound at the width the policy digest
  carries, replacing `u32::try_from(MAX_UTTERANCE_CHARS).unwrap_or(u32::MAX)` —
  a narrowing conversion whose failure arm would have put a bound in the digest
  that the resolver does not apply. `the_digest_input_bound_is_the_shipped_bound`
  asserts the two spellings are one number; run as a mutant (512 → 256) it fails.

### lgwks_bot — a lost publication response and a contradicted one are one match (nine-axis sweep)

`review.rs` selected between `Reconcile::None` twice — once through a guard that
proved `created_id` was `Some` and then read it through
`created_id.unwrap_or_default()`. The two arms are now one match on the id
itself, so the value that decides the outcome is the value the arms read.

### lgwks_bot — `retry`'s contract tests start from one fixture (nine-axis sweep)

Eight of `retry`'s tests wrote out the same `RetryFacts::new(...)` preamble by
hand, and two of them were byte-identical. They differ from each other in
exactly one clause — the contract, its retention window, or its late-arrival
behaviour — and that is the property the assertions rest on: a refusal is only
attributable if nothing else moved.

- **`unresolved()` is that fixture**: a live authority over an attempt that may
  have landed, zero attempts used, zero elapsed, the shared payload. Every
  contract test now changes one clause on top of it, so a second difference
  cannot creep in unnoticed.
- `permissive(class)` is unchanged and still the fixture for the tests that move
  the class.

No behaviour change.

### lgwks_bot — the broker's dispatch tests build their subject once (nine-axis sweep)

Four of `broker`'s tests each assembled the same five lines — a broker, a
registered environment, the key carrying its generation, a journal, and an
admitted attempt. Two of them were byte-identical, which is how one of them
drifts: a test that assembles its own may admit one key and authorize another,
and every assertion after that is about a situation the module cannot produce.

- **`AdmittedEnvironment` is now the fixture** those four tests share, and all
  four build from it. The generation the broker authorizes, the key that carries
  it, and the admission already on the journal are one value now, so they cannot
  disagree.
- **`Broker::environments` declares its bound.** One entry per environment the
  host created, added by `register` or `adopt`, never refilled on its own, and
  `close` marks an entry closed rather than removing it — so a warrant for a
  closed environment is refused as `Closed` rather than looking like one for an
  environment the broker never heard of.

No behaviour change.

### lgwks_bot — the logical clock saturates in its own arithmetic (nine-axis sweep)

`Clock::virtual_at` narrows an origin `Duration` into the `u64` nanosecond
counter every budget, deadline and snapshot is derived from, and it did that
through a `try_from` whose failure arm substituted `u64::MAX`.

- **`duration_to_nanos` splits the duration** into `as_secs` and
  `subsec_nanos` — two infallible projections — and does the scaling with
  `saturating_mul`/`saturating_add`. The ceiling is now produced by the
  arithmetic rather than by a fallback value standing in for a conversion that
  failed, and the function cannot return an error to ignore.
- **`Inner::origin` names its two cases.** A wall clock carries the instant it
  was placed at; a virtual clock has none, and `Instant::now()` is that clock's
  only honest origin, not a substitute for a missing one.
- **The saturation identity is now measured, not asserted.** A seeded sweep over
  the whole representable range — zero, sub-second, whole-second, one second
  below the ceiling, at the ceiling, and `Duration::MAX` — pins
  `virtual_at(origin).now() == min(origin, ceiling)` and the same identity for
  `advance`. Two mutants were run against it: dropping the sub-second term and
  truncating instead of saturating are each caught by
  `the_nanos_conversion_keeps_subsecond_precision_and_stops_at_the_ceiling` and
  `no_swept_origin_reads_outside_the_representable_range`.
- `sim_clock`'s receipt carries `Duration` rather than a `u64` nanosecond count,
  because narrowing one needs a fallback and a fallback makes a wrapped reading
  and an unstarted clock the same number in the receipt.

Behaviour is unchanged: the old fallback and the new saturation both report
`u64::MAX` nanoseconds for an origin past the ceiling.

### lgwks_bot — the four verb traits declare return-position `impl Future` (nine-axis sweep)

The crate carried exactly one lint suppression: a crate-level
`#![allow(async_fn_in_trait)]` whose reason was that the verbs must stay
non-`Send`. `rust-guard` refuses every `allow`/`expect`, crate root included,
and a suppression is a rule that does not exist, so the shape moved instead of
the attribute.

- **`Observe::poll`, `Execute::execute_action` and `Query::query` are declared
  `fn … -> impl Future<Output = Result<_, BotError>>`** instead of `async fn`.
  A domain still writes `async fn` in its impl — the erased
  [`BoxFuture`] boundary is unchanged — so this is a declaration change, not an
  authoring change, and no `Send` bound is introduced: the future stays local to
  the driving thread, which is what `Bot::tick` and the `lgwks_std::task`
  driver are built for.
- **`Execute`'s doc comment was a truncated duplicate** and had swallowed
  `EffectLifetime`'s own documentation, so the enum's rustdoc read as a run-on
  of the trait's first paragraph. Each item carries its own text again.
- `BoxFuture`'s doc no longer claims the traits are "native `async fn`".

### lgwks_bot — `Supervisor::wait_idle`: the drain no longer pays a timer tick (#269)

`Supervisor` had no awaitable join, so a caller that wanted every task finished
polled `reap()` in a sleep loop, and Tokio's 1 ms timer granularity charged each
drain 1.3–1.6 ms that raw Tokio (awaiting `JoinSet::join_next`) never paid. That
was the whole of the 3–5x p99 gap `bench/async` reported on its quiet rows.

- **`Supervisor::wait_idle().await`** joins every task on the set's own wakeup,
  cancels nothing, returns how many it joined, and leaves the supervisor usable.
- **`spawn` at a full bound waits on the task set**, the semaphore or the
  supervisor's token — whichever resolves first — instead of a timed retry. A
  permit held by a process group still being cleaned up is rechecked every
  100 ms, and a newly registered cleanup owner wakes the waiter.
- `bench/async` drains through `wait_idle`; its output goes through
  `lgwks_std::trace`.
- **Supervised processes start through the std form of the engine's
  `Command`** and are owned by a private `OwnedChild` that reaps with
  `try_wait`, and kills and parks the child if dropped unreaped — so
  `tokio::process::Command::spawn` stays banned with no exception in the crate.

Measured on an Apple M5 Pro, `bench/async --rounds=15 --alloc-report`, load
average 11.60, on the committed tree: facade/raw-Tokio p99 ratio 1.25x
(quiet-async-bot), 1.00x (high-fanout), 1.14x (at-capacity), 1.07x
(single-permit); p50 ratio 1.18x / 1.02x / 1.09x / 1.08x. Before, 4.87x on the
quiet row. The `--tiers` ladder to 100,000 tasks ends at facade p99 104 ms
against raw Tokio's 107 ms. Peak memory footprint 2.6 MB. The 10,000-tier group-commit saturation sim now drives its runs
64 at a time, so it exercises batching and finishes in 10–12 s alone instead of
83–116 s.

### lgwks_bot — the supervisor's per-task cost, measured and cut (#269)

Measured on an Apple M5 Pro with `bench/async`'s allocation attribution, 1,024
tasks at bound 8, release: **`Supervisor`'s per-task allocations went from 15.33
to 3.61** against a raw Tokio baseline's 2.02, and the facade's excess over that
baseline from 13.31 to 1.59. The issue's ≤ 6 allocations/task target is met. Two
changes, no public API change and no behaviour change:

- **`CancellationToken`'s signal channel is built on first use.** `Inner` held a
  `watch::Sender` constructed eagerly, so every token and every child token paid
  for a channel that nothing awaited: `CancellationToken::new` cost 11 heap
  allocations and `child_token` another 11. The `AtomicBool` is the authority
  and a channel only *wakes* waiters, so the channel is a `OnceLock` built by the
  first subscriber, which re-reads the flag after installing it. Both are now 1
  allocation. Two regression tests cover the window a lazy channel opens — a
  cancel that finds no channel, and a child cancelled before its own — because
  every existing test either waits before the cancel or cancels after a wait has
  already built one.
- **`Supervisor::claim` no longer arms a 100 ms timer on an uncontended spawn.**
  It raced the permit against the token through `timeout(100 ms)` inside
  `run_until_cancelled`, which costs 13 allocations and a timer-wheel
  registration, for a wait an uncontended spawn never takes. It now takes a free
  permit and re-checks cancellation on it — the same decision, the same
  cancellation-wins-the-race rule, and the contended path untouched.
  `claim_now` now shares one non-counting `try_take`, so a full pool on the
  backpressure door stays backpressure rather than becoming a refusal.

`INV-BOT-12`, `-13` and `-31` are unchanged: the fast path keeps the cancel-first
gate, the reap-before-admit order and the re-check on the permit, and neither the
identity map nor the capped report buffer was touched.

**The latency half of the same target is not met, and that is the finding.**
Removing 76% of the allocations moved the paired p50/p99 ratios not at all:
`quiet-async-bot` is 4.49x the raw baseline against 4.65x before, on the same
harness. The remaining gap is work — the per-spawn reap, the identity map, the
`TaskOutcome` the wrapper builds — not allocation. The full measurement, the item
by item breakdown, the six-bound saturation curve, the in-flight tiers to
1,048,576 concurrently admitted tasks and the overload/recovery run are in
`bench/async/README.md`.

### The saturation curve, the open-loop driver and the in-flight tiers (#269)

`bench/async` gained an open-loop driver: offered rate as a parameter, each
arrival's latency measured from its **intended** start rather than from when the
generator reached it, and a HdrHistogram-style log-bucketed recorder implemented
in the bench crate with no new published-crate edge. Latency is bucketed at
1/256 relative error at every magnitude, so a 40 ns body and a 4 s stall are both
representable; min, max and total are exact and samples past the ceiling are
counted rather than folded in.

Measured on an Apple M5 Pro (15 cores, 24 GB, macOS 27.0, rustc 1.99.0):

- **A saturation curve at six in-flight bounds** — 64, 1,024, 10,000, 16,384,
  100,000, 131,072 — with the knee declared on both `Supervisor` and raw Tokio.
  **The two declare the same knee at every bound**, agreeing to within 0.5% on
  achieved rate at five of six. The body cost is derived per bound so every
  ceiling's declared capacity lands on one declared target; a constant body cost
  put the wide ceilings above what an in-process generator can offer, and every
  rung above the first then measured the generator's backlog rather than the
  ceiling's.
- **Past the knee `Supervisor` refuses rather than growing.** At 320,000
  arrivals/s, a bound of 16,384 admitted **exactly 16,384** tasks, 100,000
  admitted **exactly 100,000**, 131,072 admitted **exactly 131,072** — the
  declared bound to the task, on both sides — refusing and counting 59% of the
  offer, with the p99 of what was admitted still one service time.
- **1,048,576 tasks concurrently admitted** and all completed, on both sides:
  peak RSS **2,039 bytes per in-flight task** for the facade against the
  baseline's 2,595, 2.14 GB resident for the process.
- **Overload and recovery**: 30 s at twice the knee built a 229,554-arrival queue
  and took the served p99 from 6.7 ms to 11.4 s with nothing refused and nothing
  lost (offered = admitted = completed, gated); the facade drained in 0.023 ms
  against 4.801 ms, and both sides were back inside their own baseline p99 at the
  first recovery window, 500.7 ms and 505.1 ms.
- **17 seeded simulation tests** for the driver's schedule arithmetic, all with a
  trace-hash replay assertion, the ladder bracketing its knee at every declared
  bound and body cost, both sides offered the same arrivals, the derived body
  putting every ceiling at the declared target, and the knee budget separating
  the regimes at 1x and 16x capacity.

**The 1–2 vCPU / 1–2 GB VPS profile is NOT measured** and is stated as such in
`bench/async/README.md` and `docs/production-readiness.md` §4.2. macOS exposes no
cgroup, no `taskset`, no `taskpolicy` CPU set and no `cpulimit`; the closest
runnable thing, `--workers=2`, produced knees identical to `--workers=15` because
every body in the sweep is a timer, so a thread count is not a vCPU count and
this workload would not separate them.

### Acceptance evidence is now executable (#271)

Every T01–T36 falsifier row in `docs/orchestration-acceptance.spec.md` names
the tests that address it, in `docs/acceptance/t-rows.toml`. That claim was
previously unfalsifiable; `scripts/acceptance-receipts.py` now runs exactly
those tests through one anchored nextest filter and records each row's outcome
in a SQLite database under the state directory, one row per revision, row, test,
platform and feature set.

The receipt can lower a row's claimed state and never raise one, so a green run
cannot promote `present` to `exercised`, and it exits non-zero when a named test
is absent, failing, or a row claims more than the run shows. The one promotion,
`accepted`, requires the receipt's revision to be the head being rendered. The
spec's per-row table is rendered from the database for one exact revision between
`<!-- acceptance-table: start -->` markers, `--check` fails when the committed
table disagrees with it, `--export` writes one JSON artifact for a CI upload, and
`--test` runs the generator's own fifteen regression cases. No new dependency:
the database is Python's standard-library `sqlite3`.

CI builds the receipt from what the suite already ran. The `ci` nextest profile
writes a JUnit report per shard, and a job with no Rust toolchain merges the four
with `--from-junit` in about 0.2 s, rather than executing the 215 named tests a
second time.

Every row now also answers to its own id in a test name: 15 new row-addressed
tests across `crates/lgwks-bot/tests/it/t_rows.rs` and
`crates/lgwks-bot/tests/it/sim_t_rows.rs`, so
`cargo nextest list --workspace -E 'test(/_t07$/)'` says which tests address a
row without reading the map. Six of them are seeded sweeps replay-checked against
their trace hash. They are new tests rather than renames precisely so that every
existing citation of an existing test keeps resolving. No crate's public API
changed.

### lgwks_ast — the parse had no time bound, and the walk was quadratic (#277)

Two bounds the crate already claimed, and the numbers behind them.

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

- `task::configure_blocking_pool(threads)` fixes the pool's thread ceiling
  once, before the pool first runs a job. The pool's own creation is the
  arbitration — a configure and a first use race to build it, so neither can
  miss the other's write — and a ceiling is therefore never silently ignored.
  A later attempt is refused with the new `PoolConfigError`: `InUse` once the
  pool has run work, `AlreadyConfigured` naming the ceiling in force,
  `InvalidCeiling` for a ceiling below one. Asking again for the ceiling
  already in force succeeds: the pool is at it.
- `task::shutdown_blocking_pool(within)` closes admission, lets the queued
  and running jobs finish, and joins every pool thread inside the deadline.
  A thread with no job leaves instead of parking, and a parked thread is woken
  to take a waiting job or leave, so the pool empties by finishing its work
  rather than by cancelling it. The new `PoolShutdown` reports what the wait
  found: `Drained { threads }` means every thread was joined and none outlives
  the call; `DeadlineExceeded { joined, running, queued }` names the threads
  still executing and the jobs still waiting, and their handles stay
  registered so a later shutdown joins them.
- `task::SpawnError::Shutdown` is the refusal both entry points give after a
  shutdown. `try_spawn_blocking` returns it; `spawn_blocking`, whose 1.0
  contract is that it never refuses, fails its awaiter with it as the
  payload, as it already did for an `Os` refusal. The closure never runs.
- A thread is no longer detached. Each start registers its handle under the
  same lock that counted the thread, so a shutdown can never observe a thread
  it cannot join.
- **A start now joins the threads that have already returned**, so a process
  whose load is bursty — a burst, an idle period, a burst — no longer keeps
  one handle per exited thread for ever. What the handle list holds is exactly
  `live + (threads that have left the accounting and not yet returned)`: every
  entry beyond `live` is a real thread still executing its last instructions,
  and that second group has no constant bound, because a departure frees its
  slot immediately. What is bounded is the accumulation: every thread that
  *has* returned is joined at the next start or at a shutdown.
- The pool is an `Arc`, so a thread owns its own reference and a caller can own
  and drop a pool; the tests' last `Box::leak` is gone.
- Two seeded simulation families cover the new paths: `sim_pool` drives the
  accounting through configure attempts that move nothing and a shutdown
  closing admission mid-schedule (2,000 seeds × 2,000 steps), and
  `sim_pool_lifetime` drives real OS threads through burst drains, an expired
  deadline, a parked thread, a refused ceiling, and burst/idle cycles that
  would grow the handle list if nothing reaped it. A process-owning test
  binary exercises the two public functions against the real process-wide
  pool, because a shutdown closes admission for the life of its process.

### lgwks_std — `random` reaches every target its backend does, and says why it failed (#276)

- `random` no longer refuses to compile on every target but Linux, macOS and
  Windows. The three-target `compile_error!` was a narrower claim than the
  backend it wraps, so it refused FreeBSD, the other BSDs, illumos, Solaris,
  Android, iOS, `wasm32-wasip1` and every other target `getrandom` already
  supported. The backend is the one authority on where an entropy source
  exists, so its own refusal is now the single compile-time gate and this crate
  holds no target list that could drift narrower.
- `EntropyError` carries the cause as data instead of a `String`: a
  `#[non_exhaustive]` `EntropyErrorKind`, `raw_os_error()` for the OS's own
  code, and `io_error_kind()` for its portable `std::io` classification. A UEFI
  status wider than `i32` is dropped rather than truncated into a code naming a
  different failure. Additive for 1.x: `backend()` and the `Display` rendering
  are unchanged, and the `String` was never in the public surface.
- `fill_bytes` documents that a refused draw leaves the buffer **unspecified**
  and must not be read; `bytes` has no such window, since a failed draw returns
  no array. A test drives the refused path through a crate-private seam, because
  `getrandom` offers no constructor for a backend failure carrying an OS code
  and an integration test cannot reach a private seam.
- New `tests/it/sim_random_error.rs`: seeded sweeps over draw lengths from empty
  to a mebibyte, concurrent drawers at 100, 1 000, 10 000 and 100 000, the UUID
  version and variant masks, and a seeded generator that must not be able to
  predict a draw.

### The gate: every `lgwks_std` feature alone, and the declared target matrix (#276)

- The `feat-std` lane now builds all twelve non-`full` features, each alone.
  `core`, `trace`, `random`, `ron` and `process` were built by nothing except
  the rustdoc lane, so a feature that quietly depended on a second feature was
  invisible to every build receipt.
- New required lanes `tests-std-per-feature-a`, `-b` and `-c`: one
  `cargo nextest run --no-default-features --features <f>` per feature, because
  a build receipt is not an execution receipt. Three shards of four features,
  each its own CI job: the twelve runs as one step took 228 s on GitHub and put
  the run at 307 s, past the five-minute budget.
- New required lane `target-matrix`, running the new
  `scripts/check-target-matrix.sh`. It installs each declared target with
  `rustup` and checks `lgwks_std`, `lgwks_ast` and `lgwks_deps` against it: the
  Rust-only surface is required everywhere, and a check that needs a C
  toolchain the runner lacks is recorded with its exact error and a named
  reason instead of being dropped. A failure that is not that reason fails the
  lane. It runs as its own CI job rather than on the critical path.
- Measured on aarch64-apple-darwin: 42 checks, 69 s from an empty target
  directory. `lgwks_std --features random` builds on all seven cross targets;
  nine `full`/`lgwks_ast` checks are exempt for a missing C cross-compiler.

### The gate: the saturation tiers spawn the fake the way a default binding does (#272)

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

## [lgwks_deps 2.0.0] - 2026-10-05

### lgwks_deps — the gate compiles in no repository's policy (breaking)

`lgwks_deps` is published and audits other repositories, but it compiled in
this repository's licence set (`ACCEPTED_LICENSES`), its five surface names and
URL (`SURFACES`, `SURFACE_REPOSITORY`), its frozen surface and tier
(`FROZEN_SURFACES`, `FROZEN_TIER`), and a maintainer's e-mail address in the
crates.io `User-Agent`. No consumer could change any of them, so the estate's
own MPL-2.0 `lgwks_bot` was refused in every repository that ran the gate.
All of it now comes from the register's `[policy]` (INV-DEP-16).

- **Removed** `lgwks_deps::accepted_licenses()`: the accepted set is a register
  decision, read back with `Contract::accepted_licenses()`.
- **Added** `[policy]` keys `accepted_licenses`, `surfaces`, `frozen_surfaces`
  and `frozen_tier`, each a comma-separated string refused whole on an empty,
  repeated or malformed member; `Refusal::LicensePolicyUndeclared`;
  `ContractError::IncompletePolicy` (a freeze written without its tier, or the
  reverse); the `check --json` receipt's `contract.accepted_licenses`.
- **Changed**: a register that approves an observed edge and declares no
  `accepted_licenses` is refused once, naming the line to add, rather than
  judged by a set compiled into the gate. A register that declares no
  `surfaces` or freeze binds none, which is what every repository other than
  this one already got.
- **Fixed**: the `freshness` crates.io lookup's `User-Agent` is the tool's name,
  its real version and its repository, read from the manifest (it said `0.1`
  and carried a personal address).
- **Migration**: add one line to `[policy]`, e.g.
  `accepted_licenses = "MIT, Apache-2.0, MPL-2.0"`. This repository's register
  declares exactly the set the gate used to compile in, plus its surfaces and
  `lgwks_ast`'s freeze, so its own verdict is unchanged.

## [lgwks_std 1.1.0 / lgwks_deps 1.1.0 / lgwks_macros 1.1.0 / lgwks_bot 1.1.0] - 2026-10-05

`lgwks_ast` stays at 1.0.0: its source is unchanged since that tag. Every
other change below is additive, and every enum that gained a variant is
`#[non_exhaustive]`. The exception is `lgwks_macros`: a script that relied on
a refusal now added (#265) no longer compiles. That is a correction to the
guard, recorded here as a minor bump rather than hidden in a patch.

### lgwks_std — Digest equality measured constant-time (#275)

- `Digest`'s `==` now delegates to `blake3::Hash`'s equality, the
  `constant_time_eq` routine behind an optimisation barrier, instead of a
  hand-written XOR/OR fold that the compiler was free to turn into an
  early exit. No new dependency: `constant_time_eq` was already in the graph
  through `blake3`.
- New `Digest::ct_eq`, the same comparison by name, for checks against a
  secret-derived or adversary-written value. `lgwks_bot`'s `verify_chain`
  compares recorded chain heads through it.
- `Digest`'s `Ord`/`PartialOrd` are documented as variable-time, for
  collections only. They are kept, because removing them would break 1.x.
- `examples/digest_timing.rs` is a dudect-style Welch's t test over two
  classes at 1,000,000 samples each, with an early-exit negative control it
  must detect. The `digest-timing` gate lane runs it on the release build,
  locally (aarch64-apple-darwin) and in CI (x86_64-unknown-linux-gnu).

### One process-group backend (#263)

- `lgwks_std::process::process_group_exists` (feature `process`) is the
  signal-zero group probe, on rustix beside `kill_process_group`, so the
  supervisor's kill and the check that confirms it share one syscall binding
  and one error mapping. Zero and negative ids are refused as `InvalidInput`;
  off Unix it reports `Unsupported`.
- `lgwks_deps::process_group::exists` is deprecated and forwards to it. The
  `process-group-probe` feature now enables `lgwks_std/process` and no longer
  pulls in `nix`. That edge is withdrawn from `contract/APPROVED.toml`, and
  the workspace no longer builds `nix`.
- `lgwks_bot`'s `process` feature no longer enables
  `lgwks_deps/process-group-probe`.

### lgwks_bot — fan-out, journal status, process stdio (#257, #259, #260)

- `script::FanOut` and `script::FanOutError`: a one-call fan-out over `script::each`, bounded
  by `at_most(limit)` and `within(deadline)`, whose error names the failing
  item or the timeout.
- `journal::AttemptStatus::VerificationFailed`: a failed verification
  recovers as its own status instead of collapsing into `Applied` (#257).
  `Verified` is no longer terminal, so a later verdict revises it (#260).
  Recovered status exposes the latest verification and a per-attempt history
  of `Transition`s, each with its position in the journal (#259).
- `rt::process::ProcessSpec::stdout_to_file` and `stderr_to_file`.
- `rt::runtime::Builder::thread_stack_size`, bounded by
  `MAX_THREAD_STACK_SIZE` (256 MiB).

### lgwks_deps — attestation bound to the tree under review (#258)

- `invariants::Status::Attested` is refused unless the recorded revision is
  in the history of `HEAD` and the enforcer is unchanged since then. The
  refusal is `InvariantError::EvidenceNotBound`, carrying an `EvidenceGap`.
  Before this, any well-formed hex revision certified any commit.

### Property tests with shrinking (#273)

- `proptest` 1.11 is admitted as a dev-only edge (`contract/APPROVED.toml`,
  default features off). No shipped artifact links it.
- New property targets, each from one fixed seed with failures persisted
  under `proptest-regressions/`, and each paired with a mutant it must catch
  and shrink: `lgwks_std` `prop_codecs` (hex, base64, percent, leb128,
  RFC 3339, wire, and glob against a regex oracle); `lgwks_bot` `prop_journal`
  (`recover()` over arbitrary proposal histories, refused appends, reopen)
  and `prop_each` (order, in-flight bound, fail-fast); `lgwks_deps`
  `prop_parsers` (register and lockfile round trips, duplicate keys); and
  `lgwks_macros`' line splitter and indentation tree.
- `lgwks_deps::lock::parse` refused nothing when a `[[package]]` block
  assigned `name`, `version`, `source` or `checksum` twice: the last
  assignment won. It now refuses with the new `LockError::DuplicateKey`, at
  the line of the second assignment.

### lgwks_std — bounded blocking pool (#264)

- `task::spawn_blocking` runs on one process-wide pool of at most 512 threads
  instead of one new OS thread per call. A job submitted while every thread is
  busy waits its turn; a thread idle for ten seconds exits. Its signature and
  its never-refuses contract are unchanged. Jobs that wait on each other must
  now number fewer than 512.
- New `task::try_spawn_blocking` and `task::SpawnError`: the same pool with a
  wait queue bounded at 16,384, refusing past it as `SpawnError::AtCapacity`,
  and as `SpawnError::Os` when no thread can be started. A refused job never
  runs. Before this, an OS refusal to start a thread surfaced only as a panic.
- `task::join_all` gives each child its own waker: a wake re-polls only the
  child that woke, so `n` children waking `k` times cost `n·(k+1)` polls
  rather than a scan of every pending child per wake.
- A job that starts a pool thread is handed to it directly instead of through
  the queue, so under the ceiling a job starts as fast as a thread of its own
  (p50 11 µs, p99 20–60 µs at 500 concurrent jobs, against 10–11 µs and
  19–20 µs before the pool). Past the ceiling a job waits for a thread: at
  10,000 × 20 ms jobs, p50 238 ms and p99 484 ms to start.
- The pool's accounting (queued, live, idle, claimed wakeups) is a set of
  transitions a seeded simulation drives through every interleaving of
  submit, thread start and start failure, lost notify, spurious wake and
  keep-alive expiry: every admitted job runs exactly once, a refused one
  never, and the pool drains to nothing.

### lgwks_bot

- `domain::net::Endpoint`'s poll uses `try_spawn_blocking`, so a burst of
  concurrent polls cannot start a thread per poll; a refusal is a
  `DomainError` with `DispatchCertainty::Refused`. Ten thousand concurrent
  polls against a full pool are all refused this way and send nothing; once
  the pool frees, the same ten thousand all run.

### lgwks_macros — refusals by path, and lints the consumer enforces (#265)

- `script!` now refuses `unwrap`/`expect`/`unwrap_err`/`expect_err` called by
  path (`Option::unwrap(x)`) as well as by method; every `assert*!` and
  `debug_assert*!`; `process::exit`, `process::abort` and `mem::forget`;
  indexing and slicing; a `use` inside a flow that renames any refused call;
  and a machine path anywhere in a string literal. Each refusal names its
  replacement. A script that relied on any of these no longer compiles.
- Every generated flow carries `#[forbid(..)]` on `unsafe_code` and on the
  clippy lints for the same defects, after the author's own attributes, so a
  name imported outside the script is refused by the consumer's `cargo clippy`
  and an `#[allow]` above a flow cannot lower it.

## [lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0] - 2026-10-04

### 1.0.0 — lgwks_std, lgwks_bot, lgwks_ast, lgwks_deps, lgwks_macros

First stable cut: from here the public API follows semantic versioning (a
breaking change takes a major version). No public item changes relative to
`lgwks_std 0.10.0`, `lgwks_bot 0.8.0`, `lgwks_ast 0.4.0`, `lgwks_deps 0.4.0` and
`lgwks_macros 0.1.2`, with these `lgwks_deps` changes since 0.4.0: new
`metadata::read_with_members` and `Refusal::UnknownSurface` (a workspace member
or approval owner outside the five INV-DEP-1 surfaces is refused by name, #207);
`Refusal::VendorTierConflict` (#210) and `Refusal::LicenseNotAccepted` (#208);
a register entry now requires a `license` key holding an SPDX expression, so a
register written for 0.4.0 must add it; a `Cargo.lock` with a nameless package
block or no packages is refused (#209). `vendor/` is present but not configured
as a Cargo source.

### Benchmarks

- `bench/ai-authoring`: five fixed user profiles and a third held-out task.
  `--profiles` takes `first-time`, `expert-hurry`, `anxious`, `misuser` and
  `agent` and defaults to all five; each is a *fixed* persona prepended to the
  same prompt skeleton, so the API sheet, the hidden oracle, the repair budget
  and the `sandbox-exec` closed-book profile are identical across profiles and
  the profile is the only thing that differs between two cells of one
  `(model, api, task)`. `profile` is recorded in every `results.jsonl` line and
  `summary.json` groups by `(model, api, task, profile)`.
  The new task, `recovery`, needs host setup, a derived run identity, a task
  helper and recovery together; its oracle has one test per clause and its
  mutant fails `a_resume_does_not_rerun_a_completed_unit` and only that clause.
  Three per-trial metrics are added and every existing one is kept:
  `oracle_wall_ms` and `oracle_peak_rss_bytes`, both taken under
  `/usr/bin/time -l` (the RSS parser reads macOS's value-before-label form and
  GNU's kibibyte form), and `cleanup_ok`, which reads the task's drop clause and
  reports `false` for a crate that never compiled.
  **This is not a measurement of human authorability and no person was
  consulted.** The README's earlier claim that authorability "needs the
  Director" is replaced by a statement of what the profile axis does and does
  not reach. `recovery` has no old-API cell, and the reason is recorded in each
  run's `protocol.skipped_cells`: the old `rt` surface has no durable run store,
  no `remember`, no run identity and no resume, so it cannot express the task.

### lgwks_bot Breaking

- `task::StoreError`: a new `FormatVersion { found, expected }` variant, and the
  run store's file format version moves from `\x01` to `\x02` (T15). **Migration:**
  opening a `\x01` run store is refused with `StoreError::FormatVersion { found:
  1, expected: 2 }`, which names both versions, instead of the generic
  `StoreError::NotAStore` it previously produced. The refusal is typed rather
  than generic on purpose: `NotAStore` asserts the bytes were never this store's,
  and a `\x01` store was written by an earlier version of this very crate, so
  that message would tell an operator their own data was never theirs — the one
  conclusion a refusal must never produce, because it is what makes someone
  delete a file a system still relies on. There is no migration, deliberately:
  the two available readings are to invent a `DefinitionIdentity` those records
  never carried, which would make every pre-version resume look exactly
  compatible, or to discard acknowledged evidence. The format has never shipped a
  version that could lose a record, so there is nothing to convert. A deployment
  that needs its records keeps its own copy and re-runs. A file whose magic does
  not match is still `NotAStore`, and the two refusals are asserted apart by
  `tests/task_resume.rs::a_pre_version_store_is_refused_naming_both_versions` and
  `::a_foreign_file_is_still_refused_as_not_a_store`.

### lgwks_bot Fixed

- `tests/sim_repair.rs`: seven repair arms no longer reopen a store an earlier run
  on the same host left behind. The tests built their store directory under the
  system temp root from a *fixed* name (`lgwks-first-step`,
  `lgwks-reopened-repair`, `lgwks-repair-recovery`,
  `lgwks-cross-tenant-ticket`, and the `-{reach}`/`-{tier}` families), so the
  directory was a ledger the test did not own: a run that predated the `\x02`
  format left records there, and every later run refused to open them with
  `StoreError::FormatVersion { found: 1, expected: 2 }` — a refusal that named a
  version this branch has never written. Each now takes its directory from the
  `shared::Scratch` guard already shared with `tests/repair.rs` (random hex in the
  name, removed when the guard drops), held for the whole test, so no run can see
  another run's files and none leaves one behind (INV-BOT-116). This is a test
  isolation and ephemerality fix, not a format change: `check_format_version` is
  untouched, and a `\x01` store written inside a run's *own* scratch directory is
  still refused by `tests/task_resume.rs::a_pre_version_store_is_refused_naming_both_versions`.
- Merge resolution against `#240` (group commit): the run store's ordered append
  keeps **both** sides' guarantees rather than either one wholesale. The two-phase
  stage is main's — every queued request's checks, fence, framing and `write_all`
  run in submission order, one `sync_all` covers the batch, and only then are the
  answers published and the records folded — and the record/index fold additionally
  carries this branch's `DefinitionIdentity`, so a run's records still name the
  definition they were written under and `check_format_version`/`FlowError::Store`
  are unchanged. `Awaiting::poll` keeps main's ordering exactly: register the waker
  *before* reading the slot, or a publish landing between the two is a lost wakeup
  (INV-BOT-140). Both `journal::owner` tests from `#240` are kept.

### lgwks_bot Added

- `EcsBuilder::with_poll_deadline`, `DEFAULT_POLL_DEADLINE` and
  `MAX_POLL_DEADLINE` (INV-BOT-123). Every source poll in a tick's observation
  wave now runs under a declared per-poll deadline, so one source that never
  resolves can no longer hold the whole tick. `MAX_IN_FLIGHT_POLLS` bounded the
  wave's fan-out but not its wait, and the wave is joined on the calling thread,
  so every other chain's action was held behind a source nobody could make
  progress for. A poll that misses its deadline is dropped mid-flight: it commits
  nothing, keeps its chain's baseline and forced-refresh mark standing, and is
  reported in `TickReport::stalled` with the chain, the source's `domain_id` and
  the budget applied. The chains beside it commit and act in the same tick.
  The deadline is measured on the wall watchdog of the crate's one declared
  clock, not its caller-advanceable counter, because a wedged source is not
  waiting for time; it is a watchdog thread the poll owns and joins on every
  path, because `Bot::tick` is `lgwks_std::task::block_on` and has no reactor for
  a timer. Zero and over-ceiling deadlines are refused at build rather than
  clamped. **Migration:** none; the default is applied to every existing bot.
  `clock` moved from `rt::clock` to the crate root (re-exported unchanged under
  `rt`), so a `--no-default-features` build can bound its sources too.
- `BotError::{PollStalled, PollDeadlineUnbounded, PollDeadlineExceeded}` and
  `StalledSource`. A cancelled poll is `NotDelivered`, so a retry classifier
  reads the next tick as a plain retry. **Migration:** a caller matching
  `BotError` exhaustively must handle the three new arms; `BotError` is
  `#[non_exhaustive]`, so an existing match already compiles with a wildcard.
- `verb::RefreshReason` and `Observe::cache_state` (INV-BOT-120). A source that
  caches can now declare that its cached baseline is unsound, naming which of
  four failures it was: `Disconnected`, `WatchOverflow`, `StaleRemoteKey` or
  `InvalidationFailed`. Before this the substrate's shortcut — do not re-poll a
  source whose value compares equal — assumed a baseline was sound, and in all
  four cases "unchanged" and "I stopped looking" are the same observation, so the
  bot settled into a permanent quiet state whose only symptom was zero fired
  effects. The reason is read from the source itself after its poll resolves and
  never guessed, because the substrate cannot know whether somebody else's
  transport is up. **Migration:** none. `cache_state` defaults to `None`, so
  every existing `Observe` impl keeps compiling and keeps its current behaviour;
  an existing source that overrides the deprecated `fingerprint` should move its
  state to `cache_state`, since that method no longer suppresses anything.
- `TickReport` and `Bot::tick_report` (INV-BOT-120, INV-BOT-121). What the last
  tick observed about its own sources, beside the count of effects it fired:
  `forced` names the chains whose baseline the tick refused and re-read and the
  cause each source declared, `superseded` names the observations replaced before
  any entry acted on them. The count says what ran; this says what had to be
  re-read to decide, and a quiet bot is exactly where only the first is
  uninformative. Published before the schedule step runs, so a tick that failed
  still reports what its sources had already declared. Every field is private
  behind an accessor. **Migration:** none; reading it is optional.
- `TickReport::watchdogs` (INV-BOT-124). The deadline watchdog threads the tick
  actually started, beside `stalled`: zero for the ordinary tick where every
  source answered on its first poll, and one per observation wave that had a
  source still pending. **Migration:** none; it is an accessor.
- Source-visible simulation evidence for the observation layer's deadline,
  watchdog, refresh and attribution rows (INV-BOT-120..124). Fourteen seeded
  properties in `tests/sim_observe_refresh.rs` drive the public
  `Bot`/`TickReport` surface over wave widths from 1 to 64: an ordinary wave
  starts no watchdog while a wave with a poll that yielded once starts exactly
  one, and the count over a run equals the number of ticks with a pending wave;
  a tick dropped mid-wave leaves the bot usable; a wedged chain keeps its
  baseline and forced-refresh mark and is re-polled next tick; a per-poll
  deadline is accepted at both edges and refused one step past the ceiling; each
  chain's own declared cause, each replaced unacted revision and every stall is
  reported against the right chain; a mass of 100/1,000/10,000 held chains never
  starves an independent chain; and two tenants never cross attribution. No
  production code changed.
- `task::DefinitionIdentity` and `task::Drift`: a recorded step value is only
  replayable under the definition that produced it. Every run-store record now
  carries the task name, a declared definition revision, the input digest, a
  declared durable-value schema id and the count of durable steps (T15).
  **Migration:** the run store's file format version moves from `\x01` to
  `\x02` and a `\x01` store is refused at open as a `FormatVersion` naming both
  versions rather than migrated — reading one as the unversioned identity would
  make every pre-version resume look compatible rather than unprovable. See the
  **lgwks_bot Breaking** section above for the migration note and the refusal's
  shape.
- `Host::run_under`, `Host::resume_under`, `Host::definition` and
  `HostBuilder::durable_codec`: the doors that carry a declared definition
  identity. `resume_under` returns a `Disposition::Refused` report carrying
  `FlowError::Incompatible` — naming the axis that disagrees — before admission,
  so no step body is polled and no record is written. Only a *declared* identity
  is compared; a run that declares none gets an identity derived from its tenant,
  task name and run id, so every durable step that already worked still records
  something stable and still resumes. The declaration is an addition, never a
  precondition.
- `Broker::adopt`: takes ownership of an environment at the generation a journal
  already on the disk was written at, and moves past it (T16). `Broker::register`
  starts at generation 1 whatever the journal holds, so a process adopting
  another worker's journal would mint warrants for a generation that worker had
  been replaced past — internally consistent and jointly wrong. The new refusals
  are `BrokerError::{Journal, ForeignEnvironment, NothingToAdopt}`.
- `script::FlowError::Store`: a typed arm carrying the run store's own
  `StoreError`. A store that cannot read its own records now reaches the caller
  as itself rather than as a `Failed` reason string, so a device refusal is
  distinguishable from a definition drift by the variant alone (INV-BOT-7,
  INV-BOT-59). `FlowError` is `#[non_exhaustive]`, so this is an additive minor
  change.
- `rt::process::CapturedStream::frames(ceiling)`, the door a caller reads its own
  child's output through: infallible, and the reason a `ProcessRun`'s captured
  stdout can be read as frames without the caller re-plumbing the bytes into a
  reader. It is `pub` because callers outside the crate read their child's
  output through it, and it is the path the T05 tests exercise rather than a
  hand-plumbed slice.
- `rt::process::read_frames`, a bounded reader for the length-framed records a
  supervised child's output carries. It reuses the crate's existing frame
  grammar (`journal::frame`) rather than restating it, so a torn tail has one
  meaning across the file stores and a subprocess's streams. A record is
  `FrameRead::Frame` only when its prefix named the bytes that followed; the two
  truncations, a malformed prefix and the caller's ceiling are refusals that
  carry no payload, and `FrameRead::payload()` returns `None` for every one of
  them. The payload ceiling is charged from the prefix before a payload is read,
  so a stream cannot request an allocation by claiming a large record. Accepting
  rows T03, T05, T21 and T22. No existing item changed.
- `rt::process::DEFAULT_FRAME_CEILING`, the one retained-byte ceiling a caller
  needs in order to read a child's framed output without inventing a bound.
- `domain::sys::Process::frame_stdout(ceiling)` and
  `ProcessState::stdout_frames()`, which wire the frame reader above into the
  sys domain's real verb path: a `Process` built with `frame_stdout` reports each
  run's stdout as a framed reading on the `ProcessState` its `Observe`, `Execute`
  and `Query` calls return, byte-exact where the lossy `stdout()` is not, and a
  domain built without it reports `None` and an unchanged `stdout()`.
  `CapturedStream::frames`/`read_frames` (INV-BOT-110/114) previously had no
  production caller; the domain's verbs are now that caller (INV-BOT-115).

### lgwks_bot Tests

- `tests/sim_store_faults.rs` and `tests/sim_epoch_identity.rs`: seeded
  deterministic families for this branch's T15/T16/T12 claims, each a
  source-visible `#[test]` driving the real path through the `sim::assert_replays`
  band pattern. The read-fault family arms `RunStore::fail_next_index_read` at
  drawn store shapes and replays and asserts the refusal reaches the report as
  `FlowError::Store` carrying the store's own `StoreError::Storage` while a
  reopen recovers (INV-BOT-59); the version family re-stamps a real store with a
  drawn version byte and asserts only `\x02` is admitted (INV-BOT-55); the
  takeover family sweeps open/takeover/append orders and asserts adoption claims
  the generation after the journal's own history (INV-BOT-56); and the identity
  family draws one of the seven fields per seed and asserts each is refused by
  the check that is about it (INV-BOT-58). Two tenants are swept in the store
  family; the concurrency rows stay with the pre-existing families
  (`sim_store_scale.rs`, `sim_task_resume.rs`).

### lgwks_bot Fixed

- The per-poll deadline no longer spawns and joins one OS thread per source
  poll, per tick (INV-BOT-124). The watchdog is now one per observation wave
  (`MAX_IN_FLIGHT_POLLS` chains) and starts lazily, only when the first poll in
  the wave returns `Pending`; an ordinary wave whose every source answers on its
  first poll starts no thread at all. The reaper is joined on every path and its
  spawn is serialized, so a poll can never park against a thread that never
  started. Measured release, 200 ticks per configuration
  (`examples/poll_deadline_cost.rs`): ordinary-tick p50/p95/p99 in µs, AFTER
  1/32/1,000/10,000 chains = 1/3/5, 9/9/12, 158/191/218, 1082/1120/1156 with 0
  watchdogs at every tier, against BEFORE (`bad47c6d`) 30/37/48, 611/959/1047,
  19171/19421/19512, 160646/191745/193909; 1,000 chains with one wedged source
  under a 100 ms budget costs p50 104.7 ms (about one deadline, not one per
  chain) with one watchdog per tick, against 127.9 ms before.
- An intermediate observation overtaken before any entry acted on it is now
  reported rather than vanishing between "fired" and "retired" (INV-BOT-121).
  Which chain's committed value had been admitted into a generation was a
  boolean, so "never observed" and "observed and overtaken" read the same — and
  every chain's first commit was reported as a skip. It is three states now, and
  admission is marked where a generation *takes* the value rather than where the
  transition is handed back, which for an entry awaiting evidence is never.
- The T15 drift check compared a run's records against themselves: `Host::execute`
  read the identity it was going to *check* from the same lookup its steps use to
  find their records, so every definition drift passed and every drifted resume
  succeeded. It now compares the identity the caller declared against the one
  recorded. The unit tests around `DefinitionIdentity` did not catch this — they
  tested the identity type, not the check's placement — and
  `tests/sim_replay_drift.rs` did, which is why the row now has a seeded sweep.
- A run store read failure is an error, never a disagreement (INV-BOT-7,
  INV-BOT-59). The step's compatibility check returned `false` on a store that
  could not read its own index, and that `false` was rendered as "recorded under
  a different definition" — a specific, actionable claim about a definition made
  by a device that established nothing. `Records::agrees` now returns the store's
  own typed error unchanged, the durable step refuses as `FlowError::Store`
  carrying the store's `StoreError`, and the host's admission pre-flight refuses
  every non-drift store error the same way instead of admitting the run and
  letting the step discover it. Asserted by `tests/store_read_failure.rs` and the
  `an_unreadable_store_is_refused_as_itself` family of
  `tests/sim_replay_drift.rs`.
- The drift refusal now carries the exact typed `Drift` (R2): `FlowError::Incompatible`
  names for each axis the two revisions, input digests, durable-step counts or
  schema ids that disagreed, so a caller learns which axis moved and against what
  rather than having to parse a rendered sentence. Asserted by the
  `every_axis_is_refused_with_its_exact_drift` family of
  `tests/sim_replay_drift.rs`, which destructures the `Drift` for all four axes
  on a real `Host::resume_under` over a reopened file store.
- `rt::process`: a capture's own cut is no longer reported as the child's
  truncation. When `CapturedStream::truncated()` is true the retained bytes are a
  prefix **the capture** cut, so a framed read of them could end in
  `TruncatedPrefix`/`TruncatedPayload` — or, worse, read as a clean
  `EndOfStream` when the cut landed on a record boundary — and a caller would
  take a capture's bound for the child's own failure to write.
  `CapturedStream::frames` now overrides exactly those three endings with
  `FrameRead::CeilingReached { ceiling: <the capture's retained capacity> }` and
  `is_complete()` is `false`. An untruncated capture still reports the child's own
  truncation, and a reader's ceiling reached over an untruncated capture still
  reports the reader's; the two ceilings are separate facts and are no longer
  conflated.
- `rt::process`: the capture-ceiling override above no longer overwrites the two
  endings it had no business touching. A `MalformedPrefix` was decided from a
  whole prefix the capture *did* retain — declared `0`, or past the reader's
  ceiling — so it is rot in the child's output and stands. A `CeilingReached`
  the *reader* reached stopped the pass before the cut mattered, so it keeps the
  reader's ceiling. Replacing either was fail-open: a caller looking for
  corruption was handed a bound it never hit, and a caller looking for its own
  bound was told something larger stopped it.
- `rt::process`: `Frames::of_slice`'s unreachable `Err` arm fails closed. It
  returned an empty `EndOfStream` — a *complete* reading, from a pass that
  stopped without one. It now keeps the whole records and retained payload bytes
  the pass had read and ends in `CeilingReached { ceiling: retained_bytes }`,
  which is not complete. The arm is unreachable by construction (a byte slice's
  reads cannot fail), so no test exercises it.
- `rt::process`: `FrameRead::MalformedPrefix` no longer claims a legal record is
  rot. A declared length of `0` or past the ceiling still names no record this
  grammar writes and is still refused, but a legal declared length that merely
  exceeds the room remaining after earlier records is now
  `FrameRead::CeilingReached` — a well-formed record with nowhere to go is a
  bound, not corruption. The charge is still made before a payload byte is read,
  so no allocation past the ceiling is possible.
- `rt::process`: a record's payload is read into its own exactly-sized `Vec`,
  allocated only after the ceiling charge and then moved into the record. The
  shared "reused" buffer this replaces allocated and copied every payload a
  second time, so every byte was copied twice and the reuse comment was untrue;
  on truncation the partial is that same `Vec` truncated in place rather than a
  second copy.

- A host-side stop is no longer recorded as a request's outcome. `Host::submit`
  wrote a `@terminal` record for every disposition that was not a success, so
  `Cancelled` (the host's stop arrived mid-run) and `Refused` (the host declined
  before admission) became the request's permanent recorded verdict. Since the
  key *is* the request's identity and its body runs at most once under it, one
  shutdown left a key that no later submission could ever complete: every repeat
  reattached to the stop and the body's recorded durable steps were never
  resumed. `@terminal` is now written for exactly the three dispositions that
  are the request's own verdict under its declared task — `Succeeded`, `Failed`
  and `DeadlineExceeded`, the deadline included because the same declaration
  that fixed the key also fixed the run's budget — and a host stop writes
  nothing. The classification is one exhaustive `match` over `Disposition`, so
  a variant added later fails to compile until its relationship to a request
  key is decided by hand. The stop is still reported to the caller that saw it
  (`Submission::Executed` carrying the disposition); what no longer happens is a
  restart turning into a request that can never succeed. INV-BOT-102.
- `Host::resume` settles a request that `Host::submit` left incomplete, under
  the same rule: a resumed run whose receipt exists and whose verdict does not
  records that verdict, so a request interrupted by a host stop (or by a client
  that walked away) can reach a recorded terminal at all instead of reporting
  `InFlight` forever. Settling is deliberately narrow — `Host::run` writes no
  reserved record, a resume of a run with no `@request` receipt is an ordinary
  resume, an already-settled run is left alone, and a `Refused`/`Cancelled`
  report is returned untouched, so a cross-tenant resume stays `Refused`. A
  store that refuses to record a verdict a run *reached* is reported as
  `Failed`, since recording an outcome and reporting success are one fact.
- **Breaking:** `Host::resume` and `Host::resume_ticket` now require
  `O: lgwks_bot::script::Durable`. A resumed run may be settling a request, and
  a recorded verdict is made of an archived output. `Host::run` is unchanged.
  **Migration:** a caller whose task returns a value that is not archivable must
  return a `Durable` one, or route the run through `Host::run` plus an explicit
  run id it owns rather than a resume.
- `Host::repair` settles a request too. A request `Host::submit` started that
  blocked on authority records no verdict (`Blocked` is not the request's
  outcome), and the repair is the attempt that reaches one — but it did not
  record it, so a key repaired to success still answered `InFlight` to every
  later submission until a separate `Host::resume` ran. The repair now settles
  under the same rule as `resume`, and the key reattaches, from a reopened store
  as well (`tests/request_key.rs::a_repaired_request_is_settled_and_reattaches`).
- **Breaking:** `Host::repair` now requires `O: lgwks_bot::script::Durable`, for
  the reason `Host::resume` does. **Migration:** as for `Host::resume`.

### lgwks_bot Changed

- `rt::supervise::CleanupReceipt`'s documentation now states what
  `CleanupConfirmed` does and does not claim. It claims that every process still
  *in the supervised group* when the group was last observed is gone — an
  observation of `killpg(group, 0)`. It does not claim that no process the
  supervisor started is still running: a descendant that called `setsid` has
  left the group by construction, so its survival is not a counterexample.
  Nothing about the type or its variants changed; a caller needing the stronger
  guarantee needs a kernel job object or a cgroup, which this crate does not
  have. Accepting row T21.

### lgwks_std Breaking

- `similarity`: the `Similarity` implementation for `Cosine` now returns the
  normalized `(raw + 1) / 2` score instead of raw cosine, so the trait's
  documented `[0.0, 1.0]` interval holds for every implementation behind it
  (#160 S1). **Migration:** a caller reading `Similarity::score` for a cosine
  comparison and comparing against a raw-cosine threshold must call
  `Cosine::try_score`, whose `[-1.0, 1.0]` domain is unchanged. The named
  mapping is `Cosine::normalized_score`.
- `similarity`: `ComponentOutcome` and `EvidenceVerdict` expose accessors
  (`index`, `weight`, `score`, `reason`, `outcomes`) instead of public fields
  (#160 S2). **Migration:** replace `verdict.score` with `verdict.score()` and
  `outcome.index` with `outcome.index()`. The fields are private because an
  outcome is a report about a measurement; a caller that could set a refused
  component's score would defeat the contract.

### Fixed

- The group-commit failure test is no longer a test that does not test. The
  seeded `tests/sim_group_commit.rs` family named for a failed batch never failed
  one — the flush-failure switch is a `#[cfg(test)]` seam no simulation can reach —
  so it is renamed to what it actually proves,
  `every_flushed_batch_acknowledges_every_member`, and the all-or-nothing answer of
  a *failed* batch is now injected and observed on the shipped store by
  `journal::owner::tests::a_failed_batch_flush_acknowledges_nobody_and_folds_nothing`:
  a three-member batch whose covering `sync_all` is refused answers every member
  with the failure, folds none of them into the handle's index, latches one poison
  against which every later submit is refused, and is read back from a reopen of
  the file rather than from the handle (INV-BOT-130/131).
- `journal::owner`'s ordered step grew the third answer shape `Stage::Committed`,
  for a step that flushes its own bytes inside the step, so the run ledger's charge
  shares the one storage owner — thread, bounded rings and poison latch — with the
  group-committed step store, and `StorageOwner::enqueue_awaiting` returns the
  concrete `Send` future the host's own path needs (INV-BOT-132).
- `lgwks_bot`: `tests/sim_review_path.rs`'s saturation tiers shard each
  saturation tier across receivers of at most 100 runs, so the fixture's
  read-back stays linear. The merged receiver `cat`'d its whole
  `reviews.jsonl` on every read, so 1,000 and 10,000 runs piped ~10 GB and
  pushed most runs past the capture ceiling and the review ceiling, ending them
  `Unknown` while the family's three inequalities still passed — the big tiers
  were timing a degenerate world. Every run now reaches a verified
  `Published`, and creates are asserted `==` runs per receiver and in total
  rather than `<=`. The tiers, the single `Host`, the `join_all_bounded`
  pipeline and its `min(N, 64)` bound are unchanged; measured
  143.202s → see INV-BOT-97 (#151 review finding).
- `lgwks_bot`: the fake `gh`'s review store commits each record with a
  trailing newline, and a read-back keeps only newline-terminated lines. An
  `O_APPEND` write is atomic against other appends but not against a reader:
  on tmpfs (CI's `TMPDIR=/dev/shm`) a concurrent read-back saw the front half
  of another run's record, cut mid-string at byte 8,193, and
  `saturation_r32_tier_10000` failed with one run `Unknown` out of 100 on a
  receiver. `tests/gh_binding.rs::a_record_still_being_appended_is_not_read_back`
  plants a committed record and a torn one and asserts only the first is read.
- `lgwks_bot`: the fake `gh` in `tests/support/fake_gh.rs` no longer forks an
  external helper on the common path. One `gh api` create or read used to fork
  `sed`/`cat`/`tail`/`tr` several times (a saturation family runs five calls per
  review); the behaviour file and the create payload are now cut with shell
  parameter expansion, the receiver's store is read back with the `read`
  builtin and its leading separator dropped with `${store#?}`, and the two
  append-only counters take their byte count with `read` and `${#..}` rather
  than `wc -c`. Measured: one clean run forks 21 helper processes before and 0
  after; a `gh api` create went 9 → 0, a review-list read 2 → 0, a pull-request
  read 4 → 0. The race-free store is unchanged — one `O_APPEND` write with a
  leading separator, the first byte dropped on read, an empty store reading
  `[]` — and every existing assertion is untouched. `saturation_r32` fell
  152.964s → 72.063s on this host (ubuntu CI runs it under `dash`, where the
  removed forks cost more). The family is now one `#[test]` per tier
  (`saturation_r32_tier_100`, `saturation_r32_tier_1000`,
  `saturation_r32_tier_10000`) calling the same `run_saturation_tier`, so
  nextest schedules the tiers alongside the rest of the suite instead of a
  serialized loop: 0.610s / 5.372s / 60.388s sequential, 66.371s combined. No
  tier was dropped, shrunk or `#[ignore]`d.
- `tests/http_alloc.rs` joins every single-shot server thread (warm-up, exact and
  cut) before the next measurement is armed, so a detached server can no longer
  free its `reply` inside a later window and net the eager peak to zero; the
  servers carry bounded read/write timeouts so the join cannot block. No
  assertion, ceiling or bound changed; the probe is deterministic across 30 runs.
- `lgwks_bot`: the storage owner's awaited answer could be written and never
  woken. `Awaiting::poll` read the answer slot and only then registered its
  waker, while the owner writes the slot and only then takes the waker to fire
  it, so a publish landing between the poll's read and its registration found no
  waker and left the awaiting task parked for ever. GitHub CI showed it as
  `tests/sim_repair.rs::saturation_applies_each_ticket_once_band_09` (PR #239)
  and `band_03` (PR #241) parked past 600 s, with the job cancelled at its
  15-minute timeout, and the poll now registers its waker before it reads the
  slot so whichever side moves second observes the other (INV-BOT-140).

### lgwks_std Added

- Seven new deterministic simulation families, 116 source-visible `#[test]`
  functions, over the `lgwks_std` modules that had none: `tests/sim_hex.rs`,
  `tests/sim_encoding.rs`, `tests/sim_id.rs`, `tests/sim_leb128.rs`,
  `tests/sim_hash.rs`, `tests/sim_codec.rs` and `tests/sim_pattern.rs`. Each
  file drives the shipped public API over seeded payloads and checks the answer
  against a reference model written in the test from the documented contract
  rather than against the implementation under test, carries the boundary
  lengths `0/1/2/3/255/4 KiB` (or the module's own), and ends in the
  `same_seed_same_trace_hash` and `distinct_seeds_diverge` oracles from
  `tests/support/seeded_sweep.rs`. **No behaviour change:** every family passed
  against the code as shipped, and where a first draft disagreed with the
  shipped decoder it was the draft that was corrected — the reference model is
  the thing that was wrong, and the corrections are named in the commits. New
  `INV-STD-HASH-1` records the determinism property `hash` was already
  documenting but not pinning.
- `glob`, `similarity` and `retry` now state their sharing contract on the
  types rather than leaving it to inference, and it is checked. `GlobPattern`
  and `RetryPolicy` are documented `Send + Sync`; `CheckedEvidence` is now
  genuinely shareable, because `CheckedEvidence::new` takes
  `Box<dyn CheckedSimilarity<Value = Value> + Send + Sync>` and
  `CheckedSimilarity::Value` is `Sync` (#154 item 7, hyperscale axis).
  **Migration:** a custom `CheckedSimilarity` implementation must now satisfy
  `Send + Sync` (and its `Value` type must be `Sync`) to be installable in a
  `CheckedEvidence`. A stateless `Copy` scorer satisfies both with no code
  change; one that holds interior mutability is refused at compile time rather
  than producing a policy that is thread-safe from the outside and racy inside.
  A custom scorer used single-threaded behind `Weighted` is unaffected.
- `bench/std-measure`, a before/after latency harness for the `lgwks_std` paths
  issues #153, #154, #160 and #164 changed, committed with its raw sample
  output in `bench/std-measure/results.txt`. It reproduces the #154 G2 table
  (`*a*` at n = 256..2048 and the six-token pattern, p50/p95/p99 over raw
  samples) and the #164 retry flat-latency rows, and adds the
  `O(attempt)`-walking backoff the shipped shift-and-compare form replaced so
  the flat-latency claim has something to be flat against. See
  `bench/std-measure/README.md` for the exact command.

- `similarity::CheckedSimilarity`, the checked scoring seam, and
  `similarity::CheckedEvidence`, the authority-facing composition that carries
  component identity, the refusal, and applicability through to the acceptance
  decision (#160 S2). Any refused component withdraws the whole verdict at
  every threshold including `0.0`; surviving weights are not renormalized, and
  all-zero effective evidence is `EvidenceError::InsufficientEvidence`.
- `similarity::EvidenceError`, the typed refusal shared by every checked
  scorer: `DimensionMismatch`, `ZeroMagnitude`, `NonFinite`, `InputTooLong`,
  `CollectionTooLong`, `InsufficientEvidence`, and `Composition` (#160 S2).
- `similarity::{Evidence, ComponentOutcome, EvidenceVerdict}` (#160 S2).
- `similarity::BoundedJaccard`, a set scorer that charges its element budget
  before dedup and before the quadratic scan, so hostile input is refused
  without paying the work it was trying to cause (#160 S4). `Jaccard` remains
  available and is documented as unbounded.
- `similarity::is_exact_path_match`, the exact path comparison that the lossy
  `PathSimilarity` heuristic is not (#160 S4).
- `similarity::EditDistance::normalized_length`, the unit the scorer's budget
  actually charges: the lower-case-expanded scalar count, which differs from the
  raw count because `İ` expands to two (#160 S4).
- `glob::GlobPattern::token_count` and
  `glob::GlobScratch::{scalar_capacity, row_capacity, row_count,
  storage_bytes}`, so a caller can report pattern storage, scalar indexing, and
  rolling rows separately rather than as one RSS figure (#154).

- `tests/fixtures/wire/consumer_record_v1.hex` is a retained archive that pins
  an application schema and the effective rkyv format (pointer width 32,
  little-endian, `u32` alignment 4). `tests/wire_fixture.rs` reads it on every
  target whose format matches and reproduces it byte for byte, so a format or
  schema drift is caught on every OS rather than inferred from the host.
  `tests/wire_feature_unification.rs` builds a separate consumer crate that
  selects `rkyv/pointer_width_16`, `rkyv/big_endian` and `rkyv/unaligned` and
  proves `wire::format_descriptor()` and the emitted bytes move with the
  unified features, and `tests/sim_wire.rs`
  checks 64 seeded nested values for byte-repeatability, round-trip and refusal
  of truncated or misaligned archives. (#167)
- The HTTP body ceiling is measured, not merely asserted (#163).
  `tests/http_alloc.rs` builds a throwaway consumer crate with a counting global
  allocator and reports separately the retained body bytes, retained header
  bytes, the live heap held while a `Response` is alive, the peak heap during a
  call, and an eager reader's peak. At a 3 003-byte ceiling the retained body is
  324 bytes and the peak is ~267 KB whether the body is 3 KB or 3 MB, while an
  eager reader of the same 3 MB reports a 3 MB peak — so the same metric
  discriminates the mutant the ceiling exists to stop. `tests/sim_http.rs`
  checks 48 seeded scripts per family against a model: ceiling outcomes at
  non-power-of-two sizes, a transport torn before its declared length,
  and two tenants polling one endpoint concurrently without header bleed.
  `http::tests::the_read_window_is_a_fixed_chunk_and_capacity_is_clamped_to_the_ceiling`
  records that the body reader is handed one `READ_CHUNK_BYTES` window and that
  retained capacity clamps to the ceiling. No public behaviour changes; the
  tests pin INV-STD-HTTP-2.
- `online::tests::a_whole_probe_fits_one_wall_clock_budget` observes, from
  outside the injected dial, that one probe's resolved candidates share a single
  wall-clock budget rather than restarting per address. (#163)

### lgwks_std Fixed

- `similarity`: a refused component can no longer become an acceptance. The
  infallible `Weighted::is_accepted` still maps a refusal to `0.0` for source
  compatibility and is documented as lossy; `CheckedEvidence::verdict` is the
  path that retains it (#160 S2).
- `similarity`: the `Similarity` implementation for `EditDistance` no longer
  reports two identical over-limit inputs as `0.0` through the trait's identity
  contract when the checked form refuses them (#160 S1).

### Documentation

- **#155/#170 — documentation claims reconciled to the code at this revision.**
  No public API changed; this entry records sentences that described code the
  tree does not contain, or overstated what it does.
  - `journal/file.rs` and `registry.rs` module docs described pre-repair
    behaviour: a journal with no writer exclusion, and a registry where a
    duplicate identifier silently first-wins. Both now describe what ships
    (`FileJournal::open` takes an exclusive advisory lock before scanning and
    refuses a second opener; `validate` and both `build_*` paths refuse
    duplicate identifiers). The journal doc now states the lock's three real
    limits — advisory, lifetime/host-scoped, filesystem-dependent — so it is
    not read as a distributed lease.
  - `docs/production-readiness.md`: 33 grammars → 28; "13 jobs across three
    operating systems" → 20 job definitions with 14 ubuntu / 2 macos / 1
    windows / 1 `matrix.os` (AppCUI), plus the distinction between a *build*
    receipt and an *executed containment* receipt; the lint-ceiling arithmetic
    (25 + 4 + 50 = 79 over a 75-entry corpus) → 69 clippy + 5 rustc + 1
    rustdoc; "the journal grows without bound" → the real
    `MAX_JOURNAL_BYTES` / `MAX_JOURNAL_EVENTS` refusal and why a hard refusal is
    not a long-running-service availability proof.
  - `docs/async-parity.md`: removed a recommendation (`join_all_bounded` /
    `Supervisor::spawn` for non-`Send` futures) that does not compile — both
    require `Send + 'static` — and added §5a separating tracked lifetime,
    cancellation request, actual termination, queue/byte bounds and retained
    authoritative state.
  - `README.md`, `llms.txt`, all four crate READMEs, `CODEBOOK.md`,
    `AGENTS.md`, `GOVERNANCE.md`, `docs/orchestration-acceptance.spec.md` and
    three stale guide version pins: crate count four → five, the
    zero-dependency claim stated at the level it is true at, the real
    dependency graph, the ban list restated as a ban on *new* edges, and T22
    marked as having candidate tests while remaining unaccepted.
  - Added `docs/std-ast-deps-closure-matrix.md`: all twenty `lgwks_std`
    modules, `lgwks_ast` and `lgwks_deps`, each with a state from a fixed
    vocabulary and the test that exists on the named revision. Recorded as
    `INV-DOC-2`. Four rows are marked `assurance-gap` (constant-time hashing,
    fallible thread admission, entropy failure meaning, feature-isolated
    ergonomics) — unproven, not disproven, and deliberately not green.

### lgwks_bot Added

- Group commit on `journal::owner`: concurrent durable appends now share one
  `fsync` instead of paying one each (#152). An ordered step became two phases —
  every queued request's checks, fence, framing and `write_all` run in submission
  order, then **one** `sync_all` covers the whole batch, and only then are the
  answers published and the records folded. No append is acknowledged before the
  flush covering its bytes returns `Ok`; a failed batch acknowledges *none* of its
  members and latches the poison once for all of them, exactly as a failed single
  append did; a dropped waiter still poisons the handle; and the layout order,
  the length fence and the hash chain hold across batch boundaries.
  Measured, release build, same harness and payload as the merge base built in a
  separate target directory: at concurrency 16, 256 appends went from **2.913s
  (88/s, 256 fsyncs)** to **0.184s (1,394/s, 25 fsyncs)** — 15.9x, at 0.09
  fsync per record, with p50/p95/p99 falling from 149,839/396,700/590,351us to
  7,987/47,991/48,101us. A lone append still pays exactly one `sync_all` with no
  linger (16 fsyncs for 16 sequential records), because the owner drains what is
  queued when it wakes rather than waiting for a batch to fill. Bounded and
  declared: `MAX_BATCH_RECORDS` and `MAX_BATCH_BYTES` cap one flush, and a
  caller arriving at a full request ring now *waits* in a second bounded ring
  rather than being refused — which is why a store that previously refused outright
  with `QueueFull` at concurrency 256 now serves 128 concurrent submitters and
  refuses the 129th.
- `RunStore::flush_counts`, the mechanism's own `sync_all` and staged-record
  counters (#152). The batching factor is the one claim in this change that a
  latency difference could only hint at, so it is measured where it happens — on
  the owner thread — and read through the store's public surface. A caller that
  gave up on its answer is still counted as a staged record, so the ratio cannot
  report a better batching factor than the store achieved.
- Two missing arms of the request-keyed submission are now covered rather than
  assumed. `DeadlineExceeded` is the request's own verdict — the same declaration
  that fixed the key also fixed the run's budget — so a request that overran its
  budget reattaches to that recorded deadline instead of re-running a body that
  has already overrun once, and the durable step recorded before the overrun
  survives it. A store that refuses the `@terminal` write during `Host::resume`
  is reported as `Failed` naming both that the outcome went unrecorded and which
  bound refused it, and records **nothing**, so the request stays unsettled and a
  later client reads `InFlight` rather than a verdict that was never written.
  Enforced by `tests/request_key.rs`
  (`an_expired_deadline_is_the_requests_recorded_outcome`,
  `a_store_that_refuses_the_terminal_write_reports_the_refusal`) and by the
  seeded families `expired_deadlines_are_recorded_band_16..23` and
  `settle_refusals_are_reported_and_leave_the_request_unsettled_seed_a..p` in
  `tests/sim_request_key.rs`, two tenants over one store file, each seed swept
  twice for an identical trace hash. No production behaviour changed.
- `Disposition::Blocked` is not the request's own verdict, so a blocked run
  records no `@terminal` — recording it would poison its key exactly as a host
  stop does, and an authorized repair can still move it. It carries a
  `DISPOSITION_BLOCKED` code so `disposition_code` stays an exhaustive match over
  `Disposition`, which is what INV-BOT-102 relies on to catch a later variant.
- `Host::submit`: a request-keyed durable submission. A caller supplies a
  `RequestKey`; the host derives the run identity from the tenant and the key,
  records the input's canonical `InputDigest` under a reserved `@request` record
  before any step runs, and records the terminal outcome under `@terminal`
  after. `Submission::Executed` is a first run, `Submission::Reattached` returns
  the recorded report without re-entering the body, `Submission::InFlight`
  reports a request whose waiter was dropped mid-effect (the run to settle and
  the records that survived, kept apart), and a different input under one key is
  a typed `RequestError::Conflict` naming both digests. A host with no store
  refuses with `RequestError::NoStore`. Supported by `RequestKey`, `InputDigest`,
  `Submission`, `InFlight`, `RequestConflict` and `RequestError` (T30, T17;
  INV-BOT-100, INV-BOT-101).
- `FlowError::InvalidRequestKey`: the typed refusal a malformed `RequestKey`
  names, alongside the tenant and task-name refusals.
- Fourteen seeded simulation families, one per refusal arm of the review
  subject, coverage and partial-submission path (#231, INV-BOT-96). No
  production behaviour changed: `subject_coverage_and_partial_faults` asserted
  that every fault reaches *some* correct outcome variant, which is satisfied by
  a world in which the right arm is reached for the wrong reason, so each arm
  now asks a different question of the same seeded worlds — a coverage refusal
  reaches the receiver with zero creates; the file-count and byte ceilings are
  refused on separate axes, each naming its own bound, with the byte family's
  draws asserted to stay under the file ceiling so a refusal for the wrong bound
  cannot pass as evidence for it; a renamed repository is refused naming both the
  requested and canonical repository; a `build.rs` in the changed-file inventory
  is never executed, proven by a marker file named in the child's own environment
  and referenced by the patch text; a lost response onto a draft is `Pending`
  and a partial submission reports both the applied and intended counts, neither
  issuing a second create; a create whose read-back lost permission is
  `Unverified` with the applied review id retained and inside the receiver's own
  id range; the permission and transport arms are disjoint, naming an HTTP status
  and a credential versus a child's exit and no status, and are `Unverified` and
  `Unknown` through the journey; a publication is pinned to the commit that was
  read; two tenants' coverage verdicts stay isolated; and concurrent runs on one
  pull request conserve their creates and their read-backs. The test file's share
  of the repository's deterministic-simulation evidence is restored, which the
  `simulation-evidence` gate requires at one half of all tests.
- `domain::gh::read_diff`, a bounded changed-file inventory for the PR-review
  subject (#87 step 6, T31): `Gh::read_diff` reads a pull request's changed
  files as **data** and bounds them on two separate axes — at most
  `MAX_DIFF_FILES_PER_PULL` files (`GhError::DiffFileCeiling`) and at most
  `MAX_DIFF_BYTES` bytes of patch text (`GhError::DiffTooLarge`) — refusing the
  whole inventory rather than truncating it. A server that declines to render
  the diff (`406`) is `GhError::DiffUnavailable`. A renamed repository is
  `GhError::MovedRepository` naming both the requested and canonical
  repositories, so the subject identity is never silently re-pointed. A client
  that names an HTTP `401`/`403`/`404` is `GhError::Unauthorized` rather than a
  transport failure. `GhOutcome::http_status` exposes the status the client
  named. No file in the inventory — including a `build.rs` — is compiled,
  imported, built, shelled or loaded (INV-BOT-21).
- `ReviewOutcome::{Incomplete, Pending, Partial, Unverified}` (#87 step 6,
  T31/T33/T34): an unavailable or over-ceiling diff is an `Incomplete` coverage
  decision (nothing is published); a lost response reconciled onto an
  unsubmitted draft is `Pending`; a submitted review that landed with fewer
  inline comments than intended is `Partial` with both counts; and a create
  that returned an id whose read-back lost permission is `Unverified` with the
  applied review id retained. No path issues a second create, and a read that
  failed for any other reason stays `Unknown`. `ReviewComment`,
  `ReviewPayload::with_comments` and `ReviewRecord`'s comment-count comparison
  carry the inline comments the partial check reads. INV-BOT-81.
- Repairing a blocked run, end to end (feature `script`; `ephemeral` for minting
  a run id). A step reaches for authority with `Scope::require(&[Cap])`, and a
  run whose authority — the host's grant plus any repair delta — does not cover
  it is `Blocked` with a complete `Deficit` rather than `Failed`: the host was
  willing and the authority was missing, which is the distinction a repair acts
  on. `Report::needs` and `Report::repair` are both derived from that one value,
  so they cannot disagree. `Host::repair(ticket, grant, task, input, spend)`
  resumes under the run's own id, so the steps recorded before the block replay
  without their bodies being polled and only the blocked remainder runs. The
  grant may not be short (`RepairError::NotAuthorized`) nor carry any capability,
  shipped or custom, the ticket does not name (`OverWide`); the authority the
  repaired run receives is built from the ticket's needs, never taken from the
  grant, and the host's own grant is never widened — the next run on that host is
  still blocked. `HostBuilder::grants`,
  `repair_ledger` and `repair_bounds` configure it.
- `task::RunLedger`: the durable per-run control ledger — root spend and attempt
  budget, repair epoch, and the set of applied tickets — over the shared frame
  grammar and the shared storage-owner thread (INV-BOT-51). A ticket's identity is
  its content (run, tenant, epoch, sorted needs), so the same ticket delivered
  twice is refused `AlreadyApplied` and an older ticket is refused `StaleEpoch`;
  decide-and-write is one ordered step on the ledger's own thread, so "applied
  once" is a fact about bytes. A refused repair charges nothing, mints no epoch
  and leaves the ledger byte-identical. A repair consumes the root budget rather
  than refilling it, so a permanent refusal plus repeated `NotApplied` reaches a
  finite `BudgetSpent` (#87 step 3, T13/T23/T24).
- `task::repair`: `RepairTicket` (a report, never a grant), `RepairError` with a
  typed arm per refusal.
- Deterministic-simulation evidence for the repair door, one seeded family per
  arm rather than one family that reads a final counter (#87 T13/T23/T24). The new
  families in `tests/sim_repair.rs` are: `the_step_that_reaches_is_the_step_that_blocks`
  (a `Task::requiring` reach is `Blocked` at the admission boundary with no body
  poll, no permit, no record and no root attempt charged) and
  `a_wide_need_set_costs_one_analysis` (a shortfall of one to four capabilities
  costs exactly one analysis, and the report's and the ticket's needs are the same
  set in the same order);
  `a_custom_capability_is_refused_at_every_width` (the over-wide check walks the
  grant rather than a candidate list, so a custom name is refused at one need and
  at four, and the ledger is left byte-identical) and
  `a_ticket_never_names_another_tenants_run` (the ticket's own tenant check
  refuses before admission and before the ledger, so the asking tenant's ledger
  never gains an entry);
  `a_mixed_decision_order_pins_each_arm` (each arm observed per decision rather
  than inferred from the endpoint), `every_repair_charges_the_root_budget_once`
  and `a_spent_budget_refuses_every_later_attempt` (the budget sequence, and a
  finite refusal at either ceiling that charges nothing and is stable across later
  attempts), `a_reopen_reads_back_the_charged_budget` and
  `a_repaired_run_survives_a_reopened_host` (the replay rests on bytes a second
  host opened), `a_host_spent_on_one_run_still_repairs_the_next` (a spent run's
  ceiling bounds that run alone, and recovery after the refusal is through a
  reopen — the handle the refusal poisons is INV-BOT-50's, not this door's), and
  `a_bounded_sweep_repairs_every_ticket_once` (100 and 1,000 runs over one
  ledger, each ticket applied exactly once and each run charged exactly twice; the
  10,000 tier of the declared claim stays on the opt-in
  `the_declared_repair_tiers_are_measured`, measured at 488 s against 48 s for the
  two that run on every ordinary pass).
  No behaviour changed; these are the properties INV-BOT-33/34/35 stated with no
  arm-level evidence, recorded as INV-BOT-36/37/38.
  **Known limit, stated rather than left to be discovered:** `Host::repair` still
  has no production caller inside `lgwks_bot` — it is the door the front door
  exposes and an embedding host calls, so every exercise of it here is a test. The
  behaviours it gates (the blocked disposition, the ledger charge, the ticket) are
  all on the real `Host::run` path, and this package adds no new capability that
  only tests reach; it widens the evidence over the one that had a single point.
- `task::Control` is re-exported from `lgwks_bot::task`. `RunLedger::control`
  already returned it — a caller reading a run's budget or epoch had to name the
  type to hold it, and there was no path to the name — so the name is now public
  beside the handle that returns it. Additive: no existing signature changes.
- `Disposition::Blocked` on the front door, distinct from `Refused`: a `Refused`
  run was refused by the host and no authority would change it, while a `Blocked`
  run is the one an authorized repair can move. `Task::requiring` is the blunt
  form for a task that reaches in its first step; `Scope::require` is the one that
  leaves the work before the block replayable. `FlowError::Blocked` is
  `#[non_exhaustive]`-added and never retryable.
- `proposal` (feature `script`): the boundary where untrusted model output
  becomes work. Issue #87's rule is that an AI proposal is an **untrusted task
  input**, and that validation, provenance, no-progress detection, bounded repair,
  tenant-scoped artifacts and serialized writes belong in the host/adapter
  contract rather than in a prompt — so this module reads bytes and refuses them
  rather than prompting a model. It is **not** a fifth verb: an admitted `Plan` is
  a list of names to perform through the existing verbs, the operations a proposal
  may name are exactly the ones the host registered in a `Surface`, and the crate
  calls no network model at all. `StubModel` is a deterministic double from a seed
  to output bytes, because the guarantee here is about *admission* and admission is
  identical whoever produced the bytes (#87 T26–T29, T35).
  - `Decoder`: a hand-written bounded line grammar with a byte ceiling, a
    per-field ceiling and a field-count ceiling. `install`, `credential` and `host`
    are *recognised* so their refusals name what was asked for — a decoder that had
    never heard of them would report `Malformed` and an attempt to widen authority
    would read as a broken document. An unknown field is refused rather than
    ignored, and a refused payload returns no plan beside its refusal.
  - `Refusal`: a typed arm per refusal, with `SandboxEscape` as its own arm so an
    escape stays observable, and `is_privilege_attempt` for the five arms that are
    attempts rather than syntax. Every outcome carries `Provenance` — the source
    (model, tool output or document), the tenant and the digest of the exact bytes.
  - `Completion`: admitted only with the evidence it names present.
    `Coverage::from_claim` maps *every* payload claim onto `Partial`, so a plan
    cannot be talked into full coverage, and `Coverage::Complete` has no
    constructor reachable from a decoder.
  - `RepairLedger` and `PlanBudget`: one unchanged fingerprint past its ceiling is
    a typed `Intervention`, and recording new evidence moves no repetition count,
    so it cannot erase what a failure already cost.
  - `Checkpoint`: `Durable`, so a context reset recovers completed steps, user
    corrections *with their kind*, `Unknown`-classed effects and evidence
    references through the run store.
  - `ArtifactStore`: keyed by `(tenant, digest)`, so identical bytes from two
    tenants are two artifacts; writes to one key serialized and idempotent by
    content, reads lock-free of the writer, every bound a typed refusal that
    leaves the store unchanged.
- `script::admit` (feature `script`): the one step a task body uses to admit model
  or tool output, and the fix for the defect a reviewer found on the `proposal`
  module above — it shipped with **no production caller**, so every property
  INV-BOT-90..94 state held for a boundary nothing invoked. The estate rule is
  "wired or it does not exist", so the capability lands called from the run path
  in the same change rather than deleted (#87 T26–T29).
  - It is a `script` block rather than a helper because a task body can only act on
    a `FlowError`: it enters its step, so a refusal reads `admit/plan` like every
    other located failure; it charges one **run-scoped** `Gate` — decoder, surface,
    `PlanBudget`, `RepairLedger` — so the fourth identical refusal across four
    separate `Host::run`s is a finite typed `Intervention` rather than four
    refusals a caller has to correlate; and it records each refusal through the
    run store under `<step>/refusal` *before* returning the error, so a run resumed
    on a fresh host reads back what the first run refused rather than re-deriving
    that nothing was refused.
  - `FlowError` gains two arms, `Refused { at, refusal, provenance }` and
    `Intervention { at, intervention }`. Both are typed and both carry the
    `Provenance` of the refused bytes, so no refusal reaching a `Report` is a
    string a caller must parse or an unattributable failure. Both are non-retryable:
    a payload refused for its content is refused however often it is re-read, and
    another attempt is exactly the repair an intervention refused.
  - `Gate` is shared by clone and its lock is held across the charge, the decode
    and the ledger update and **never across an `.await`**, so a fan-out can hand
    one gate to every body without the budget becoming per-body.
  - It admits a `Plan` of operation *names* and performs nothing. Not a fifth verb,
    and not an untyped plan interpreter: performing a plan's operations is still
    the caller's job through the existing verbs, and `EffectKnowledge` continues
    to report that a run performed no external effect.
- `task::RunStore::lookup`: the reader's door onto a durable step's value — the
  archived bytes committed for a step key under a run, or `None`. Without it, the
  only way to see what a previous instance recorded was to re-run the step that
  wrote it, which is why a resumed run could not read back a refusal. `Err` is a
  read failure (a run another tenant owns), never a miss (INV-BOT-7).
- `script::Readiness<T>`, a typed, generation-bound readiness fact and the wait
  that consumes it (#87 T18 / LC-09). A `Ready<T>` carries the `Generation` its
  instance was admitted under, so the four ways a readiness can say "no" are four
  different facts rather than one boolean: a signal from an older instance is
  `StaleGeneration`, one from a generation this readiness never issued is
  `UnknownGeneration`, a second release at a settled generation is `AlreadyReady`
  (the first release survives it — it is not a second release), and a duplicate
  failure is `AlreadyFailed`. `ReadinessError::released` is false for every arm,
  and `is_permanent` separates the one retryable refusal from the rest, so an
  enclosing `retry` cannot loop on a stale instance.
- `Readiness::fail` reaches a dependant that has **already been released**: it
  cancels the token of every dependant the release handed out, so a dependant
  still running learns the service is gone rather than talking to a dead
  process, and `failed_at` distinguishes that from a failure before anyone was
  released (which owes nobody a cancellation). `Readiness::shutdown` is the
  separate fact — a teardown is a stop, not a failure, and the waiting side sees
  `FlowError::Cancelled` so a routine restart does not read as an outage.
- `Generation`: a monotone, saturating counter with `FIRST`, `next` and `at`. Not
  a timestamp, because cross-host clock skew is unmeasured (INV-BOT-30), and not a
  random token, because a token can answer "the same or not" but never "older",
  which is exactly the question a restarted instance's surviving handle asks. A
  restart is a **new** readiness at a **new** generation, and that is what makes
  the old instance's handle unable to release anybody.
- `Readiness::wait(scope, limit)`: one admission, one `watch` subscription and one
  `select!`. No sleep and no poll loop, charged to the step's own budget through
  the existing `within` so it is bounded and stopped by the scope's stop, and an
  already-released readiness resolves without spending its budget — the property
  a guessed sleep cannot have. A readiness admits at most `MAX_DEPENDANTS`
  dependants, charged *before* a slot is taken, so a refused admission leaves
  capacity exactly as it was.
- `Supervisor::run_process_observed`, a stdout-line observer for a supervised
  child, and the `rt::supervise::LineObserver` alias that names it. The observer
  is called from inside the same pipe read that retains the child's output, on the
  bytes that read just observed, so an observation and a capture cannot disagree
  about what the child wrote; it fires before the capture ceiling is consulted
  and whether or not the bytes are retained, and never arms a timer. A long-lived
  service's readiness therefore comes from what the child printed — a `sh -c`
  child that prints a readiness line gates on its own output, with no timer
  anywhere in the path. `Supervisor::run_process` is that call with `None` and
  is now one method on every target, the Unix and non-Unix bodies differing
  behind it.
- More than one million task executions in flight at once on one node, measured
  (#152 §4). `tests/task_million.rs` (opt-in, `LGWKS_MILLION=1`) admits
  1,048,576 `Host::run` executions across sixteen tenant hosts, each saturated
  to its own `MAX_ADMITTED_TASKS` ceiling of 65,536, waits until every one is
  observed suspended inside its body at the same time, then releases them. The
  subjects are admitted, executing runs holding their permits — not queued
  requests or inert records. Measured on an Apple M5 Pro, release build, one
  current-thread runtime: all 1,048,576 in flight after 1.74 s; peak RSS
  6,445,662,208 bytes by `/usr/bin/time -l` (about 6.1 KiB per suspended run);
  release-to-resume p50=2.80 s p95=3.99 s p99=4.17 s, the single thread
  draining a million wakeups; every run returned its own doubled input, every
  host got every permit back and none admitted past its ceiling.
- `rt::clock`: one declared logical clock (`Clock`) and the wall-clock watchdog
  (`WallClock`) that pausing it cannot disable. `Clock::wall` follows real time
  and refuses a caller advance with a named reason; `Clock::virtual_at` is
  caller-advanceable, so retry, sleep, readiness and continuation deadlines are
  exactly drivable from a test. An advance past the declared ceiling saturates
  rather than wrapping, and a clock already at the ceiling refuses rather than
  reporting movement it did not make. `ClockSnapshot` is a **duration**, not an
  instant: what survives a restart is the remaining budget, which means the same
  thing on another host (#152).
- `rt::time::Deadline`: a deadline that names the clock governing it, carrying
  both the logical bound (`elapsed`, `remaining`, `is_exhausted`, driven by the
  declared clock) and the independent wall watchdog (`watchdog`,
  `watchdog_exceeded`, real time). Additive; no existing signature changed (#152).
- `Supervisor::snapshot` -> `SupervisorSnapshot`: a bounded, point-in-time view
  of live capacity, the declared ceiling, the supervisor's own counters, whether
  admission is closed, and the next eligible action (`NextAction::Admit`,
  `WaitForCapacity`, `Stopped`). It reads the owner's own admission and reporting
  fields, so it is never a second ledger; terminal outcomes stay in the report
  stream where the retention cap already governs them. The live listing is capped
  at the in-flight ceiling and reports how many it excluded, and every field is
  private behind an accessor. Additive (#152).
- The declared `Clock` now **governs the real run path** (#152, `INV-BOT-30`):
  `script::within`, the task `Host` (`HostBuilder::clock`, default wall) and the
  Supervisor's budget and deadline checks all read time through it. A body handed
  a caller-advanceable clock reports the same deadline refusal a real overrun
  would, with no real wait; a scope given no clock is unchanged. Additive: every
  existing signature still compiles and behaves as before.
- The run store's durable write reaches the disk off the executor (#87 step 5).
  `remember` awaited `RunStore::commit` directly, so a step's `write_all` plus
  `sync_all` ran on the thread polling it and parked the whole runtime for the
  length of an `fsync` — the blocking-write defect #122 removed from
  `FileJournal`, inherited here because this store was written later. The
  run store now shares `journal::file`'s storage-owner thread, generalised to run
  the caller's whole critical section (the in-memory checks, the length fence,
  the write, the `sync_all` and the fold into the store's index) as one ordered
  step no other append can overtake. `RunRecords` gains `append_async`, which
  `remember` awaits, so a step waits for its record instead of sitting through
  the flush; the default implementation calls the synchronous door, which is
  correct and is exactly the blocking shape the method exists to remove.
  `RunStore::open_with_stalled_device` and `RunStore::storage_gate` are public
  for the same reason `FileJournal::open_with_stalled_storage` is: an operator
  has to be able to ask what a run does while its record store has stopped
  answering, and a probe that only exists inside the crate's test binary cannot
  answer it. A caller that walks away from an outstanding record still latches
  the handle's poison, because the bytes may be on the disk under no
  acknowledgment; only a reopen, which replays the truth, clears it.
  `journal::frame` and `journal::owner` are the two extracted mechanisms, and
  `journal::file`'s behaviour and tests are unchanged by the extraction.
  INV-BOT-50.
- One frame grammar for both file-backed stores (#87 step 5).
  `RunStore` re-implemented `journal::file`'s frame codec — the length prefix,
  the 32-byte head, the torn-tail scan and the refusal of a frame no writer
  produces — which is a second definition of "what a torn tail is" that only one
  store's tests would see. `journal::frame` now holds that grammar once, and
  `frame_record` performs a record's whole archive/bound/chain/lay-out step,
  parameterised by each store's record type, archiver and head-chaining function.
  `FileJournal` and `RunStore` call it; neither writes a frame by hand. No
  behaviour change to either store: `journal::file`'s existing tests pass
  unchanged. INV-BOT-51.
- Measurements for the durable step (#87 step 5).
  `examples/resume_cost.rs` compares three mechanisms at one payload size: a
  plain step (p50=1us), a `remember` through the store (p50=17984us, p95=33189us,
  p99=44988us) and one `FileJournal` append (p50=15977us, p95=29949us,
  p99=39949us) — so a durable step is not paying twice for one mechanism.
  `tests/task_resume.rs::concurrent_runs_across_tiers` runs 100, 1000 and 10000
  runs over one store with two tenants alternating and re-reads every record
  from a reopened store: 100 → p50=3019us p95=3977us p99=4138us in 316.11ms;
  1000 → p50=3005us p95=4001us p99=4136us in 3.13s; 10000 → p50=3017us
  p95=4144us p99=5312us in 32.74s, with no record lost, none duplicated and
  none attributed to the wrong tenant. Both measurements are opt-in through an
  environment variable and report that they did not run rather than a bound
  nobody checked. `tests/resume_liveness.rs` measures the liveness claim
  directly: with the device parked and an independent OS thread releasing it,
  an unrelated ready task turned 1,917 times while one flush was parked,
  where a blocking implementation reaches one poll and then sits inside the
  fsync. The count is reported rather than merely asserted, so a reader can see
  whether the durable wait is a wait or a near-total stall. INV-BOT-52, INV-BOT-53.

- Three defects found while merging `bot/hardening-122` (#87 step 5).
  `a_killed_process_resumes_without_rerunning_finished_steps` failed roughly
  one run in eight under load and reported that a step had run zero times when
  it had demonstrably run: the parent waited on `path.exists()`, and
  `std::fs::write` creates the file before its bytes are in it, so the kill could
  land while the marker was still empty and `ran` parsed that as zero. The wait
  is now for non-empty content.
  The same test's `alpha` and `beta` markers sat inside their `remember` bodies,
  where seeing one proved only that the body had started while the parent used
  them as proof the record was already on the disk; the kill could land between
  the marker and the append and the resume would re-run a step the test had
  already called durable. Both markers are published after the await returns.
  `sim::band_of` also used `swap_remove`, which answered a band index past the
  last band with an out-of-bounds panic naming the length rather than the
  mistake. No behaviour change to any store. INV-BOT-53.
- The storage owner no longer needs a runtime (#87 step 5). It queued requests
  through `rt::sync::mpsc`, which is the estate's bounded channel and the right
  choice when a runtime exists, but this thread outlives every future that waits
  on it and must not depend on a runtime being alive to be driven — so
  `cargo clippy --no-default-features` could not compile it, and the journal has
  compiled without `rt` since before this crate had a run store. `std::sync::mpsc`
  is disallowed workspace-wide precisely because it has no bounded form; the
  bound is now a `VecDeque` inside the lock the owner already holds, with a
  checked push that refuses rather than grows, so a caller that outruns the
  device by more than `DEFAULT_QUEUE_DEPTH` still gets `SubmitError::QueueFull`.
  `StorageGate` becomes one type for both stores rather than a view
  parameterised by one store's state and another's answer type. INV-BOT-50.
- One definition each for four fixtures the two branches duplicated (#122).
  `durable_crash_observation.rs` and `journal_writer_fence.rs` each carried their
  own scratch-path builder and cleanup guard beside the pair in
  `support/journal.rs`; `sim_task_resume.rs` had its own `Scratch` beside the one
  in `support/resume.rs`; and `journal_liveness.rs` and `resume_liveness.rs` each
  had their own `Parked` and `heartbeat`. All five now come from the shared
  modules. The one real difference is kept rather than erased: a journal's
  append is position-fenced and a record store's is not, so `Parked` carries an
  optional tail and the journal family opens it with `Parked::at`. Migration:
  none; every change is inside the crate's own tests.

- Host-held resumable runs (#87 step 5). `HostBuilder::run_store(dir)` installs a
  durable, file-backed per-step record store — `task::RunStore`, chain-framed and
  `fsync`-ed per record, opened and replayed at installation so a refusal happens
  before a run claims durability. `script::remember(&scope, "step", || async
  { .. })` returns a committed step's decoded value **without polling its
  future**, and otherwise runs the future, syncs the record, and returns it.
  `Host::run` mints a `RunId` when a store is installed; `Report::run_id()` names
  it; `Host::resume(run_id, &task, input)` re-runs the body with recorded steps
  replayed, and `Host::resume_ticket` takes a `Report::ticket()` naming the run,
  its owning tenant, its disposition and the step path it stopped at. Records are
  keyed by tenant, so resuming another tenant's run id is a typed `Refused`; a
  broken chain is refused rather than trimmed; an interrupted final append is the
  only thing dropped; and the three ceilings (per-record bytes, records per run,
  total bytes) are typed refusals that leave the store byte-identical.
  `Report::effects()` gains `EffectKnowledge::StepRecords { records }` beside
  `None`; without a store the report says `None`, names no run, and offers no
  ticket, so no in-memory sink is ever reported as durable. Documented honestly:
  a step that ran but whose record did not land re-runs on resume — exactly-once
  for a recorded step, at-least-once for an unrecorded one — and an external
  effect still needs the effect journal. Migration: none; every added item is
  additive, and a host that installs no store behaves exactly as before.
  INV-BOT-54.
- `domain::gh` is a real adapter (#151): the GitHub CLI runs as one supervised
  child through `Supervisor::run_process`, with bounded capture, a deadline and
  process-group cleanup, instead of the typed `binding required` refusal the
  domain returned before. `Gh::snapshot`, `Gh::read_reviews` and `Gh::publish`
  each admit a validated `ProcessSpec`; arguments are a vector, so no shell is
  involved. `Repository`, `CommitId` and `ReviewPayload::new` refuse what
  GitHub would reject rather than sending it. A publication payload is staged as
  a private file (`create_new`, mode 0600) and removed on every exit path; the
  name comes from `lgwks_std::random` under `ephemeral`, and a build without
  that feature refuses to publish rather than reuse a name the OS recycles.
  Without the `process` feature every call refuses with `GhError::NoRunner`
  rather than reporting an empty answer a caller could mistake for "GitHub has
  no reviews". `GhQuery` and `PrSnapshotSource` expose the same binding through
  `Query` and `Observe`; both require `bot.sys` and `bot.net`.
- `review`, the canonical PR-review task (#87 step 6, #151): `review_pr` pins a
  subject, runs a caller-supplied analysis, checks freshness, publishes once at
  the reviewed commit, and verifies through a separate read-back.
  `ReviewOutcome` is five states a caller can act on without reading a message —
  `Published { verified }`, `Unknown`, `TargetMoved { reviewed, current }`,
  `Refused`. A lost response is reconciled by one read, never a second create;
  only a `Refused` certainty proves nothing was written, because a non-zero
  exit cannot distinguish "never arrived" from "applied and the answer was
  lost". Verification compares subject, body and state and deliberately ignores
  the application marker, which locates a candidate and is not proof. The body
  manages no pid, no reap loop and no retry over publication.
  `cargo run -p lgwks_bot --features process --example review_pr -- <repo> <pr>
  <EVENT> <body>` runs the whole path; `LGWKS_REVIEW_PUBLISH=0` is a
  draft-only profile.
- `domain::gh::MAX_REVIEWS_PER_PULL` and `GhError::ReviewCeiling` (#151):
  `--paginate` follows GitHub's review pages until the client is done, so the
  review list used to grow with a pull request's history rather than with any
  bound of this crate's own. A review read now refuses a list longer than the
  declared ceiling — a *typed refusal*, not a shortened list. The distinction
  matters because a truncated list that decoded cleanly is indistinguishable
  from the whole history, and a verification built on it would report "no
  matching review" for a review that exists on a page nobody read. Exactly the
  ceiling is accepted; one more is refused, naming the endpoint, the count and
  the ceiling. This is a bound, not a truncation: a build without the
  `process` feature still refuses every call with `NoRunner` rather than
  reporting an empty snapshot, an empty review list, or review id `0`.
- `inspect`, typed in-process structural code inspection (#150, R8; feature
  `inspect`): `inspect(&InspectRequest)` parses the subject's bytes with
  `lgwks_ast` and walks the tree against the versioned `RuleSet::STRUCTURAL_V1`
  rules (`rust/no-unwrap`, `rust/no-todo`, `rust/no-panic`), returning an
  `Inspection` whose typed `Verdict` distinguishes `Clean` (no match within an
  explicitly complete supported scope), `Violations`, `Unsupported` (no compiled
  grammar, unknown/mismatched rule set, or a grammar the rules cannot read),
  `Undecidable` (a declared language version this build cannot confirm),
  `Incomplete` (source/node/depth/work/findings/output budget exhaustion or parse
  recovery) and `InfrastructureFailure`. The subject is never compiled, imported,
  built, shelled or loaded; `Budgets` bounds bytes, nodes, depth, work, findings
  and output as separate axes, and `Inspection::coverage` reports per-rule
  support and evaluation. The report serializes through the shared JSON facade
  with its input digest, exact byte spans and rule revision intact, and
  `Inspection::assurance` states that a clean result is not proof of safety.
  Host-only, like `process`/`fs`/`net`: it draws native tree-sitter grammars, so
  it is not in the default feature set; `full` enables it.

- `domain::inspect`, the same operation wired onto the verbs and the task front
  door (#150, R8): `Inspector` is a `Query` over an `InspectionJob` (caller-
  supplied bytes, no capability), and `Subject` is an `Observe` source that
  reads the artifact under `bot.fs` (bounded before the read), so a `BotSpec` or
  a native bot reaches `inspect` through the registry and admission path every
  other domain uses, and `inspection_task` exposes it as a `Host`-run `Task`.
  Every door returns the operation's own `Inspection`. The `inspect_scale`
  example is the print-only measurement harness (per-tier latency percentiles
  and per-size `Resources` counters) for `/usr/bin/time -l`.

- `task::{Host, Task, Report}`, the front door (#87 step 1): build a `Host` once
  (tenant, stop token, admission ceiling, default deadline, trail capacity, all
  finite and readable through `Host::limits`), define a `Task` with `task(name,
  body)`, and `host.run(&task, input).await` returns a `Report` carrying the
  disposition (`Succeeded`, `Failed`, `Cancelled`, `DeadlineExceeded`,
  `Refused`), the typed output, the located error, a bounded step trail with a
  dropped-step count, and `EffectKnowledge::None`. Bodies are polled on the
  calling task, so they need not be `Send` and inputs may borrow; nested runs
  on one host are charged to the parent's permit (INV-BOT-20).
  `Host::block_on` is the synchronous entry and refuses to run inside an
  existing runtime.
- `Bot::from_spec` materializes a validated `BotSpec` against a `DomainRegistry`
  into a runnable `Bot`, through the same `assemble`/`build` path a native bot
  uses — no second interpreter and no second execution path — so a JSON-built bot
  and a natively built one produce the same operation trace. Authority still
  comes only from the caller's `GrantSet`, never from the document. Admission is
  all-or-nothing and reports every presently knowable unmet need in one
  attributed value (`Admission`, `NeedSet`, `Need`): an unknown source or action
  domain, a constructor that rejects its target, a missing capability, or an
  unknown condition. A duplicate registry identifier and an unsupported spec
  version are typed refusals, and nothing is polled or executed on refusal. (#87)
- `Condition`: an erased condition handle, so a materialized chain and a native
  one share one condition erasure and one type-mismatch report.
- `Source::condition` resolves a wire condition identifier against the source's
  own output type, from the closed vocabulary `changed`, `always`,
  `threshold::above(<n>)`, `threshold::below(<n>)`.
- `Supervisor::run_process` runs one child under the same in-flight ceiling,
  process-group ownership and deadline machinery as `spawn_process`, and returns
  its `ProcessRun`: the exit status or signal, bounded captured stdout and
  stderr with truncation accounting, whether a deadline stopped it, and the
  process-group cleanup receipt.
- `rt::process::StdioPolicy::Capture(NonZeroUsize)` captures a standard stream
  up to a non-zero ceiling, keeps draining past it, and reports the exact total.
- `domain::sys::Process` runs real processes with the `process` feature and
  reports exit code or signal, the captured streams and a finite default
  capture ceiling and deadline; without the feature it keeps its refusal.
  Concurrent verb calls on one `Process` share a bounded slot pool
  (`DEFAULT_MAX_CONCURRENT`, set with `Process::max_concurrent`), claimed
  before the fork, so a burst of calls never forks a burst of children.
- `FileJournal::replay` returns a `Replay` that streams the committed frames
  from its own read-only descriptor, one event at a time, so a caller that only
  folds the record pays for the largest frame rather than the whole history.
  It applies the same frame validation `open` does and is bounded by
  `MAX_JOURNAL_EVENTS`; the materialized `events()` view is unchanged (#122
  item 2).
- `StorageGate::storage_gate` is public, so a slow-store liveness test can
  release a parked device from its own thread rather than from the runtime
  awaiting the append (#156).
- `EffectJournal::reserve_handoff_capacity` reserves room for a whole external
  handoff — intent, preparation and the settlement that lands after the effect
  has left the process — before the first rung is written. The default is
  permissive; `FileJournal` refuses the handoff against its event ceiling, so a
  durable journal can never leave an attempt admitted and unable to settle
  (#122 item 2 / #156).
- The `compare_orchestration` example gains a `host` way — `Host::run` per item
  with the host's admission ceiling as the fan-out bound and matched semantics
  against the hand-written `JoinSet`+`Semaphore` and `join_all_bounded` ways —
  and a `measure_overhead` example prints p50/p95/p99 for `Host::run` and
  `sys::Process`.

### lgwks_deps Added

- `[[approved]]` entries accept an optional `origin`: the exact admitted origin
  for the entry's source class — a complete registry source, a Git repository
  plus its admitted revision/reference policy, or an external path authority.
  Admission now compares origin as well as class, so replacing an approved Git
  repository, registry, path, or Git revision produces a typed
  `Refusal::OriginDrift` carrying the approved and observed identities. A legacy
  class-only entry is exact for crates.io (both its Git and sparse spellings)
  and insufficient for a Git or path edge; an unknown origin scheme is refused
  at load and never admitted (INV-DEP-12, #158 A1).
- Sparse-registry sources (`sparse+…`) are classified as the `registry` source
  class rather than an unknown scheme, so a sparse crates.io mirror compares as
  crates.io.
- Drift diagnosis with several approvals for one crate reports the dimension on
  the approval that admits the edge's source class, instead of the first
  mismatch from an unrelated class (#158 acceptance).
- `[[approved]]` entries accept the admitted-capability policy keys `features`,
  `required_features`, `uses_default_features`, `optional` and `target`, and an
  explicit `aliases` list. `DirectEdge` now carries Cargo's authored `features`,
  `uses_default_features`, `target` and `rename`, so a capability that changes
  without a class or origin change is a typed `Refusal::FeatureDrift`,
  `DefaultFeaturesDrift`, `OptionalityDrift` or `TargetDrift` instead of a pass.
  A dimension an entry does not author is grandfathered (#158 A2, INV-DEP-13).
- `metadata::DirectEdge::features`/`uses_default_features`/`target`/`rename` are
  readable through accessors; `rename` is the manifest-local spelling and
  `package` remains the upstream Cargo identity (#158 A2).
- `check` prints a receipt binding the subject root, the contract identity and
  schema version, the exact metadata subject, the policy mode and the assurance
  scope, and `check --json` exposes the same under the stable keys `mode`,
  `contract.{digest,schema,entries,repository}`, `subject.{digest,edges,resolved}`
  and `scope`. `Contract::digest`/`schema` and the `Subject`/`Verdict` types back
  it; `check_verdict` returns the receipt-bearing verdict (#158 A6, INV-DEP-15).
- A register may author `[policy] schema`; the committed register is migrated to
  `schema = 2`. Schema 1 remains readable (#158 A7).

### lgwks_deps Changed

- **Breaking for a Git or path edge: a class-only approval is insufficient.**
  An `[[approved]]` entry whose `source` is `git` or `path` and that authors no
  `origin` admits nothing in that class; it is no longer an implicit approval of
  every origin. **Migration:** add `origin = "<exact source>"` to each Git or
  path entry (a Git repository plus its `?rev=`/`?branch=` policy, or the exact
  path authority). A class-only `registry` entry still admits crates.io in both
  its Git and sparse spellings, so registry entries need no change (#158 A1,
  INV-DEP-12).
- Package and owner matching is now byte-exact against the Cargo-authored
  identity. **Migration:** a register that relied on the implicit `-`/`_` (or
  case) fold to match a differently-spelled package must either write the exact
  Cargo name or add `aliases = "<spelling>"` to that entry; an alias is
  collision-checked and names exactly one package. The committed register uses
  exact names throughout and needs no alias (#158 A2, INV-DEP-14).

### lgwks_bot Changed

- `BotSpec` carries a `version`, defaulted to `BotSpec::CURRENT_VERSION` when
  absent so a document written before the field existed still parses. A version
  this build does not implement is refused by `BotSpec::from_json` and by
  `Bot::from_spec`.
- `DomainRegistry::source` and `DomainRegistry::action` return `None` for an
  identifier declared more than once, not the first matching constructor. An
  ambiguous identifier no longer resolves by declaration order; `validate()`
  already refused such a registry by name at every construction path, and this
  closes the raw lookup so a caller that skips validation cannot reach an
  ambiguous constructor either (#122). Migration: a caller that relied on the
  first-wins result should pick the duplicate it means, or the registry should
  be repaired; `validate()` reports both positions.

### lgwks_bot Fixed

- The `registry` module documentation said "A duplicate is not refused: lookup
  is in declaration order and the first entry wins", which had been false since
  `DomainRegistry::validate` landed. It now states the truth: `validate` refuses
  a duplicated identifier and names both positions, and the raw `source`/`action`
  lookups refuse an ambiguous identifier too (#122).

- A process group whose leader is an unreaped zombie reports `EPERM` on a
  further `killpg` (macOS/BSD). That is a still-present group, not a refused
  termination, so process-group cleanup reports `CleanupPending` and settles via
  the post-reap signal-zero probe instead of `CleanupFailed` (INV-BOT-19).
- The drop-time group kill (a supervised run dropped mid-flight) was one
  `killpg`. It now repeats while the unreaped leader still pins the group id,
  so a member the first signal missed is reached. Known limit on macOS: a
  child the leader is forking at the instant of the kill can still be created
  after the leader dies, and the zombie leader then makes every further
  `killpg` return `EPERM` without reaching it; Linux aborts such a fork. A
  drop after the fork (T20) leaves no running member on either.

- The journal-scale invariants were renumbered `INV-BOT-23..28` to
  `INV-BOT-40..45`, because the old numbers were taken by another branch's
  register. Only the identifiers moved; every enforced-by reference still
  resolves. `INV-BOT-45` now states the tiered 100/1,000/10,000 sweep and its
  requested/reached/ceiling receipt (#122 item 1).
- `FileJournal::storage_gate`'s documentation now says what the handle is: a
  fault-injection and liveness instrument whose held gate parks every append on
  that journal by design, opened only through `open_with_stalled_storage`, never
  by `open` (#122 item 2).
- The registry-identifier invariant and the ambiguous-append invariant this
  branch added were renumbered to `INV-BOT-46` and `INV-BOT-47`, because the
  numbers they first carried were already taken on `main` (structural
  inspection, and the task front door). Only the identifiers moved; both
  invariants' `enforced by` references still resolve (#122, #118).

### lgwks_bot Breaking

- `Source::new` now requires `O::Output: Clone + PartialEq + InputIdentity`
  (was `PartialEq` only); every source usable in a chain already satisfied
  `PartialEq + InputIdentity`, and `Clone` is what the wire `changed` condition
  keeps. Such a source answers `changed` and `always`. A source whose output is
  also `PartialOrd + FromStr` registers with the new `Source::ordered` to answer
  `threshold::above(<n>)` and `threshold::below(<n>)` too, so the threshold
  bounds bind only the sources that offer thresholds.
- `BotSpec`, `ChainSpec` and `ActionSpec` no longer expose their string/collection
  fields as `pub`: read them through `BotSpec::version`/`name`/`chains`,
  `ChainSpec::source`/`target`/`on`, and `ActionSpec::domain`/`target`. The types
  are `#[non_exhaustive]`, so a struct literal was already unavailable outside
  the crate; only field reads change. Serde round-trips are unchanged.
- `domain::sys::ProcessState::stdout` is no longer a `pub` field: read it, and
  the new `stderr`, through the `ProcessState::stdout()`/`stderr()` accessors.
  The state is a report of what the process wrote, so it is read, never edited
  in place.

### Documentation

- The frontier comparisons now cite primary sources with pinned versions and an
  access date (2026-10-02). `docs/framework-comparison.md` and
  `docs/async-parity.md` name each external project's release or tag
  (`spider-rs` v2.52.2, `discord.py` 2.7.1, `serenity` v0.12.5, Stagehand
  `@browserbasehq/stagehand@3.7.3`, `@crawlee/core` 3.18.2, Scrapy 2.19.0,
  tokio 1.53.1, tokio-util 0.7.19, async-std 1.13.2, smol 2.0.2, Bevy 0.19.1).
  Claims a source contradicts are corrected (the `spider-rs` managed-mode and
  declared-limits quotes, which were `main`'s wording rather than the `v2.52.2`
  tag's; smol's missing cancellation token) and claims that cannot be sourced are
  marked *unsourced* rather than deleted (`docs/framework-comparison.md`,
  `docs/frontier.md`). No code, gate or ledger changes. (#155)

## [lgwks_std 0.10.0 / lgwks_ast 0.4.0 / lgwks_deps 0.4.0 / lgwks_bot 0.8.0 / lgwks_macros 0.1.2] - 2026-09-30

The third cut of the day, for the #191-#194 review fixes that landed in #199.
`lgwks_std`, `lgwks_deps`, `lgwks_ast` and `lgwks_bot` each carry a break, so
each takes the minor position. `lgwks_macros` only raises its `lgwks_deps`
requirement and takes a patch.

### lgwks_std Breaking

- `fs::capability::Dir::entry_names` takes `ListLimits` and returns a
  `Listing` instead of `Vec<OsString>`. Names are `String`s, the only name type
  the other `Dir` methods accept; a name that is not UTF-8 is counted by
  `Listing::unaddressable` rather than returned. The listing is bounded by entry
  count and name bytes, is sorted, and reports `is_truncated`. (#192)

### lgwks_std Added

- `http::Options::deadline` bounds the whole call, from the first lookup to the
  last body byte, across every redirect hop. `timeout` still bounds each phase
  of each hop. An expired deadline fails at `FailureStage::Deadline`. (#191)

### lgwks_std Changed

- A response body's buffer grows geometrically up to `max_body_bytes` instead of
  reserving exactly each 8 KiB read. (#191)
- `WalkLimits` states which part of a bounded walk depends on filesystem order:
  completeness does not (unless symlinks are followed without sorting); the
  entries an incomplete report keeps, in the directory where the budget ran
  out, do. (#192)

### lgwks_deps Breaking

- `metadata::read`, `metadata::workspace_members`, `check_dependencies`,
  `check_dependencies_against`, `check_invariants` and `invariants::check`
  return their result inside `metadata::Collected`. When Cargo's output was
  read in full but a capture file could not then be removed, the result is kept
  and the cleanup failure travels beside it (`Collected::into_parts`); before,
  the whole collection was refused. The CLI prints it as a `WARN` line on
  stderr and keeps the verdict. (#193)

### lgwks_deps Changed

- A `ProcessCleanup` refusal names the failed step (kill or reap), the cargo
  pid, the OS error, and each capture path it could not remove. Its
  `Error::source` is the primary failure when there is one. (#193)
- `check --json` may write one `WARN` line to stderr, for a Cargo capture file
  that could not be removed; stdout stays one JSON document. (#193)

### lgwks_ast Fixed

- A span whose offset falls inside a multi-byte character resolves to that
  character's start instead of panicking. (#194)

### lgwks_ast Breaking

- `AstMetrics` no longer implements `Default`. A default value had
  `complete == false` and read as a partial walk that nothing performed. (#194)

### lgwks_ast Changed

- `ParseError::to_diagnostic` places an `InvalidSyntax` refusal at its earliest
  retained recovery node instead of at the end of the file. (#194)

### lgwks_bot Breaking

- `domain::net::NetState::body` is a method; the field is crate-private, so a
  held value keeps the `BODY_PREVIEW` bound. A poll is bounded by a 10 s
  whole-call deadline, redirects included. (#191)

### lgwks_macros Changed

- Requires `lgwks_deps` 0.4. No change to the `script!` syntax.

## [lgwks_std 0.9.0 / lgwks_deps 0.3.0 / lgwks_bot 0.7.0 / lgwks_macros 0.1.1] - 2026-09-30

The second cut of the day, for the #157, #158, #159, #161, #162, #167 and #168
work that landed in #197. `lgwks_std` and `lgwks_deps` each carry a break, so
they take the minor position. `lgwks_bot` re-exports `lgwks_std::json` and
depends on `lgwks_deps`, so it moves with them. `lgwks_macros` only raises its
`lgwks_deps` requirement and takes a patch. `lgwks_ast` does not move.

### lgwks_std Breaking

- `pattern::PatternError` fields `pattern` and `message` are private; read them
  with `pattern()`, `message()` and the new `kind()` (`PatternErrorKind`:
  `PatternTooLarge`, `CompiledTooLarge`, `Syntax`, `Other`). It no longer
  implements `Clone`, and `Regex`'s `Debug` output quotes and escapes the
  pattern (`Regex("a+")`, not `Regex(a+)`). (#168)
- `ron::to_writer` returns `WriterError`, which separates `Serialize` (nothing
  was written) from `Write` (the writer may hold a prefix), instead of folding
  an I/O failure into `ron::Error`. (#162)

### lgwks_std Added

- `pattern::Regex::with_config(pattern, PatternConfig)` returns a
  `BoundedRegex` that enforces source-pattern bytes, compiled size, nesting,
  input bytes and replacement-output bytes. Every search, split and replace
  returns `PatternRunError` on a breach rather than a partial result.
  Bounded replacement expands `$name`, `${n}` and `$$` exactly as the engine
  does, including `${+1}`, and is tested against it template by template.
  `Regex::replace_borrowed` and `replace_all_borrowed` return `Cow` and do not
  allocate when nothing matched. The docs now state the real cost: one search
  is O(m·n), while full greedy iteration may be O(m·n²). (#168)
- `wire::format_descriptor()` reports the byte order, pointer width and `u32`
  alignment of the archives this build produces. The module no longer claims a
  canonical or versioned encoding. (#167)
- `json` and `ron` serializers accept unsized values (`T: ?Sized`), and
  `from_str`/`from_slice` take `Deserialize<'de>`, so borrowed fields such as
  `&str` deserialize without copying. `ron` re-exports `serde`. (#162)

### lgwks_deps Breaking

- `contract::Contract::entries`, every `contract::Entry` field,
  `invariants::Register::entries`, every `invariants::Entry` field and
  `invariants::Outcome::id` are crate-private. Use `Contract::entry_count`,
  `Register::entry_count` and `Outcome::id()`. A parsed approval can no longer
  be edited after it passed validation. `Contract::approval_for` and
  `approvals_for` still answer whether a crate is approved, but the `Entry`
  they return is opaque outside the crate. (#157)
- `ContractError::BadDate` and `invariants::ErrorKind::BadDate` carry the
  field's `line`. `ContractError` gains `InvalidString`, `DuplicateEntryKey`
  and `InvalidField`. (#157)

### lgwks_deps Fixed

- Both registers read one strict TOML subset. A repeated key in an entry is
  refused and names both lines instead of letting the last write win. Values
  are single-line basic strings; no value may contain a control character,
  raw or escaped, and literal, multiline, bare and trailing-token values are
  refused. `source`, `allowed_kinds`, `allowed_consumers` and the
  crate/owner identifiers are closed vocabularies. `approved_on` must be a
  real Gregorian date, so `2026-13-45` and `1900-02-29` are refused. (#157)
- Workspace metadata in which a member has no package record, several records
  or a shared manifest directory is refused as a schema error instead of
  being filtered into an empty or misattributed graph. (#158)
- Metadata collection is tested with an injected fault at every stage
  (capture, spawn, wait, stat, read, kill, reap, unlink). Each stage either
  removes its capture files or leaves them in a retryable `CleanupObligation`.
  (#159)

### lgwks_bot Changed

- Depends on `lgwks_std 0.9` and `lgwks_deps 0.3`. `lgwks_bot::json` is
  `lgwks_std::json`, so its serializers now accept unsized values and its
  decoders borrow from their input, as described above.

### lgwks_macros Changed

- Depends on `lgwks_deps 0.3`. The `script!` syntax is unchanged.

## [lgwks_std 0.8.0 / lgwks_ast 0.3.0 / lgwks_deps 0.2.0 / lgwks_bot 0.6.0 / lgwks_macros 0.1.0] - 2026-09-30

Every crate that moves takes the minor position, because each carries a break
against what crates.io holds. `lgwks_std` changes `http`, `time` and `glob`
signatures. `lgwks_ast` changes the return type of `try_detect_content`.
`lgwks_deps` makes the public fields of its metadata records private.
`lgwks_bot` re-exports `lgwks_std::json` and depends on `lgwks_deps`, so it
moves with both of them. `lgwks_macros` is new and must be uploaded before
`lgwks_bot`, which depends on it.

### lgwks_std Added

- `fs`: `walk_dir_bounded` and `walk_dir_tolerant_bounded` under
  `WalkLimits` (entries, per-directory entries, path bytes, omissions), charged
  before retention; `WalkReport::policy`, `is_complete_within_policy`,
  `budget_exhausted` and `into_parts`; `WalkFailure` keeps the path, stage and
  original I/O error; `OmissionStage` gains `RootResolution` and
  `ResourceBudget`. Walk output paths are absolute under the canonicalized
  root, and a descendant reached through a symlink keeps its logical path.
- `fs::capability::Dir` (feature `fs-raw`): handle-relative access beneath one
  opened directory (`openat`/`statat`/`unlinkat`/`mkdirat`/`readlinkat`), one
  path component per call, no implicit symlink following, owner-only modes
  (0o600 / 0o700) and close-on-exec descriptors. `entry_names` is Linux-only;
  on non-Unix targets every call reports `Unsupported`.
- `hex::decode_into` validates the whole input and the exact destination length
  before writing, so a refusal leaves the destination unchanged.
- `online::probe_resolved` probes caller-resolved addresses under one budget.
  Each candidate is offered an equal share of what remains, so a blackholed
  address no longer starves the next.
- `glob::GlobPattern` (`compile`, `compile_with_dialect`, `is_match`,
  `is_match_with` + reusable `GlobScratch`) with typed `PatternError`s.
- `time::format::unix_parts_lossy` / `from_unix_parts_lossy`, the explicit
  names for the old infallible behaviour.

### lgwks_std Fixed

- `similarity::Geometry::score` accepts `BoundingBox` as documented, not only
  `[f64; 4]`.
- `retry::RetryPolicy` backoff reaches the exact `max_delay` cap for every
  retry index (it overflowed past attempt 31) without work proportional to the
  index; jitter covers the full `Duration` range.
- Percent-decoding errors report original-input byte offsets; UUID and LEB128
  decoding refuse malformed and non-minimal input with offsets.
- Glob matching is O(N) per token over Unicode scalars, not the claimed
  O(M×N) with byte semantics.
- A signal (`EINTR`) during an HTTP exchange is `FailureKind::Interrupted` at
  every stage, never a transport outage and never proof of no effect; an
  `HTTPS://` URL's TLS failures are filed under the TLS stage.

### lgwks_ast Breaking

- `try_detect_content` returns `Result<ContentDetection, ParseError>`
  (`NoMatch`, `Unique(Language)`, `Ambiguous`) instead of
  `Result<Option<Language>, _>`, so two clean candidates are no longer reported
  as no match. Each distinct compiled candidate is parsed once, and a parser or
  budget refusal is an `Err`, never negative evidence.
- `AstMetrics` gains `complete`, `node_limit` and `stop_reason`
  (`InspectionStopReason`), so a partial walk is visible.

### lgwks_ast Added

- `diagnostic`: positioned `Diagnostic`s (`Pos`, `Span`, `Severity`, `render`)
  and `diagnostics` / `recovery_count` over a tree.
- `tree_diagnostics` returns at most `MAX_SYNTAX_DIAGNOSTICS` (32)
  `SyntaxDiagnostic`s with recovery kind and original-source byte spans; a
  truncated report keeps the earliest, in source order.
- `Language::of_shebang` resolves extensionless scripts, reading at most
  `MAX_SHEBANG_BYTES`.

### lgwks_deps Breaking

- The metadata records' public fields are private behind getters:
  `consumer()`, `package()`, `requirement()` and `name()`.

### lgwks_deps Added

- Features `bevy-app`, `bevy-time` and `bevy-state` now export
  `bevy_app`, `bevy_time` and `bevy_state`; before, they were enableable and
  unreachable.
- Cargo metadata collection returns a `CleanupObligation` when it cannot
  confirm the child is gone; `retry_cleanup` retries it, and dropping it makes
  a bounded last attempt to kill and reap the child instead of leaking it.

### lgwks_bot

- Moves with `lgwks_std` 0.8 and `lgwks_deps` 0.2 (it re-exports
  `lgwks_std::json`). The `lgwks_bot` and `lgwks_macros` entries below ship
  in this release.

### lgwks_bot Added

- `script!` and the `script` module (feature `script`, default on):
  orchestration written as indented flows (`each x in xs:`, `within 2s:`,
  `retry up to 3 times, waiting 100ms:`, `together:`, `step`, `for`,
  `if`/`else`, `run`, `give back`, `fail with`). Every flow takes a tenant
  `Scope`; every step has a stable `StepKey` over its tenant and structural
  path; failures are `FlowError`s located at a path and classed as retryable
  or not; every script emits an `ARCHITECTURE` map as text or JSON. `each`
  runs bodies on the calling task, so they may borrow locals.
- The root scope decides what an author should not: `each` with no bound runs
  64 bodies per available core (a typed concurrency literal is refused at
  compile time), and every `retry` under one root shares a retry budget of 10
  plus one per five first attempts (Finagle's `RetryBudget` defaults), ending
  in the new `FlowError::Throttled` instead of a retry storm
  (arXiv:2608.25403, arXiv:2510.03551).
- A flow is `Send` whenever what it holds is: `each` and `retry` store their
  bodies as named future types, never `dyn`, so a tenant's flow can be spawned
  onto the multi-threaded runtime. `retry` bodies take their `Scope` by value.
- An `each` item's scope is `<step>#i` (for example `sync/each:page#39`), and
  item and retry scopes share the stop of their step instead of minting a
  token each: cancelling inside a body ends the whole fan-out, like `break`.
- `bench/orchestration` compares `script!` with `join_all_bounded`, tokio
  `JoinSet`, asyncio `TaskGroup`, Trio, Go `errgroup`, a Node pool and
  Effect-TS on one workload, five runs per cell. `script!` is the only way to
  hold every invariant measured: no body live at return under failure, cancel
  and storm; 130 attempts in a retry storm where the others make 368-5,000;
  and an error that names the failing item. On an idle host it ran 435,000
  items/s median against 334,000 for `join_all_bounded` and 316,000 for
  `JoinSet`, with a higher p99 (12.0 ms against 9.5 ms) and more peak RSS
  (13.6 MB against 7-9.5 MB). An earlier run on a loaded host, whose
  tree was not recorded, measured 267,000 while the hand-written Rust ways held
  about 348,000. It needs 11 lines to their 23-46.

### lgwks_macros 0.1.0 Added

- New crate: the syntax of `lgwks_bot::script!`. Parses indentation from token
  spans, expands each block to one call into `lgwks_bot::script`, and refuses
  unbounded, panicking, blocking or machine-specific code with a compile error
  naming the replacement.

### lgwks_deps Added

- Feature `macro`: `syn`, `proc-macro2` and `quote` (new edge, registered in
  `contract/APPROVED.toml`) re-exported for first-party proc-macro crates.

### lgwks_std Breaking

- `time::format::from_unix_parts`, `unix_parts`, `to_rfc3339`, and
  `time::now_rfc3339` now return `Result` so clock-range failures and
  out-of-range RFC 3339 years are visible. Migrate by handling `?` or matching
  `UnixTimeError` / `FormatError`; the old lossy behavior is available only as
  explicitly named, deprecated `*_lossy` functions.
  Replace `try_from_unix_parts` with checked `from_unix_parts`; replace calls
  that intentionally relied on the old infallible epoch fallback with
  `from_unix_parts_lossy`.
- `time::parse_rfc3339` rejects offset hours above 23, offset minutes above 59,
  leap-second labels, and platform-unrepresentable instants. Its SystemTime
  profile is UTC-normalized, nanosecond-limited, and does not retain original
  offset spelling or `-00:00` provenance.
- `glob::matches` now interprets `?` and character classes as Unicode scalar
  values rather than UTF-8 bytes. ASCII results and literals are unchanged;
  callers that relied on multiple wildcards consuming one multibyte scalar
  should update their pattern. `GlobPattern::compile` offers checked strict
  syntax, while `GlobDialect::Legacy` preserves permissive bracket and `**`
  forms during migration.
- `http` failures are one structured `Error::Failure { stage, kind, cause }`
  with `FailureStage` and `FailureKind` (including `Interrupted` for `EINTR`,
  at every stage); the `Error::Timeout`, `Error::Interrupted` and
  `Error::Transport(String)` variants are removed. Match on `kind` instead of
  display text. The `Options::user_agent` and `Options::headers` fields and
  the `Response::headers` and `Response::body` fields are private: set them
  with `Options::user_agent(value)` and `Options::header(name, value)`, read
  them with `headers()` and `body()`. `Options::redirect_policy`,
  `Response::header_values`, `final_target` and `redirect_chain` are added.
- `http` redirects are explicit (`RedirectPolicy::{NoFollow, Follow,
  FollowAtMost}`, at most ten hops) and are followed by this crate, not ureq: a
  hop to another origin carries none of the caller's headers (ureq forwarded
  every header but `Authorization` and `Cookie`, so an `X-Api-Key` reached any
  host a server redirected to), a same-origin hop drops `Authorization`,
  `Cookie` and `Proxy-Authorization`, and a 307/308 of a POST is refused
  rather than replaying its body.

## [lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13] - 2026-09-28

The first upload of this train. It carries everything below, including the
2026-09-21 cut, which was never published. `lgwks_std` takes the minor position
(0.7.0), not the cut's 0.6.7, because the entries under "lgwks_std Breaking"
rename and remove public items, and Cargo treats 0.6.6 → 0.6.7 as compatible.
`lgwks_bot` 0.5.0 is already a minor bump from the published 0.4.2; it re-exports
`lgwks_std::json`, so it moves with std. `lgwks_deps` 0.1.13 only adds a command
and exposes no `lgwks_std` type, so it stays a patch. `lgwks_ast` does not move.

### lgwks_std Breaking

- `time` no longer re-exports the items of its `calendar` and `format`
  submodules, so `time::to_rfc3339` is now `time::format::to_rfc3339` and
  `time::civil_from_days` is `time::calendar::civil_from_days`. Both submodules
  were already `pub`, so the re-export was a second name for the same item, and
  nothing in the workspace used the short one. `time::parse_rfc3339` and
  `time::ParseError` stay: they are the module's parse half and the error it
  returns, and `time::parse::parse_rfc3339` would stutter.
- `BoundingBox::as_array` is now `to_array`. The method builds the four-element
  array from four separate fields, so the call copies rather than reborrows and
  the `as_` prefix promised a cost it did not have. Same signature, same value.
- `http::get_response` is now `http::get`, so the pair reads `get` / `get_with`
  the way `post` / `post_with` already did.
- `Uuid::from_bytes` is gone, replaced by `impl From<[u8; 16]> for Uuid`. The
  behaviour is unchanged; the conversion is now reachable by a caller that
  bounds on `From<[u8; 16]>` rather than on this crate's naming.
- `Pattern::find_all` returns `impl Iterator<Item = Match<'_>>` and
  `Pattern::split` returns `impl Iterator<Item = &str>`. Code that assigned the
  result to a `Vec` needs a `.collect()`; code that only counted the matches,
  took the first, or short-circuited no longer allocates one item per
  occurrence.
- `Weighted::try_new` is removed. It was an alias for `Weighted::new` with no
  callers anywhere in the workspace.


### lgwks_std Added

- `trace` now includes a default debugger bootstrap: `DebugConfig`,
  `DebugFormat`, `install_default`, `LGWKS_LOG`/`RUST_LOG` filtering, compact
  and pretty local output, and JSON-line output via `LGWKS_LOG_FORMAT=json`.
  The surface stays default-on with `trace`; `#[instrument]` remains out of
  scope because `tracing-attributes` would pull the proc-macro stack into the
  foundation crate.
- `Debug` for `Hasher` (reports bytes written), `Weighted` (reports the
  composition rather than the components) and `task::JoinHandle` (reports
  running / done / panicked / taken). All three were unprintable, so a consumer
  that wanted to log one wrote a wrapper that then had to change whenever the
  type did.


### lgwks_bot Breaking

- **The crate root stopped carrying three sets of aliases.** `lgwks_bot::gh`,
  `lgwks_bot::net`, `lgwks_bot::fs` and six others named `domain`'s modules a
  second time; `lgwks_bot::frontier::Frontier` and ten siblings repeated what
  `pub mod frontier` already exposed; and `lgwks_bot::rt::Builder` was a third
  path to a type the root already re-exported. Each was a name for something
  already nameable one segment away, and the only user of the nine domain
  aliases anywhere in the workspace was a single doctest. The paths are
  `lgwks_bot::domain::gh`, `lgwks_bot::frontier::Frontier`, and either
  `lgwks_bot::Builder` or `lgwks_bot::rt::runtime::Builder`.
- `broker::prepare_dispatch` and `broker::Prepared` are `pub(crate)`. The only
  caller is the dispatch path in `ecs`, a sibling module, which is what
  `pub(crate)` is for. `Prepared::authority` and `Prepared::ack` are gone with
  them: `into_parts` returns both halves, and it is what the caller uses.
- **Effect dispatch is authorized and durable, and the scope is required.** Every
  `Bot` is built around an `EffectScope`: the run's `EffectIdentity` (which run
  this is, the `EnvironmentId` it acts on, and the `FlowRevision` it came from),
  the `Broker` that owns the environment's generation, and the `EffectJournal`
  the dispatch is written to. Both builder entry points refuse to build without
  one, as `BotError::IncompleteSpec { field: "effects" }`. An identity the caller
  did not choose is one it cannot recover against, so there is no default and no
  implicit in-memory scope.
- `Bot::resolve_effect` takes an `EffectKey` instead of a work id and a
  caller-supplied revision. `EffectKey` is now the settlement identity: `Bot::pending`
  hands one back, and evidence is compared by `ActionDigest` and `AttemptId`.
- `RetryFacts::with_authority(bool)` is now
  `RetryFacts::with_live_authority()`. Authority defaults to not live and the
  refusing direction is the default, so `true` was the only setting a caller
  could meaningfully pass: `with_authority(false)` spelled out the default under a
  name that read like a choice, and the parameter made every call site decode
  which way round the boolean went. Removing it removes the question.


### lgwks_bot Added

- **`EffectJournal::compare_and_append_async`, `FileJournal::open_with_stalled_storage`,
  `FileJournal::release_storage`, and `FileJournal::storage_gate` visibility.**
  The async half of the journal now shares the sync half's frame preparation and
  its authoritative length-check, write, `sync_all` path, so the two cannot
  drift into writing different bytes. The async surface is additive: the sync
  signature and every on-disk frame and chain format are unchanged, so no
  existing store needs migrating. `FileView` gives lock-free fence checks
  without a second open, and `FileJournal` drives its writes from a single
  owner thread over a capacity-one request slot, which is what makes ordering
  a property of the type rather than of a caller.
- **An ambiguous write is poisoned, never reported as a clean failure.** When a
  waiter awaiting an append is dropped, the owner learns of it and poisons the
  handle with the reason. Before this, a dropped waiter was invisible to the
  owner: it completed the write, and a later append on the same handle would
  have proceeded as though nothing had been uncertain. The repair is a reopen,
  which replays to the same facts.
- **`BotError::EffectUnrecorded`, `DispatchCertainty::Occurred`, and
  `TransitionHold::RecordingFailed`.** A post-effect journal failure now
  carries the exact `EffectKey` and the observed `Applied`/`NotApplied` fact
  instead of being retyped as a pre-dispatch refusal. `Occurred` answers "the
  effect definitely happened" — the arm `Refused` (nothing left the process)
  and `NotDelivered` (a retry is a retry) both lied about. All three types are
  `#[non_exhaustive]`, so this is additive.
- **The locator ladder: `Anchor`, `Ladder`, and `RecognitionVector::recognize_with_ladder`.**
  A ladder is the search order `docs/general-bot-fold.md` §3.2 sequences as step
  4: stable `id`, then `data-testid`, then ARIA role, then visible text, then a
  class path. `ElementFacts` now carries the anchors a snapshot offers
  (`with_anchor` / `anchor`), because a ladder that cannot be joined to its
  candidates is a half-built path.

  The walk decides by the rung's own anchor and by nothing else. One hit
  resolves; two or more is `Ambiguous` and is **not** retried at a weaker rung —
  two elements that share an `id` are not disambiguated by the fact that only
  one of them happens to say "Submit" today. Zero hits falls through, and the
  final `Absent` reports the strongest near-miss any rung observed rather than a
  bare zero.

- **`EffectScope::ephemeral()`, and a run identity a host can mint.** Building a
  bot with no host, no persisted history and no flow document meant writing an
  identity out by hand: a `RunId` and an `EnvironmentId` as literal hex, a
  `FlowRevision` as a literal digest, a registered broker and an in-memory
  journal, in that order, in every test and example. `EffectScope::ephemeral()`
  mints the two identities from OS entropy, registers that environment, and
  pairs them with a `MemoryJournal`.

  It is a capability rather than a convenience because of what the journal
  refuses: `MemoryJournal` reports `DurabilityPromise::Ephemeral`, and
  `EffectJournal::admit_external_handoff` returns `JournalError::PromiseUnmet`
  for it — so an effect that would leave the process fails at the boundary
  instead of proceeding on a record that cannot outlive the process that wrote
  it. The difference between this and "no journal" is that this one says so.

  `RunId::mint()` and `EnvironmentId::mint()` are public for the same reason
  from the durable side: the module has always said the run identity is
  *generated* before the first admission, and until now the only way to generate
  one was to supply hex. `ActionId` deliberately gains none — the crate already
  derives it from a bot's structure, and a second way to make one value is the
  thing the dependency and API doctrine both refuse. `FlowRevision` is a fixed
  domain-separated constant in the ephemeral case, because an ephemeral run has
  no flow document and minting a revision would assert a content change that did
  not happen.
- `MintError` and `EphemeralError`, both `#[non_exhaustive]` and both carrying
  their cause rather than flattening it to a string.
- Feature `ephemeral`, **opt-in**, which turns on `lgwks_std/random`. It authors
  no edge of its own: `getrandom` is owned by `lgwks_std` under
  `contract/APPROVED.toml` and this is a feature of a dependency the crate
  already has. `lgwks_std` enforces INV-RANDOM-ONE-SOURCE, which is why there is
  no cheaper fallback here — a run id derived from a clock, a pid or a counter
  is the collision that invariant exists to refuse.

  It is not in the default set, and the reason is the target rather than the
  cost. Minting needs OS entropy, `lgwks_std::random` is linux/macOS/windows
  only and refuses the rest with a `compile_error!`, and this crate's default
  feature set is built for `wasm32-wasip1` by the WASI boundary job. A
  default-on `ephemeral` makes the default set fail to build on a target the
  crate supports. `signal` is host-only in the same way and is default-off for
  the same reason. `full` includes `ephemeral`, and the runner step tests
  `--features full`, so the ephemeral tests execute in CI rather than only
  compiling.


- **Write-ahead dispatch.** `IntentAdmitted` and `DispatchPrepared` are committed
  to the journal before the effect leaves the process, and `Broker::revalidate`
  re-checks the environment generation at the handoff, with no await between mint
  and check.
- **A restart continues the record rather than the process.** A bot
  reconstructed against a journal holds an attempt whose outcome was never
  established instead of resending it, and retires an attempt whose effect
  landed. Two defects the new tests found are fixed: a restart re-minted
  `AttemptId::FIRST` and so reproduced a key the journal had already walked past,
  which refused every `NotApplied` retry as `OutOfOrder`; and a crash after a
  landed effect left the run unable to continue past that entry at all.
- `Debug` for the public surface. Every public type in the crate now implements
  it except the seven `*Resolver` companions rkyv generates for the three
  macro-invoked families in `effect.rs` (`id_role!`, `counter_role!`,
  `digest_role!`): `RunIdResolver` through `ActionDigestResolver`. The named
  types and their `Archived*` companions do have it, so the
  `#[rkyv(derive(Debug))]` attribute reaches the archived type and stops short of
  the resolver. Derived where a derive is the right rendering, manual where it is
  not, with the reason on the impl. `SemanticResolver` uses
  `finish_non_exhaustive` rather than acquiring an `E: Debug` bound that no
  `Embedder` is required to satisfy. `Session` prints counts rather than the
  transcript entry by entry, and the `bevy_ecs`-backed types do not descend into
  a schedule that has no `Debug`.
- `#[must_use]` on `eval::{Changed, Below, Above}::new`, which their three
  sibling constructors already carried. `must_use_candidate` does not reach
  `-> Self` constructors, which is how the inconsistency survived a green build.


### lgwks_bot Breaking

- `Execute::effect_lifetime` declares whether the action's effect reaches
  outside this process. The default is `EffectLifetime::External`, the
  conservative reading of an unclassified handoff: an action that is only
  local says so. An ephemeral journal now refuses an external effect on the
  actual dispatch path — `Effects::prepare` calls
  `EffectJournal::admit_external_handoff` and checks the `DispatchPrepared`
  acknowledgment's promise against what the handoff requires, instead of
  discarding it. A journal that advertises `ProcessCrash` and acks `Ephemeral`
  is refused. `MemoryJournal` stays `Ephemeral` and stays useful for
  explicitly local work.
- `Observe::Output` must implement the new `InputIdentity`, alongside the
  `PartialEq` it already needed. The dispatch digest used to bind the
  process-local `Revision` counter, which reopens at 1 after every
  reconstruction, so a restarted bot read a *different* request as already
  applied and silently omitted it. The digest now binds the admitted input's
  content identity. `InputIdentity` is implemented for the fixed-width
  integers, `bool`, `String` and `&str`; a caller whose output is something
  else writes the identity bytes themselves. Two events with equal payloads
  stay distinguishable by tagging them with `EventId`, whose id is part of
  both `InputIdentity` and `PartialEq` — content equality is not event
  identity.

### lgwks_bot Added

- `InputIdentity` and `EventId`. The first names an admitted observation for
  the purpose of a dispatch digest; the second tags a payload with an event id
  so two equal payloads are two events.

### lgwks_bot Fixed

- `process` now enables `time`. `ProcessSpec`'s deadline is enforced with
  `rt::time::timeout`, and `rt::time` is behind the `time` feature, so a
  `--no-default-features --features process` build did not compile
  (issue #131). The CI feature matrix now has a `Clippy process-only` lane
  so that combination cannot rot again.

- An ephemeral scope enforces its advertised external-effect refusal.
  `Effects::prepare` used to discard the `DispatchPrepared` acknowledgment as
  `_ack` and never called `admit_external_handoff`, so a local in-memory
  ladder admitted an effect that outlived the process. Durability admission is
  now on the handoff path and the acknowledgment is checked, not the
  advertisement (`tests/durable_dispatch.rs::an_ephemeral_scope_refuses_an_external_effect_before_it_runs`).
- Action identities are length-framed and index-portable. `derive_action_id`
  used to concatenate `bot` and `domain` around `usize::to_le_bytes` with no
  length prefix, so `"ab"`+`"c"` and `"a"`+`"bc"` derived the same `ActionId`
  and a 32-bit and a 64-bit target derived different ones for the same bot.
  Fields are now `u64`-width length-prefixed; the domain separators are
  `action-id.v2` and `action-digest.v2`.
- Assembly rejects a recovered key from another flow or another environment,
  not only another run. Folding in a foreign journal is how a bot dispatches
  an action whose earlier attempt belongs to a different document.
- A recovered unknown is a barrier in front of a false condition, not
  something a new observation can skip past. `plan_chain` used to evaluate
  the current value first and emit `Decision::Skip` for an entry whose action
  a journal already recorded as dispatched with no outcome; `run_chain`
  marked that entry `Skipped` before it ever consulted `effects.blocks`, so
  the successor ran and the tick could return `Ok` while `pending` still
  named the unknown. A changed source is not evidence that the earlier
  effect did not occur. The barrier is now checked before the condition, a
  `Skip` that somehow reaches the walk for a blocked action stops the chain
  instead of burying the hold, and `pending` / `first_unresolved` /
  `unresolved_count` are one fold over the declared work — so a recovered
  unknown is reported whether or not a transition exists, and the three
  reports agree (`tests/durable_dispatch.rs::a_recovered_unknown_blocks_a_false_condition_and_its_successor`
  and the unattempted false-condition control in `ecs::tests`).
- Live settlement is journaled before it is acknowledged, and a repeat of the
  same evidence is idempotent across a restart. `Ledger::settle` used to move
  only in-memory state while `settle_recovered` appended `OutcomeObserved`, so
  a process that settled an effect and then died left a journal that recorded
  the dispatch and nothing else: the next bot held the attempt forever, and a
  caller repeating the acknowledgement it had already given was told
  `NoSuchWork`. Settlement is now one write (`Effects::ensure_outcome`) shared
  by the live and recovered paths, decided by a read-only
  `classify_settlement` and applied by `apply_settlement` only when the live
  slot still needs moving — an abandoned entry reopened by confirmation that
  its effect did not land is exactly that case, and answering `Duplicate`
  before the move is how that reopen stopped happening. When the live slot has
  nothing left to move, the journal's record answers: same evidence is
  `Settled::Duplicate`, the other evidence is `Settled::Contradicted`, and only
  a key the journal never recorded is `NoSuchWork`
  (`tests/durable_dispatch.rs::a_live_settlement_is_journaled_before_it_is_acknowledged`).
- Locator ladder candidates now share the flat recognizer's frame and kind
  eligibility gate, while anchor mismatches preserve measured fingerprint
  evidence instead of reporting a falsified zero score.
- Process supervision now establishes its process-group guard before scheduling
  the child and reports bounded descendant cleanup evidence instead of treating
  the leader's exit as proof that the process tree is gone.
- A post-effect journal failure is no longer reported as a pre-dispatch
  refusal and terminal abandonment. `run_chain` used to replace the action's
  known outcome with `BotError::EffectRefused` when the `OutcomeObserved`
  append failed, so `dispatch_certainty()` answered `Refused` and
  `RetryClass::Never` became `Abandoned` — a controller that read that as
  "safe to replan" would issue a new logical action and duplicate the
  external operation. Execution outcome, persistence acknowledgment,
  verification and cleanup are now separate dimensions: the entry is held as
  `RecordingFailed` with its key and evidence, and a retry of the recording
  never re-enters the action. Append failures *before* the action stay
  `EffectRefused`, which is the negative control.
- Observation fingerprints no longer commit before the values they describe.
  `poll_sources` used to publish each source's digest as it polled, then
  `observe_fold` committed payloads all-or-nothing; a failed sibling poll left
  the successful sources' digests advanced over values the fold refused to
  hold, and the next tick skipped that uncommitted work forever. Digests are
  now staged in a per-tick `Candidates` map and published to `Fingerprints`
  only when the fold admits every observation. An aborted fold discards the
  candidates. `Fingerprints` also never leaves the world during a tick, so a
  tick dropped mid-poll can no longer empty the map and permanently disable
  caching. Regression tests cover the issue #99 acceptance list: a failed
  sibling on first poll, a committed source that moves during a sibling
  failure, several failed siblings, a held transition plus a newer candidate,
  and cancellation during a source poll.
- `interface::RecognitionVector` and `language::LanguageResolver` each carried a
  comment explaining why they were deliberately not `Debug`. The first stopped
  being true when `lgwks_std`'s `Weighted` gained an impl that reports
  composition rather than addresses. The second was wrong when it was written:
  the alias table is a `BTreeMap`, so a derived `Debug` prints it in key order
  and is stable across runs. Both now derive `Debug`, and both comments say what
  is true.
- `#[must_use]` on `EcsBuilder::observe`, `EcsObserveBuilder::on` and
  `EcsObserveBuilder::observe`. Each one consumes a builder and hands back the
  next, so a discarded result silently drops every `on` declared up to that
  point. A sweep of the crate's public surface found 103 methods returning
  `Self`: 92 carry `#[must_use]`, and the 11 that do not are all `new`
  constructors. `EcsObserveBuilder::on` was the only consuming builder without
  it. The two `observe` methods return `EcsObserveBuilder<_>` rather than `Self`,
  so a scan for `-> Self` does not see them, and neither of those carried it
  either. `must_use_candidate` reaches none of the three, which is how they
  survived a green build.
- A returning landed event retires rather than refusing or re-entering. Live
  `OutcomeObserved` is now written through `Effects::ensure_outcome`, which
  folds `Applied` into the in-memory applied set, so `applied_in` can see it.
  A raw append left that set empty: the same `EventId` came back after another
  event, minted `AttemptId::FIRST` again, and the journal answered `OutOfOrder`
  after the entry was already marked `Unrecorded` — a false barrier instead of
  a retirement (`tests/durable_dispatch.rs::a_returning_landed_event_is_retired_not_refused`).
- A handoff refused before the action ran no longer leaves a false
  `Unrecorded` barrier. `begin_attempt` used to move `record.begun` and
  `EntryState` before the journal writes, so a `PromiseUnmet` left the entry
  claiming an effect might be live. The write-ahead pair is committed first and
  the memory moves only after its acknowledgments are strong enough
  (`tests/durable_dispatch.rs::a_refused_handoff_leaves_no_unrecorded_barrier`).
- A weak per-append acknowledgment is refused before an external handoff can
  run. A weak intent acknowledgment leaves nothing prepared. A weak
  `DispatchPrepared` acknowledgment may already be committed, so the kernel
  returns `PromiseUnmet` and recovery preserves `OutcomeUnknown` rather than
  inventing `NotApplied`
  (`tests/durable_dispatch.rs::a_weak_ack_does_not_record_dispatch_prepared`).
- Durable outcome settlement binds a receipt to the exact `OutcomeObserved`
  position before releasing work. An unrelated or forged receipt holds
  `RecordingFailed` and retries recording only; it never re-enters the action
  (`tests/durable_dispatch.rs::an_unrelated_outcome_receipt_is_refused_without_reentering_the_effect`).
- A ladder-complete `OutOfOrder` is idempotent success only when the *same*
  evidence is already recorded. `ensure_outcome` used to treat any such refusal
  as success without looking, so a contradictory `Applied` could be
  acknowledged. `RecordingFailed` retries now go through the same
  `ensure_outcome` write, which is what makes an ambiguous commit-then-error
  retry land rather than loop.
- `classify_settlement` checks the environment half of ownership identity, not
  only run and flow. A key cloned from the pending key with a foreign
  environment used to settle the live entry and journal the foreign key.

### lgwks_bot Documentation

- Five citations in `docs/guides/lgwks-bot/` were re-anchored to the lines this
  change moved in `ecs.rs`. `getting-started.md` also credited the
  `Observe::Output: PartialEq` requirement to `EcsBuilder::observe`, which
  carries no such bound; it lands on `EcsObserveBuilder::observe`, the call that
  closes a chain, and the citation now points at the bound.
- The `ephemeral` scope grew `ecs.rs` by 113 lines and `spec.rs` by four, and the
  guides cite both by line, so all twenty-four citations under them landed on
  whatever now occupied the number. Nine were caught, having resolved to a
  closing brace, a blank line or an empty `///`. The other fifteen resolved to a
  plausible line that was not the one cited, which is the class the checker
  deliberately does not judge. All twenty-four were re-anchored to the line they
  named before the growth, matched by the text of that line rather than by the
  numbers the offset would predict; the five whose text is not unique in the
  file were confirmed by reading them.

### lgwks_deps Added

- `lgwks-deps debug [PATH] [--json]`, a cargo-doctor-style command for the
  `lgwks_std::trace` lifecycle. It installs the default debugger in its own
  process, emits lifecycle events, inspects `crates/lgwks-std/Cargo.toml`, and
  refuses if the SDK debugger no longer rides the default `trace` feature.

### Repository Added

- `scripts/check-std-first.py`, and a CI step that runs it. `lgwks-deps check`
  enforces the manifest half of the `std`-first rule and cannot see the other
  half, which lives in the source: a `use` of a crate no manifest declares
  (buildable only because something else in the graph re-exports it), and a
  capability hand-rolled beside the `lgwks_std` module that already provides it.
  Every crate reached past `std` and the four surfaces is reported with the
  approval record behind it — owner, capability and the reason the approver
  wrote — so `--justify` prints the answer to "why is this edge here" instead of
  leaving it to a reviewer's memory.

  It carries four written exemptions, each pinning the exempted line's text as
  well as its number, because a `path:line` key alone would silently cover
  whatever later occupied that line. A moved exemption is a `STALE EXEMPTION`
  finding, not a silent yes.


### Repository Changed

- **CI runs the matrix once per change instead of twice.** `on.push` is scoped
  to `main` now, because a pull request already runs all eleven jobs for its head
  commit and the branch push ran every one of them again for the same tree. A
  `concurrency` group supersedes a run on a branch that has been pushed again,
  and never supersedes a run on `main`.
- **The rustdoc gate is runnable outside CI.** `scripts/doc-lanes.sh` holds the
  four lanes the Docs job runs, and the job, `AGENTS.md` and `docs/releasing.md`
  all call it instead of restating the commands. Which links break depends on which features are on,
  so the gate is four lanes and not one command, and none of them was reachable
  from the `Checks that must pass` list, which named no doc build at all: a
  broken intra-doc link is a warning rather than an error, so the crate built
  green and the break surfaced only after the push.

### Repository Changed

- **A citation that moves is now a failure rather than a silent pass.**
  `scripts/check-doc-citations.py` pins the text of every cited line in
  `scripts/doc-citations.lock`, so a citation whose line no longer holds what it
  was pinned to fails the Docs job. The check before this one rejected only a
  citation that landed on a bare delimiter, which is a floor: on 2026-09-21
  `ecs.rs` grew by 1,070 lines, and 22 of the 25 citations under it kept
  resolving onto unrelated code while CI reported green. Demonstrated on a copy
  of this tree with `cap.rs` grown by one line: the previous check exits 0 with
  "107 citations resolve", this one exits 1 and names all seven that moved.
  `--update` rewrites the lock and prints every line whose text changed under a
  citation and which page cites it, so the review is the diff.

### Repository Fixed

- `crates/lgwks-bot/src/domain/mod.rs` is deleted. It declared the nine domain
  modules, but `lib.rs` declares `domain` as an inline module, so that file was
  never in the module tree: the build's dep-info lists every `src/domain/*.rs`
  and not it. It was a second copy of the module list that nothing compiled and
  nothing kept in step.

## [2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above]

### Proofs

- **`proofs/bot-spec.sml` states and proves seven theorems about the
  spec/condition layer**, machine-checked by the HOL4 kernel: totality of the
  loop-free evaluator (`T1`), budget monotonicity (`T2_MONO`), budgeted-verdict
  agreement (`T3`), **capability-gate soundness** — every entry that fires is one
  the grant permits (`T4`) — gate monotonicity in the grant (`T5`), and
  **record/replay agreement** (`T6`). `T7` proves that the loop constructor
  admits no value at any finite budget, which is the formal reason `T1`'s
  totality is a property of the loop-free language and does not survive the
  extension. `proofs/proof-run.log` is the unedited run; no `mk_thm`,
  `new_axiom` or `cheat` appears anywhere in the script.
- **It is not a refinement proof, and the README says so first.** The theorems
  are about the script's own hand-written datatypes, not the Rust; nothing here
  proves that `spec.rs` implements the model. The claim is therefore split: the
  *design* has these properties (proved), and the *implementation* cannot express
  the states that would break them (argued from type construction — a private
  `Auth` constructor, a `Cap` newtype, a grant written only in `assemble`). The
  model's abstractions are enumerated too: `Contains` is an opaque literal,
  `Env` is total, effects are not modelled, capability shortfall is not modelled,
  and the runtime is absent entirely. A bug in the model is a bug in what is
  proved.

### Benchmarks

- **`bench/` measures `lgwks_bot` against a hand-rolled baseline doing identical
  work**, gated on the bot and the baseline agreeing on both effect counts and
  condition-evaluation counts. That second axis is load-bearing: gating on
  effects alone let a baseline that returned `entries` as an integer instead of
  looping over them report a 2937x ratio where the counted-work gate measures
  72.38x at the same scenario. The rig is its own Cargo workspace root so the
  dependency contract never sees it — `lgwks-deps check .` still reports the four
  crates.
- **The measured result is that the bot is 72x–256x slower than the loop, and
  ~98% of a tick is the poll and change-detection phase before any decision is
  made** (poll-only 2540.0 ns/tick against steady 2582.1 ns/tick at 64 chains).
  Source cost is linear; change detection now pays 4.23x, because both changes
  below made a quiet tick cheaper without making a churning one cheaper, so a
  workload that never goes quiet pays about four times what a quiet one does.
- **The rig found a defect, and the fix it was measured against has landed.**
  `Auth::check` grew 3794x for a 128x increase in required capabilities, reaching
  12.9 µs per call at 128 capabilities, because coverage was a linear scan of
  granted names per required name. The sorted-and-deduplicated proof with a
  binary search replaced it, and the same rig measures growth of 750.5x and
  3.64 µs at 128. The bench directory measures and does not touch crate code.
- **These are the figures for the tip of this release, and every one is paired
  within a run.** `bench/README.md` carries the method, the drift control and the
  attribution between the two changes; `bench/results.json` is the committed run
  they come from.

### Decision

- **`lgwks_bot` is relicensed to MPL-2.0**, from Apache-2.0. The bot is the
  artefact the rest of the estate embeds, and a permissive licence on it lets
  anyone modify it and ship the modifications closed with no later release able
  to recover that. MPL-2.0 is file-level copyleft: use stays unrestricted
  including in proprietary products (MPL-2.0 §3.3), and only modification of the
  MPL-covered files carries the §3.2 source obligation. `lgwks_std`, `lgwks_ast`
  and `lgwks_deps` remain Apache-2.0.
- **No published version changed, and the new terms land at `lgwks_bot` 0.5.0.**
  Licences are not retroactive; every version on crates.io including `lgwks_bot`
  0.4.2 stays Apache-2.0. The release this section is part of is the first cut
  from this tree under MPL-2.0, so 0.5.0 is the version the terms take effect at.
  The four dependent repositories pin exact published versions and none is
  affected until it moves.
- **The repository root now carries the MPL-2.0 text.** There was no root
  `LICENSE`, so the repository's own licence was unstated while the bot's was
  not, and a reader arriving at the tree rather than at a crate had nothing to
  read. The root file covers the documentation, the scripts and the CI
  configuration; each crate is still governed by the `LICENSE` in its own
  directory, so `lgwks_std`, `lgwks_ast` and `lgwks_deps` remain Apache-2.0 as
  consumed artefacts. `LICENSING.md` states which file governs what.
- **`LICENSING.md` records the model**, including the commercial licence that
  grants relief from §3.2 for organisations that need to modify the bot without
  publishing those modifications. Offering two licences requires holding rights
  in every contribution, so a contributor licence agreement has to be in place
  before a non-trivial `lgwks_bot` contribution can be merged.
- **`lgwks_bot` is closed to outside contributions, and the contributor licence
  agreement that would reopen it is deferred.** A patch sent today has no path
  to merge, so `CONTRIBUTING.md` and `LICENSING.md` now say *closed* rather than
  *not merged until an agreement exists*. The earlier phrasing left a
  contributor to send a patch that would wait on a decision no one has made; the
  block is stated as a refusal on arrival instead. It lifts when an instrument is
  chosen and recorded in `LICENSING.md`. The other three crates are unaffected.
- **A capability refusal states the whole shortfall, in real time, and derives
  its own repair** (owner direction, 2026-09-21). The check returned at the first
  ungranted capability, so repairing a bot was a loop: grant what you were told,
  rebuild, be told the next word. Each individual refusal was true and the
  sequence was still the wrong instrument, because the gate had already computed
  the whole difference — the required set minus the granted set — and reported
  only its head. `Deficit` is that difference reported in full, `Shortage` names
  the domain that declared each requirement, and `Deficit::to_grant_set` turns
  the diagnostic back into the grant set that closes it, so the repair is derived
  rather than transcribed. This is the shape the owner asked for explicitly:
  isolate what is missing at the point of the attempt, hand back the missing
  pieces, and continue — not a build loop that refuses one word at a time.
- **A run-time capability hold was designed, built, and then removed as
  unreachable.** The step after a total refusal is the natural one: deny an
  action at run time, hold the entry, let the caller supply the capability, and
  continue from that point rather than restarting the chain. It cannot be
  reached, and the reason is structural rather than incidental. `assemble` admits
  every declared requirement before the world exists, and every per-call proof is
  minted from **the same list** — `run_any` issues
  `grants.issue(self.0.required_caps())` and the action checks
  `call.0.check(self.required_caps())` — so the declaration and the check cannot
  disagree, and nothing narrows `Grants` after `assemble`. `Auth::check` cannot
  fail inside a running bot. The hold was removed rather than shipped behind a
  contrived test, and the finding is recorded in
  `docs/guides/lgwks-bot/authority.md` with its line citations.
- **The gap the hold was reaching for is the opposite one, and it is real: an
  action that declares `&[]` and performs a side effect passes the gate
  silently.** The gate checks the declared set and `&[]` is trivially covered, so
  "this action needs nothing" and "this action's author never said" are the same
  value and the crate cannot distinguish them. Every guarantee about authority is
  a guarantee about capabilities a domain *declares*. Closing that, or making a
  run-time hold meaningful, is a design change rather than a repair, and neither
  is made here.
- **Four `lgwks_bot` questions were put to the project owner on 2026-09-21 and
  decided.** They are recorded here because each was previously stated in a
  first-party document as *open*, and those documents now say what was decided.

  - **The scheduler is a first-party `lgwks_std` module**, not zed's vendored
    `scheduler` crate. `docs/bot-on-ecs.md` §9 and §10 reserved the choice,
    because the doctrine and the instruction to reuse existing open source point
    at different rungs. It was taken against a finding recorded in full in §9:
    the tick is already deterministic by construction, so a seeded scheduler has
    no caller until the recorder lands. One consequence is named there too — a
    seeded PRNG needs a carve-out from INV-RANDOM-ONE-SOURCE, argued in the
    module rather than assumed at the call site.
  - **The `Time<Virtual>` clock root (§10 step 4) is closed as already
    satisfied.** Two time layers exist and both are tested: `lgwks_std::time`
    (RFC 3339 and calendar arithmetic, INV-TIME-PURE) and `lgwks_bot::rt::time`
    (`sleep`, `timeout`, `interval`, `Instant`). No `Clock` trait or mock time
    source exists anywhere in the workspace, so the two `Instant::now()` reads in
    `rt::supervise` can still only be tested by genuinely waiting. That is
    accepted and stated, not overlooked.
  - **The `from_spec` gap is a gap, and the registry is scheduled work.** The
    crate doc argued that the absent materializer was a deliberate design
    position; that reading was rejected. `experience/invariants/sdk.yaml` records
    the adjudication. Two constraints bind the implementation, because they are
    what made the absence defensible: grants keep coming from a caller-held
    `GrantSet` and never from the spec, so wire data cannot choose what a bot
    reaches, and there is still no `bot!` proc-macro.
  - **No release is cut yet.** Five public modules (`session`, `language`,
    `semantic`, `interface`, `frontier`) and the `lgwks_bot` MPL-2.0 relicense
    are on `main` and unpublished. They ship in one release once the registry and
    the scheduler have landed.

### lgwks_std Added

- **`wire` re-exports the `rkyv` crate, so a consumer crate can derive against
  it.** The module re-exported the three derive macros but not the crate those
  macros expand against, and the generated code names `::rkyv::…` absolutely. A
  type outside `lgwks_std` that derived `Archive` through this module therefore
  failed with `cannot find 'rkyv' in the crate root` before it ever reached a
  layout question, which made the estate's binary surface usable only by its own
  tests. A consumer now writes `#[rkyv(crate = lgwks_std::wire::rkyv)]` and
  derives the macros from here as before.
- **`Digest` carries the archive derives behind `wire`.** Thirty-two bytes with
  no indirection, so it archives in place and any record that contains one
  reaches it without a pointer — which is the property that makes the journal's
  chain head a value a reader can compare against without decoding the record.


- **`ron::Error` and `ron::error::SpannedError` are re-exported.** The `ron`
  module's own functions returned them and no caller could name them, because
  the `ron` crate is an optional dependency and nothing re-exported the types.
  A facade that returns a type has to let a caller write it down, which is the
  same reason `wire` re-exports `rkyv`. Additive.

### lgwks_bot Breaking

**There is no longer any public API that starts concurrent work or a process and
hands the caller something droppable.** A `JoinHandle` a caller can drop is a
task whose outcome nobody ever sees; a `Child` is the same shape with a
process attached. Both were reachable, and both are gone. The only way to start
concurrent work is `Supervisor`, and the only way to start a process is a
supervised one. Nothing is weakened to pay for it — the guarantees move from
convention to the type system, because the constructors are gone.

Removed, each with what replaced it:

- `rt::task::spawn`, `rt::task::spawn_local`, `rt::task::spawn_blocking` — the
  three functions that returned a droppable `JoinHandle`. Fan-out is
  `rt::task::join_all_bounded` (bounded, ordered) or a `JoinSet` the caller
  owns; what outlives the call belongs in a `Supervisor`. `spawn_local`'s
  re-export of `LocalSet` went with it: a local set whose tasks could be
  detached is the same hole one thread down.
- The `rt::task` re-exports of `JoinHandle` and `LocalSet` — nothing public
  returns either any more. `JoinSet`, `JoinError`, and `AbortHandle` remain:
  those are the tracked envelope and the results it yields.
- `rt::runtime::Handle::spawn` — the same droppable handle, reached through the
  runtime instead of the module. This one was not in the original scope and is
  removed because the stated principle decides it: a handle that cannot start
  work but can *drive* it (`Handle::block_on`) still covers every legitimate use.
- `rt::process::{Child, ChildStdin, ChildStdout, ChildStderr}` — a `Child` is a
  handle to a running process, and `Child::kill` reaches neither a shell's
  grandchildren nor a process whose handle was dropped. `Command` stays public:
  a caller must still be able to describe what to run.

Added:

- `Supervisor::spawn_process(&mut Command) -> io::Result<TaskId>`. Takes a
  command, awaits an in-flight permit exactly as `spawn` does, and returns no
  handle — a `TaskId`, which cannot join, abort, or wait. The task it places is
  the process's only owner: the child is put in its own process group and the
  **group** is killed when the task is cancelled or the supervisor drops, so a
  shell cannot leave grandchildren behind. This is what `contract/APPROVED.toml`
  records `rustix` under `lgwks_std` for. A command that cannot start returns
  the `io::Error` to the caller without consuming a slot.
- `Supervisor::default()`. The ceiling is discovered from
  `std::thread::available_parallelism` instead of required, so the safe
  constructor is the one that resolves first; `Supervisor::new(max_in_flight)` is
  unchanged for callers with an opinion. Bounded either way.
- `TaskOutcome::Failed { task, status }`, `TaskOutcome::is_process_failure`,
  `TaskOutcome::exit_status`, `Stats::failed`, and `ShutdownReport::failed`. A
  process that exited non-zero, died from a signal, or whose status could not be
  read is a *failure* carrying its `ExitStatus` — not a completion, and not the
  same report as a kill this supervisor ordered. Without this the three cases
  were one increment, and a bot running a failing command read as a healthy one.
- `clippy.toml` bans `tokio::process::Command::spawn` with
  `Supervisor::spawn_process` as its named replacement. `Command` is the
  engine's own type, so its methods cannot be narrowed, and this is the one
  remaining path that is closed by a lint rather than by the type system — named
  here rather than left to be discovered.

Changed:

- The `process` feature now implies `sync`, its `io`, `lgwks_std/process`, and
  `lgwks_deps/tokio-process` edges unchanged. A `process` build without `sync`
  would expose `Command` and not the runner, leaving the banned `Command::spawn`
  as the only way to use it. No dependency was added: `rustix` stays
  `lgwks_std`'s, under its recorded `allowed_consumers`.
- `Supervisor::shutdown`'s grace before the abort is now a **wall-clock** bound
  (50 ms) rather than eight `yield_now` calls. A counted grace is a same-thread
  heuristic: `yield_now` reschedules the yielding task, so on a multi-threaded
  runtime it gives a body parked on another worker no chance to observe its
  cancellation, and a cancelled task was reported as `Aborted`. Found by this
  change's own process tests, which could not tell a killed command from a
  failed one because of it. A body that returns is settled the moment it does,
  so this is a ceiling on the wait, not a cost charged to every shutdown.
- `rt::task` no longer re-exports anything that starts work; `rt::process`
  exports `Command` and a module doc explaining the one path that is a lint
  rather than a type-level removal.
- `docs/guides/lgwks-bot/background-work.md`, `crates/lgwks-bot/README.md`, and
  the `rt::task` / `rt::process` module docs now describe the supervised-only
  surface rather than the deleted one.
- **`Bot::resolve_effect` takes the `PendingWork` it is about rather than
  `(WorkId, revision)`.** `PendingWork` carries the address, the generation and
  the attempt, and all three are compared before anything is read or written. A
  caller passes back the value `pending()` handed it, so "settle an attempt I
  invented" and "settle the wrong attempt" stop being representable rather than
  being refused after the fact. `WorkId` remains as the address a report names,
  which is what it always was: SPEC-02's "a slot index is instrumentation only".
  This is the intentional tightening RQ-059 asks to be documented — a program
  that named a slot and a revision still compiles if it reads its work from
  `pending()`, and a program that fabricated the pair no longer does.

- **`EffectEvent::to_bytes` returns `lgwks_std::wire::AlignedVec` rather than
  `Vec<u8>`.** The journal record is archived by the estate's binary format now
  instead of a framing this crate maintained beside it, and an `AlignedVec` is
  what the archive has to live in for a reader to access it in place rather than
  decode it. The tag bytes, the width constants and the hand-written
  `encode_into` are gone with it. **The bytes are what a chain head commits to**,
  so this is a durable break as well as a signature one: a journal written before
  this release does not verify against one written after it.

**No crate version is bumped.** The change is unpublished like the five public
modules above it, and it ships in the same release.

### lgwks_bot Added

- **The `domain_id -> constructor` registry exists.** A `BotSpec` carries
  identifiers rather than code, and something has to turn `"github::pr_status"`
  back into a running `Observe`. `DomainRegistry` is that list, and `domains!`
  is the one entry point that declares it:
  ```rust
  domains! {
      pub DOMAINS {
          observe { "github::pr_status" => GithubPrStatus::from_target }
          execute { "notify::slack"     => SlackNotify::from_target }
      }
  }
  ```
  The list is a `static` built at compile time — no constructor to call, no
  global to mutate, so two components cannot race to register a domain and the
  set a binary can run is readable from its source. `Source` and `Action` are
  the erased handles a constructor returns, and `build_source`/`build_action`
  refuse an unlisted identifier with `BotError::UnregisteredDomain`, escaped
  because the name came from the document.
  - **A declarative list, not a proc macro.** The mapping is data a reader can
    see whole. `domains!` is `macro_rules!`, so this adds no dependency and
    leaves the repo's stated position on the `syn` stack untouched —
    `lgwks-std/Cargo.toml` records that policy, and it still holds. A `bot!`
    proc-macro remains deliberately absent for the second reason it always had:
    it would hide the per-call `Auth::check` that auditors read.
  - **The registry is not a permission.** It answers which constructor an
    identifier names, never what a bot may reach. Authority still comes from the
    caller-held `GrantSet`, so a spec cannot grant itself anything by naming a
    domain.
  - **`from_spec` is still open, and now for stated reasons.** It is not
    plumbing: `ChainSpec::on` is `Vec<(String, ActionSpec)>`, so a condition is a
    bare identifier with no slot for `Above<u16>`'s threshold, and an erased
    `Box<dyn ObserveAny>` carries no serializable statement of its `Output` type
    (`spec::Witness` is a `TypeId`, process-local). Both are recorded in
    `experience/invariants/sdk.yaml`, which gains a `registry` lane.
  - `crates/lgwks-bot/tests/registry.rs` enforces the above, including that a
    source identifier never resolves as an action, and
    `docs/guides/lgwks-bot/domains.md` is the guide.

- **A flow can be written in RON, as well as JSON.** `FlowSpec::from_ron` and
  `FlowSpec::to_ron` are the codec, over the same serde types, so no second set
  of impls exists. RON is the estate's notation for human-facing documents and a
  flow is one, so a hand-written flow now takes comments, trailing commas and
  unquoted keys.
  - **The two notations are one document.** Both parsers converge on one
    validation, so the `MAX_FLOW_BYTES` size limit, the
    `BotError::UnknownNodeKind` diagnostic and the structural checks are
    identical on either path. A document cannot be acceptable in one notation and
    refused in the other.
  - **A variant is spelled with its own name.** Every enum in a flow document is
    externally tagged, so a unit variant is its own name (`end`) and a variant
    carrying fields takes them in parentheses (`say(text: "hello")`); in JSON the
    same two are the bare string `"end"` and `{"say": {"text": "hello"}}`. This
    is a change to the wire spelling rather than to the notation: the seven enums
    a document carries (`NodeKind`, `FlowEdge`, `Terminal`, `Predicate`,
    `ValueExpr`, `Value`, `VarType`) previously took a `kind` field, and a
    document written in the older spelling is now refused rather than
    reinterpreted.
  - **The unknown-kind diagnostic reads differently on each path.** JSON checks a
    variant name against nothing, so `BotError::UnknownNodeKind` names the node
    and the kind it carried; RON checks the name against the variant list it is
    handed and refuses before our own visitor runs, so its refusal names the
    variant and the enum but cannot name the node. Both refuse; only what each
    can say differs.
  - `crates/lgwks-bot/tests/flow_ron.rs` pins all of the above, including that
    the internally tagged spelling no longer decodes.

- **`effect`, the durable effect identity the ledger was missing: a settlement
  now names *which attempt* it is about.** The ECS ledger already settles an
  entry and already refuses a contradicting settlement. What it never carried is
  identity: `EntryState::DefinitelyFailed` holds `attempts: u32`, so two
  deliveries that both arrive as "attempt 3" are indistinguishable, and a repeat
  of an old settlement looks exactly like a statement about a new one at the
  moment the ledger decides whether to accept it. `EffectKey` binds the seven
  fields a settlement must name (run, action, attempt, flow revision, action
  digest, environment, environment epoch), so "is this the same settlement?" is
  a comparison rather than a judgement about a counter.
  - **It reuses the estate's one content hash rather than minting a second.**
    `FlowRevision` and `ActionDigest` are newtypes over
    `lgwks_std::hash::Digest`, not replacements for it. They have different
    domains: one binds the validated flow document, the other binds operation,
    target, preconditions, postcondition and exact input. A newtype each is what
    stops a call site passing one where the other is required.
  - **`AttemptId` and `EnvironmentEpoch` refuse to wrap.** `checked_next()`
    returns `None` at `u64::MAX` instead of restarting at 1. A wrapped counter
    hands out an identity it has already issued for this sequence, and the ledger
    would then refuse a genuine settlement as a duplicate. Running out is
    recoverable; silently reusing an identity is not.
  - **The schema admits two digest algorithms; this crate accepts one.**
    `sha256` parses and is then refused as `UnsupportedAlgorithm` rather than
    rejected as malformed, because it is well-formed on the wire and the error a
    caller sees should say the algorithm is unsupported rather than that their
    JSON was wrong. A settlement is accepted because the receiver recomputed the
    digest, and a tag naming an algorithm it cannot recompute is an unverifiable
    claim dressed as identity.
  - **Ids are strict on parse**: 32 lowercase hex characters, big-endian, with
    uppercase rejected rather than folded and the all-zero id refused. Zero is
    the schema's own exclusion and also what a zeroed or truncated buffer
    produces, so refusing it turns a class of uninitialised-identity bugs into a
    parse error.
  - **19 unit tests, plus 6 that encode the identity half of eval cases E06 and
    E07** (`tests/effect_identity.rs`), both of which `okf/evals.json` records as
    `not_run`. What those six establish is that the identity predicates
    distinguish every delivery the two cases name: a reused slot, a forward epoch
    bump, a previous attempt, an exact duplicate, and a rewritten payload. What
    they do not establish is the durable half, because E06 and E07 both require a
    real receiver or persistent-state oracle and the crate has neither a journal
    nor an environment. That gap is not narrowed by anything here.
  - **One of the six is the design result worth keeping.** A key cannot
    distinguish a settled attempt from a contradicted one, because both are the
    same key with different evidence. That is why the ledger keeps its per
    generation settlement record beside the entry rather than deriving settlement
    from identity alone, and why this module does not attempt to replace it.
  - Nothing is wired to the ledger yet. This is the identity layer; the
    settlement call site needs a run and an environment to key against, and a
    grep for either concept across the crate returns nothing before this module.
- **`journal`, the durable append that has to land before the irreversible
  boundary, and the durability grade that decides whether a store may host one.**
  `effect` supplied the identity a settlement is about; an identity held only in
  memory is lost at exactly the moment it is needed. A controller that hands
  bytes to an external system and then dies cannot say whether they arrived, and
  a controller that guesses either duplicates a non-idempotent effect or drops
  one. `EffectJournal::compare_and_append` is the seam: append `event` if and only
  if `expected_tail` is still the committed tail, returning a `DurableAck` that
  names the committed position and the promise claimed for it.
  - **The tail is a position, not a counter.** `JournalPosition` carries a
    sequence number *and* a hash over every event up to and including that one,
    chained so that the same two events in the other order produce a different
    head. A sequence alone would let two journals that diverged agree on where
    they were. `verify_chain` recomputes the chain and names the first entry that
    does not follow, which is what makes a dropped or reordered entry detectable
    rather than merely suspicious.
  - **The ordering ladder is enforced at the append rather than trusted.** One
    attempt at one intent walks `IntentAdmitted`, `DispatchPrepared`,
    `OutcomeObserved`, `Verified`, exactly once. A second `DispatchPrepared` for
    an existing key is refused as `OutOfOrder`, so a second dispatch of one
    attempt is refused rather than merely discouraged. A legitimate retry is a
    new `AttemptId` and therefore a new key with its own fresh ladder, and
    `tests/effect_journal.rs` pins the retry alongside the refusal so the guard
    cannot be mistaken for a ban on trying again.
  - **What the ladder does not enforce: RQ-009's retry admissibility.** A caller
    that mints a new attempt for the same intent gets a clean ladder, and nothing
    here checks `budget_remaining`, live authority, unchanged intent or proof
    that the effect did not land. The ladder makes a *resend* unrepresentable; it
    does not decide whether a *retry* is admissible, and that decision does not
    exist anywhere in the crate yet.
  - **Durability is graded, and the grade is what refuses.** `DurabilityPromise`
    is `Ephemeral`, `ProcessCrash` or `PowerLoss`, and `admit_external_handoff`
    refuses anything below `ProcessCrash`. The in-memory adapter reports
    `Ephemeral` and is refused, which is the whole reason it is a named type
    rather than a default: a caller that reaches for it gets a refusal at the
    boundary instead of a green test that means nothing.
  - **The acknowledgment records the promise rather than implying one was
    proven.** Nothing here can verify a durability claim, because a filesystem, a
    device cache or a virtualization layer can each accept a write and lose it
    anyway. `DurableAck` carries the promise it was minted under and its
    documentation says so. The tests that would decide the claim are crash tests
    against a real store, and they are not unit tests.
  - **`EffectEvidence` is the ledger's, re-exported rather than restated.** The
    journal and the ledger answer the same question, and a second enum with the
    same two arms would drift from the first the next time an arm was added. It
    gained `Hash`, which is additive on a fieldless enum.
  - **`EffectKey::to_bytes` is the canonical encoding the chain hashes**, fixed
    width at 130 bytes, every field written and every integer big-endian, with
    each digest carrying its algorithm tag so swapping the algorithm moves the
    chain position. The length is summed from the field widths rather than
    written as a literal, so adding a field to the key is a compile error until
    the encoding carries it.
  - **24 unit tests, plus 7 that encode the journal half of eval cases E10 and
    E11** (`tests/effect_journal.rs`), both of which `okf/evals.json` records as
    `not_run`. E10 is a lost reply after a real external commit and E11 is a
    crash at every durable boundary. Both require a real receiver, a real restart
    or a persistent-state oracle, and the crate has none of the three, so
    neither case moves off `not_run` on the strength of this entry.
  - **Nothing is wired to a real store or to the ledger yet.** `MemoryJournal` is
    a reference adapter that exists in order to be refused at the boundary, and
    no call site outside the tests appends to it.
- **`broker`, the environment fence: authority for one attempt at one
  generation, and the refusal that stops a stale command from dispatching.** A
  durable record of a dispatch aimed at an environment that has already been
  replaced is accurate and still wrong, so the record cannot be where that is
  caught. `Broker` owns which environments a run has and which generation each
  is at, and `authorize` mints an `Authority` only when the generation named by
  the `EffectKey` is the one currently held.
  - **A stale generation and a generation the broker never issued are different
    errors.** `Superseded` means the key names an older generation: the real
    case, a command that was correct and is now fenced. `NeverIssued` means it
    names a newer one the broker never minted, so the key did not come from this
    broker at all. One "generation mismatch" error would print the same sentence
    for both while calling for opposite investigations.
  - **`Authority` is a sealed proof, and the seal is the constructor.** Fields
    private, no public constructor, `Broker::authorize` the only minter, the same
    shape as `cap`'s `Auth`. It is deliberately not `Clone`, which stops a
    warrant being scattered. That is not what makes a double dispatch impossible:
    the journal's ordering ladder is, and the type documents that rather than
    implying otherwise.
  - **The fence runs twice, and it has to.** Authorizing and handing over are
    two instants. A replacement landing between them leaves a warrant that was
    minted legitimately and is now stale, and a check that only runs at mint time
    does not deliver RQ-006's "replacement invalidates all old commands". So
    `revalidate` re-runs the identical comparison at the boundary, and it shares
    one private `check_generation` with `authorize` rather than repeating it: a
    second copy that drifted would refuse to mint a warrant and accept it at the
    boundary, which is the failure fencing exists to prevent.
  - **`prepare_dispatch` is where the RQ-007 order stops being prose.** It
    authorizes, then appends `DispatchPrepared`, and only then returns a
    `Prepared` holding both. A caller cannot append before it is authorized, or
    hand over before the append landed, because there is nothing to hand over
    until both steps have run. A superseded key never reaches the append, so a
    fenced command cannot become a recorded attempt.
  - **An environment is a resource with a declared lifetime.** `register` takes
    ownership, `replace` bumps the generation and invalidates every warrant
    already handed out, and `close` releases it. Closing is idempotent, because a
    cleanup path that has to know whether it already ran is one that will
    sometimes not run. Generation exhaustion is named rather than wrapped, since
    a wrapped generation would re-authorize commands the broker already fenced.
  - **18 unit tests.** They pin each refusal, both directions of the fencing
    comparison, the boundary re-check on both a replaced and a closed
    environment, the exhaustion path, and that a superseded key leaves the
    journal untouched. What they do not do is reach a real environment: no process,
    socket or input seat exists here, so `EnvironmentId` arrives from the host's
    own entropy and nothing in this crate creates one.
- **`retry`, RQ-009's admissibility rule: the vocabulary existed, the decision
  did not.** `RetryClass` already said what a failure permits and
  `DispatchCertainty` already said what it established, with
  `BotError::retry_class` as the total map between them that never reads a
  rendered cause. What was missing is the rule that composes those with the
  budget, the authority, the intent and a remote deduplication contract:
  `retry_admissible = budget_remaining AND live_authority AND
  unchanged_logical_intent AND (proven_not_applied OR contract_valid)`.
  - **It is a pure function of facts the caller established**, the shape
    `DispatchCertainty` already has with its producers. It reaches for no clock,
    no broker and no journal, so every branch is pinned by a test rather than by
    a scenario.
  - **`DeduplicationContract` records all five things RQ-009 lists**: the
    logical request key, the payload binding, the scope, the retention window
    and the late-arrival behaviour. The key and the payload reuse the effect
    key's own vocabulary, because that split is already the right one: the
    `ActionId` is the logical intent that stays fixed across a resend, and the
    `ActionDigest` is the bound payload that must not change with it.
  - **The late-arrival answer is where a contract can be worth nothing.** Inside
    the retention window the remote deduplicates by construction. Past it, the
    contract rules out a duplicate only if the remote demonstrably *refuses* a
    late arrival. `Applied` means a second application; `Unspecified` means
    nobody knows, and not knowing is not a basis for sending. An adapter that
    leaves that field unstated gets a refusal rather than a duplicate.
  - **Every failing conjunct is reported, not the first.** The four have four
    unrelated repairs, and the estate already made this argument for
    capabilities: a check that reveals one missing item at a time makes the
    repair a loop. The one exception is a permanent failure, which short-circuits
    because the remaining conjuncts are moot and reporting them would be noise.
  - **What a retry cannot reach: a GUI click.** RQ-009's contract half is
    reachable only by supplying a recorded contract, and nothing here can
    express one for a click, so an unsettled click is never retried on that half
    of the disjunct.
  - **21 unit tests**, covering both directions of the retention boundary, a
    contract recorded for another action and for another payload, the refusing
    default on the authority fact, and the all-conjuncts-failed report. No eval
    case moves off `not_run`: RQ-009's rule is a decision over stated facts, and
    the cases that would exercise it end to end need a real receiver.
- **`Observe::fingerprint`, and with it the lazy seam: a source that holds still
  is no longer polled.** Change detection is an *equality* question — the
  substrate reduces every observation to one bit and discards the value — so
  making a source produce the value in order to ask the bit is the eager part of
  the loop. `fn fingerprint(&self) -> Option<u128>` lets a source answer the
  equality question from a key it already holds (an `ETag`, an `mtime`, a row
  version, a mutation counter, a hash of a framebuffer), and the tick then
  **never calls `poll`**: no value is constructed, nothing is boxed, and no
  future is built. The decision is taken *before* the future exists — a check
  inside the future would still have allocated the box carrying it, which on a
  quiet tick is the only thing there was to allocate.
  - **The contract is exact: equal fingerprints must imply equal values.** A
    digest that collides across two different values makes the substrate skip a
    real movement, which is a silent no-op rather than a wrong effect — the one
    failure this can introduce. The digest is recorded at the moment it is read,
    *before* the poll, so a source that moves in between leaves a digest that no
    longer matches and the next tick polls again: the error is one redundant
    poll, never a missed movement. A poll that fails clears the digest, so a
    source that errors still reports every tick, exactly as before.
  - **The default is `None`, which is today's path exactly** — every source
    written before this method existed keeps its behaviour and its cost, and
    there is no configuration in which a domain silently loses an observation.
  - Measured, `bench/` A/B/A in one session, seam versus the tick-allocations
    change beneath it: `poll-only-64x100` **1.65x faster**, `steady-64x100`
    **1.68x**, `wide-256x10` **1.46x**. `fanout-1x64` and `churn-64x1` are
    unchanged: their moves (1.8% and 7.6%) are smaller than the control's own
    drift at the same scenario (6.9% and 10.8%). Allocations per tick:
    **167.6 -> 22.9** steady, **171.0 -> 21.4** poll-only, **650.9 -> 141.7**
    wide. Against the base before either change: **2.71x**, **1.99x** and
    **1.66x**.
  - The rig's own source implements it as the identity (`u128::from(value)`),
    deliberately: a real source would use a key cheaper than the value, so the
    reported gain is a **floor** and not a best case.
  - Three tests, each verified to fail against the seam removed: a quiet source
    is polled once over twenty-one ticks (it reports 21 without the seam), a
    digest that moves is polled again and fires, and a source with no digest is
    polled every tick exactly as before.
- **The tick no longer allocates its own bookkeeping, and a source that holds
  still is no longer boxed.** A steady-state tick at 64 chains made **171 heap
  allocations**; it now makes **97**, and the tick is up to **1.6x faster**.
  Four changes, all measured by the `bench/` rig rather than argued:
  - `fire_plan` cleared the plan and then assigned it freshly collected `Vec`s,
    so the `clear` bought nothing and every tick paid two allocations per chain
    to rebuild buffers it was about to overwrite. The per-tick scratch
    (`Plan::steps`, `Plan::failures`, `Polled`, `Observed`, `Moving`, `Moved`)
    is now taken, cleared and handed back, and the intermediate `Vec<usize>` of
    moved chains and the per-tick `Order::clone()` are gone.
  - **`ObserveAny::poll_any` takes the payload the substrate currently holds for
    its chain and returns `Ok(None)` when the source produces an equal value.**
    The comparison now happens where the output is still a concrete
    `T::Output`, which is the only place it can happen without boxing: a
    caller-side `==` on two `Box<dyn Any>` costs an allocation, a memcpy and a
    free apiece, spent to answer a question that could be asked of the value in
    hand — and in a steady-state bot the answer is "equal" on nearly every tick.
    The blanket `impl ObserveAny` therefore carries `T::Output: PartialEq`,
    which is not a new requirement: every chain is closed by
    `EcsObserveBuilder::observe`, which already demands it.
  - **A source whose equality is narrower than its identity now holds the
    *earlier* of two equal values.** This is a real semantic tightening and it
    is the one to read carefully. `Observed` keeps the payload it already has
    when the source reports equality, rather than being overwritten with the
    newer-but-equal instance. Nothing an effect observes changes — a retained
    transition already refuses to let an equal-valued observation displace its
    binding, so the action was receiving the older instance anyway. What changes
    is a caller reading the observation back after an equal-valued tick. A type
    whose `PartialEq` ignores a field it nevertheless carries should not be a
    chain's output type.
  - The measurement, and the per-tick allocation trace that gives it, are
    `bench/`'s `--alloc-report` mode, which counts through a forwarding
    `#[global_allocator]` in that rig only. It is a diagnostic: no timing in
    `results.json` is taken from a counting run, and no `unsafe` reaches
    `lgwks_bot` or `lgwks_std`.
- **`ObserveBuilder` is now generic over its source, and a chain that does not
  type against that source no longer builds.** `EcsObserveBuilder::on` tied the
  condition to a *free* type parameter connected to nothing else, so the builder
  accepted a wiring that could not work and the tick found out instead: a `u16`
  source, a `u16` condition, and an action whose `Execute::Input` was `u32`
  compiled, ran, and failed every attempt as a domain error the retry policy
  then retried. `on<C, A, T>` is now `on<C, A>` with `C: Evaluate<S::Output>`
  and `A: Execute<Input = S::Output>`, and the builder holds `source: S` until
  the erasure boundary. **This is a deliberate source-breaking tightening**, not
  an accident of the rewrite:
  `Bot::builder("x").observe(source).on(condition, action)` now fails to compile
  when `condition`'s `Evaluate<T>` is not `Evaluate<S::Output>` or `action`'s
  `Execute::Input` is not `S::Output`. Both rejected shapes only ever failed at
  tick time, so nothing that worked stops working — but code that compiled and
  misbehaved now does not compile at all, and a caller who was relying on the
  old looseness must fix the wiring rather than the budget. `observe`/`build`
  remain the erasure boundary and still box into `Box<dyn ObserveAny>`. Sound
  because `Evaluate::check` returns `bool` — a pure predicate with no derived
  output — so `Source::Output == Condition::Input == Action::Input` is already
  the real semantics. The rule a future verb has to keep: no stage may introduce
  a caller-selected type parameter disconnected from its input; a transform must
  carry `Transform<I>::Output` as an associated type. Every `.on` call site in
  the estate already annotated its closure or named its condition type, so no
  call site needed a turbofish added.
- **The erasure boundary carries a witness, and a mis-pairing across it is a
  loud invariant violation rather than a domain error.** `Observed` was a
  `Vec<Option<Box<dyn Any>>>` paired to `Chains` by *index*, and nothing about
  that pairing was checked, so a value delivered to the wrong chain produced a
  downcast miss reported as a domain failure — which the retry policy then
  retried. The staging slot now holds an `Erased` — the boxed value and the
  `TypeId` witness of the type it was erased from, one allocation rather than
  two — and the witness is compared at the rendezvous before any downcast. A
  miss is `BotError::TypeMismatch`, naming the site, the chain, and both types.
  The witness rides with the value rather than only on the chain, so the
  comparison is a producer's *claim* against a consumer's *expectation* rather
  than ground truth against an expectation. `TypeId` is process-local and not
  serializable, so the witness serves the Rust path only; the durable schema key
  belongs to the registry, and `Erased::witness` is the field it will land in.
  One limitation is documented and not solved here: the witness distinguishes
  *types*, not *chains*, so two chains that both produce a `u16` are
  indistinguishable and a mis-pairing between them still passes.


- **A decision carries its provenance, and its receipt is written before the
  transition.** `Resolver::resolve` returns `Verdict`: the `Resolution` and its
  `Provenance` in one value, because "this score came from that model under that
  rule" is one fact and a second call can only re-derive it. `Provenance` names a
  `PolicyVersion` always — content-addressed over the tier's own parameters, so
  retuning changes the revision — and an `EmbedderIdentity` only when a model was
  consulted. `Session` writes a versioned, serializable `DecisionReceipt` per
  decision through a required
  `Journal::record_decision -> Result<ReceiptAcceptance, JournalError>`, binding
  session and flow revision, node, the digest of the *ordered* option list, the
  verdict verbatim, provenance, the selected option's **text** rather than an
  index that moves, and the route. Because the receipt is written first, a
  refused write aborts the answer with `BotError::ReceiptNotRecorded` and leaves
  cursor, scope, transcript and receipt list untouched.


- **`Deficit`, `Shortage` and `Demand`: the whole capability shortfall, and the
  repair it derives.** `Auth::check` and `GrantSet::admit` return
  `BotError::CapabilityDenied` naming **every** ungranted requirement rather than
  the first, each `Shortage` carrying the `Demand` — the domain that declared it
  — where the check site knows it. `Bot::build` walks every source and every
  action and reports the whole bot's unmet requirements from one pass. New
  methods: `Auth::uncovered`, `Auth::covers_cap`, `GrantSet::uncovered`,
  `GrantSet::grants`, `Deficit::shortages/len/is_empty/first/to_grant_set`.
- **`Deficit::to_grant_set` derives the repair.** The shortfall already names
  every capability that would close it, so a caller hands back the set the
  deficit derived instead of writing a repair from the message — the step where a
  hand-written repair covers the first line and misses the rest. The repair is
  the *shortfall*, not the requirement: an already-granted capability is not in
  it, and closing the requirement is that set folded into the held one.
- **`Cap` names are `Cow<'static, str>`.** The shipped four were `String`-backed,
  so `Cap::net()` allocated and — because `GrantSet::issue` mints a proof by
  copying the requirement list — every effect execution re-allocated the same
  constants. `Cow::Borrowed` makes a shipped capability a pointer copy and
  authority for it allocation-free. Equality, ordering and hashing compare
  contents, so a `Cap` deserialized from a spec and one built from a constant are
  one capability; a test asserts that in both directions, because if they
  compared unequal the gate would deny a bot it had granted.
- **`Auth::check` is logarithmic in the granted set.** The covered set is sorted
  and de-duplicated at mint and queried by binary search, replacing a linear scan
  inside a loop over the requirement list. The measured defect and its numbers
  are in `bench/README.md`: 12.9 microseconds for 128 capabilities against 3.4
  nanoseconds for one, growing with the *product* of the two counts.

### lgwks_bot Fixed

- **A settlement is about one attempt, and a repeat of it can no longer land on
  the next one.** The ledger guarded the slot and the generation but not the
  attempt, and a chain retries *within* a generation: an entry held as an
  unknown outcome is settled `NotApplied`, that makes it eligible, and the next
  tick begins attempt 2 at the same address under the same revision. A repeat of
  the attempt-1 report — which settlement tolerates on purpose, because a caller
  that never saw its first delivery acknowledged has to be able to send it again
  — then found the entry held, passed every check, and recorded "definitely did
  not happen" about attempt 2 on the strength of a statement about attempt 1.
  Attempt 3 ran against an effect that may have been live. The ledger now
  numbers the attempts it begins and refuses evidence whose attempt is not the
  outstanding one (`BotError::EvidenceStaleAttempt`), and the number is handed
  back by the ledger rather than derived beside it, so a settled entry cannot
  restart the count at one and give two attempts the same name.
  `tests/ecs_tick.rs::a_settlement_names_the_attempt_it_settles_and_no_other`
  fails with the guard removed and passes with it.
- **A transition that is dropped hands its payload back to the chain that owned
  it, so a settled chain settles.** Found by `bench/`'s fairness gate and by
  nothing else, which is the point: **the bot fired 896,000 effects where the
  hand-rolled baseline fired 17,920 for identical input — a 50x over-run — and
  every one of the crate's 611 tests passed straight through it.** A transition
  *takes* the observed value out of its slot, and while the transition is
  retained that binding is where the chain's newest value lives; when the
  transition finished and was dropped, the value went with it and the chain was
  left with no baseline at all. The next tick read "no baseline" as "the source
  moved", re-opened the chain and fired the entry again, every tick, forever.
  The binding now goes back into the empty slot — only into an empty one, since
  a slot the observation phase filled this tick holds something newer.
  Regression: `ecs::tests::a_settled_chain_does_not_fire_again_on_every_later_tick`,
  which fails against the defect with `tick 3: left: 1, right: 0` and passes
  after it. It uses a new `Holds` fixture rather than the existing `Script`,
  because a source that advances on every poll cannot tell a chain that settled
  from a chain that is still working — which is the reason the existing
  change-filter tests did not catch this.
- **A transition is bound to the observed payload it was opened under, and a
  newer value is admitted only once it has nothing open.** Conditions were
  evaluated and actions were run against the *newest* observation, while a
  resumed transition kept the revision and the entries of the old one. A chain
  whose first action succeeded and whose second refused under one input would
  retry that second action under the *next* input, so one transition produced
  effects of two different command inputs and reported itself as a single
  finished unit of work — the acknowledgment of the first combined with an
  effect of the second. The observation is now *moved* out of the fold's slot
  and into the transition when it opens or resumes, every entry of that
  transition reads the binding, and the slot stays empty for as long as the
  binding is out on loan. It is moved rather than copied, so nothing here asks a
  source's `Output` for `Clone`: `EcsObserveBuilder::observe` requires
  `PartialEq + 'static` exactly as before. A movement that arrives mid-transition
  is deferred, not dropped — it is still in the slot, and it becomes the next
  transition's payload on the first tick after the current one drains. The
  ledger becomes a non-send resource as a consequence, because the payload
  travels inside the transition rather than in a second index that every
  `take`, `put`, `begin`, `skip` and `fail` would have to keep in step.
- **A failure now carries what it establishes, not only what went wrong.**
  `BotError::DomainError` was the adapter's catch-all, and `failure_state`
  classified from the variant — so it was "retryable" by construction. A
  permanent refusal and a wiring defect both burned the whole
  `RetryPolicy::DEFAULT` budget and were reported as `AttemptsExhausted` ("we
  ran out of budget") when the truth was `Terminal`. `DomainError` carries a
  required `certainty: DispatchCertainty` (`Refused` / `NotDelivered` /
  `Unsettled`), `BotError::retry_class()` is the single total map to `RetryClass`
  (`Never` / `Safe` / `RequiresEvidence`) that never inspects a rendered cause,
  and `failure_state` reads that and nothing else. A wiring defect or a parse
  refusal is `Terminal` and costs exactly one attempt whatever the budget says.
  Every one of the 16 production `DomainError` construction sites was classified
  and the classification is now required by the type, so a new producer cannot
  omit it: 15 are `Refused`, and the `JsonStore` local read in `domain/data.rs`
  is `NotDelivered` — a read that never left the process is the one failure here
  that is safe to repeat.
- **An abandoned entry is a barrier to its successors, and a tick over one is
  never clean.** `plan_chain` treated `Abandoned` like `Succeeded` and walked
  past it, so the entry behind a prerequisite that had been given up on ran
  anyway: reserve a draft, fail the reserve under `RetryPolicy::ONE_ATTEMPT`, and
  the next tick sent the draft that was never reserved. Declaration order is a
  prerequisite chain, not a list of independent steps, and abandonment is the
  strongest statement that the entry behind it must not run. The walk now stops
  there; successors stay `NotStarted` and stay reported, and evidence
  (`EffectEvidence::NotApplied`) remains the way past, reviving the entry and
  them with it.
- `EntryState::is_open` excludes `Abandoned`, so the tick's completion check — a
  scan for the first open entry — walked past an abandonment too and returned
  `Ok(0)` while `Bot::pending()` still named it. A caller reading a clean `Ok` as
  "this chain is handled" read the opposite of what the ledger held. The check is
  now `Ledger::first_unresolved`, over open *or* abandoned entries, and
  `outstanding` counts that same set: the abandonment is named first, because it
  is the entry that explains every successor blocked behind it.
- **A settlement now names the generation it settles, and one naming any other
  is refused.** A chain holds one transition at a time and reuses its slot for
  every generation over it, so `(chain, entry)` alone could not distinguish a
  report about the attempt the caller was shown from one about the attempt that
  replaced it. `Bot::resolve_effect` read a delayed report for revision N against
  revision N+1: `Applied` acknowledged an effect at a generation nobody had
  asked about, and `NotApplied` — the sharper half — made an attempt the caller
  was never shown eligible to run again, which is a duplicate merge, message or
  launch. `resolve_effect` now takes the `revision` that `Bot::pending()` reports
  with the work, compares it before reading or writing anything, and refuses a
  superseded generation with `BotError::EvidenceSuperseded`, carrying both the
  named and the live revision so the caller can re-read and report again. This
  is a breaking signature change; the revision is not inferable, which is why it
  is required rather than defaulted.
- `BotError::EvidenceContradicted` is the second new refusal: evidence saying the
  opposite of what already settled *this* generation's entry. Repeating the same
  evidence is not that — it succeeds idempotently, so a caller whose first
  delivery was ambiguous can send it again without having to know whether it
  landed. Neither new variant is `NoSuchWork`, which keeps meaning exactly "there
  is no held effect at this address"; collapsing the three would leave a caller
  reading a stale-report refusal as a wrong address, which is a different repair.
- **A tick can no longer be run from inside an async runtime through the
  synchronous adapter.** `Bot::tick` drives the tick with a thread-parking
  executor. Called from a thread that an async runtime is driving — a
  current-thread runtime above all — parking that thread stops the reactor dead,
  so a tick with a timer, a socket, or a sibling task never returned: it was a
  deadlock, not a slow tick. `tick` now returns `BotError::TickInsideRuntime`
  when a runtime already owns the calling thread. It refuses on a multi-worker
  runtime too, where parking the caller may happen to work, because which thread
  the caller was handed is not knowable from inside the tick.
- `Bot::tick_async` is the async adapter: the same four phases, awaited on the
  caller's executor, returning the same `Result`. It is the entry point inside a
  runtime, and the one to reach for when a verb needs this crate's timer or a
  driver.
- The tick is now decidable before it is doable. The decision system
  (`fire_plan`) records the effect program as an ordered list of steps before any
  effect runs, and the driver then awaits those steps (`EcsBot::run_steps`).
  That is what makes a condition failure's position well defined: the steps the
  walk cleared before the failing condition still run, and the tick reports the
  failure.
- `rt::task::repeat` no longer starves cancellation. It raced each iteration
  against the token but never handed the executor back, so a body whose future
  was ready on its first poll made the whole loop one uninterruptible poll: a
  cancel was delivered only after the budget was spent, and on a current-thread
  runtime the cancelling task could not be scheduled at all. `repeat` now
  completes a bounded number of iterations per poll and then yields.
- Cancellation trees no longer poll or drop recursively. `Inner::cancelled`
  built a `race_two` nesting with one stack frame per ancestor link, and the
  derived `Drop` for `parent: Option<Arc<Inner>>` recursed once per link, so a
  deep chain exhausted the stack without polling or a runtime. Both walk the
  chain once and iteratively now, and `Inner` detaches each parent link as it is
  dropped.
- **A supervised task's outcome is reported, not discarded.** `Supervisor::reap`
  evaluated only `try_join_next().is_some()` and dropped the inner
  `Result<(), JoinError>`, so a return, an abort and a panic were all one
  increment of `completed` — a panicking task counted as a success. Every task
  now ends in exactly one `TaskOutcome` (`Completed`, `Cancelled`, `Aborted`,
  `Panicked { message }`) carrying the `TaskId` assigned in spawn order; `Stats`
  splits `succeeded` / `cancelled` / `aborted` / `panicked`; and `shutdown`
  returns a `ShutdownReport`. A panic payload is preserved, capped at 512
  characters, and the report buffer is capped at the in-flight bound with
  `Stats::reports_dropped` counting what it missed: detail is lost, never memory.
- **Three first-party documents still promised live revocation, and this crate
  has no revoke operation to promise.** `GrantSet` has no revoke operation, and a
  built bot holds a snapshot of the set it was admitted with, so withdrawing a
  capability means rebuilding or ending the bot. `docs/security-posture.md`,
  `docs/general-bot-fold.md`, and `docs/guides/lgwks-bot/getting-started.md` all
  asserted an authority model this workspace does not have; all three now state
  the snapshot boundary. The new `crates/lgwks-bot/tests/authority.rs` enforces
  it — a scan over every first-party text file refuses a claim this crate cannot
  honour — and `a_proof_minted_from_a_grant_set_outlives_that_set` /
  `withdrawing_authority_after_build_means_building_again` pin the behaviour the
  prose now describes.

### lgwks_bot Documentation

- The README, the crate docs, and the `lgwks-bot` guides described the tick as
  one synchronous pass. They now document both adapters, the four phases, the
  refusal, and what a cancelled tick leaves behind.
- **Three first-party documents described `lgwks_bot`'s state as something other
  than what the tree shows; they now match it.** `docs/bot-on-ecs.md` listed the
  `Time<Virtual>` root and the exclusive-systems move as outstanding when both
  are settled — the second landed with step 2 and the migration list went on
  showing it as pending — and its §9 reserved the scheduler for sign-off, which
  has now been given. `crates/lgwks-bot/src/lib.rs` argued that the missing
  `from_spec` materializer was a deliberate position; the invariant ledger's
  reading of it prevailed and the paragraph now says so. And two citations in
  `docs/guides/lgwks-bot/index.md` pointed `Bot::tick` and `Bot::tick_async` at
  lines inside their doc blocks rather than at the functions — a class
  `scripts/check-doc-citations.py` cannot catch, because it verifies that a
  citation resolves and not that it names the right item.

### Fixed

- **A resolver measured its margin against a field the threshold had already
  emptied** (`lgwks_bot`). Below-threshold candidates were discarded before the
  lead was computed, so a winner holding a `0.02` lead against a required `0.08`
  reported its whole score as the lead and resolved where it should have stayed
  ambiguous. The threshold and the margin are two questions asked in that order:
  the threshold asks whether an option *may* win, the margin asks whether it
  separated itself from the next best thing actually observed, and that second
  question cannot be answered against a field the first has emptied. Both tiers
  now hand every measured score to `decide`, which applies the threshold and
  then measures the lead against the real runner-up, including one below it.
- **A comparison that was never made no longer reads as one that was**
  (`lgwks_bot`, `lgwks_std`). A zero or non-finite embedding was scored
  `ZeroMagnitude` and then skipped, so a degenerate candidate vanished from the
  field and a degenerate utterance was reported as `Absent { best_score: 0.0 }`
  — the value a healthy model returns for a field it rejected. A comparison set
  missing even one member cannot produce the verdict a complete set produces, so
  an unusable vector now refuses the set and the session records
  `resolver-degraded` rather than advancing.
- **Missing element facts no longer score as matching facts** (`lgwks_bot`).
  Every recognition component delegated to a metric that scores two empty inputs
  `1.0`, so "no identifying attribute was observed" became maximum identity
  confidence, and an element's tag was never checked at all. Identity, path and
  text now decide presence before consulting their metric — absent on either
  side contributes nothing — and the tag is a gate rather than a weight. The
  component weights are deliberately *not* renormalized: a missing component
  keeps its weight missing, which is what lets the acceptance threshold state
  which facts a match actually requires.

- **The invariant audit reported "enforced" for states it never examined**
  (`lgwks_deps`). A register entry naming a lint no manifest declares, a file
  that exists but declares no test, or a lint declared at `allow` all produced a
  clean verdict, so the gate could not fail. The register now answers four
  questions separately — registration validity, reference resolution, execution,
  and verified outcome — and `check` prints `resolve`/`resolved`/`attested`,
  never `enforced`. Every verdict carries a `SCOPE` line stating that no enforcer
  was executed, so a resolved reference is not read as proof the invariant holds.
  A reference that walks out of the repository (`..`) is refused at load, before
  the filesystem is consulted.
- **A malformed `policy.enforce` silently disabled both gates** (`lgwks_deps`).
  The token was read as `value == "true"`, so `True`, `"true"`, `1`, `yes`, and
  the empty string all became `false` with no diagnostic, standing the gate down.
  A closed Boolean grammar now accepts exactly `true` and `false` and otherwise
  refuses with a typed error naming the line and the value; the key set is
  closed, and a repeated key or `[policy]` section is refused rather than
  last-write-wins.
- **The self-exemption trusted a package name** (`lgwks_deps`). Any edge whose
  *name* was `lgwks_std` or `lgwks_deps` was exempt, including an external path
  or Git package wearing that name, so an unapproved source could be admitted by
  being called the right thing. The name list is gone; the only exemption is
  Cargo's own `workspace_members` list.

- **Flow validation accepted a read before initialization** (`lgwks_bot`).
  Validation tested variable names for *global* writer membership, so a document
  was accepted whenever a writer existed anywhere — including one that runs only
  after the read. Validation now runs a forward definite-assignment analysis: a
  node may read a variable only if every entry-reaching path assigns it. Reads
  of undeclared names still report `UndeclaredVariable` first, and a branch's
  own condition variable is not counted as a read because the executor never
  resolves it. New typed variant `BotError::VariableReadBeforeInit`.
- **The scanner reported discarded errors that were never errors**
  (`lgwks_deps`). ERROR-SWALLOW was a claim with no evidence behind it: every
  `.unwrap_or_default()` and every initialized `let _` was reported, including
  `Option` fallbacks and infallible bindings. Fallibility is now resolved from
  the file being scanned — a free function whose written or aliased return type
  is `Result`, a curated table of std routines, or an annotated binding — and a
  receiver resolved to a non-`Result` is never reported. Where the type is not
  visible the finding says so, and its evidence states that the shape is all
  there is rather than implying proof.
- **A shadowing binding satisfied the use check while the error was discarded**
  (`lgwks_deps`). Any ident spelling counted as a read, so a local that rebound
  the error's name cleared the finding and the error was dropped. A read now
  requires a value-position path naming the binding with no enclosing scope
  having rebound it; field and method segments, macro strings, and comments are
  not evidence, and `drop(binding)` is a discard rather than a use.

- **A validated flow could expand to gigabytes from a sub-megabyte document**
  (`lgwks_bot`). `FlowBounds` bounded *steps*, validation bounded the document's
  *shape*, and nothing bounded *bytes*: a template repeating `${answer}` 20,000
  times is a 200 KB document that interpolates to 163,840,000 bytes in one step,
  and four such steps reach 10 GB. Four ceilings now bound it — utterance,
  stored value, computed record expansion, and aggregate session retention — and
  a template's expansion is *sized* with checked arithmetic before it is
  rendered, so the amplification costs a bounded number of additions instead of
  an unbounded allocation. A template whose literal bytes alone exceed the
  ceiling is refused at load, because it could never render at any value.
  `ResourceLimits` lets an operator tighten the shipped ceilings; a document may
  tighten its own and may not raise them.
- **A declared terminal outcome was accepted and then ignored**
  (`lgwks_bot`). `Handoff` and `Refer` computed their outcome from the node
  alone, so a document that said a handoff was not authorized ran as a handoff.
  One calculation now backs both validation and execution, and a declaration
  that contradicts its node is refused at load rather than accepted and dropped.
- **An ask could offer an option the declared variable cannot store**
  (`lgwks_bot`). Validation did not decode ask candidates, so a flow offering an
  unanswerable option loaded cleanly and failed for the person answering it. The
  same decoding that runs at store time now runs at load over every candidate.

- **Punctuation normalization turned a negative integer into a positive match**
  (`lgwks_bot`). The fold maps `-5` and `5` to the same normalized form, so an
  answer of `-5` resolved at the `Exact` tier to the option `5` — the sign was
  discarded before anything compared it. Answers now carry a declared domain
  (`AnswerDomain::{Label, Integer}`, derived from the variable's type), and an
  integer question is resolved by decoding the utterance and comparing *values*.
  No lexical, phonetic, fuzzy, alias or semantic tier runs on an integer answer,
  because each of them compares folded text or a similarity judgement and each
  re-enters the sign loss. A number no option holds is now `Absent` rather than
  a nearby option.
- **A fuzzy competitor could veto an exact answer** (`lgwks_bot`). Each option
  was tagged with its tier and the mixture was sorted by raw score, so a fuzzy
  candidate at `0.97` outranked a phonetic one and a single margin over the
  mixture reported `Ambiguous` for an answer typed in full. Precedence is now
  resolved over the whole candidate set first: the best tier present wins,
  everything below it is dropped, and the margin applies only inside that tier.
  `Resolution::Ambiguous` carries the tier it tied in, because two exact
  candidates folding to one form, two phonetic candidates sharing a key, and two
  fuzzy candidates inside the margin are three different ties.
- **A learned alias bound to an option's position** (`lgwks_bot`). The table
  stored `normalized utterance -> usize` and spent that index against whatever
  option list arrived next, so a phrase confirmed at one question silently
  selected a different answer at another. An alias is now keyed by question and
  stores the option's own text verbatim, resolved into the current candidate set
  at use time. `Resolution::StaleAlias` reports a binding whose option is gone;
  the session records it and re-asks rather than resolving to a position.
- **An HTTP body was read into memory before any ceiling applied**
  (`lgwks_std`, `lgwks_bot`). A remote server chose the process's memory
  footprint, and the bot's preview limit was a post-hoc trim of an arbitrary
  allocation. The read is now bounded *while* reading: each window is clamped to
  what remains of a declared ceiling, so no buffer exceeds it. The overflow
  policy is a typed choice — `BodyPolicy::Whole` refuses past the limit with
  `Error::BodyTooLarge`, `BodyPolicy::Preview` keeps the prefix and reports
  `Truncation::{Complete, Cut}`. The bot selects `Preview` because a body larger
  than a preview is the normal case for a live endpoint, and its ceiling is
  derived from the preview size so the two numbers cannot drift.
- **A comment naming a log macro suppressed an unlogged-error finding**
  (`lgwks_deps`). The scan matched evidence over a window of physical lines, so
  a marker inside a comment satisfied the check for a return that discards its
  error. Evidence is now a statement rather than prose: the physical-line window
  is gone.
- **`--contract FILE` also became the audit target** (`lgwks_deps`). An
  invocation naming a register could audit the wrong tree and return a success
  verdict for it. `check` now parses its own arguments in one consuming pass,
  refusing a missing value, an option used as a value, a repeated override, a
  surplus positional and an unknown flag — with nothing audited. A *relative*
  target was additionally resolved against two different working directories
  because `--manifest-path` is resolved by cargo against cargo's own cwd; it is
  now made absolute before the child sees it.

- **A clean tick could mean work had been silently abandoned** (`lgwks_bot`).
  The `fire` system selected on `Changed<Revision>` and the revision was
  committed before anything ran, so a mid-chain failure marked the source
  handled: untried entries were never reached while the source held still, a
  failure in the first chain stopped every later chain, and `tick` returned
  `Ok`. Eligible work now lives in a ledger keyed by chain and entry, and the
  revision only *opens* a transition. The walk resumes at the first unresolved
  entry and never replays an acknowledged effect to reach a successor.
  `tick` returns `Err(PendingTransition { work, outstanding })` when work is
  held and nothing failed, so a clean tick now means nothing is held.
  `RetryPolicy` bounds attempts with a spent budget abandoning the entry rather
  than retrying forever, and an effect that may have happened is never
  re-attempted without `resolve_effect` supplying evidence.
- **A selector could starve every permitted host** (`lgwks_bot`).
  `next_admissible` reimplemented the admission predicate by hand and omitted
  the rules check, so a permanently disallowed lexicographically-first host was
  selected on every turn of the loop. One side-effect-free decision now backs
  both `admit` and `next_admissible`, and expiry has a single definition.
- **A frontier permit could be released against the wrong key**
  (`lgwks_bot`). Completion recomputed a request's constraints from current
  topology, so a host whose name re-resolved leaked a slot and released one it
  never held. The reservation now *is* the permit: `Admission::Admit` carries an
  opaque non-clonable `InFlightPermit` that completion consumes, and the
  release/record methods that took a key are gone.

### Added

- `DegradedReason::UnmeasurableEmbedding` (`lgwks_bot`) and
  `CosineError::NonFinite` (`lgwks_std`). A vector no angle can be computed from
  is reported apart from an unavailable embedder, because the causes and the
  repairs differ — nothing is down, one of the vectors is degenerate. Both enums
  are `#[non_exhaustive]`, so these variants are additive.

- Nine `BotError` variants (`lgwks_bot`): `AskOptionNotAssignable`,
  `AskOptionTooLarge`, `ConflictingTerminalDeclaration`, `RecordTooLarge`,
  `ResourceLimitAboveCeiling`, `SessionRetentionExceeded`,
  `TemplateExpansionTooLarge`, `UtteranceTooLarge`, and `ValueTooLarge`. All are
  additive.
- `ResourceLimits`, `ResourceAxis`, `CompiledTemplate`, `TemplatePart`, and the
  four `MAX_*` byte ceilings, exported from the crate root (`lgwks_bot`).

- `BotError::PendingTransition` and `BotError::NoSuchWork` (`lgwks_bot`), and the
  work-tracking vocabulary re-exported from `spec`: `AbandonReason`,
  `EffectEvidence`, `PendingWork`, `RetryPolicy`, `TransitionHold`, `WorkId`
  (`lgwks_bot`). All additive.
- `crates/lgwks-bot/examples/failed_tick.rs`, so the guide's program is compiled
  and run by the gate rather than being prose that nothing checks.

### lgwks_bot Changed

- **Breaking, effective at the next `lgwks_bot` version published from this
  tree.** `BotError::CapabilityDenied`'s payload changes from
  `required: Cap` — one capability — to `deficit: Deficit`, which carries all of
  them. A consumer matching `CapabilityDenied { required }` must read
  `deficit.first().required()`, or iterate `deficit.shortages()` to see the
  whole shortfall. The variant name, and every `CapabilityDenied { .. }` match,
  are unchanged. `lgwks_bot` 0.4.2 on crates.io keeps the old shape; nothing in
  this tree is published by this change.
- `MemoryJournal` keeps `PartialEq` and loses `Eq`: it now holds
  `DecisionReceipt`s, which carry the `f64` scores a verdict was reached on, and
  `f64` is not `Eq`. The `session` module is not in the published
  `lgwks_bot-v0.4.2` tag, so no shipped consumer is affected.
- `lgwks_bot`'s existing `lgwks_std` dependency gains the `hash` feature, so
  `PolicyVersion` content-addresses through the estate's own `blake3` wrapper
  rather than adding a hashing dependency.

### Changed

- `Resolver` takes a `Question` — id, options and answer domain — instead of an
  option slice, because resolving a tier needs the question's identity.
  `Resolution::Ambiguous` gained a `tier` field; the enum is `#[non_exhaustive]`,
  so that part is additive. These are development APIs and are not in any
  published version.
- `Bot::tick` reports held work as `Err(PendingTransition)`. A caller that
  treated `Ok` as "nothing outstanding" now sees the distinction. Where a
  failure and held work coincide the tick keeps the action's **typed error** in
  preference to the pending report, so a retry classifier still sees
  `EffectIndeterminate` as a variant.

## [lgwks_deps 0.1.12] - 2026-09-20

Documentation release. No API change, no behaviour change, and no command-line
change.

### lgwks_deps Fixed

- `lgwks_deps` published no rendered documentation on docs.rs for **0.1.9,
  0.1.10, and 0.1.11**. The manifest declared `all-features = true`, and docs.rs
  builds for `x86_64-unknown-linux-gnu`: `gpui` pulls `objc2`, which refuses to
  compile off Apple targets, and `ml-candle-metal` selects Candle's Metal
  backend. The crate page showed a build error instead of an API. The manifest
  now names the platform-neutral set the CI doc and clippy lanes already verify
  on Linux (`tokio-full`, `appcui`); the platform-bound features stay covered by
  the per-platform jobs.
- 0.1.4 through 0.1.8 are unaffected; the declaration became unbuildable with
  the storefront additions that landed in 0.1.9.
- The `Docs` CI job gained a step that reads each crate's declared
  `[package.metadata.docs.rs]` and builds exactly that set, so an unbuildable
  declaration fails in CI rather than on docs.rs.

### Repository Changed

Not part of any published package; recorded here because it changes files a
reader of this repository sees.

- `AGENTS.md`, `contract/APPROVED.toml`, `skills/`, `experience/`, and one CI
  step name carried the same internal vocabulary the crate docs carried. The
  register's `approved_by` field now reads `maintainer` rather than an internal
  role title; the field is a free-form string and no code validates its value.

## [lgwks_std 0.6.6 / lgwks_bot 0.4.2 / lgwks_ast 0.2.2 / lgwks_deps 0.1.11] - 2026-09-20

Documentation release. No API change in any crate. This supersedes 0.6.5 / 0.4.1 /
0.2.1 / 0.1.10, whose rustdoc carried internal project vocabulary.

### Shared Changed

- The Rust doc comments in all four crates are rewritten. `docs.rs` renders the
  `lib.rs` and module `//!` documentation as the crate front page, not the
  README, so the previous release published a front page a reader arriving from
  crates.io could not act on. 55 Rust files changed.
- Removed throughout: internal project names and shorthand, references to named
  companion repositories, references to an internal governance file, and
  citations to internal issue numbers that a public reader cannot resolve.
- Em dashes across `crates/**/*.rs`: 316 to 56. The 20 that remain on
  doc-comment lines are the ``- `item` — description`` form in feature and
  module lists, which reads as a definition list rather than as prose
  punctuation. The 36 on code lines are the separator in error-message format
  strings, which are program output rather than prose; they are left unchanged
  so that no caller matching on message text is affected.
- No changed line outside a comment except in `lgwks_deps`, listed below. The
  intra-doc link targets, code fences, and fenced example content are byte
  identical to the previous release, so no documented example changed behaviour.

### lgwks_std Fixed

- The `retry` module documentation linked to `crate::random`, which is behind
  the `random` feature. The link resolves under `--all-features`, so it built on
  `docs.rs`, and it failed under `--no-default-features`. Found while verifying
  this release; predates it. The module is now named in code font instead of
  linked. The `docs` CI job gained a `lgwks_std` no-default-features doc build,
  which is the lane that would have caught it.

### lgwks_deps Changed

- The CLI help and admission-ladder text no longer carry internal vocabulary.
  The `about` line now reads `lgwks-deps: dependency admission for the core
  surface`, and the ladder heading reads `The core admission ladder
  (INV-DEP-EDGE-OWNED)`. This is the only user-visible output change in the
  release; no command, flag, exit code, or register format changed.
- Approval fixtures in the contract tests use `reviewer` rather than an internal
  role name. Test-only data.

## [lgwks_std 0.6.5 / lgwks_bot 0.4.1 / lgwks_ast 0.2.1 / lgwks_deps 0.1.10] - 2026-09-20

Documentation release. No API change and no behaviour change in any crate. The
published packages carry a rewritten public surface, which is what a reader
arriving from crates.io or docs.rs reads first.

### Shared Changed

- The root README is restructured as an index: a crate table, a quickstart, the
  use cases, the adjacent-crate comparison, and a documentation index into
  `docs/`. Dependency policy and the consumption contract moved to
  `docs/dependency-doctrine.md`, where the depth belongs.
- Every crate README rewritten for a reader arriving from crates.io. Each opens
  with a statement of what the crate is, then what it does, and the internal
  project vocabulary is gone.
- `docs/` rewritten in the same register. Each admission document states the
  decision, the alternatives measured against it, the authority for it, and the
  verification.
- The four `description` fields rewritten, which is the crates.io listing text.
  `lgwks_std`'s previously used internal shorthand and described the crate as
  async, which its default build is not.
- Manifest and `clippy.toml` comments de-jargoned, because those render on
  docs.rs and in the repository alongside the code.
- The README version-pin CI check now also matches the root README crate table,
  so the table cannot drift from the manifests.
- The root README quickstart is fixed. It did not compile: a tail expression
  returned `io::Error` where `main` promised `Box<dyn Error>`. The `lgwks_deps`
  call it also showed fails in a fresh project, because the gate reads a
  committed register. The snippet now covers `lgwks_std` and `lgwks_bot`, and the
  gate is shown as the CLI sequence it is, including the `cargo generate-lockfile`
  step the gate requires. The snippet is mirrored in
  `crates/lgwks-bot/examples/quickstart.rs`, which `cargo test --all-targets`
  compiles, and a `Docs` CI step fails if the two diverge.
- The root README license pointer no longer names a `LICENSE` file at the
  repository root, which does not exist. Each crate ships its own.

## [lgwks_bot 0.4.0 / lgwks_std 0.6.4 / lgwks_deps 0.1.9 / lgwks_ast 0.2.0] - 2026-09-20

Four crates move together. Two are breaking and take the minor position:
`lgwks_bot` (the ECS substrate and a synchronous `tick`) and `lgwks_ast` (a
renamed enum variant). `lgwks_std` and `lgwks_deps` carry compatible additions
and take patches.

**Breaking.** The four verbs now execute as systems on a `bevy_ecs` schedule,
and that substrate is the only path. There is no feature flag that selects it,
because a default-off implementation is a candidate rather than an architecture.

### lgwks_bot Changed

- `Bot::tick` is **synchronous**. The exclusive systems drive the non-`Send`
  verb futures themselves, so there is nothing for a caller to await.
  `bot.tick().await?` becomes `bot.tick()?`, and the bot binding is now `mut`.
- A condition fires on the tick its source value **moves**
  (`Changed<Revision>`), not on every tick it holds. A source that never changes
  fires nothing.
- A tick is **all-or-nothing**: every source is polled before any effect runs,
  so a failing poll fires nothing and returns the first error. The previous
  contract fired the chains declared before the failure.
- A schedule that cannot be ordered deterministically is refused at `build()`
  (`ambiguity_detection: LogLevel::Error`), not misordered at tick.

### lgwks_bot Removed

- `Bot::block_on_tick` — a pure alias for `tick` once `tick` became
  synchronous. Two equivalent entry points for one job is one too many.
- `Bot::chains()` — replaced by `Bot::source_domains()`, which answers what a
  caller actually wanted ("what is this bot watching") without exposing erased
  observer objects.
- The `Chain`, `ChainEntry`, and `ObserveBuilder::entries` types. `ChainEntry`
  was public only because `chains()` returned it.

### lgwks_bot Added

- `Bot::fired`, `Bot::world`, `Bot::revisions`, `Bot::source_domains`: the
  change-detection state a caller needs to reason about a tick.
- `rt::sync::CancellationToken` (+ `DropGuard`). Every background task must
  listen to one, and the type was previously absent, so the requirement named
  something unobtainable. Built on `tokio::sync::watch`, not `tokio-util`: no
  new third-party edge.
- `rt::supervise` — `Supervisor`, `Budget`, `Outcome`, `repeat`. Background work
  that cannot leak and cannot run away. A task cannot leak because nothing is
  detached (no handle to drop), there is no unbounded constructor and no
  internal queue (a bounded permit is acquired *before* the spawn, and
  `try_spawn` refuses rather than grows), every entry point reaps finished tasks
  so the retained set tracks the live bound rather than the lifetime total, and
  `Drop` cancels and aborts. A loop cannot run away because `repeat` cannot be
  written without a `Budget`, and every iteration *races* cancellation instead
  of checking it between iterations, so a cancel interrupts a body that is
  still awaiting rather than waiting for it to finish.
- `rt::task::LocalSet` and `rt::task::spawn_local`. Every verb is deliberately
  non-`Send`, and `spawn` requires `Send`, so the crate's own futures could not
  be spawned at all.
- `domain::net::NetState::BODY_PREVIEW` is now public. Its cap was documented in
  prose beside a public field, which is how a documented limit drifts from the
  real one; the field's doc links the constant instead, so they cannot disagree.
- `rt::io` (feature `io`): `AsyncRead`/`AsyncWrite`/`AsyncBufRead` and their
  extensions, `BufReader`, `BufWriter`, `duplex`, `copy`. The storefront gained
  `tokio-io` for it, because `io-util` was previously reachable only through the
  whole networking stack. `net`, `process`, and `fs` now imply `io`.

### lgwks_std 0.6.4 Added

- `trace` (default-on): structured, levelled logging via `tracing`, with
  `tracing` registered in `contract/APPROVED.toml` under `owner = "lgwks_std"`.
  Library code in this workspace does not print to the terminal and `tracing`
  is the replacement, but no crate could reach it.
- Measured cost: four crates (`tracing`, `tracing-core`, `pin-project-lite`,
  `once_cell`). `attributes` is off, so `#[instrument]` is unavailable and no
  `syn` proc-macro stack enters the foundation; no subscriber is bundled.
  `--no-default-features --features core` is still genuinely zero-dependency.

### lgwks_deps 0.1.9 Added

- `check --json`: one JSON object on stdout, nothing on stderr, exit code
  unchanged. Keys are always `root`, `enforce`, `admitted`, `approvals`,
  `refusals[]`, `error`.
- `tokio-io` storefront feature; `tokio-net` now builds on it.
- Both JSON printers are built through `lgwks_std::json`. `freshness --json` had
  been escaping only the double quote, so any string containing a backslash
  produced invalid JSON.

### lgwks_deps 0.1.9 Fixed

- `check --json` exited **0** for a repository with no register. The gate
  passing without reading its own contract, which the fail-closed rule exists to
  prevent. Reachable only through the new flag. The exit code checks the error
  arm first, and `admitted` is `error.is_none() && refusals.is_empty()` so a
  payload cannot report admission beside a failed read.

### lgwks_ast 0.2.0 Breaking

- `Language::C` is renamed **`Language::CLang`**. The workspace forbids
  `clippy::min_ident_chars`, and `forbid` cannot be lowered from source, so the
  variant name itself had to change. An `#[allow]` there is a hard E0453, not a
  suppression. The lint fires on the macro's input token, so an alias elsewhere
  would not have helped. `Language::name()` still reports `"c"`, which is the
  stable identity findings and callers match on, so only Rust code naming the
  variant is affected. Nothing in the workspace referenced it.

### lgwks_ast Changed

- `Language` and its four lookup tables are now generated from a single row per
  grammar, so a language cannot be added by halves: the variant, its slot in
  `Language::ALL`, and its arm in each table are gated together. A grammar that
  is compiled out has no variant for a table arm to mention, which is what keeps
  the enum exhaustive without a wildcard.
- `CustomLang` gained explicit extension handling (`with_extensions`,
  `of_path`) and traversal metrics that keep `nodes` saturating, `max_depth`
  monotone, and `has_syntax_issues` sticky.
- `examples/parse.rs` extended.

### Shared Changed

- The bot README is compiled: `#[cfg(doctest)] #[doc =
  include_str!("../README.md")]` runs every Rust block in it. Two defects fell
  out on the first run. A `r#"…"#` literal terminated early by a `"#deploys"`
  payload, and a round-trip whose error types were documented as one type when
  `from_json` returns `BotError` and `to_json` returns `json::Error`.
- crates.io metadata and the GitHub repository description and topics no longer
  use internal vocabulary ("estate", "std+", "lane"), which matched no search.

## [lgwks_deps 0.1.8] - 2026-09-17

### lgwks_deps Added

- Default-off `appcui` storefront feature, pinned to 0.5.1, exposing native
  terminal widgets and input-driven drawing through the storefront owner.
- Admission records the AppCUI selection and its use directive; the authorized
  commit is the policy record, not a cryptographic signature.
- Native feature CI and re-export macro doctest. Publication remains separate.

### Shared verification Changed

- Package-smoke disposal uses the operating-system Trash instead of irreversible
  removal; missing Trash tooling fails loudly and retains the owned artifact.

## [lgwks_std 0.6.3] - 2026-09-13

### Added

- `retry` (core, zero-dep): `RetryPolicy` value type: attempts, exponential
  backoff with caller-supplied jitter, total deadline. Pure data: no I/O, no
  threads, no clock reads. O(1) saturating `delay`.
- `http::Options::idempotency_key`: attach an `Idempotency-Key` header; the
  client never invents the key.
- `ron::FromSliceError::source`: forwards the wrapped UTF-8 / RON error
  instead of dropping the chain.
- Quickstart example (`examples/quickstart.rs`), compiled and run in CI.

### Changed

- `time::ParseError` `Display` now carries the `at` offset on every variant
  (previously dropped on four of six). Messages changed; match on variants,
  not strings.
- `http` documents the pooling boundary: one attempt per call, no connection
  reuse, retries are caller policy via `retry`.

## [lgwks_bot 0.3.2] - 2026-09-13

### Changed

- `error` documents that `String` causes at the domain boundary are
  intentional (typed envelope, escaped payload), narrowing
  INV-BOT-ERROR-TYPED wording without weakening enforcement.
- `cap` documents that `Cap::new` accepts any name by convention and
  enforcement is by grant equality; prefer the shipped constants.
- Both domain paths (`lgwks_bot::net` and `lgwks_bot::domain::net`) documented
  as stable; `BotSpec` documented as validate-only (no `from_spec`
  materializer, no `bot!` macro, by decision; see below).
- `rt` intra-doc links repaired (absolute `crate::` paths) under the new
  `broken_intra_doc_links` deny lane.

## [lgwks_ast 0.1.3] - 2026-09-13

### Added

- Shipped LICENSE (was declared but missing from the package).
- Parse-tour example (`examples/parse.rs`, `lang-rust`-gated).
- Feature-matrix acceptance: `--no-default-features` asserts an empty grammar
  table selects nothing (no more vacuous green).
- Discoverability metadata (keywords) and docs.rs `all-features` config.

## [lgwks_deps 0.1.7] - 2026-09-13

### Added

- Checkout-audit example (`examples/check.rs`) exercising
  `check_dependencies_against` the way CI does.
- Discoverability metadata (categories, keywords) and docs.rs
  `all-features` config.

### Changed

- README leads with the storefront (was gate-first): install-and-select
  snippets with `default-features = false`, tokio-via-facade guidance,
  CLI-vs-library split.

## Shared repo bar (all four crates, 2026-09-13)

- Root `CHANGELOG.md`, `SECURITY.md`, `CONTRIBUTING.md`,
  `docs/distributed-boundaries.md` (mesh transport/identity/time/
  replication/backpressure/observability/schema boundaries, each with an
  owner).
- Shared `[workspace.lints]` (`missing_docs` deny, `unsafe_code` forbid,
  `broken_intra_doc_links` deny) with per-crate opt-in; new CI lanes: docs
  builds (all-features + no-default), per-crate clippy matrix,
  README-version drift guard.
- Crate docs on every `pub mod` listing; README quickstarts pinned at current
  versions.

### Decisions (no code)

- **Declarative API stands; no proc-macro.** Measured: ~26 small heterogeneous
  verb impls; a macro would save tens of lines while dragging `syn` into every
  consumer (banned outside the `lgwks_deps` gate tool), hiding the audited
  `Auth::check` lines, and repeating the already-rejected `thiserror`-in-bot
  tradeoff. Revisit only past ~30 shipped domains with a machine-checkable
  cross-consistency invariant. The `#[serde(crate = ...)]` attribute stays:
  four one-line attributes beat a derive-wrapper macro plus its docs.

## [0.6.2] `lgwks_std` - 2026-09-13

- Keyed BLAKE3 MAC, HTTP custom headers, process-group kill primitive.
- `serde_json` `Deserializer` re-export for prefix-tolerant parsing.

## [0.3.1] `lgwks_bot` - 2026-09-13

- Storefront-owned async runtime surface (`tokio` edge owned by `lgwks_deps`).

## [0.1.6] `lgwks_deps` - 2026-09-13

- Storefront `tokio` engine features; gate-tool `scan` default.

## [0.1.2] `lgwks_ast` - 2026-09-13

- Typed-error derive moved here from `lgwks_std`; `thiserror` owned by this
  job so the core stack stays lean.
