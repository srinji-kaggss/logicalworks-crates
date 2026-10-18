# lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04 (13/13)

Release: lgwks_std 1.0.0 / lgwks_ast 1.0.0 / lgwks_deps 1.0.0 / lgwks_bot 1.0.0 / lgwks_macros 1.0.0 - 2026-10-04. Page 13 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Changed (continued)

- `BotSpec` carries a `version`, defaulted to `BotSpec::CURRENT_VERSION` when
  absent so a document written before the field existed still parses. A version
  this build does not implement is refused by `BotSpec::from_json` and by
  `Bot::from_spec`.
- `DomainRegistry::source` and `DomainRegistry::action` return `None` for an
  identifier declared more than once, not the first matching constructor. An
  ambiguous identifier no longer resolves by declaration order; `validate()`
  already refused such a registry by name at every construction path, and this
  closes the raw lookup so a caller that skips validation cannot reach an
  ambiguous constructor either (#122). Migration: a caller that relied on the
  first-wins result should pick the duplicate it means, or the registry should
  be repaired; `validate()` reports both positions.

### lgwks_bot Fixed

- The `registry` module documentation said "A duplicate is not refused: lookup
  is in declaration order and the first entry wins", which had been false since
  `DomainRegistry::validate` landed. It now states the truth: `validate` refuses
  a duplicated identifier and names both positions, and the raw `source`/`action`
  lookups refuse an ambiguous identifier too (#122).

- A process group whose leader is an unreaped zombie reports `EPERM` on a
  further `killpg` (macOS/BSD). That is a still-present group, not a refused
  termination, so process-group cleanup reports `CleanupPending` and settles via
  the post-reap signal-zero probe instead of `CleanupFailed` (INV-BOT-19).
- The drop-time group kill (a supervised run dropped mid-flight) was one
  `killpg`. It now repeats while the unreaped leader still pins the group id,
  so a member the first signal missed is reached. Known limit on macOS: a
  child the leader is forking at the instant of the kill can still be created
  after the leader dies, and the zombie leader then makes every further
  `killpg` return `EPERM` without reaching it; Linux aborts such a fork. A
  drop after the fork (T20) leaves no running member on either.

- The journal-scale invariants were renumbered `INV-BOT-23..28` to
  `INV-BOT-40..45`, because the old numbers were taken by another branch's
  register. Only the identifiers moved; every enforced-by reference still
  resolves. `INV-BOT-45` now states the tiered 100/1,000/10,000 sweep and its
  requested/reached/ceiling receipt (#122 item 1).
- `FileJournal::storage_gate`'s documentation now says what the handle is: a
  fault-injection and liveness instrument whose held gate parks every append on
  that journal by design, opened only through `open_with_stalled_storage`, never
  by `open` (#122 item 2).
- The registry-identifier invariant and the ambiguous-append invariant this
  branch added were renumbered to `INV-BOT-46` and `INV-BOT-47`, because the
  numbers they first carried were already taken on `main` (structural
  inspection, and the task front door). Only the identifiers moved; both
  invariants' `enforced by` references still resolve (#122, #118).

### lgwks_bot Breaking

- `Source::new` now requires `O::Output: Clone + PartialEq + InputIdentity`
  (was `PartialEq` only); every source usable in a chain already satisfied
  `PartialEq + InputIdentity`, and `Clone` is what the wire `changed` condition
  keeps. Such a source answers `changed` and `always`. A source whose output is
  also `PartialOrd + FromStr` registers with the new `Source::ordered` to answer
  `threshold::above(<n>)` and `threshold::below(<n>)` too, so the threshold
  bounds bind only the sources that offer thresholds.
- `BotSpec`, `ChainSpec` and `ActionSpec` no longer expose their string/collection
  fields as `pub`: read them through `BotSpec::version`/`name`/`chains`,
  `ChainSpec::source`/`target`/`on`, and `ActionSpec::domain`/`target`. The types
  are `#[non_exhaustive]`, so a struct literal was already unavailable outside
  the crate; only field reads change. Serde round-trips are unchanged.
- `domain::sys::ProcessState::stdout` is no longer a `pub` field: read it, and
  the new `stderr`, through the `ProcessState::stdout()`/`stderr()` accessors.
  The state is a report of what the process wrote, so it is read, never edited
  in place.

### Documentation

- The frontier comparisons now cite primary sources with pinned versions and an
  access date (2026-10-02). `docs/framework-comparison.md` and
  `docs/async-parity.md` name each external project's release or tag
  (`spider-rs` v2.52.2, `discord.py` 2.7.1, `serenity` v0.12.5, Stagehand
  `@browserbasehq/stagehand@3.7.3`, `@crawlee/core` 3.18.2, Scrapy 2.19.0,
  tokio 1.53.1, tokio-util 0.7.19, async-std 1.13.2, smol 2.0.2, Bevy 0.19.1).
  Claims a source contradicts are corrected (the `spider-rs` managed-mode and
  declared-limits quotes, which were `main`'s wording rather than the `v2.52.2`
  tag's; smol's missing cancellation token) and claims that cannot be sourced are
  marked *unsourced* rather than deleted (`docs/framework-comparison.md`,
  `docs/frontier.md`). No code, gate or ledger changes. (#155)
