# 4.6 Ephemeral, continued — the ceiling and continuation

Part of [production readiness](../production-readiness.md), §4, nine axes.
[4.6](4-6-ephemeral.md) has the axis score, the real-store observations and the
length-prefix refusal this page follows.

**The ceiling is a backstop now, and continuation is what a long run meets
(#267).** Both `open` and `append` still refuse with
`JournalError::CapacityExceeded` at `MAX_JOURNAL_BYTES` or `MAX_JOURNAL_EVENTS`,
naming the limit, and never truncate. A journal opened with
`FileJournal::open_continuing` continues itself before it gets there: at 80% of
either ceiling (`ContinuationPolicy::declared`, which leaves the settlement
headroom #143 asked for) it writes one sealed checkpoint frame, byte for byte, into
itself and into a successor (`<name>.cont/000001`, then the next six-digit name).
The checkpoint carries the folded state, one record per action for what is settled
and every unresolved attempt in full, so an attempt that was `OutcomeUnknown` stays
`OutcomeUnknown` across the boundary until evidence settles it, and a settled
attempt presented again is refused rather than re-admitted. The controller asks the
trigger on its own append path (`ecs::Effects::append`), so a bot built over a
continuing journal continues without the host doing anything
(`durable_dispatch::the_controller_continues_its_own_journal_on_the_shipped_append_path`).

Measured, not estimated
(`cargo run --locked --release -p lgwks_bot --example journal_continuation`,
Apple silicon, one run each, the shipped trigger):

| | default allocator | `MallocSpaceEfficient=1` |
|---|---|---|
| 10,000,000 attempts (1% unknown, 2% failing verification) | 488 continuations, 81 s | 488 continuations, 106 s |
| oracle agreement | at every one of the 488 boundaries, and after a reopen | same |
| every attempt presented a second time | 10,000,000 refused | same |
| resident set at 1 M / 5 M / 10 M attempts | 1,152 / 1,155 / 870 MB | 24.6 / 16.3 / 18.8 MB |
| peak resident set, whole process (`/usr/bin/time -l`) | 1,205 MB | 53 MB |
| reopen of the live successor behind 1 K / 100 K / 1 M attempts, p50 | 79 / 59 / 58 µs | 89 / 65 / 64 µs |
| reopen p99 | 104 / 78 / 79 µs | 135 / 97 / 85 µs |
| live successor on disk behind 1 K / 100 K / 1 M attempts | 19,780 / 20,100 / 20,100 bytes | same |

The two columns are one program. The journal's live memory is the right column,
flat at about 20 MB; the left column's resident set is macOS's allocator keeping
freed large blocks for reuse (`vmmap` attributes 276 MB at 300,000 attempts to
"Malloc Large (empty)" against 28 MB of live heap), which a host under memory
pressure gets back and which glibc returns for blocks over its mmap threshold.
Neither column grows with the history: a successor weighs its carry, not what is
behind it.

Crash safety is proved with real processes:
`journal_continuation::a_kill_at_every_seal_boundary_leaves_exactly_one_authoritative_journal`
`SIGKILL`s a child at each of the four write and sync boundaries inside a
continuation. Each reopen through the original path finds exactly one journal that
takes appends, holds every settled attempt as settled and refuses each a second
time, still holds the unknown attempt as unknown, and continues again on demand.
`tests/it/sim_continuation_seal.rs` sweeps the mechanism underneath as 25 seeded
properties, each replayed for an identical trace: where each trigger falls, the
generation names, the seal frame's bytes in both files, every refusal around a
sealed or half-sealed chain, both checkpoint bounds at their size and one past it,
and what a reopen and a streaming replay read back on every file of a chain. The
sweep found three defects the example tests had not: a successor could not be
streamed at all, the action bound was a count no checkpoint could reach, and a
lengthened event behind a carried seal was trimmed as a torn append.

**Not claimed.** Sealed predecessors are kept until the host removes them; the
journal has no retention policy of its own, and the measurement above deletes each
one once its successor exists. A run holding more than 128 unresolved attempts at a
boundary cannot continue (`MAX_CHECKPOINT_UNRESOLVED`), and it is refused rather
than truncated, so a controller that never reconciles still meets the ceiling. A
checkpoint folds at most 160 actions (`MAX_CHECKPOINT_SETTLED`), the most whose
widest records fit the one 64 KiB frame beside a full unresolved carry, so a bot
with more actions than that cannot continue and is refused by that count. And
the byte watermark bounds the file, not the records needed to settle work already
handed to the outside world.
