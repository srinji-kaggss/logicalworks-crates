# lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28 (2/5)

Release: lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28. Page 2 of 5. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

  The walk decides by the rung's own anchor and by nothing else. One hit
  resolves; two or more is `Ambiguous` and is **not** retried at a weaker rung —
  two elements that share an `id` are not disambiguated by the fact that only
  one of them happens to say "Submit" today. Zero hits falls through, and the
  final `Absent` reports the strongest near-miss any rung observed rather than a
  bare zero.

- **`EffectScope::ephemeral()`, and a run identity a host can mint.** Building a
  bot with no host, no persisted history and no flow document meant writing an
  identity out by hand: a `RunId` and an `EnvironmentId` as literal hex, a
  `FlowRevision` as a literal digest, a registered broker and an in-memory
  journal, in that order, in every test and example. `EffectScope::ephemeral()`
  mints the two identities from OS entropy, registers that environment, and
  pairs them with a `MemoryJournal`.

  It is a capability rather than a convenience because of what the journal
  refuses: `MemoryJournal` reports `DurabilityPromise::Ephemeral`, and
  `EffectJournal::admit_external_handoff` returns `JournalError::PromiseUnmet`
  for it — so an effect that would leave the process fails at the boundary
  instead of proceeding on a record that cannot outlive the process that wrote
  it. The difference between this and "no journal" is that this one says so.

  `RunId::mint()` and `EnvironmentId::mint()` are public for the same reason
  from the durable side: the module has always said the run identity is
  *generated* before the first admission, and until now the only way to generate
  one was to supply hex. `ActionId` deliberately gains none — the crate already
  derives it from a bot's structure, and a second way to make one value is the
  thing the dependency and API doctrine both refuse. `FlowRevision` is a fixed
  domain-separated constant in the ephemeral case, because an ephemeral run has
  no flow document and minting a revision would assert a content change that did
  not happen.
- `MintError` and `EphemeralError`, both `#[non_exhaustive]` and both carrying
  their cause rather than flattening it to a string.
- Feature `ephemeral`, **opt-in**, which turns on `lgwks_std/random`. It authors
  no edge of its own: `getrandom` is owned by `lgwks_std` under
  `contract/APPROVED.toml` and this is a feature of a dependency the crate
  already has. `lgwks_std` enforces INV-RANDOM-ONE-SOURCE, which is why there is
  no cheaper fallback here — a run id derived from a clock, a pid or a counter
  is the collision that invariant exists to refuse.

  It is not in the default set, and the reason is the target rather than the
  cost. Minting needs OS entropy, `lgwks_std::random` is linux/macOS/windows
  only and refuses the rest with a `compile_error!`, and this crate's default
  feature set is built for `wasm32-wasip1` by the WASI boundary job. A
  default-on `ephemeral` makes the default set fail to build on a target the
  crate supports. `signal` is host-only in the same way and is default-off for
  the same reason. `full` includes `ephemeral`, and the runner step tests
  `--features full`, so the ephemeral tests execute in CI rather than only
  compiling.


- **Write-ahead dispatch.** `IntentAdmitted` and `DispatchPrepared` are committed
  to the journal before the effect leaves the process, and `Broker::revalidate`
  re-checks the environment generation at the handoff, with no await between mint
  and check.
- **A restart continues the record rather than the process.** A bot
  reconstructed against a journal holds an attempt whose outcome was never
  established instead of resending it, and retires an attempt whose effect
  landed. Two defects the new tests found are fixed: a restart re-minted
  `AttemptId::FIRST` and so reproduced a key the journal had already walked past,
  which refused every `NotApplied` retry as `OutOfOrder`; and a crash after a
  landed effect left the run unable to continue past that entry at all.
- `Debug` for the public surface. Every public type in the crate now implements
  it except the seven `*Resolver` companions rkyv generates for the three
  macro-invoked families in `effect.rs` (`id_role!`, `counter_role!`,
  `digest_role!`): `RunIdResolver` through `ActionDigestResolver`. The named
  types and their `Archived*` companions do have it, so the
  `#[rkyv(derive(Debug))]` attribute reaches the archived type and stops short of
  the resolver. Derived where a derive is the right rendering, manual where it is
  not, with the reason on the impl. `SemanticResolver` uses
  `finish_non_exhaustive` rather than acquiring an `E: Debug` bound that no
  `Embedder` is required to satisfy. `Session` prints counts rather than the
  transcript entry by entry, and the `bevy_ecs`-backed types do not descend into
  a schedule that has no `Debug`.
- `#[must_use]` on `eval::{Changed, Below, Above}::new`, which their three
  sibling constructors already carried. `must_use_candidate` does not reach
  `-> Self` constructors, which is how the inconsistency survived a green build.


### lgwks_bot Breaking

- `Execute::effect_lifetime` declares whether the action's effect reaches
  outside this process. The default is `EffectLifetime::External`, the
  conservative reading of an unclassified handoff: an action that is only
  local says so. An ephemeral journal now refuses an external effect on the
  actual dispatch path — `Effects::prepare` calls
  `EffectJournal::admit_external_handoff` and checks the `DispatchPrepared`
  acknowledgment's promise against what the handoff requires, instead of
  discarding it. A journal that advertises `ProcessCrash` and acks `Ephemeral`
  is refused. `MemoryJournal` stays `Ephemeral` and stays useful for
  explicitly local work.
- `Observe::Output` must implement the new `InputIdentity`, alongside the
  `PartialEq` it already needed. The dispatch digest used to bind the
  process-local `Revision` counter, which reopens at 1 after every
  reconstruction, so a restarted bot read a *different* request as already
  applied and silently omitted it. The digest now binds the admitted input's
  content identity. `InputIdentity` is implemented for the fixed-width
  integers, `bool`, `String` and `&str`; a caller whose output is something
  else writes the identity bytes themselves. Two events with equal payloads
  stay distinguishable by tagging them with `EventId`, whose id is part of
  both `InputIdentity` and `PartialEq` — content equality is not event
  identity.

### lgwks_bot Added

- `InputIdentity` and `EventId`. The first names an admitted observation for
  the purpose of a dispatch digest; the second tags a payload with an event id
  so two equal payloads are two events.

### lgwks_bot Fixed
