# 4.6 Ephemeral — survives process loss

Part of [production readiness](../production-readiness.md), §4, nine axes. The
continuation of the journal under a long run is in [4.6, continued](4-6-ephemeral-continuation.md).

**⚠️ — the design is right; five of #109's eight observations ran against a real
store, and three rows closed without one.**

This is the axis the effect kernel exists for, and the axis #109's register
covered until it closed on 2026-09-23.

Landed and unit-tested against `MemoryJournal`:

- the four-rung journal ladder, with `OutOfOrder` refusing a skipped rung
- `recover`'s single fold into `AttemptStatus`, so no second authority derives a
  second answer
- `OutcomeUnknown` as a barrier that never auto-replays
- durable input identity separating an event from a content value (`#129`)
- an unreadable journal reported as `EffectRefused`, never as `NoSuchWork` (`#123`)
- external handoff refused before the effect leaves when the journal cannot
  outlive the process
- an acknowledged outcome that cannot be upgraded by a weaker promise (`#119`)

**Every one of those was proved on a journal that lives in process memory.**
Now five of them have been proved on one that does not. `FileJournal`
(`journal/file.rs`) is a real store: a length-framed file whose appends are
written and `sync_all`-ed before the acknowledgment is minted, whose stored
chain heads make a tampered frame a refusal rather than a trim, and whose torn
tail — an append a killed writer never finished — is truncated on open
because it was never acknowledged. `tests/it/durable_crash_observation.rs` runs
the register's observations against it:

- **#106**: a child process walked a key to `OutcomeObserved(Applied)`, was
  `SIGKILL`-ed, and the restarted journal recovered `Applied`; a duplicate
  settlement is refused out-of-order; a new key climbs fresh.
- **#102 / #104**: a child killed between `DispatchPrepared` and any outcome
  recovered `OutcomeUnknown`, named in `uncertain()`, never `NotApplied`;
  evidence appended after the restart settled it without a resend. A
  settlement followed by a torn (failed) recording append still recovers the
  settlement.
- **#101**: after the kill, a re-admission under a changed digest is a second
  identity with a fresh ladder — `recover()` reports both, never one.
- **#100**: the external marker exists only after the durable ack, and the
  real store admits the handoff its promise earns (`ProcessCrash`, honestly
  below `PowerLoss`).

`Until that run exists` no longer describes these rows. #99 and #108 closed on
their repair and in-process tests without an external observation, and #107's
T21 is observed only as an honest non-containment (§1); with rows of its own
axis unobserved, this axis stays ⚠️ and the §1 verdict stands.

**A length prefix that lies about an acknowledged frame is refused, not
trimmed ([#262](https://github.com/srinji-kaggss/logicalworks-crates/issues/262)).**
Until this change the one hold the module documentation itself named was real: an
acknowledged final frame whose stored length was changed from `L` to `L + k`
was truncated on open, because a complete prefix over a frame the file cannot
hold looks like an append cut short, and the head-short shape (`k` in `1..=32`)
was classified as torn without being asked at all. An open now decides on the
bytes. Any payload length under the bytes present that reproduces the stored head,
or a later whole frame that authenticates, was acknowledged, and the file is
refused with `JournalError::Corrupt` and left byte-identical; only a tail that is a
prefix of one cut-short append is trimmed. Observed by
`journal::file::tests::a_lengthened_acknowledged_final_frame_is_refused_not_trimmed`
(`k` in `1..=1024`, file hash compared),
`an_inflated_non_final_length_is_refused_and_every_byte_survives`,
`an_append_cut_at_every_byte_of_the_final_frame_is_repaired` (the control: no
genuine cut is refused), and by `tests/it/sim_journal_tail.rs`, which sweeps seeded
journals of one to a thousand frames through inflated, deflated, bit-flipped and
cut prefixes, two tenants concurrently. **What it does not cover:** a final frame
whose length and head are both damaged has nothing to authenticate it and is
trimmed (`a_final_frame_with_a_lying_length_and_a_damaged_head_is_the_stated_limit`),
and a hand that truncates a file mid-frame is indistinguishable from a crash. The
journal stays below the unattended-automation bar for the reasons in the ceiling
paragraph of the continuation page, not for this one. The run store and the repair ledger shared the
same defect through `frame::read_raw`, which treated every cut frame as a clean end;
they now ask the same question of the same bytes, with each store's own head over
its decoded record (`task::store::tests` and `task::ledger::tests`, including a
ceiling-sized noise tail decided in bounded time). Their tests were not run against
the unfixed code, so for those two stores the control is the cut-append test only.
The cost of the decision is measured, not estimated
(`cargo run -p lgwks_bot --features script,ephemeral --release --example tail_cost`,
64-frame files, Apple silicon, one run, peak RSS 6.3 MB for the whole process):

| Open of | journal p50 / p95 / p99 | run store p50 / p95 / p99 |
|---|---|---|
| an undamaged file (n=400) | 65 / 100 / 121 µs | 268 / 319 / 370 µs |
| a final frame lengthened by 1–2048, refused (n=400) | 109 / 130 / 154 µs | 210 / 245 / 360 µs |
| an append cut at a seeded byte, repaired (n=60, an `fsync` each) | 3,127 / 4,010 / 5,922 µs | 3,806 / 3,930 / 4,687 µs |
| a ceiling-sized prefix over noise, trimmed (n=8) | 2,266,477 / 2,371,675 / 2,371,675 µs | 5,685 / 5,864 / 5,864 µs |

The last row is the stated worst case and it is slow for the journal: the search
tries every payload length under the bytes present, each is a blake3 over a
length-framed payload that cannot be extended from the one before, so a 64 KiB tail
costs about 2.3 s once, at open. It is bounded and it needs a tail that is a
maximal-size frame cut mid-write (or a hand-made one); a journal's ordinary frames
are a few hundred bytes and cost the third row. The run store decodes each candidate
and a wrong length fails at once, which is why its worst case is milliseconds.

The adapter's costs are measured, not estimated: a throwaway release-mode
probe (since deleted) timed each path on the development machine's file
system, and the numbers with their method are recorded in the pull-request
evidence rather than gated in the tree. One `sync_all` is the platform's
durable-append floor and costs about 3.0 ms there; a per-rung append pays it once per rung, and
`FileJournal::compare_and_append_all` — the group commit every durable log
converges on — pays it once per batch, all-or-nothing, with every
acknowledgment still minted after the shared flush. The per-key ladder check
is indexed, so an append's cost does not grow with the journal's length
(measured flat from an empty journal to 32,000 prior attempts, against the
linear walk it replaces, which measured 98× slower at 8,000). Replay holds
every committed entry *of one generation* in memory and reopens in time linear
in that generation's file, which the continuation page bounds.
