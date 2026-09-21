# Changelog

All notable changes to the four crates are recorded here. Versions move
independently; each release lists per-crate deltas. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
`0.x`, so any minor may carry breaking changes, which are then listed
explicitly under that crate.

## [Unreleased]

### Decision

- **`lgwks_bot` is relicensed to MPL-2.0**, from Apache-2.0. The bot is the
  artefact the rest of the estate embeds, and a permissive licence on it lets
  anyone modify it and ship the modifications closed with no later release able
  to recover that. MPL-2.0 is file-level copyleft: use stays unrestricted
  including in proprietary products (MPL-2.0 §3.3), and only modification of the
  MPL-covered files carries the §3.2 source obligation. `lgwks_std`, `lgwks_ast`
  and `lgwks_deps` remain Apache-2.0.
- **No published version changed.** Licences are not retroactive; every version
  on crates.io including `lgwks_bot` 0.4.2 stays Apache-2.0. The new terms take
  effect at the next version published from this tree, which is why no crate
  version is bumped here. The four dependent repositories pin exact published
  versions and none is affected until it moves.
- **`LICENSING.md` records the model**, including the commercial licence that
  grants relief from §3.2 for organisations that need to modify the bot without
  publishing those modifications. Offering two licences requires holding rights
  in every contribution, so a contributor licence agreement has to be in place
  before a non-trivial `lgwks_bot` contribution can be merged.
- **`lgwks_bot` is closed to outside contributions, and the contributor licence
  agreement that would reopen it is deferred.** A patch sent today has no path
  to merge, so `CONTRIBUTING.md` and `LICENSING.md` now say *closed* rather than
  *not merged until an agreement exists*. The earlier phrasing left a
  contributor to send a patch that would wait on a decision no one has made; the
  block is stated as a refusal on arrival instead. It lifts when an instrument is
  chosen and recorded in `LICENSING.md`. The other three crates are unaffected.
- **A capability refusal states the whole shortfall, in real time, and derives
  its own repair** (owner direction, 2026-09-21). The check returned at the first
  ungranted capability, so repairing a bot was a loop: grant what you were told,
  rebuild, be told the next word. Each individual refusal was true and the
  sequence was still the wrong instrument, because the gate had already computed
  the whole difference — the required set minus the granted set — and reported
  only its head. `Deficit` is that difference reported in full, `Shortage` names
  the domain that declared each requirement, and `Deficit::to_grant_set` turns
  the diagnostic back into the grant set that closes it, so the repair is derived
  rather than transcribed. This is the shape the owner asked for explicitly:
  isolate what is missing at the point of the attempt, hand back the missing
  pieces, and continue — not a build loop that refuses one word at a time.
- **A run-time capability hold was designed, built, and then removed as
  unreachable.** The step after a total refusal is the natural one: deny an
  action at run time, hold the entry, let the caller supply the capability, and
  continue from that point rather than restarting the chain. It cannot be
  reached, and the reason is structural rather than incidental. `assemble` admits
  every declared requirement before the world exists, and every per-call proof is
  minted from **the same list** — `run_any` issues
  `grants.issue(self.0.required_caps())` and the action checks
  `call.0.check(self.required_caps())` — so the declaration and the check cannot
  disagree, and nothing narrows `Grants` after `assemble`. `Auth::check` cannot
  fail inside a running bot. The hold was removed rather than shipped behind a
  contrived test, and the finding is recorded in
  `docs/guides/lgwks-bot/authority.md` with its line citations.
- **The gap the hold was reaching for is the opposite one, and it is real: an
  action that declares `&[]` and performs a side effect passes the gate
  silently.** The gate checks the declared set and `&[]` is trivially covered, so
  "this action needs nothing" and "this action's author never said" are the same
  value and the crate cannot distinguish them. Every guarantee about authority is
  a guarantee about capabilities a domain *declares*. Closing that, or making a
  run-time hold meaningful, is a design change rather than a repair, and neither
  is made here.
