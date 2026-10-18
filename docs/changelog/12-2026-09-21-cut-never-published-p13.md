# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (13/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 13 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Changed (continued)

- **Breaking, effective at the next `lgwks_bot` version published from this
  tree.** `BotError::CapabilityDenied`'s payload changes from
  `required: Cap` — one capability — to `deficit: Deficit`, which carries all of
  them. A consumer matching `CapabilityDenied { required }` must read
  `deficit.first().required()`, or iterate `deficit.shortages()` to see the
  whole shortfall. The variant name, and every `CapabilityDenied { .. }` match,
  are unchanged. `lgwks_bot` 0.4.2 on crates.io keeps the old shape; nothing in
  this tree is published by this change.
- `MemoryJournal` keeps `PartialEq` and loses `Eq`: it now holds
  `DecisionReceipt`s, which carry the `f64` scores a verdict was reached on, and
  `f64` is not `Eq`. The `session` module is not in the published
  `lgwks_bot-v0.4.2` tag, so no shipped consumer is affected.
- `lgwks_bot`'s existing `lgwks_std` dependency gains the `hash` feature, so
  `PolicyVersion` content-addresses through the estate's own `blake3` wrapper
  rather than adding a hashing dependency.

### Changed

- `Resolver` takes a `Question` — id, options and answer domain — instead of an
  option slice, because resolving a tier needs the question's identity.
  `Resolution::Ambiguous` gained a `tier` field; the enum is `#[non_exhaustive]`,
  so that part is additive. These are development APIs and are not in any
  published version.
- `Bot::tick` reports held work as `Err(PendingTransition)`. A caller that
  treated `Ok` as "nothing outstanding" now sees the distinction. Where a
  failure and held work coincide the tick keeps the action's **typed error** in
  preference to the pending report, so a retry classifier still sees
  `EffectIndeterminate` as a variant.
