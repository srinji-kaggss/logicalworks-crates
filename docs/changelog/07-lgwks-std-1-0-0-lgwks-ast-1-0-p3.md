# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (3/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 3 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

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
