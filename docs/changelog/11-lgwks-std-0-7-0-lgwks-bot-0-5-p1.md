# lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28 (1/5)

Release: lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28. Page 1 of 5. [Changelog index](../../CHANGELOG.md)

The first upload of this train. It carries everything below, including the
2026-09-21 cut, which was never published. `lgwks_std` takes the minor position
(0.7.0), not the cut's 0.6.7, because the entries under "lgwks_std Breaking"
rename and remove public items, and Cargo treats 0.6.6 → 0.6.7 as compatible.
`lgwks_bot` 0.5.0 is already a minor bump from the published 0.4.2; it re-exports
`lgwks_std::json`, so it moves with std. `lgwks_deps` 0.1.13 only adds a command
and exposes no `lgwks_std` type, so it stays a patch. `lgwks_ast` does not move.

### lgwks_std Breaking

- `time` no longer re-exports the items of its `calendar` and `format`
  submodules, so `time::to_rfc3339` is now `time::format::to_rfc3339` and
  `time::civil_from_days` is `time::calendar::civil_from_days`. Both submodules
  were already `pub`, so the re-export was a second name for the same item, and
  nothing in the workspace used the short one. `time::parse_rfc3339` and
  `time::ParseError` stay: they are the module's parse half and the error it
  returns, and `time::parse::parse_rfc3339` would stutter.
- `BoundingBox::as_array` is now `to_array`. The method builds the four-element
  array from four separate fields, so the call copies rather than reborrows and
  the `as_` prefix promised a cost it did not have. Same signature, same value.
- `http::get_response` is now `http::get`, so the pair reads `get` / `get_with`
  the way `post` / `post_with` already did.
- `Uuid::from_bytes` is gone, replaced by `impl From<[u8; 16]> for Uuid`. The
  behaviour is unchanged; the conversion is now reachable by a caller that
  bounds on `From<[u8; 16]>` rather than on this crate's naming.
- `Pattern::find_all` returns `impl Iterator<Item = Match<'_>>` and
  `Pattern::split` returns `impl Iterator<Item = &str>`. Code that assigned the
  result to a `Vec` needs a `.collect()`; code that only counted the matches,
  took the first, or short-circuited no longer allocates one item per
  occurrence.
- `Weighted::try_new` is removed. It was an alias for `Weighted::new` with no
  callers anywhere in the workspace.


### lgwks_std Added

- `trace` now includes a default debugger bootstrap: `DebugConfig`,
  `DebugFormat`, `install_default`, `LGWKS_LOG`/`RUST_LOG` filtering, compact
  and pretty local output, and JSON-line output via `LGWKS_LOG_FORMAT=json`.
  The surface stays default-on with `trace`; `#[instrument]` remains out of
  scope because `tracing-attributes` would pull the proc-macro stack into the
  foundation crate.
- `Debug` for `Hasher` (reports bytes written), `Weighted` (reports the
  composition rather than the components) and `task::JoinHandle` (reports
  running / done / panicked / taken). All three were unprintable, so a consumer
  that wanted to log one wrote a wrapper that then had to change whenever the
  type did.


### lgwks_bot Breaking

- **The crate root stopped carrying three sets of aliases.** `lgwks_bot::gh`,
  `lgwks_bot::net`, `lgwks_bot::fs` and six others named `domain`'s modules a
  second time; `lgwks_bot::frontier::Frontier` and ten siblings repeated what
  `pub mod frontier` already exposed; and `lgwks_bot::rt::Builder` was a third
  path to a type the root already re-exported. Each was a name for something
  already nameable one segment away, and the only user of the nine domain
  aliases anywhere in the workspace was a single doctest. The paths are
  `lgwks_bot::domain::gh`, `lgwks_bot::frontier::Frontier`, and either
  `lgwks_bot::Builder` or `lgwks_bot::rt::runtime::Builder`.
- `broker::prepare_dispatch` and `broker::Prepared` are `pub(crate)`. The only
  caller is the dispatch path in `ecs`, a sibling module, which is what
  `pub(crate)` is for. `Prepared::authority` and `Prepared::ack` are gone with
  them: `into_parts` returns both halves, and it is what the caller uses.
- **Effect dispatch is authorized and durable, and the scope is required.** Every
  `Bot` is built around an `EffectScope`: the run's `EffectIdentity` (which run
  this is, the `EnvironmentId` it acts on, and the `FlowRevision` it came from),
  the `Broker` that owns the environment's generation, and the `EffectJournal`
  the dispatch is written to. Both builder entry points refuse to build without
  one, as `BotError::IncompleteSpec { field: "effects" }`. An identity the caller
  did not choose is one it cannot recover against, so there is no default and no
  implicit in-memory scope.
- `Bot::resolve_effect` takes an `EffectKey` instead of a work id and a
  caller-supplied revision. `EffectKey` is now the settlement identity: `Bot::pending`
  hands one back, and evidence is compared by `ActionDigest` and `AttemptId`.
- `RetryFacts::with_authority(bool)` is now
  `RetryFacts::with_live_authority()`. Authority defaults to not live and the
  refusing direction is the default, so `true` was the only setting a caller
  could meaningfully pass: `with_authority(false)` spelled out the default under a
  name that read like a choice, and the parameter made every call site decode
  which way round the boolean went. Removing it removes the question.


### lgwks_bot Added

- **`EffectJournal::compare_and_append_async`, `FileJournal::open_with_stalled_storage`,
  `FileJournal::release_storage`, and `FileJournal::storage_gate` visibility.**
  The async half of the journal now shares the sync half's frame preparation and
  its authoritative length-check, write, `sync_all` path, so the two cannot
  drift into writing different bytes. The async surface is additive: the sync
  signature and every on-disk frame and chain format are unchanged, so no
  existing store needs migrating. `FileView` gives lock-free fence checks
  without a second open, and `FileJournal` drives its writes from a single
  owner thread over a capacity-one request slot, which is what makes ordering
  a property of the type rather than of a caller.
- **An ambiguous write is poisoned, never reported as a clean failure.** When a
  waiter awaiting an append is dropped, the owner learns of it and poisons the
  handle with the reason. Before this, a dropped waiter was invisible to the
  owner: it completed the write, and a later append on the same handle would
  have proceeded as though nothing had been uncertain. The repair is a reopen,
  which replays to the same facts.
- **`BotError::EffectUnrecorded`, `DispatchCertainty::Occurred`, and
  `TransitionHold::RecordingFailed`.** A post-effect journal failure now
  carries the exact `EffectKey` and the observed `Applied`/`NotApplied` fact
  instead of being retyped as a pre-dispatch refusal. `Occurred` answers "the
  effect definitely happened" — the arm `Refused` (nothing left the process)
  and `NotDelivered` (a retry is a retry) both lied about. All three types are
  `#[non_exhaustive]`, so this is additive.
- **The locator ladder: `Anchor`, `Ladder`, and `RecognitionVector::recognize_with_ladder`.**
  A ladder is the search order `docs/general-bot-fold.md` §3.2 sequences as step
  4: stable `id`, then `data-testid`, then ARIA role, then visible text, then a
  class path. `ElementFacts` now carries the anchors a snapshot offers
  (`with_anchor` / `anchor`), because a ladder that cannot be joined to its
  candidates is a half-built path.
