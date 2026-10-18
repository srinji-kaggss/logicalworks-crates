# lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28 (4/5)

Release: lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28. Page 4 of 5. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Fixed (continued)

- An ephemeral scope enforces its advertised external-effect refusal.
  `Effects::prepare` used to discard the `DispatchPrepared` acknowledgment as
  `_ack` and never called `admit_external_handoff`, so a local in-memory
  ladder admitted an effect that outlived the process. Durability admission is
  now on the handoff path and the acknowledgment is checked, not the
  advertisement (`tests/durable_dispatch.rs::an_ephemeral_scope_refuses_an_external_effect_before_it_runs`).
- Action identities are length-framed and index-portable. `derive_action_id`
  used to concatenate `bot` and `domain` around `usize::to_le_bytes` with no
  length prefix, so `"ab"`+`"c"` and `"a"`+`"bc"` derived the same `ActionId`
  and a 32-bit and a 64-bit target derived different ones for the same bot.
  Fields are now `u64`-width length-prefixed; the domain separators are
  `action-id.v2` and `action-digest.v2`.
- Assembly rejects a recovered key from another flow or another environment,
  not only another run. Folding in a foreign journal is how a bot dispatches
  an action whose earlier attempt belongs to a different document.
- A recovered unknown is a barrier in front of a false condition, not
  something a new observation can skip past. `plan_chain` used to evaluate
  the current value first and emit `Decision::Skip` for an entry whose action
  a journal already recorded as dispatched with no outcome; `run_chain`
  marked that entry `Skipped` before it ever consulted `effects.blocks`, so
  the successor ran and the tick could return `Ok` while `pending` still
  named the unknown. A changed source is not evidence that the earlier
  effect did not occur. The barrier is now checked before the condition, a
  `Skip` that somehow reaches the walk for a blocked action stops the chain
  instead of burying the hold, and `pending` / `first_unresolved` /
  `unresolved_count` are one fold over the declared work — so a recovered
  unknown is reported whether or not a transition exists, and the three
  reports agree (`tests/durable_dispatch.rs::a_recovered_unknown_blocks_a_false_condition_and_its_successor`
  and the unattempted false-condition control in `ecs::tests`).
- Live settlement is journaled before it is acknowledged, and a repeat of the
  same evidence is idempotent across a restart. `Ledger::settle` used to move
  only in-memory state while `settle_recovered` appended `OutcomeObserved`, so
  a process that settled an effect and then died left a journal that recorded
  the dispatch and nothing else: the next bot held the attempt forever, and a
  caller repeating the acknowledgement it had already given was told
  `NoSuchWork`. Settlement is now one write (`Effects::ensure_outcome`) shared
  by the live and recovered paths, decided by a read-only
  `classify_settlement` and applied by `apply_settlement` only when the live
  slot still needs moving — an abandoned entry reopened by confirmation that
  its effect did not land is exactly that case, and answering `Duplicate`
  before the move is how that reopen stopped happening. When the live slot has
  nothing left to move, the journal's record answers: same evidence is
  `Settled::Duplicate`, the other evidence is `Settled::Contradicted`, and only
  a key the journal never recorded is `NoSuchWork`
  (`tests/durable_dispatch.rs::a_live_settlement_is_journaled_before_it_is_acknowledged`).
- Locator ladder candidates now share the flat recognizer's frame and kind
  eligibility gate, while anchor mismatches preserve measured fingerprint
  evidence instead of reporting a falsified zero score.
- Process supervision now establishes its process-group guard before scheduling
  the child and reports bounded descendant cleanup evidence instead of treating
  the leader's exit as proof that the process tree is gone.
- A post-effect journal failure is no longer reported as a pre-dispatch
  refusal and terminal abandonment. `run_chain` used to replace the action's
  known outcome with `BotError::EffectRefused` when the `OutcomeObserved`
  append failed, so `dispatch_certainty()` answered `Refused` and
  `RetryClass::Never` became `Abandoned` — a controller that read that as
  "safe to replan" would issue a new logical action and duplicate the
  external operation. Execution outcome, persistence acknowledgment,
  verification and cleanup are now separate dimensions: the entry is held as
  `RecordingFailed` with its key and evidence, and a retry of the recording
  never re-enters the action. Append failures *before* the action stay
  `EffectRefused`, which is the negative control.
- Observation fingerprints no longer commit before the values they describe.
  `poll_sources` used to publish each source's digest as it polled, then
  `observe_fold` committed payloads all-or-nothing; a failed sibling poll left
  the successful sources' digests advanced over values the fold refused to
  hold, and the next tick skipped that uncommitted work forever. Digests are
  now staged in a per-tick `Candidates` map and published to `Fingerprints`
  only when the fold admits every observation. An aborted fold discards the
  candidates. `Fingerprints` also never leaves the world during a tick, so a
  tick dropped mid-poll can no longer empty the map and permanently disable
  caching. Regression tests cover the issue #99 acceptance list: a failed
  sibling on first poll, a committed source that moves during a sibling
  failure, several failed siblings, a held transition plus a newer candidate,
  and cancellation during a source poll.
- `interface::RecognitionVector` and `language::LanguageResolver` each carried a
  comment explaining why they were deliberately not `Debug`. The first stopped
  being true when `lgwks_std`'s `Weighted` gained an impl that reports
  composition rather than addresses. The second was wrong when it was written:
  the alias table is a `BTreeMap`, so a derived `Debug` prints it in key order
  and is stable across runs. Both now derive `Debug`, and both comments say what
  is true.
- `#[must_use]` on `EcsBuilder::observe`, `EcsObserveBuilder::on` and
  `EcsObserveBuilder::observe`. Each one consumes a builder and hands back the
  next, so a discarded result silently drops every `on` declared up to that
  point. A sweep of the crate's public surface found 103 methods returning
  `Self`: 92 carry `#[must_use]`, and the 11 that do not are all `new`
  constructors. `EcsObserveBuilder::on` was the only consuming builder without
  it. The two `observe` methods return `EcsObserveBuilder<_>` rather than `Self`,
  so a scan for `-> Self` does not see them, and neither of those carried it
  either. `must_use_candidate` reaches none of the three, which is how they
  survived a green build.
- A returning landed event retires rather than refusing or re-entering. Live
  `OutcomeObserved` is now written through `Effects::ensure_outcome`, which
  folds `Applied` into the in-memory applied set, so `applied_in` can see it.
  A raw append left that set empty: the same `EventId` came back after another
  event, minted `AttemptId::FIRST` again, and the journal answered `OutOfOrder`
  after the entry was already marked `Unrecorded` — a false barrier instead of
  a retirement (`tests/durable_dispatch.rs::a_returning_landed_event_is_retired_not_refused`).
- A handoff refused before the action ran no longer leaves a false
  `Unrecorded` barrier. `begin_attempt` used to move `record.begun` and
  `EntryState` before the journal writes, so a `PromiseUnmet` left the entry
  claiming an effect might be live. The write-ahead pair is committed first and
  the memory moves only after its acknowledgments are strong enough
  (`tests/durable_dispatch.rs::a_refused_handoff_leaves_no_unrecorded_barrier`).
- A weak per-append acknowledgment is refused before an external handoff can
  run. A weak intent acknowledgment leaves nothing prepared. A weak
  `DispatchPrepared` acknowledgment may already be committed, so the kernel
  returns `PromiseUnmet` and recovery preserves `OutcomeUnknown` rather than
  inventing `NotApplied`
  (`tests/durable_dispatch.rs::a_weak_ack_does_not_record_dispatch_prepared`).
