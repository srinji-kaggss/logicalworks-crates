# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (9/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 9 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_std — the nine-axis sweep: the codec and policy primitives, one value per width (9-axis sweep) (continued)

- **`leb128` encodes at four widths through one algorithm.** `group_byte` narrowed
  a masked group through `TryInto` and answered a refusal with `0`, a value no
  caller could tell from a real group. The group is now read from the value's own
  little-endian byte order — infallible at `u32`, `u64`, `i32` and `i64` alike —
  and the four `encode_*_step` copies are one `Emittable` trait with the
  termination rule per width, so an unsigned and a signed step are written once
  each instead of four times. Encoded bytes, refusals and offsets are unchanged.
- **`hex` and `encoding::base64` no longer invent the bytes they do not have.**
  `decode_pair` indexed a two-character window it documented as panicking, and
  the base64 encoder read a missing final byte as `0` before masking it away;
  both now name the bytes they carry, so a short quantum is `Option`-shaped
  rather than zero-filled. The half-length and quantum-count divisions are exact
  `div_ceil`s instead of `checked_div(..).unwrap_or(0)`.
- **`hash::write_framed` builds its length prefix from the length's own bytes.**
  `u64::try_from(data.len()).unwrap_or(u64::MAX)` would have framed `u64::MAX` for
  a part too long to name; the prefix is now assembled from the little-endian
  bytes, which is the length itself on every target Rust supports. The frame
  width, and so every digest, is unchanged.
- **`wire::format_descriptor` reports widths in `usize`, the domain
  `size_of`/`align_of` answer in.** Narrowing an alignment into a `u8` needed a
  value to report when it did not fit, and that value was indistinguishable from
  a measurement. `FormatDescriptor::pointer_width_bits` and
  `::archived_u32_alignment` are now `usize`; the two pinned fixture constants
  that compared against them moved with it. `Endianness::Unknown` remains the
  only sentinel, and it is a named variant.
- **`retry` states its own clamps.** `duration_from_nanos` replaced three
  sentinels — zero seconds, `u64::MAX` seconds, zero nanoseconds — with the
  nearest value a `Duration` can name at each bound, and the jitter window is
  carried as a `NonZeroU128` so the divisor a remainder is taken over exists by
  type. The backoff sequence, the cap and the inclusive jitter formula are
  unchanged for every caller.
- **`time::parse` refuses a fractional run it cannot read.** A missing byte in
  the fraction was read as `0`, which left-aligns a short fraction correctly and
  also hides a truncated document; `compute_fraction` now returns the typed
  `TooShort` refusal the rest of the module returns. `trace::DebugConfig::from_env`
  reads its three filter sources as three cases, each propagating its own
  refusal, instead of chaining two reads through one default.

Tests: 201 pass (`nextest -p lgwks_std --all-features`, the leb128, hex, hash,
encoding, wire, retry and time families).
### `bench/ai-authoring` — the reference solutions no longer blame a stage that succeeded

Two of the three new-API reference solutions resolved a missing fact to a value
that named the wrong thing. `new-aggregate` reported `SolveError::Fetch { id: 0 }`
when no fetch had failed, and `new-pipeline` reported
`PipelineError::Stage { name: FetchA }` when no stage had failed — both of which
are false claims about a run, and both of which a model copying the reference
would have learned to make.

- **`new-aggregate`'s failure cell is `Option<u32>`**, `None` meaning no fetch
  failed; the first failure is recorded and later ones cannot overwrite it, and
  a fan-out error with no fetch failure is `Cancelled` with the cause on the
  trace stream rather than a fabricated id.
- **`new-pipeline` records `combine` and `publish` under their own names**, so a
  stage that failed for its own sake names itself, and an error from the flow
  itself is `Cancelled` with a diagnostic rather than a default of `FetchA` —
  a stage that ran and succeeded.
- Each file recovers from a poisoned lock through one documented `match`
  instead of `unwrap_or_else(PoisonError::into_inner)`, saying why the guarded
  value is still whole after a panic.

Verified end to end through the benchmark runner, which copies each reference
into a generated trial crate, builds it `--locked --offline` and runs the hidden
oracle: `run.py --dry-run --apis new --tasks aggregate,pipeline,recovery
--trials 1` gives aggregate 6/6, recovery 5/5, pipeline 5/5, all compiled, and
`run.py --mutants` still fails exactly the clauses it is built to fail
(aggregate 2/6, pipeline 1/5, recovery 1/5) — the harness's ability to detect a
wrong solution is unchanged.

### `bench/ai-authoring` — the harness refuses a value no run produced

Four fixtures in the AI-authoring benchmark resolved a missing value to a number
and three recovered from a poisoned lock four different ways. The first kind is
the one that matters: `Stage::run` read `combine`'s inputs and `publish`'s input
as `0` when no earlier stage had produced them, so a pipeline that ran `combine`
first published `0` — a number no stage ever produced, and a number a reader of
the oracle would read as a real result.

- **`combine` refuses without both of `a` and `b`, and `publish` refuses without
  a `combine`**, returning `StageError::Stage { name }` and naming the missing
  input through `diagnostic`. The variant's doc now says what it means: a stage
  produced no value, whether because it was configured to fail or because an
  input it requires was never produced. Its shape is unchanged — the `pipeline`
  prompt freezes `PipelineError`, and the reference solutions in other
  partitions match on it exhaustively.
- **`StageState` holds one `StageValues` under one lock** rather than three
  `Mutex<Option<u64>>`, so `combine` reads a pair from one instant of the run
  instead of two.
- **`World::unit` refuses an index past the world's width** with a diagnostic,
  rather than running a body for a unit that does not exist; the body's counter
  is refused too, where the old code silently skipped the count and let the
  recovery clauses read a body run that never happened.
- **`UnitStats::runs(index) -> Option<u32>`**: `None` is *no such unit*, which is
  not the same answer as `Some(0)` — a unit that exists and has not run. The
  `recovery` prompt's two frozen call sites are updated to match; this is a
  harness-contract change, and it is the one caller outside the partition that
  had to change with it.
- **One `crate::lock` decides what a poisoned lock means** for the crate, and
  `recovery` draws its delay from the same `planned_delay` the rest of the
  harness uses, which makes its "one plan governs every instrument" doc true.
- **The recovery oracle stops allocating a counter vector per index per poll**:
  `interrupt_after` reads one snapshot through `UnitStats::total_runs`, and its
  scratch directory name propagates the `SystemTime` error instead of defaulting
  the timestamp to `0`.
- **Deleted `recovery::Signal`**, a `pub` type with no caller in any partition,
  oracle or API sheet, whose `wait` was an unbounded 1 ms poll loop.

Verified end to end through the benchmark runner itself: `run.py --dry-run
--apis new --tasks aggregate,pipeline,recovery --trials 1` builds each reference
solution into a generated trial crate and passes every oracle (aggregate 6/6,
recovery 5/5, pipeline 5/5), and `run.py --mutants` still fails the clauses it is
built to fail (aggregate 2/6, pipeline 1/5, recovery 1/5). `cargo clippy
-p ai_task_support --all-targets --locked` is clean with `-D warnings` on the
default and `--no-default-features` lanes.
