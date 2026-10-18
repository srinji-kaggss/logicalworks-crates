# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (6/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 6 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- **`broker`, the environment fence: authority for one attempt at one
  generation, and the refusal that stops a stale command from dispatching.** A
  durable record of a dispatch aimed at an environment that has already been
  replaced is accurate and still wrong, so the record cannot be where that is
  caught. `Broker` owns which environments a run has and which generation each
  is at, and `authorize` mints an `Authority` only when the generation named by
  the `EffectKey` is the one currently held.
  - **A stale generation and a generation the broker never issued are different
    errors.** `Superseded` means the key names an older generation: the real
    case, a command that was correct and is now fenced. `NeverIssued` means it
    names a newer one the broker never minted, so the key did not come from this
    broker at all. One "generation mismatch" error would print the same sentence
    for both while calling for opposite investigations.
  - **`Authority` is a sealed proof, and the seal is the constructor.** Fields
    private, no public constructor, `Broker::authorize` the only minter, the same
    shape as `cap`'s `Auth`. It is deliberately not `Clone`, which stops a
    warrant being scattered. That is not what makes a double dispatch impossible:
    the journal's ordering ladder is, and the type documents that rather than
    implying otherwise.
  - **The fence runs twice, and it has to.** Authorizing and handing over are
    two instants. A replacement landing between them leaves a warrant that was
    minted legitimately and is now stale, and a check that only runs at mint time
    does not deliver RQ-006's "replacement invalidates all old commands". So
    `revalidate` re-runs the identical comparison at the boundary, and it shares
    one private `check_generation` with `authorize` rather than repeating it: a
    second copy that drifted would refuse to mint a warrant and accept it at the
    boundary, which is the failure fencing exists to prevent.
  - **`prepare_dispatch` is where the RQ-007 order stops being prose.** It
    authorizes, then appends `DispatchPrepared`, and only then returns a
    `Prepared` holding both. A caller cannot append before it is authorized, or
    hand over before the append landed, because there is nothing to hand over
    until both steps have run. A superseded key never reaches the append, so a
    fenced command cannot become a recorded attempt.
  - **An environment is a resource with a declared lifetime.** `register` takes
    ownership, `replace` bumps the generation and invalidates every warrant
    already handed out, and `close` releases it. Closing is idempotent, because a
    cleanup path that has to know whether it already ran is one that will
    sometimes not run. Generation exhaustion is named rather than wrapped, since
    a wrapped generation would re-authorize commands the broker already fenced.
  - **18 unit tests.** They pin each refusal, both directions of the fencing
    comparison, the boundary re-check on both a replaced and a closed
    environment, the exhaustion path, and that a superseded key leaves the
    journal untouched. What they do not do is reach a real environment: no process,
    socket or input seat exists here, so `EnvironmentId` arrives from the host's
    own entropy and nothing in this crate creates one.
- **`retry`, RQ-009's admissibility rule: the vocabulary existed, the decision
  did not.** `RetryClass` already said what a failure permits and
  `DispatchCertainty` already said what it established, with
  `BotError::retry_class` as the total map between them that never reads a
  rendered cause. What was missing is the rule that composes those with the
  budget, the authority, the intent and a remote deduplication contract:
  `retry_admissible = budget_remaining AND live_authority AND
  unchanged_logical_intent AND (proven_not_applied OR contract_valid)`.
  - **It is a pure function of facts the caller established**, the shape
    `DispatchCertainty` already has with its producers. It reaches for no clock,
    no broker and no journal, so every branch is pinned by a test rather than by
    a scenario.
  - **`DeduplicationContract` records all five things RQ-009 lists**: the
    logical request key, the payload binding, the scope, the retention window
    and the late-arrival behaviour. The key and the payload reuse the effect
    key's own vocabulary, because that split is already the right one: the
    `ActionId` is the logical intent that stays fixed across a resend, and the
    `ActionDigest` is the bound payload that must not change with it.
  - **The late-arrival answer is where a contract can be worth nothing.** Inside
    the retention window the remote deduplicates by construction. Past it, the
    contract rules out a duplicate only if the remote demonstrably *refuses* a
    late arrival. `Applied` means a second application; `Unspecified` means
    nobody knows, and not knowing is not a basis for sending. An adapter that
    leaves that field unstated gets a refusal rather than a duplicate.
  - **Every failing conjunct is reported, not the first.** The four have four
    unrelated repairs, and the estate already made this argument for
    capabilities: a check that reveals one missing item at a time makes the
    repair a loop. The one exception is a permanent failure, which short-circuits
    because the remaining conjuncts are moot and reporting them would be noise.
  - **What a retry cannot reach: a GUI click.** RQ-009's contract half is
    reachable only by supplying a recorded contract, and nothing here can
    express one for a click, so an unsettled click is never retried on that half
    of the disjunct.
  - **21 unit tests**, covering both directions of the retention boundary, a
    contract recorded for another action and for another payload, the refusing
    default on the authority fact, and the all-conjuncts-failed report. No eval
    case moves off `not_run`: RQ-009's rule is a decision over stated facts, and
    the cases that would exercise it end to end need a real receiver.
