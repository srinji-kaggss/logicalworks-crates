# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (8/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 8 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_std — the nine-axis sweep: one poison recovery per lock shape, and a thread that is owned (9-axis sweep) (continued)

- **`task`'s lock-poison recovery is written once per lock shape.** `lock`, `wait`
  and `wait_timeout` each answer a poisoned guard with `into_inner()` in a
  `match`, and each carries the argument for why the guarded state is still
  consistent: a critical section moves a whole `Job` in or out, writes no partial
  state, and the panicking thread's panic is already resumed on its awaiter. Four
  spellings of one recovery became three named helpers, one per shape.
- **The blocking pool's tests carry no suppression.** Both
  `#[expect(clippy::disallowed_methods)]` attributes are deleted: the six waits
  in `task`'s tests and the four in `sim_pool_lifetime` use
  `std::thread::park_timeout`, and the waker thread the `PendingThenReady` future
  starts is a named `thread::Builder` thread whose handle the future owns and the
  test joins, so nothing is detached. A spawn refusal is recorded on the future
  so the test reports it instead of hanging on a wake that never comes.
- **A capacity refusal reports the bound that was actually reached.**
  `queue_limit.unwrap_or(usize::MAX)` named an unreachable queue limit as 2^64
  jobs; the refusal now reports the caller's own limit, with the observed queue
  depth as the stated fallback.
- **`unreachable!` is gone from the pool tests.** A refusal of another shape is
  now reported with the shape it actually had — an OS error's kind and message, or
  the unexpected refusal — instead of a panic claiming none could arrive.
- **`glob`'s three sentinels are gone**: a class close index reads the terminal
  entry that says "unclosed", a scalar offset past the end of the input measures
  the end of the input, and a radix bucket reads the key byte it masks. Its
  malformed-pattern assertions share one helper, so the three cases cannot drift.
- **`similarity`'s lossy `Similarity::score` states the loss once** (in one
  `lossy_score` helper rather than three `unwrap_or(0.0)` sites), and emits each
  refusal with the scorer's name.

Tests: 30 pass for the `task` family (including both seeded pool simulations and
their replay oracles) and 61 for the `glob`/`pattern` family (including
`glob_agrees_with_a_regex_oracle_on_generated_patterns`); clippy clean with
`--all-features` and `--no-default-features`.

### lgwks_std — the nine-axis sweep: a refusal is a fact, and a test may not silence a lint (9-axis sweep)

- **`online` no longer carries four `#[expect]`s.** Each parked a thread with
  `std::thread::sleep`, which this workspace bans; the tests now use
  `std::thread::park_timeout`, the sanctioned synchronous wait, so the reason a
  dial has to wait lives in the test's own doc instead of in a suppression. The
  budget split answers its own two sentinels too: the remaining-candidate count
  is a `NonZeroU32` the loop's own bound proves non-zero, and a count of zero
  means the whole remainder stands for the one candidate left.
- **`similarity`'s lossy `Similarity::score` states the loss once.** The three
  infallible impls called `unwrap_or(0.0)` on a checked score; one `lossy_score`
  helper now answers a refusal with `0.0` and emits it with the scorer's name, so
  a caller reading a zero off the compatibility path can see that it was a
  refusal. `CheckedSimilarity` remains the authority-facing path, and the values
  it returns are unchanged.
- **The seeded pool simulations draw what they name.** `SCENARIOS` is destructured
  into the match arms, so a fifth scenario is a compile error rather than a
  silent fall-through; the handle count is folded through the sweep's own
  `fold_usize` instead of a local saturating cast; and both families bind their
  first seed with a slice pattern on a non-empty const array instead of
  `first().unwrap_or_default()`.

Tests: 63 pass (the online budget family, the similarity evidence contract, and
both seeded pool simulations including `sim_a_seed_replays_its_pool_lifetime_trace`).

### lgwks_std — the nine-axis sweep: the calendar divides where it is bounded (9-axis sweep)

`time`'s conversions carried a `divide` helper whose `checked_div` refusal was
answered with `0`, a sentinel narrower than any real quotient; a `days_in_month`
that answered an out-of-range month with a zero month length; and three
narrowings whose refusals were answered with `1`. None of them is reachable
today, and each is a value a caller could not tell from a measurement.

- **Every division is a `div_euclid`/`rem_euclid` over a non-negative numerator
  and a non-zero constant**, which is what the module doc already claimed. The
  `divide` helpers in `calendar` and `format` are gone, and the day-of-year
  pipeline computes the month and day in the `u32` the public boundary names, so
  `civil_from_days` narrows exactly once — on the era day offset, which
  `shifted_to_era` bounds to `0..=146_096`.
- **`days_in_month` is gone.** Its only caller was the parser, which now asks
  `try_days_in_month` and propagates a typed `OutOfRange` naming the month; a
  month length of `0` no longer stands in for "not a month".
- **`days_from_civil` states its saturation.** A day count outside `i64`
  saturates at the bound it crossed and emits a debug record, rather than
  answering a narrowing refusal with whichever of `i64::MIN`/`i64::MAX` a
  conditional picked.
- **The RFC 3339 renderer is one integer domain.** Digits, two-digit pairs,
  four-digit years and the time of day are all rendered from `i64` counts, so
  the `u32`→`u8` and `i64`→`u32` narrowings are gone; one documented low-byte
  read at the ASCII boundary is what remains. `from_unix_parts` carries a
  nanosecond count into seconds with the euclid forms and widens a
  non-negative second count by reinterpreting the same eight bytes.
- **The deprecated lossy wrappers name their clamp.** `unix_parts_lossy` and
  `from_unix_parts_lossy` still saturate exactly as documented, written as the
  answer to a refusal; `from_unix_parts_lossy` now emits that loss as a debug
  record so a caller that reaches for it by mistake can see it happened.

Behaviour is unchanged for every value the RFC 3339 profile admits: 13
`sim_time_profile` tests pass, including `seeded_calendar_model_replays_exactly`,
`calendar_roundtrips_endpoints_neighbors_and_overflow_transition` and
`t5_endpoints_are_exact_under_the_repaired_narrowing`.

### lgwks_std — the nine-axis sweep: the codec and policy primitives, one value per width (9-axis sweep)

Every line of this crate now meets the nine axes and every rust-guard finding is
repaired at its cause instead of annotated. This first group is the codecs and
the pure policies: each `unwrap_or` that stood in for a value that is not there
is gone, replaced by what its case actually means. No public signature changes;
the one public field-width change is stated below.