- **Four `lgwks_bot` questions were put to the project owner on 2026-09-21 and
  decided.** They are recorded here because each was previously stated in a
  first-party document as *open*, and those documents now say what was decided.

  - **The scheduler is a first-party `lgwks_std` module**, not zed's vendored
    `scheduler` crate. `docs/bot-on-ecs.md` §9 and §10 reserved the choice,
    because the doctrine and the instruction to reuse existing open source point
    at different rungs. It was taken against a finding recorded in full in §9:
    the tick is already deterministic by construction, so a seeded scheduler has
    no caller until the recorder lands. One consequence is named there too — a
    seeded PRNG needs a carve-out from INV-RANDOM-ONE-SOURCE, argued in the
    module rather than assumed at the call site.
  - **The `Time<Virtual>` clock root (§10 step 4) is closed as already
    satisfied.** Two time layers exist and both are tested: `lgwks_std::time`
    (RFC 3339 and calendar arithmetic, INV-TIME-PURE) and `lgwks_bot::rt::time`
    (`sleep`, `timeout`, `interval`, `Instant`). No `Clock` trait or mock time
    source exists anywhere in the workspace, so the two `Instant::now()` reads in
    `rt::supervise` can still only be tested by genuinely waiting. That is
    accepted and stated, not overlooked.
  - **The `from_spec` gap is a gap, and the registry is scheduled work.** The
    crate doc argued that the absent materializer was a deliberate design
    position; that reading was rejected. `experience/invariants/sdk.yaml` records
    the adjudication. Two constraints bind the implementation, because they are
    what made the absence defensible: grants keep coming from a caller-held
    `GrantSet` and never from the spec, so wire data cannot choose what a bot
    reaches, and there is still no `bot!` proc-macro.
  - **No release is cut yet.** Five public modules (`session`, `language`,
    `semantic`, `interface`, `frontier`) and the `lgwks_bot` MPL-2.0 relicense
    are on `main` and unpublished. They ship in one release once the registry and
    the scheduler have landed.

### lgwks_bot Fixed

- **A tick can no longer be run from inside an async runtime through the
  synchronous adapter.** `Bot::tick` drives the tick with a thread-parking
  executor. Called from a thread that an async runtime is driving — a
  current-thread runtime above all — parking that thread stops the reactor dead,
  so a tick with a timer, a socket, or a sibling task never returned: it was a
  deadlock, not a slow tick. `tick` now returns `BotError::TickInsideRuntime`
  when a runtime already owns the calling thread. It refuses on a multi-worker
  runtime too, where parking the caller may happen to work, because which thread
  the caller was handed is not knowable from inside the tick.
- `Bot::tick_async` is the async adapter: the same four phases, awaited on the
  caller's executor, returning the same `Result`. It is the entry point inside a
  runtime, and the one to reach for when a verb needs this crate's timer or a
  driver.
- The tick is now decidable before it is doable. The decision system
  (`fire_plan`) records the effect program as an ordered list of steps before any
  effect runs, and the driver then awaits those steps (`EcsBot::run_steps`).
  That is what makes a condition failure's position well defined: the steps the
  walk cleared before the failing condition still run, and the tick reports the
  failure.
- `rt::task::repeat` no longer starves cancellation. It raced each iteration
  against the token but never handed the executor back, so a body whose future
  was ready on its first poll made the whole loop one uninterruptible poll: a
  cancel was delivered only after the budget was spent, and on a current-thread
  runtime the cancelling task could not be scheduled at all. `repeat` now
  completes a bounded number of iterations per poll and then yields.
- Cancellation trees no longer poll or drop recursively. `Inner::cancelled`
  built a `race_two` nesting with one stack frame per ancestor link, and the
  derived `Drop` for `parent: Option<Arc<Inner>>` recursed once per link, so a
  deep chain exhausted the stack without polling or a runtime. Both walk the
  chain once and iteratively now, and `Inner` detaches each parent link as it is
  dropped.
- **A supervised task's outcome is reported, not discarded.** `Supervisor::reap`
  evaluated only `try_join_next().is_some()` and dropped the inner
  `Result<(), JoinError>`, so a return, an abort and a panic were all one
  increment of `completed` — a panicking task counted as a success. Every task
  now ends in exactly one `TaskOutcome` (`Completed`, `Cancelled`, `Aborted`,
  `Panicked { message }`) carrying the `TaskId` assigned in spawn order; `Stats`
  splits `succeeded` / `cancelled` / `aborted` / `panicked`; and `shutdown`
  returns a `ShutdownReport`. A panic payload is preserved, capped at 512
  characters, and the report buffer is capped at the in-flight bound with
  `Stats::reports_dropped` counting what it missed: detail is lost, never memory.
- **Three first-party documents still promised live revocation, and this crate
  has no revoke operation to promise.** `GrantSet` has no revoke operation, and a
  built bot holds a snapshot of the set it was admitted with, so withdrawing a
  capability means rebuilding or ending the bot. `docs/security-posture.md`,
  `docs/general-bot-fold.md`, and `docs/guides/lgwks-bot/getting-started.md` all
  asserted an authority model this workspace does not have; all three now state
  the snapshot boundary. The new `crates/lgwks-bot/tests/authority.rs` enforces
  it — a scan over every first-party text file refuses a claim this crate cannot
  honour — and `a_proof_minted_from_a_grant_set_outlives_that_set` /
  `withdrawing_authority_after_build_means_building_again` pin the behaviour the
  prose now describes.

