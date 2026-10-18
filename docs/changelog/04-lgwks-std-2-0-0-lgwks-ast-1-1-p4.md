# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (4/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 4 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — row 1 of #278: a torn read is pending, never a change (continued)

- `stability` (new module) owns the stable-read protocol an `Observe` adapter
  needs. `read_stable` requires two *independent* readings of a subject to
  agree on its reported length, its modification time and the BLAKE3 digest of
  its bytes before either is handed back; the bound is
  `stability::MAX_STABILITY_READS` (8) reads, and a subject that never settles
  is `ReadFailure::Unstable` naming the axis that kept moving and both digests
  it saw. `read_stable_file` is the same protocol over a real file, on the
  blocking pool so a parked filesystem cannot stall a tick.
- `stability::Drift` has four named axes rather than one boolean: `Length`,
  `Modified`, `Contents` (an in-place rewrite of the same length within one
  filesystem timestamp granularity — the case a size-and-mtime check admits),
  and `Truncated` (a read cut short by the writer, which is a property of one
  reading and is checked before any comparison).
- `BotError::UnstableObservation { domain, unstable }` is the typed outcome.
  It is `NotDelivered`, so `dispatch_certainty` and `retry_class` read it as a
  plain retry: nothing was read, nothing was committed, and the chain keeps the
  value it already holds. It is not a `DomainError` because nothing is wrong
  with the domain — a source that keeps reporting it is telling you about a
  writer that never settles.
- Wired on the shipped path: `domain::data::JsonStore` reads through
  `stability::read_stable_file` on every `Observe` and `Query`, so a bot
  polling a JSON-backed store no longer commits a half-written document. **Not
  claimed:** that a settled reading is valid JSON, or that a writer which
  rewrites in place and pauses is caught — the stronger guarantee belongs to
  the writer's `rename`-into-place discipline, and the module says so.
- Evidence: `tests/it/stability.rs` drives a real file against a real
  child-process writer (a tight append loop and a rename-into-place writer),
  asserts that a moving store is never reported unreadable, that the refusal
  names its reads and its axis, that an absent store is unreadable rather than
  unsettled, and that the store recovers once the writer is gone.
  `tests/it/sim_stability.rs` sweeps 1,024 seeds over a modelled writer: no
  torn reading is ever admitted, a writer that never pauses is refused at the
  declared bound for every seed, a renamed subject settles whole every seed,
  and the same seed replays to the same trace hash with adjacent seeds
  diverging. `docs/production-readiness.md` §4.4 moves the
  "a file was half-written when read" row on those tests.
### lgwks_deps — `long-try-chain` charges each `?` to its own statement

The scan's `long-try-chain` rule counted every `?` inside a `for`, `while`,
`loop` or `if` body against the enclosing statement as well as against the
statement that holds it, including those under a `let`, which the rule's own
contract says break a chain. A ten-statement loop of single-`?` bindings was
reported as one ten-`?` chain at the loop's line. The counter no longer descends
into a nested block, whose statements the walker inspects one by one, so a `?`
is counted once, at its own line; a loop header is still its own statement's.
`scan::tests::a_loop_is_not_charged_with_the_statements_in_its_body` and
`a_chain_inside_a_loop_body_is_still_a_chain` failed before the change.

### lgwks_bot — the effect journal continues as new instead of stopping at its ceiling (#267)

A `FileJournal` refused every append at 100,000 events or 64 MiB, so a bot acting
once a minute stopped after about seventeen days. A journal opened with
`FileJournal::open_continuing` (or `open_continuing_with` and a
`ContinuationPolicy`) now continues itself at 80% of either ceiling: one sealed
checkpoint frame, written byte for byte into the predecessor and a successor at
`<name>.cont/NNNNNN`, carrying one folded record per action and every unresolved
attempt. `FileJournal::open_active` follows the chain from the original path;
`EffectJournal::continuation_watermark` and `continue_as_new` are the trait's
lifecycle; `ecs::Effects` asks the trigger on its own append path.

Defects found and fixed on the way, each with a test that failed before the fix:

- `committed_entry` indexed a successor's events from the genesis, so every
  position a continued journal acknowledged read back as absent and a controller
  settling an attempt across a boundary reported `EffectUnrecorded`.
- A successor's own file was named generation one, so its successor's checkpoint
  claimed a generation already held.
- The successor writer asked whether it had created the generation directory
  *after* creating it, so a new directory's name was never synced.
- `Replay` refused at a successor's first frame, the seal it carries, and then
  ended, so no file after the first continuation could be streamed at all. It now
  gives both seal frames the open scan's dispositions and streams exactly
  `events()` on every file of a chain.
- `MAX_CHECKPOINT_SETTLED` was 256, whose widest checkpoint archives to 90,208
  bytes in a 65,536-byte frame: a count no checkpoint could reach, refused by the
  frame as a storage fault instead of by the count. A settled record no longer
  stores its action and attempt beside the key that holds them (272 to 240
  bytes), and the count is 160, whose widest checkpoint beside a full unresolved
  carry is 58,976 bytes.
- An ambiguous tail in a successor with no event of its own was resolved against
  the genesis rather than the carried seal, so an acknowledged event under a
  lengthened prefix was trimmed as a torn append. It is resolved against the last
  whole frame.

`tests/it/sim_continuation_seal.rs` pins the mechanism as 25 seeded properties,
each swept twice for an identical trace: the watermark, the generation names, the
seal frame, every refusal around a sealed or half-sealed chain, both checkpoint
bounds, and what a reopen and a replay read back.

Measured over 10,000,000 attempts and 488 continuations (production-readiness
§4.6): oracle agreement at every boundary, every attempt refused a second time,
live memory flat at about 20 MB, reopen p50 58–89 µs behind 1 K to 1 M attempts.
`SIGKILL` at each of the four seal boundaries leaves exactly one authoritative
journal. INV-BOT-152.
### lgwks_bot — per-tenant capacity: one tenant cannot starve another (#268)
