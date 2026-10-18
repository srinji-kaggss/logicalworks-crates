# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (10/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 10 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

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