### lgwks_bot Documentation

- The README, the crate docs, and the `lgwks-bot` guides described the tick as
  one synchronous pass. They now document both adapters, the four phases, the
  refusal, and what a cancelled tick leaves behind.
- **Three first-party documents described `lgwks_bot`'s state as something other
  than what the tree shows; they now match it.** `docs/bot-on-ecs.md` listed the
  `Time<Virtual>` root and the exclusive-systems move as outstanding when both
  are settled — the second landed with step 2 and the migration list went on
  showing it as pending — and its §9 reserved the scheduler for sign-off, which
  has now been given. `crates/lgwks-bot/src/lib.rs` argued that the missing
  `from_spec` materializer was a deliberate position; the invariant ledger's
  reading of it prevailed and the paragraph now says so. And two citations in
  `docs/guides/lgwks-bot/index.md` pointed `Bot::tick` and `Bot::tick_async` at
  lines inside their doc blocks rather than at the functions
  (`crates/lgwks-bot/src/ecs.rs:1490`→`1496`, `crates/lgwks-bot/src/ecs.rs:1408`→`1411`)
  — a class
  `scripts/check-doc-citations.py` cannot catch, because it verifies that a
  citation resolves and not that it names the right item.

### Fixed

- **A resolver measured its margin against a field the threshold had already
  emptied** (`lgwks_bot`). Below-threshold candidates were discarded before the
  lead was computed, so a winner holding a `0.02` lead against a required `0.08`
  reported its whole score as the lead and resolved where it should have stayed
  ambiguous. The threshold and the margin are two questions asked in that order:
  the threshold asks whether an option *may* win, the margin asks whether it
  separated itself from the next best thing actually observed, and that second
  question cannot be answered against a field the first has emptied. Both tiers
  now hand every measured score to `decide`, which applies the threshold and
  then measures the lead against the real runner-up, including one below it.
- **A comparison that was never made no longer reads as one that was**
  (`lgwks_bot`, `lgwks_std`). A zero or non-finite embedding was scored
  `ZeroMagnitude` and then skipped, so a degenerate candidate vanished from the
  field and a degenerate utterance was reported as `Absent { best_score: 0.0 }`
  — the value a healthy model returns for a field it rejected. A comparison set
  missing even one member cannot produce the verdict a complete set produces, so
  an unusable vector now refuses the set and the session records
  `resolver-degraded` rather than advancing.
- **Missing element facts no longer score as matching facts** (`lgwks_bot`).
  Every recognition component delegated to a metric that scores two empty inputs
  `1.0`, so "no identifying attribute was observed" became maximum identity
  confidence, and an element's tag was never checked at all. Identity, path and
  text now decide presence before consulting their metric — absent on either
  side contributes nothing — and the tag is a gate rather than a weight. The
  component weights are deliberately *not* renormalized: a missing component
  keeps its weight missing, which is what lets the acceptance threshold state
  which facts a match actually requires.

- **The invariant audit reported "enforced" for states it never examined**
  (`lgwks_deps`). A register entry naming a lint no manifest declares, a file
  that exists but declares no test, or a lint declared at `allow` all produced a
  clean verdict, so the gate could not fail. The register now answers four
  questions separately — registration validity, reference resolution, execution,
  and verified outcome — and `check` prints `resolve`/`resolved`/`attested`,
  never `enforced`. Every verdict carries a `SCOPE` line stating that no enforcer
  was executed, so a resolved reference is not read as proof the invariant holds.
  A reference that walks out of the repository (`..`) is refused at load, before
  the filesystem is consulted.
- **A malformed `policy.enforce` silently disabled both gates** (`lgwks_deps`).
  The token was read as `value == "true"`, so `True`, `"true"`, `1`, `yes`, and
  the empty string all became `false` with no diagnostic, standing the gate down.
  A closed Boolean grammar now accepts exactly `true` and `false` and otherwise
  refuses with a typed error naming the line and the value; the key set is
  closed, and a repeated key or `[policy]` section is refused rather than
  last-write-wins.
- **The self-exemption trusted a package name** (`lgwks_deps`). Any edge whose
  *name* was `lgwks_std` or `lgwks_deps` was exempt, including an external path
  or Git package wearing that name, so an unapproved source could be admitted by
  being called the right thing. The name list is gone; the only exemption is
  Cargo's own `workspace_members` list.

