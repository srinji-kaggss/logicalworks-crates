# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (13/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 13 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_std — the test sweep's own fixtures, held to the same contract (9-axis sweep) (continued)

- **The seeded stream is finalised.** `seeded_sweep::next_seed` was an
  unfinalised linear-congruential step, which has period `2**k` in its low `k`
  bits: `next_byte` therefore repeated one 256-byte block for ever, and a 4 KiB
  payload in the hex, LEB128 and UUID families was sixteen copies of it. It is
  now `xorshift64*`, the standard remedy. Measured on this host: the old low byte
  covered 160 of 256 values in 256 draws — the period exactly — while the
  finalised one covers 249 of 256 in 1024 draws with chi-square 260.5 at 255
  degrees of freedom (5% critical 293), and 1024 whole draws are 1024 distinct
  words. Two new tests hold both facts.
- **One lock-poison recovery per file.** The six `PoisonError::into_inner`
  closures in the pool fixture's `Gate`/`Release` become one `guard()` that
  takes the `LockResult` `Mutex::lock()` and `Condvar::wait()` both return, and
  the file's own tests replace the `allow(dead_code)` that hid half of it.
- **No fixture is silenced.** All seven `#![allow(dead_code)]` suppressions in
  `tests/support/` are deleted and each fixture carries its own tests instead, so
  no binary's unused half is hidden.
- **A missing measurement is named.** `consumer_probe::MISSING_MEASUREMENT`
  replaces an inline `u64::MAX`, and the shared-policy tiers now refuse a tier
  whose resident-size figure is absent rather than reading it as a reported zero.
- **Every draw is made in the width it is used in.** Widths were narrowed at
  every site with `try_from(..).unwrap_or(..)`: an identifier's sixteen bytes are
  drawn into an array rather than converted from a `Vec`, LEB128 group numbers
  are counted in the width a shift amount takes, the measurement harness counts in
  the width its paths take their arguments in, and the tenant weights, retry
  attempts, set members, alphabet letters and calendar fields are values their own
  tables or byte windows name. Each site's replacement is a total expression:
  `as` is `as_conversions = forbid`, `From<usize>` for `u64` does not exist, and
  a sentinel for a value is a value the fixture invented.
- **A missing score is not a score of -1.0.** The evidence contract compares a
  verdict's score through one helper that names a withdrawal, propagates the
  checked scorer's own refusal with `?`, and reports a panicked prober as the
  largest possible disagreement rather than as a count of zero.
- **The deprecated lossy time endpoints are no longer called.** A test that has
  to suppress `deprecated` to reach the code it checks is no longer evidence
  about it; the refusal the lossy spellings warned about is now asserted against
  the endpoints a caller migrates to.
- Two cases asserted what their own generator could produce and then deny — a
  hex payload rendering two spellings with no letter in it, and a document with
  no trailing content being refused for having some. Each now draws or states the
  case it means.

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
