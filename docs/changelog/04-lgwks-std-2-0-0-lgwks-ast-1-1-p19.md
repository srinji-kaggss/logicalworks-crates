# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (19/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 19 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — one score comparator, one predicate walk, one field under test (nine-axis sweep) (continued)

- **`session::by_score_descending` is the crate's one ranker.** The lexicon and
  the semantic tier each sorted with `partial_cmp(..).unwrap_or(Ordering::Equal)`;
  they now share one comparator, because two rankers that disagree put the order
  in the tier rather than the score, and a NaN on either side sorts *last* rather
  than winning or tying.
- **`comparison_operands` is the one list of comparisons.** `collect_predicate_
  variables` and `validate_predicate` each restated the six `Eq`/`Ne`/`Lt`/`Le`/
  `Gt`/`Ge` variants; a new comparison would have been collected by one and
  skipped by the other.
- **`semantic`'s tests share the field they ask about** (`the_field`,
  `resolve_with`, `near_tie_vectors`). The option *texts* are part of each
  premise — a misspelled one is measuring a field the lexicon resolves
  differently, and that is invisible in a diff full of vectors — and two tests
  built the identical `0.73`/`0.71` vectors by hand.
- **`integer_bytes` counts digits in `usize`, never narrowing.** It walked
  `checked_ilog10`'s `u32` into a `usize` width through a fallback that put a
  number in a byte count; it now walks the magnitude's base-ten thresholds in
  `u64` and counts in `usize`, so nothing is converted.
- **`prompt_bytes` saturates.** It returned `None` on overflow and the caller read
  that through `unwrap_or(usize::MAX)`; it now saturates in its own arithmetic and
  the caller's ceiling check answers a saturated width without being told which it
  was.
- **`FlowBounds::effective_resources` names the absent case.** A document that
  declares no byte ceilings is held to `SHIPPED_LIMITS` — the case the field's own
  documentation describes — instead of `unwrap_or_default()`, which said nothing
  about which of the two limit sets a request-less document is held to.

### lgwks_bot — inspect and frontier stop substituting values for answers (nine-axis sweep)

- **`macro_head` splits once.** `split(..).next().unwrap_or("")` cannot fail —
  `split` always yields a piece — and a headless invocation was reported as the
  empty name, which matches no rule. It is now `split_once`, whose two answers
  are both real: text with no terminator *is* its own head.
- **`bounded_preview` refuses a range that names nothing.** A byte range outside
  the source, or across a non-boundary, has no text to preview; that is the empty
  preview *with* the truncated flag set, so a caller cannot read it as a node with
  genuinely empty text.
- **`inspect`'s parse-timeout receipt saturates instead of defaulting.** It
  narrowed `Duration::as_millis` with `unwrap_or(u64::MAX)`, so a deadline the
  parser never reached would have been reported as half a million years. It now
  splits the duration through its two infallible projections and saturates in
  arithmetic. It lives beside its only caller rather than beside
  `duration_to_nanos` because the `inspect` feature is what uses it, and a
  clock helper gated on nothing is dead code in every other build.
- **`Frontier::decide` names its two answers.** No saturated constraint is
  `Admit`; a saturated one carries the fold's latest deferral.
- **Two frontier tests refuse rather than default**: a selection round trip with
  no host selected, and an origin index whose `checked_rem` cannot fail.

### lgwks_bot — the lexicon ranks a score it cannot order last, not as a tie (nine-axis sweep)

`score_all` ranked candidates with `partial_cmp(..).unwrap_or(Ordering::Equal)`.

- **An unorderable score now sorts last.** `partial_cmp` returns `None` for
  exactly one pair of `f64` values — a NaN on either side — and the blend above
  cannot produce one from two clamped unit scores and non-negative weights, so
  the arm is unreachable today. `total_cmp` alone would have ranked a NaN
  *first*, handing the win to the one candidate that measured nothing; treating
  it as a tie left an unorderable entry in the list `decide` then computed its
  lead over.
- **`INPUT_BOUND_CHARS`** is the input bound at the width the policy digest
  carries, replacing `u32::try_from(MAX_UTTERANCE_CHARS).unwrap_or(u32::MAX)` —
  a narrowing conversion whose failure arm would have put a bound in the digest
  that the resolver does not apply. `the_digest_input_bound_is_the_shipped_bound`
  asserts the two spellings are one number; run as a mutant (512 → 256) it fails.

### lgwks_bot — a lost publication response and a contradicted one are one match (nine-axis sweep)

`review.rs` selected between `Reconcile::None` twice — once through a guard that
proved `created_id` was `Some` and then read it through
`created_id.unwrap_or_default()`. The two arms are now one match on the id
itself, so the value that decides the outcome is the value the arms read.

### lgwks_bot — `retry`'s contract tests start from one fixture (nine-axis sweep)

Eight of `retry`'s tests wrote out the same `RetryFacts::new(...)` preamble by
hand, and two of them were byte-identical. They differ from each other in
exactly one clause — the contract, its retention window, or its late-arrival
behaviour — and that is the property the assertions rest on: a refusal is only
attributable if nothing else moved.

- **`unresolved()` is that fixture**: a live authority over an attempt that may
  have landed, zero attempts used, zero elapsed, the shared payload. Every
  contract test now changes one clause on top of it, so a second difference
  cannot creep in unnoticed.
- `permissive(class)` is unchanged and still the fixture for the tests that move
  the class.

No behaviour change.

### lgwks_bot — the broker's dispatch tests build their subject once (nine-axis sweep)

Four of `broker`'s tests each assembled the same five lines — a broker, a
registered environment, the key carrying its generation, a journal, and an
admitted attempt. Two of them were byte-identical, which is how one of them
drifts: a test that assembles its own may admit one key and authorize another,
and every assertion after that is about a situation the module cannot produce.

- **`AdmittedEnvironment` is now the fixture** those four tests share, and all
  four build from it. The generation the broker authorizes, the key that carries
  it, and the admission already on the journal are one value now, so they cannot
  disagree.
- **`Broker::environments` declares its bound.** One entry per environment the
  host created, added by `register` or `adopt`, never refilled on its own, and
  `close` marks an entry closed rather than removing it — so a warrant for a
  closed environment is refused as `Closed` rather than looking like one for an
  environment the broker never heard of.

No behaviour change.

### lgwks_bot — the logical clock saturates in its own arithmetic (nine-axis sweep)