- **Flow validation accepted a read before initialization** (`lgwks_bot`).
  Validation tested variable names for *global* writer membership, so a document
  was accepted whenever a writer existed anywhere — including one that runs only
  after the read. Validation now runs a forward definite-assignment analysis: a
  node may read a variable only if every entry-reaching path assigns it. Reads
  of undeclared names still report `UndeclaredVariable` first, and a branch's
  own condition variable is not counted as a read because the executor never
  resolves it. New typed variant `BotError::VariableReadBeforeInit`.
- **The scanner reported discarded errors that were never errors**
  (`lgwks_deps`). ERROR-SWALLOW was a claim with no evidence behind it: every
  `.unwrap_or_default()` and every initialized `let _` was reported, including
  `Option` fallbacks and infallible bindings. Fallibility is now resolved from
  the file being scanned — a free function whose written or aliased return type
  is `Result`, a curated table of std routines, or an annotated binding — and a
  receiver resolved to a non-`Result` is never reported. Where the type is not
  visible the finding says so, and its evidence states that the shape is all
  there is rather than implying proof.
- **A shadowing binding satisfied the use check while the error was discarded**
  (`lgwks_deps`). Any ident spelling counted as a read, so a local that rebound
  the error's name cleared the finding and the error was dropped. A read now
  requires a value-position path naming the binding with no enclosing scope
  having rebound it; field and method segments, macro strings, and comments are
  not evidence, and `drop(binding)` is a discard rather than a use.

- **A validated flow could expand to gigabytes from a sub-megabyte document**
  (`lgwks_bot`). `FlowBounds` bounded *steps*, validation bounded the document's
  *shape*, and nothing bounded *bytes*: a template repeating `${answer}` 20,000
  times is a 200 KB document that interpolates to 163,840,000 bytes in one step,
  and four such steps reach 10 GB. Four ceilings now bound it — utterance,
  stored value, computed record expansion, and aggregate session retention — and
  a template's expansion is *sized* with checked arithmetic before it is
  rendered, so the amplification costs a bounded number of additions instead of
  an unbounded allocation. A template whose literal bytes alone exceed the
  ceiling is refused at load, because it could never render at any value.
  `ResourceLimits` lets an operator tighten the shipped ceilings; a document may
  tighten its own and may not raise them.
- **A declared terminal outcome was accepted and then ignored**
  (`lgwks_bot`). `Handoff` and `Refer` computed their outcome from the node
  alone, so a document that said a handoff was not authorized ran as a handoff.
  One calculation now backs both validation and execution, and a declaration
  that contradicts its node is refused at load rather than accepted and dropped.
- **An ask could offer an option the declared variable cannot store**
  (`lgwks_bot`). Validation did not decode ask candidates, so a flow offering an
  unanswerable option loaded cleanly and failed for the person answering it. The
  same decoding that runs at store time now runs at load over every candidate.

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

### lgwks_bot Added

- **A decision carries its provenance, and its receipt is written before the
  transition.** `Resolver::resolve` returns `Verdict`: the `Resolution` and its
  `Provenance` in one value, because "this score came from that model under that
  rule" is one fact and a second call can only re-derive it. `Provenance` names a
  `PolicyVersion` always — content-addressed over the tier's own parameters, so
  retuning changes the revision — and an `EmbedderIdentity` only when a model was
  consulted. `Session` writes a versioned, serializable `DecisionReceipt` per
  decision through a required
  `Journal::record_decision -> Result<ReceiptAcceptance, JournalError>`, binding
  session and flow revision, node, the digest of the *ordered* option list, the
  verdict verbatim, provenance, the selected option's **text** rather than an
  index that moves, and the route. Because the receipt is written first, a
  refused write aborts the answer with `BotError::ReceiptNotRecorded` and leaves
  cursor, scope, transcript and receipt list untouched.

### lgwks_bot Added

- **`Deficit`, `Shortage` and `Demand`: the whole capability shortfall, and the
  repair it derives.** `Auth::check` and `GrantSet::admit` return
  `BotError::CapabilityDenied` naming **every** ungranted requirement rather than
  the first, each `Shortage` carrying the `Demand` — the domain that declared it
  — where the check site knows it. `Bot::build` walks every source and every
  action and reports the whole bot's unmet requirements from one pass. New
  methods: `Auth::uncovered`, `Auth::covers_cap`, `GrantSet::uncovered`,
  `GrantSet::grants`, `Deficit::shortages/len/is_empty/first/to_grant_set`.
