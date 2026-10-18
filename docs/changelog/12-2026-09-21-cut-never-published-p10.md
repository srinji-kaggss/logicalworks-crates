# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (10/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 10 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Fixed (continued)

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
