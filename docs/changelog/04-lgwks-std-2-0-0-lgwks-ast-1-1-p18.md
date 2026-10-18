# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (18/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 18 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — `sim_observe_refresh`: a schedule cell the run did not draw is `None` (continued)

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
### lgwks_bot — `spec`'s test doubles are declared once, and the sleep is no longer suppressed (nine-axis sweep)

- **`hold_pool_thread_for` parks instead of sleeping.** It carried an
  `#[expect(clippy::disallowed_methods)]` because `rt::time::sleep` has no
  timer driver on `lgwks_std::task::block_on` and `std::thread::sleep` is banned.
  `std::thread::park_timeout` is the substitution the codebook names for the
  banned call, so the suppression is gone. The one thing `sleep` promised and
  `park_timeout` does not is that it does not return early; nothing unparks that
  thread, so a premature return can only shorten the overlap, never remove it.
- **One action double instead of four.** `Noop`, `Counting`, `CountAction` and
  `FakeAction` were four `Execute` impls differing only in whether they owned a
  counter — and, between the two counters, in a memory ordering no test reads.
  `Action { counter: Option<Arc<AtomicUsize>> }` is one impl; `Action::new()` is
  the no-op and `Action::counting(..)` the counter.
- **One `NetSource` instead of three**, carrying the value it resolves with, and
  one `FakeSource` instead of two. A double that appears twice can differ in its
  cap as well as its value, and the cap is what those tests are about.

### docs — citations re-anchored after the ranker and predicate walks moved lines

`check-doc-citations.py` pins the *text* of every line a guide cites, so code
added above a cited line moves it. 24 citations across `general-bot-fold.md`,
`guides/lgwks-bot/resolution.md` and `guides/lgwks-bot/sessions.md` were
re-anchored to the line that now holds the pinned text; 11 were dropped by the
tool as duplicates of an anchor already recorded. No citation changed what it
points at.

### lgwks_bot — one score comparator, one predicate walk, one field under test (nine-axis sweep)