- **`Deficit::to_grant_set` derives the repair.** The shortfall already names
  every capability that would close it, so a caller hands back the set the
  deficit derived instead of writing a repair from the message — the step where a
  hand-written repair covers the first line and misses the rest. The repair is
  the *shortfall*, not the requirement: an already-granted capability is not in
  it, and closing the requirement is that set folded into the held one.
- **`Cap` names are `Cow<'static, str>`.** The shipped four were `String`-backed,
  so `Cap::net()` allocated and — because `GrantSet::issue` mints a proof by
  copying the requirement list — every effect execution re-allocated the same
  constants. `Cow::Borrowed` makes a shipped capability a pointer copy and
  authority for it allocation-free. Equality, ordering and hashing compare
  contents, so a `Cap` deserialized from a spec and one built from a constant are
  one capability; a test asserts that in both directions, because if they
  compared unequal the gate would deny a bot it had granted.
- **`Auth::check` is logarithmic in the granted set.** The covered set is sorted
  and de-duplicated at mint and queried by binary search, replacing a linear scan
  inside a loop over the requirement list. The measured defect and its numbers
  are in `bench/README.md`: 12.9 microseconds for 128 capabilities against 3.4
  nanoseconds for one, growing with the *product* of the two counts.

### lgwks_bot Changed

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

## [lgwks_deps 0.1.12] - 2026-09-20

Documentation release. No API change, no behaviour change, and no command-line
change.

### lgwks_deps Fixed

- `lgwks_deps` published no rendered documentation on docs.rs for **0.1.9,
  0.1.10, and 0.1.11**. The manifest declared `all-features = true`, and docs.rs
  builds for `x86_64-unknown-linux-gnu`: `gpui` pulls `objc2`, which refuses to
  compile off Apple targets, and `ml-candle-metal` selects Candle's Metal
  backend. The crate page showed a build error instead of an API. The manifest
  now names the platform-neutral set the CI doc and clippy lanes already verify
  on Linux (`tokio-full`, `appcui`); the platform-bound features stay covered by
  the per-platform jobs.
- 0.1.4 through 0.1.8 are unaffected; the declaration became unbuildable with
  the storefront additions that landed in 0.1.9.
- The `Docs` CI job gained a step that reads each crate's declared
  `[package.metadata.docs.rs]` and builds exactly that set, so an unbuildable
  declaration fails in CI rather than on docs.rs.

### Repository Changed

Not part of any published package; recorded here because it changes files a
reader of this repository sees.

- `AGENTS.md`, `contract/APPROVED.toml`, `skills/`, `experience/`, and one CI
  step name carried the same internal vocabulary the crate docs carried. The
  register's `approved_by` field now reads `maintainer` rather than an internal
  role title; the field is a free-form string and no code validates its value.

## [lgwks_std 0.6.6 / lgwks_bot 0.4.2 / lgwks_ast 0.2.2 / lgwks_deps 0.1.11] - 2026-09-20

Documentation release. No API change in any crate. This supersedes 0.6.5 / 0.4.1 /
0.2.1 / 0.1.10, whose rustdoc carried internal project vocabulary.

### Shared Changed

- The Rust doc comments in all four crates are rewritten. `docs.rs` renders the
  `lib.rs` and module `//!` documentation as the crate front page, not the
  README, so the previous release published a front page a reader arriving from
  crates.io could not act on. 55 Rust files changed.
- Removed throughout: internal project names and shorthand, references to named
  companion repositories, references to an internal governance file, and
  citations to internal issue numbers that a public reader cannot resolve.
- Em dashes across `crates/**/*.rs`: 316 to 56. The 20 that remain on
  doc-comment lines are the ``- `item` — description`` form in feature and
  module lists, which reads as a definition list rather than as prose
  punctuation. The 36 on code lines are the separator in error-message format
  strings, which are program output rather than prose; they are left unchanged
  so that no caller matching on message text is affected.
- No changed line outside a comment except in `lgwks_deps`, listed below. The
  intra-doc link targets, code fences, and fenced example content are byte
  identical to the previous release, so no documented example changed behaviour.

### lgwks_std Fixed

- The `retry` module documentation linked to `crate::random`, which is behind
  the `random` feature. The link resolves under `--all-features`, so it built on
  `docs.rs`, and it failed under `--no-default-features`. Found while verifying
  this release; predates it. The module is now named in code font instead of
  linked. The `docs` CI job gained a `lgwks_std` no-default-features doc build,
  which is the lane that would have caught it.

### lgwks_deps Changed

- The CLI help and admission-ladder text no longer carry internal vocabulary.
  The `about` line now reads `lgwks-deps: dependency admission for the core
  surface`, and the ladder heading reads `The core admission ladder
  (INV-DEP-EDGE-OWNED)`. This is the only user-visible output change in the
  release; no command, flag, exit code, or register format changed.
