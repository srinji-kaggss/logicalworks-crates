# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (20/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 20 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — the logical clock saturates in its own arithmetic (nine-axis sweep) (continued)

`Clock::virtual_at` narrows an origin `Duration` into the `u64` nanosecond
counter every budget, deadline and snapshot is derived from, and it did that
through a `try_from` whose failure arm substituted `u64::MAX`.

- **`duration_to_nanos` splits the duration** into `as_secs` and
  `subsec_nanos` — two infallible projections — and does the scaling with
  `saturating_mul`/`saturating_add`. The ceiling is now produced by the
  arithmetic rather than by a fallback value standing in for a conversion that
  failed, and the function cannot return an error to ignore.
- **`Inner::origin` names its two cases.** A wall clock carries the instant it
  was placed at; a virtual clock has none, and `Instant::now()` is that clock's
  only honest origin, not a substitute for a missing one.
- **The saturation identity is now measured, not asserted.** A seeded sweep over
  the whole representable range — zero, sub-second, whole-second, one second
  below the ceiling, at the ceiling, and `Duration::MAX` — pins
  `virtual_at(origin).now() == min(origin, ceiling)` and the same identity for
  `advance`. Two mutants were run against it: dropping the sub-second term and
  truncating instead of saturating are each caught by
  `the_nanos_conversion_keeps_subsecond_precision_and_stops_at_the_ceiling` and
  `no_swept_origin_reads_outside_the_representable_range`.
- `sim_clock`'s receipt carries `Duration` rather than a `u64` nanosecond count,
  because narrowing one needs a fallback and a fallback makes a wrapped reading
  and an unstarted clock the same number in the receipt.

Behaviour is unchanged: the old fallback and the new saturation both report
`u64::MAX` nanoseconds for an origin past the ceiling.

### lgwks_bot — the four verb traits declare return-position `impl Future` (nine-axis sweep)

The crate carried exactly one lint suppression: a crate-level
`#![allow(async_fn_in_trait)]` whose reason was that the verbs must stay
non-`Send`. `rust-guard` refuses every `allow`/`expect`, crate root included,
and a suppression is a rule that does not exist, so the shape moved instead of
the attribute.

- **`Observe::poll`, `Execute::execute_action` and `Query::query` are declared
  `fn … -> impl Future<Output = Result<_, BotError>>`** instead of `async fn`.
  A domain still writes `async fn` in its impl — the erased
  [`BoxFuture`] boundary is unchanged — so this is a declaration change, not an
  authoring change, and no `Send` bound is introduced: the future stays local to
  the driving thread, which is what `Bot::tick` and the `lgwks_std::task`
  driver are built for.
- **`Execute`'s doc comment was a truncated duplicate** and had swallowed
  `EffectLifetime`'s own documentation, so the enum's rustdoc read as a run-on
  of the trait's first paragraph. Each item carries its own text again.
- `BoxFuture`'s doc no longer claims the traits are "native `async fn`".
