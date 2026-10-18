# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (5/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 5 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- **`effect`, the durable effect identity the ledger was missing: a settlement
  now names *which attempt* it is about.** The ECS ledger already settles an
  entry and already refuses a contradicting settlement. What it never carried is
  identity: `EntryState::DefinitelyFailed` holds `attempts: u32`, so two
  deliveries that both arrive as "attempt 3" are indistinguishable, and a repeat
  of an old settlement looks exactly like a statement about a new one at the
  moment the ledger decides whether to accept it. `EffectKey` binds the seven
  fields a settlement must name (run, action, attempt, flow revision, action
  digest, environment, environment epoch), so "is this the same settlement?" is
  a comparison rather than a judgement about a counter.
  - **It reuses the estate's one content hash rather than minting a second.**
    `FlowRevision` and `ActionDigest` are newtypes over
    `lgwks_std::hash::Digest`, not replacements for it. They have different
    domains: one binds the validated flow document, the other binds operation,
    target, preconditions, postcondition and exact input. A newtype each is what
    stops a call site passing one where the other is required.
  - **`AttemptId` and `EnvironmentEpoch` refuse to wrap.** `checked_next()`
    returns `None` at `u64::MAX` instead of restarting at 1. A wrapped counter
    hands out an identity it has already issued for this sequence, and the ledger
    would then refuse a genuine settlement as a duplicate. Running out is
    recoverable; silently reusing an identity is not.
  - **The schema admits two digest algorithms; this crate accepts one.**
    `sha256` parses and is then refused as `UnsupportedAlgorithm` rather than
    rejected as malformed, because it is well-formed on the wire and the error a
    caller sees should say the algorithm is unsupported rather than that their
    JSON was wrong. A settlement is accepted because the receiver recomputed the
    digest, and a tag naming an algorithm it cannot recompute is an unverifiable
    claim dressed as identity.
  - **Ids are strict on parse**: 32 lowercase hex characters, big-endian, with
    uppercase rejected rather than folded and the all-zero id refused. Zero is
    the schema's own exclusion and also what a zeroed or truncated buffer
    produces, so refusing it turns a class of uninitialised-identity bugs into a
    parse error.
  - **19 unit tests, plus 6 that encode the identity half of eval cases E06 and
    E07** (`tests/effect_identity.rs`), both of which `okf/evals.json` records as
    `not_run`. What those six establish is that the identity predicates
    distinguish every delivery the two cases name: a reused slot, a forward epoch
    bump, a previous attempt, an exact duplicate, and a rewritten payload. What
    they do not establish is the durable half, because E06 and E07 both require a
    real receiver or persistent-state oracle and the crate has neither a journal
    nor an environment. That gap is not narrowed by anything here.
  - **One of the six is the design result worth keeping.** A key cannot
    distinguish a settled attempt from a contradicted one, because both are the
    same key with different evidence. That is why the ledger keeps its per
    generation settlement record beside the entry rather than deriving settlement
    from identity alone, and why this module does not attempt to replace it.
  - Nothing is wired to the ledger yet. This is the identity layer; the
    settlement call site needs a run and an environment to key against, and a
    grep for either concept across the crate returns nothing before this module.
- **`journal`, the durable append that has to land before the irreversible
  boundary, and the durability grade that decides whether a store may host one.**
  `effect` supplied the identity a settlement is about; an identity held only in
  memory is lost at exactly the moment it is needed. A controller that hands
  bytes to an external system and then dies cannot say whether they arrived, and
  a controller that guesses either duplicates a non-idempotent effect or drops
  one. `EffectJournal::compare_and_append` is the seam: append `event` if and only
  if `expected_tail` is still the committed tail, returning a `DurableAck` that
  names the committed position and the promise claimed for it.
  - **The tail is a position, not a counter.** `JournalPosition` carries a
    sequence number *and* a hash over every event up to and including that one,
    chained so that the same two events in the other order produce a different
    head. A sequence alone would let two journals that diverged agree on where
    they were. `verify_chain` recomputes the chain and names the first entry that
    does not follow, which is what makes a dropped or reordered entry detectable
    rather than merely suspicious.
  - **The ordering ladder is enforced at the append rather than trusted.** One
    attempt at one intent walks `IntentAdmitted`, `DispatchPrepared`,
    `OutcomeObserved`, `Verified`, exactly once. A second `DispatchPrepared` for
    an existing key is refused as `OutOfOrder`, so a second dispatch of one
    attempt is refused rather than merely discouraged. A legitimate retry is a
    new `AttemptId` and therefore a new key with its own fresh ladder, and
    `tests/effect_journal.rs` pins the retry alongside the refusal so the guard
    cannot be mistaken for a ban on trying again.
  - **What the ladder does not enforce: RQ-009's retry admissibility.** A caller
    that mints a new attempt for the same intent gets a clean ladder, and nothing
    here checks `budget_remaining`, live authority, unchanged intent or proof
    that the effect did not land. The ladder makes a *resend* unrepresentable; it
    does not decide whether a *retry* is admissible, and that decision does not
    exist anywhere in the crate yet.
  - **Durability is graded, and the grade is what refuses.** `DurabilityPromise`
    is `Ephemeral`, `ProcessCrash` or `PowerLoss`, and `admit_external_handoff`
    refuses anything below `ProcessCrash`. The in-memory adapter reports
    `Ephemeral` and is refused, which is the whole reason it is a named type
    rather than a default: a caller that reaches for it gets a refusal at the
    boundary instead of a green test that means nothing.
  - **The acknowledgment records the promise rather than implying one was
    proven.** Nothing here can verify a durability claim, because a filesystem, a
    device cache or a virtualization layer can each accept a write and lose it
    anyway. `DurableAck` carries the promise it was minted under and its
    documentation says so. The tests that would decide the claim are crash tests
    against a real store, and they are not unit tests.
  - **`EffectEvidence` is the ledger's, re-exported rather than restated.** The
    journal and the ledger answer the same question, and a second enum with the
    same two arms would drift from the first the next time an arm was added. It
    gained `Hash`, which is additive on a fieldless enum.
  - **`EffectKey::to_bytes` is the canonical encoding the chain hashes**, fixed
    width at 130 bytes, every field written and every integer big-endian, with
    each digest carrying its algorithm tag so swapping the algorithm moves the
    chain position. The length is summed from the field widths rather than
    written as a literal, so adding a field to the key is a compile error until
    the encoding carries it.
  - **24 unit tests, plus 7 that encode the journal half of eval cases E10 and
    E11** (`tests/effect_journal.rs`), both of which `okf/evals.json` records as
    `not_run`. E10 is a lost reply after a real external commit and E11 is a
    crash at every durable boundary. Both require a real receiver, a real restart
    or a persistent-state oracle, and the crate has none of the three, so
    neither case moves off `not_run` on the strength of this entry.
  - **Nothing is wired to a real store or to the ledger yet.** `MemoryJournal` is
    a reference adapter that exists in order to be refused at the boundary, and
    no call site outside the tests appends to it.