- Approval fixtures in the contract tests use `reviewer` rather than an internal
  role name. Test-only data.

## [lgwks_std 0.6.5 / lgwks_bot 0.4.1 / lgwks_ast 0.2.1 / lgwks_deps 0.1.10] - 2026-09-20

Documentation release. No API change and no behaviour change in any crate. The
published packages carry a rewritten public surface, which is what a reader
arriving from crates.io or docs.rs reads first.

### Shared Changed

- The root README is restructured as an index: a crate table, a quickstart, the
  use cases, the adjacent-crate comparison, and a documentation index into
  `docs/`. Dependency policy and the consumption contract moved to
  `docs/dependency-doctrine.md`, where the depth belongs.
- Every crate README rewritten for a reader arriving from crates.io. Each opens
  with a statement of what the crate is, then what it does, and the internal
  project vocabulary is gone.
- `docs/` rewritten in the same register. Each admission document states the
  decision, the alternatives measured against it, the authority for it, and the
  verification.
- The four `description` fields rewritten, which is the crates.io listing text.
  `lgwks_std`'s previously used internal shorthand and described the crate as
  async, which its default build is not.
- Manifest and `clippy.toml` comments de-jargoned, because those render on
  docs.rs and in the repository alongside the code.
- The README version-pin CI check now also matches the root README crate table,
  so the table cannot drift from the manifests.
- The root README quickstart is fixed. It did not compile: a tail expression
  returned `io::Error` where `main` promised `Box<dyn Error>`. The `lgwks_deps`
  call it also showed fails in a fresh project, because the gate reads a
  committed register. The snippet now covers `lgwks_std` and `lgwks_bot`, and the
  gate is shown as the CLI sequence it is, including the `cargo generate-lockfile`
  step the gate requires. The snippet is mirrored in
  `crates/lgwks-bot/examples/quickstart.rs`, which `cargo test --all-targets`
  compiles, and a `Docs` CI step fails if the two diverge.
- The root README license pointer no longer names a `LICENSE` file at the
  repository root, which does not exist. Each crate ships its own.

## [lgwks_bot 0.4.0 / lgwks_std 0.6.4 / lgwks_deps 0.1.9 / lgwks_ast 0.2.0] - 2026-09-20

Four crates move together. Two are breaking and take the minor position:
`lgwks_bot` (the ECS substrate and a synchronous `tick`) and `lgwks_ast` (a
renamed enum variant). `lgwks_std` and `lgwks_deps` carry compatible additions
and take patches.

**Breaking.** The four verbs now execute as systems on a `bevy_ecs` schedule,
and that substrate is the only path. There is no feature flag that selects it,
because a default-off implementation is a candidate rather than an architecture.

### lgwks_bot Changed

- `Bot::tick` is **synchronous**. The exclusive systems drive the non-`Send`
  verb futures themselves, so there is nothing for a caller to await.
  `bot.tick().await?` becomes `bot.tick()?`, and the bot binding is now `mut`.
- A condition fires on the tick its source value **moves**
  (`Changed<Revision>`), not on every tick it holds. A source that never changes
  fires nothing.
- A tick is **all-or-nothing**: every source is polled before any effect runs,
  so a failing poll fires nothing and returns the first error. The previous
  contract fired the chains declared before the failure.
- A schedule that cannot be ordered deterministically is refused at `build()`
  (`ambiguity_detection: LogLevel::Error`), not misordered at tick.

### lgwks_bot Removed

- `Bot::block_on_tick` — a pure alias for `tick` once `tick` became
  synchronous. Two equivalent entry points for one job is one too many.
- `Bot::chains()` — replaced by `Bot::source_domains()`, which answers what a
  caller actually wanted ("what is this bot watching") without exposing erased
  observer objects.
- The `Chain`, `ChainEntry`, and `ObserveBuilder::entries` types. `ChainEntry`
  was public only because `chains()` returned it.

### lgwks_bot Added

- `Bot::fired`, `Bot::world`, `Bot::revisions`, `Bot::source_domains`: the
  change-detection state a caller needs to reason about a tick.
- `rt::sync::CancellationToken` (+ `DropGuard`). Every background task must
  listen to one, and the type was previously absent, so the requirement named
  something unobtainable. Built on `tokio::sync::watch`, not `tokio-util`: no
  new third-party edge.
