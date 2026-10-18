# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (12/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 12 of 13. [Changelog index](../../CHANGELOG.md)

### Fixed (continued)

- **Punctuation normalization turned a negative integer into a positive match**
  (`lgwks_bot`). The fold maps `-5` and `5` to the same normalized form, so an
  answer of `-5` resolved at the `Exact` tier to the option `5` — the sign was
  discarded before anything compared it. Answers now carry a declared domain
  (`AnswerDomain::{Label, Integer}`, derived from the variable's type), and an
  integer question is resolved by decoding the utterance and comparing *values*.
  No lexical, phonetic, fuzzy, alias or semantic tier runs on an integer answer,
  because each of them compares folded text or a similarity judgement and each
  re-enters the sign loss. A number no option holds is now `Absent` rather than
  a nearby option.
- **A fuzzy competitor could veto an exact answer** (`lgwks_bot`). Each option
  was tagged with its tier and the mixture was sorted by raw score, so a fuzzy
  candidate at `0.97` outranked a phonetic one and a single margin over the
  mixture reported `Ambiguous` for an answer typed in full. Precedence is now
  resolved over the whole candidate set first: the best tier present wins,
  everything below it is dropped, and the margin applies only inside that tier.
  `Resolution::Ambiguous` carries the tier it tied in, because two exact
  candidates folding to one form, two phonetic candidates sharing a key, and two
  fuzzy candidates inside the margin are three different ties.
- **A learned alias bound to an option's position** (`lgwks_bot`). The table
  stored `normalized utterance -> usize` and spent that index against whatever
  option list arrived next, so a phrase confirmed at one question silently
  selected a different answer at another. An alias is now keyed by question and
  stores the option's own text verbatim, resolved into the current candidate set
  at use time. `Resolution::StaleAlias` reports a binding whose option is gone;
  the session records it and re-asks rather than resolving to a position.
- **An HTTP body was read into memory before any ceiling applied**
  (`lgwks_std`, `lgwks_bot`). A remote server chose the process's memory
  footprint, and the bot's preview limit was a post-hoc trim of an arbitrary
  allocation. The read is now bounded *while* reading: each window is clamped to
  what remains of a declared ceiling, so no buffer exceeds it. The overflow
  policy is a typed choice — `BodyPolicy::Whole` refuses past the limit with
  `Error::BodyTooLarge`, `BodyPolicy::Preview` keeps the prefix and reports
  `Truncation::{Complete, Cut}`. The bot selects `Preview` because a body larger
  than a preview is the normal case for a live endpoint, and its ceiling is
  derived from the preview size so the two numbers cannot drift.
- **A comment naming a log macro suppressed an unlogged-error finding**
  (`lgwks_deps`). The scan matched evidence over a window of physical lines, so
  a marker inside a comment satisfied the check for a return that discards its
  error. Evidence is now a statement rather than prose: the physical-line window
  is gone.
- **`--contract FILE` also became the audit target** (`lgwks_deps`). An
  invocation naming a register could audit the wrong tree and return a success
  verdict for it. `check` now parses its own arguments in one consuming pass,
  refusing a missing value, an option used as a value, a repeated override, a
  surplus positional and an unknown flag — with nothing audited. A *relative*
  target was additionally resolved against two different working directories
  because `--manifest-path` is resolved by cargo against cargo's own cwd; it is
  now made absolute before the child sees it.

- **A clean tick could mean work had been silently abandoned** (`lgwks_bot`).
  The `fire` system selected on `Changed<Revision>` and the revision was
  committed before anything ran, so a mid-chain failure marked the source
  handled: untried entries were never reached while the source held still, a
  failure in the first chain stopped every later chain, and `tick` returned
  `Ok`. Eligible work now lives in a ledger keyed by chain and entry, and the
  revision only *opens* a transition. The walk resumes at the first unresolved
  entry and never replays an acknowledged effect to reach a successor.
  `tick` returns `Err(PendingTransition { work, outstanding })` when work is
  held and nothing failed, so a clean tick now means nothing is held.
  `RetryPolicy` bounds attempts with a spent budget abandoning the entry rather
  than retrying forever, and an effect that may have happened is never
  re-attempted without `resolve_effect` supplying evidence.
- **A selector could starve every permitted host** (`lgwks_bot`).
  `next_admissible` reimplemented the admission predicate by hand and omitted
  the rules check, so a permanently disallowed lexicographically-first host was
  selected on every turn of the loop. One side-effect-free decision now backs
  both `admit` and `next_admissible`, and expiry has a single definition.
- **A frontier permit could be released against the wrong key**
  (`lgwks_bot`). Completion recomputed a request's constraints from current
  topology, so a host whose name re-resolved leaked a slot and released one it
  never held. The reservation now *is* the permit: `Admission::Admit` carries an
  opaque non-clonable `InFlightPermit` that completion consumes, and the
  release/record methods that took a key are gone.

### Added

- `DegradedReason::UnmeasurableEmbedding` (`lgwks_bot`) and
  `CosineError::NonFinite` (`lgwks_std`). A vector no angle can be computed from
  is reported apart from an unavailable embedder, because the causes and the
  repairs differ — nothing is down, one of the vectors is degenerate. Both enums
  are `#[non_exhaustive]`, so these variants are additive.

- Nine `BotError` variants (`lgwks_bot`): `AskOptionNotAssignable`,
  `AskOptionTooLarge`, `ConflictingTerminalDeclaration`, `RecordTooLarge`,
  `ResourceLimitAboveCeiling`, `SessionRetentionExceeded`,
  `TemplateExpansionTooLarge`, `UtteranceTooLarge`, and `ValueTooLarge`. All are
  additive.
- `ResourceLimits`, `ResourceAxis`, `CompiledTemplate`, `TemplatePart`, and the
  four `MAX_*` byte ceilings, exported from the crate root (`lgwks_bot`).

- `BotError::PendingTransition` and `BotError::NoSuchWork` (`lgwks_bot`), and the
  work-tracking vocabulary re-exported from `spec`: `AbandonReason`,
  `EffectEvidence`, `PendingWork`, `RetryPolicy`, `TransitionHold`, `WorkId`
  (`lgwks_bot`). All additive.
- `crates/lgwks-bot/examples/failed_tick.rs`, so the guide's program is compiled
  and run by the gate rather than being prose that nothing checks.

### lgwks_bot Changed
