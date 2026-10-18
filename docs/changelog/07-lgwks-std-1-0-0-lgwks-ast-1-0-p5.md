# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (5/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 5 of 13. [Changelog index](../../CHANGELOG.md)

### Fixed (continued)

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