- `rt::supervise` — `Supervisor`, `Budget`, `Outcome`, `repeat`. Background work
  that cannot leak and cannot run away. A task cannot leak because nothing is
  detached (no handle to drop), there is no unbounded constructor and no
  internal queue (a bounded permit is acquired *before* the spawn, and
  `try_spawn` refuses rather than grows), every entry point reaps finished tasks
  so the retained set tracks the live bound rather than the lifetime total, and
  `Drop` cancels and aborts. A loop cannot run away because `repeat` cannot be
  written without a `Budget`, and every iteration *races* cancellation instead
  of checking it between iterations, so a cancel interrupts a body that is
  still awaiting rather than waiting for it to finish.
- `rt::task::LocalSet` and `rt::task::spawn_local`. Every verb is deliberately
  non-`Send`, and `spawn` requires `Send`, so the crate's own futures could not
  be spawned at all.
- `domain::net::NetState::BODY_PREVIEW` is now public. Its cap was documented in
  prose beside a public field, which is how a documented limit drifts from the
  real one; the field's doc links the constant instead, so they cannot disagree.
- `rt::io` (feature `io`): `AsyncRead`/`AsyncWrite`/`AsyncBufRead` and their
  extensions, `BufReader`, `BufWriter`, `duplex`, `copy`. The storefront gained
  `tokio-io` for it, because `io-util` was previously reachable only through the
  whole networking stack. `net`, `process`, and `fs` now imply `io`.

### lgwks_std 0.6.4 Added

- `trace` (default-on): structured, levelled logging via `tracing`, with
  `tracing` registered in `contract/APPROVED.toml` under `owner = "lgwks_std"`.
  Library code in this workspace does not print to the terminal and `tracing`
  is the replacement, but no crate could reach it.
- Measured cost: four crates (`tracing`, `tracing-core`, `pin-project-lite`,
  `once_cell`). `attributes` is off, so `#[instrument]` is unavailable and no
  `syn` proc-macro stack enters the foundation; no subscriber is bundled.
  `--no-default-features --features core` is still genuinely zero-dependency.

### lgwks_deps 0.1.9 Added

- `check --json`: one JSON object on stdout, nothing on stderr, exit code
  unchanged. Keys are always `root`, `enforce`, `admitted`, `approvals`,
  `refusals[]`, `error`.
- `tokio-io` storefront feature; `tokio-net` now builds on it.
- Both JSON printers are built through `lgwks_std::json`. `freshness --json` had
  been escaping only the double quote, so any string containing a backslash
  produced invalid JSON.

### lgwks_deps 0.1.9 Fixed

- `check --json` exited **0** for a repository with no register. The gate
  passing without reading its own contract, which the fail-closed rule exists to
  prevent. Reachable only through the new flag. The exit code checks the error
  arm first, and `admitted` is `error.is_none() && refusals.is_empty()` so a
  payload cannot report admission beside a failed read.

### lgwks_ast 0.2.0 Breaking

- `Language::C` is renamed **`Language::CLang`**. The workspace forbids
  `clippy::min_ident_chars`, and `forbid` cannot be lowered from source, so the
  variant name itself had to change. An `#[allow]` there is a hard E0453, not a
  suppression. The lint fires on the macro's input token, so an alias elsewhere
  would not have helped. `Language::name()` still reports `"c"`, which is the
  stable identity findings and callers match on, so only Rust code naming the
  variant is affected. Nothing in the workspace referenced it.

### lgwks_ast Changed

- `Language` and its four lookup tables are now generated from a single row per
  grammar, so a language cannot be added by halves: the variant, its slot in
  `Language::ALL`, and its arm in each table are gated together. A grammar that
  is compiled out has no variant for a table arm to mention, which is what keeps
  the enum exhaustive without a wildcard.
- `CustomLang` gained explicit extension handling (`with_extensions`,
  `of_path`) and traversal metrics that keep `nodes` saturating, `max_depth`
  monotone, and `has_syntax_issues` sticky.
- `examples/parse.rs` extended.

### Shared Changed

- The bot README is compiled: `#[cfg(doctest)] #[doc =
  include_str!("../README.md")]` runs every Rust block in it. Two defects fell
  out on the first run. A `r#"…"#` literal terminated early by a `"#deploys"`
  payload, and a round-trip whose error types were documented as one type when
  `from_json` returns `BotError` and `to_json` returns `json::Error`.
- crates.io metadata and the GitHub repository description and topics no longer
  use internal vocabulary ("estate", "std+", "lane"), which matched no search.

## [lgwks_deps 0.1.8] - 2026-09-17

### lgwks_deps Added

- Default-off `appcui` storefront feature, pinned to 0.5.1, exposing native
  terminal widgets and input-driven drawing through the storefront owner.
- Admission records the AppCUI selection and its use directive; the authorized
  commit is the policy record, not a cryptographic signature.
