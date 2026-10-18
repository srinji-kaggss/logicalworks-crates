# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (3/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 3 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_bot — a tick is measured stage by stage, and the rig that measures it runs again (#279) (continued)

`bench/` publishes a ratio between this crate's tick and a hand-rolled loop
doing provably identical work, and the ratio used to be read as "the schedule is
slow" without anyone knowing which half of it. It is now measured directly, and
the answer is not what the ratio used to imply.

- **A new default-off `profile` feature, and `Bot::tick_profiled`.** The same
  four phases as `tick_async` with a per-stage instrument armed: `poll`,
  `fingerprint`, `compare`, `schedule`, `decide`, `act`. The stages are a
  partition of the tick and nest in one direction — the schedule step wraps the
  stages inside it, so `TickStage::Schedule` is the *residual* — and each charge
  reads the clock once rather than twice. Nothing in the crate's execution path
  reads the instrument, and every charge site is behind the feature, so a build
  without `profile` contains no clock read at all. `bench/` is its only caller.
- **The decision pass is now two batched passes instead of one interleaved
  walk.** Which chains moved, and whether each newest observation is admitted
  over the payload a retained transition holds, are answered for every chain
  before the walk starts; the admitted-input identity of every value a chain
  will bind is derived in one batch after that. The admission comparison is
  cached rather than recomputed inside the walk, the admitted-input stamps are
  still consumed in declaration order, and the decisions are the same ones —
  which is what makes change detection and identity derivation two separately
  measurable stages rather than one interleaved loop.
- **`bench/` runs again.** Its rig built a bot with no effect scope, which the
  crate now refuses at assembly, so it had not produced a timing in months. It
  dispatches through an in-memory journal with an `EffectLifetime::Local`
  action, each round builds and warms its own bot so the retained journal stays
  inside `MAX_JOURNAL_EVENTS` and admission sits outside the timed window, and
  every row reports the events its busiest round retained beside the ceiling.
  Two new doors: `--profile` for the stage breakdown and `--alloc-report` for
  the allocation model.
- **The published ratios went up, and the reason is stated rather than
  explained away.** The previous table (72x–256x) was taken on a tree that
  dispatched effects with no ledger, no warrant and no record, and whose
  source-level digest let a chain skip its poll entirely. Both of those are gone.
  On this tree (118x–2 665x) `act` — the ledger, warrant and record each
  dispatched effect is written through — is the largest stage in every scenario,
  41% of a quiet tick that fires nothing and 84–98% of the effect-heavy ones.
  #279's targets (≤ 10x quiet, ≤ 30x churn) are missed on every row.
  `bench/README.md` and `docs/production-readiness.md` §4.9 carry the numbers
  and the decomposition.
- **`TickProfile::per_tick` returns `Option<Duration>`**: `None` for a profile
  with no ticks, where it used to return a zero that read as a free stage, and
  the mean is divided over the whole `u64` count rather than a count saturated
  at `u32::MAX`.


### lgwks_bot — row 3 of #278: a credential that expires mid-run

The credential-expiry row of `docs/production-readiness.md` §4.4. A grant that
named no lifetime behaves exactly as before. **Breaking:** `Need::chain()`
returns `Option<usize>`, and a `gh` call the receiver refuses with 401/403/404
now ends a verb or a flow as `CredentialRejected` (`RetryClass::Never`) where it
was a `DomainError` with `NotDelivered`, which a retry policy repeated against
the same refused token.

- `GrantSet::grant_expiring(cap, ttl)` gives a capability a lifetime, and
  `GrantSet::issue_at(required, clock)` mints a proof that carries it. The
  lifetime is a **duration on the declared clock**, never a timestamp: an
  instant has no epoch, means nothing on another host, and is the value
  INV-BOT-30 refuses to persist. Re-granting the same capability with a longer
  lifetime does **not** widen it — the shorter one wins — because widening a
  credential's life by re-granting is how a token its issuer cancelled keeps
  working. A proof covering several capabilities expires with the shortest of
  them. A lifetime bounds a proof **in hand**: a built bot mints a fresh proof
  per call from the set it retained, so a lifetime does not narrow a running
  bot, and there is still no revocation.
- `Auth::check` refuses a lapsed proof before it checks coverage, as
  `BotError::CredentialExpired { capabilities, expired_at, now }`. It is a
  separate arm from `CapabilityDenied` because the repairs are opposite: a
  deficit is closed by *granting*, an expired credential by **re-granting** —
  the capabilities are still held and now name nothing. `dispatch_certainty` is
  `Refused` and `retry_class` is `Never`, so this ends a refresh-then-retry loop
  rather than feeding one. `Auth::expires_at`, `Auth::remaining` and
  `Auth::is_expired` are the total forms for a caller that wants the answer
  without the error.
- An upstream refusal is `BotError::CredentialRejected { domain, status, needs }`,
  built by `cap::upstream_credential_rejection` behind the shipped predicate
  `cap::is_credential_status` (401/403/404, the last for the reason `GhError`
  documents). It carries the repair as a `NeedSet` — the new
  `Need::CredentialExpired` — so a caller reads what to re-grant instead of
  reconstructing it from a status code, and `NeedSet::proposed_grants` derives
  the same set a deficit would. Wired on the shipped path: the client's
  `GhError::Unauthorized` reaches `PrSnapshotSource::poll` and every flow
  (`From<GhError> for FlowError`, so the review path's head read too) as that
  rejection, through one private `GhError::credential_rejection`;
  `GhError::repair` returns the same need set and `None` for a transport
  failure, which no authority repairs.
  `Need::chain()` now returns `Option<usize>`, because a lapsed credential is a
  fact about the run's authority and belongs to no chain.
- Evidence: `tests/it/credential.rs` spends a real credential's lifetime on a
  real wall clock against a real file through `domain::data::JsonStore`, then
  re-grants and reads again; `tests/it/sim_credential.rs` sweeps 1,024 seeds
  over minted proofs and virtual clocks, asserting the admitted/expired verdict
  against the model, the two readings the refusal carries, the classifier over
  every status the sweep draws, and a replayed trace hash.
  `gh_binding::a_refused_credential_reaches_the_observe_verb_as_its_repair`
  polls a real fake-`gh` child answering 401 through the `Observe` verb, and
  `sim_review_path::a_refused_credential_stops_the_review_with_its_repair`
  runs the whole review over 16 seeds drawing 401/403/404; both fail with the
  wiring removed (the old `DomainError` came back) and publish nothing.

### lgwks_bot — row 1 of #278: a torn read is pending, never a change

The first of the seven runtime-level mess rows in `docs/production-readiness.md`
§4.4. Additive: no signature changed and no existing behaviour did.