- Native feature CI and re-export macro doctest. Publication remains separate.

### Shared verification Changed

- Package-smoke disposal uses the operating-system Trash instead of irreversible
  removal; missing Trash tooling fails loudly and retains the owned artifact.

## [lgwks_std 0.6.3] - 2026-09-13

### Added

- `retry` (core, zero-dep): `RetryPolicy` value type: attempts, exponential
  backoff with caller-supplied jitter, total deadline. Pure data: no I/O, no
  threads, no clock reads. O(1) saturating `delay`.
- `http::Options::idempotency_key`: attach an `Idempotency-Key` header; the
  client never invents the key.
- `ron::FromSliceError::source`: forwards the wrapped UTF-8 / RON error
  instead of dropping the chain.
- Quickstart example (`examples/quickstart.rs`), compiled and run in CI.

### Changed

- `time::ParseError` `Display` now carries the `at` offset on every variant
  (previously dropped on four of six). Messages changed; match on variants,
  not strings.
- `http` documents the pooling boundary: one attempt per call, no connection
  reuse, retries are caller policy via `retry`.

## [lgwks_bot 0.3.2] - 2026-09-13

### Changed

- `error` documents that `String` causes at the domain boundary are
  intentional (typed envelope, escaped payload), narrowing
  INV-BOT-ERROR-TYPED wording without weakening enforcement.
- `cap` documents that `Cap::new` accepts any name by convention and
  enforcement is by grant equality; prefer the shipped constants.
- Both domain paths (`lgwks_bot::net` and `lgwks_bot::domain::net`) documented
  as stable; `BotSpec` documented as validate-only (no `from_spec`
  materializer, no `bot!` macro, by decision; see below).
- `rt` intra-doc links repaired (absolute `crate::` paths) under the new
  `broken_intra_doc_links` deny lane.

## [lgwks_ast 0.1.3] - 2026-09-13

### Added

- Shipped LICENSE (was declared but missing from the package).
- Parse-tour example (`examples/parse.rs`, `lang-rust`-gated).
- Feature-matrix acceptance: `--no-default-features` asserts an empty grammar
  table selects nothing (no more vacuous green).
- Discoverability metadata (keywords) and docs.rs `all-features` config.

## [lgwks_deps 0.1.7] - 2026-09-13

### Added

- Checkout-audit example (`examples/check.rs`) exercising
  `check_dependencies_against` the way CI does.
- Discoverability metadata (categories, keywords) and docs.rs
  `all-features` config.

### Changed

- README leads with the storefront (was gate-first): install-and-select
  snippets with `default-features = false`, tokio-via-facade guidance,
  CLI-vs-library split.

## Shared repo bar (all four crates, 2026-09-13)

- Root `CHANGELOG.md`, `SECURITY.md`, `CONTRIBUTING.md`,
  `docs/distributed-boundaries.md` (mesh transport/identity/time/
  replication/backpressure/observability/schema boundaries, each with an
  owner).
- Shared `[workspace.lints]` (`missing_docs` deny, `unsafe_code` forbid,
  `broken_intra_doc_links` deny) with per-crate opt-in; new CI lanes: docs
  builds (all-features + no-default), per-crate clippy matrix,
  README-version drift guard.
- Crate docs on every `pub mod` listing; README quickstarts pinned at current
  versions.

### Decisions (no code)

- **Declarative API stands; no proc-macro.** Measured: ~26 small heterogeneous
  verb impls; a macro would save tens of lines while dragging `syn` into every
  consumer (banned outside the `lgwks_deps` gate tool), hiding the audited
  `Auth::check` lines, and repeating the already-rejected `thiserror`-in-bot
  tradeoff. Revisit only past ~30 shipped domains with a machine-checkable
  cross-consistency invariant. The `#[serde(crate = ...)]` attribute stays:
  four one-line attributes beat a derive-wrapper macro plus its docs.

## [0.6.2] `lgwks_std` - 2026-09-13

- Keyed BLAKE3 MAC, HTTP custom headers, process-group kill primitive.
- `serde_json` `Deserializer` re-export for prefix-tolerant parsing.

## [0.3.1] `lgwks_bot` - 2026-09-13

- Storefront-owned async runtime surface (`tokio` edge owned by `lgwks_deps`).

## [0.1.6] `lgwks_deps` - 2026-09-13

- Storefront `tokio` engine features; gate-tool `scan` default.

## [0.1.2] `lgwks_ast` - 2026-09-13

- Typed-error derive moved here from `lgwks_std`; `thiserror` owned by this
  job so the core stack stays lean.
